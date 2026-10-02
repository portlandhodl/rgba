// Sprite viewer (mGBA Qt ObjView.cpp): the OAM table with every entry's
// attributes, and the selected sprite's image decoded from VRAM (unflipped
// and untransformed, as ObjView shows it), honouring 1D/2D character mapping
// and 4bpp/8bpp on GBA, 8x8/8x16 and CGB banks on GB.

use eframe::egui;

use super::tile_view::{draw_gb_tile, draw_gba_tile, export_png, gb_palette, show_image};
use super::{no_game, upload_rgba, ToolCtx, ToolWindow};
use crate::emu::Console;
use rgba_gba::video::renderers::OBJ_SIZES_TABLE;

/// One decoded OAM entry, platform-neutral for display.
struct ObjInfo {
    index: usize,
    x: i32,
    y: i32,
    width: usize,
    height: usize,
    tile: usize,
    palette: usize,
    priority: usize,
    flags: String,
    /// Shown in the list (disabled / off-screen entries are dimmed).
    visible: bool,
}

pub struct SpriteView {
    selected: usize,
    scale: f32,
    tex: Option<egui::TextureHandle>,
    pixels: Vec<u32>,
}

impl Default for SpriteView {
    fn default() -> Self {
        SpriteView { selected: 0, scale: 4.0, tex: None, pixels: Vec::new() }
    }
}

fn gba_objs(oam: &[u8]) -> Vec<ObjInfo> {
    (0..128)
        .map(|i| {
            let r = |o: usize| u16::from_le_bytes([oam[i * 8 + o], oam[i * 8 + o + 1]]);
            let (a, b, c) = (r(0), r(2), r(4));
            let affine = a & 0x100 != 0;
            let disabled = !affine && a & 0x200 != 0;
            let double = affine && a & 0x200 != 0;
            let shape = ((a >> 14) & 3) as usize;
            let size = ((b >> 14) & 3) as usize;
            let [w, h] = OBJ_SIZES_TABLE[shape * 4 + size];
            let mode = (a >> 10) & 3;
            let mut flags = Vec::new();
            if affine {
                flags.push(format!("affine #{}", (b >> 9) & 0x1F));
            }
            if double {
                flags.push("double size".into());
            }
            if disabled {
                flags.push("disabled".into());
            }
            if a & 0x1000 != 0 {
                flags.push("mosaic".into());
            }
            if a & 0x2000 != 0 {
                flags.push("256 colors".into());
            }
            match mode {
                1 => flags.push("semi-transparent".into()),
                2 => flags.push("OBJ window".into()),
                3 => flags.push("mode 3 (invalid)".into()),
                _ => {}
            }
            if !affine && b & 0x1000 != 0 {
                flags.push("hflip".into());
            }
            if !affine && b & 0x2000 != 0 {
                flags.push("vflip".into());
            }
            ObjInfo {
                index: i,
                x: ((b & 0x1FF) as i32) << 23 >> 23,
                y: (a & 0xFF) as i32,
                width: w as usize,
                height: h as usize,
                tile: (c & 0x3FF) as usize,
                palette: ((c >> 12) & 0xF) as usize,
                priority: ((c >> 10) & 3) as usize,
                flags: flags.join(", "),
                visible: !disabled && w > 0,
            }
        })
        .collect()
}

fn gb_objs(oam: &[u8], tall: bool, cgb: bool) -> Vec<ObjInfo> {
    (0..40)
        .map(|i| {
            let (y, x, tile, attr) = (oam[i * 4], oam[i * 4 + 1], oam[i * 4 + 2], oam[i * 4 + 3]);
            let mut flags = Vec::new();
            if attr & 0x80 != 0 {
                flags.push("behind BG".to_string());
            }
            if attr & 0x40 != 0 {
                flags.push("vflip".into());
            }
            if attr & 0x20 != 0 {
                flags.push("hflip".into());
            }
            if cgb && attr & 0x08 != 0 {
                flags.push("VRAM bank 1".into());
            }
            ObjInfo {
                index: i,
                x: x as i32 - 8,
                y: y as i32 - 16,
                width: 8,
                height: if tall { 16 } else { 8 },
                tile: tile as usize,
                palette: if cgb { (attr & 7) as usize } else { ((attr >> 4) & 1) as usize },
                priority: ((attr >> 7) & 1) as usize,
                flags: flags.join(", "),
                visible: y != 0 && y < 160 && x != 0 && x < 168,
            }
        })
        .collect()
}

impl SpriteView {
    fn render_gba(&mut self, vram: &[u8], palette: &[u8], oam: &[u8], dispcnt: u16, o: &ObjInfo) {
        let c = u16::from_le_bytes([oam[o.index * 8 + 4], oam[o.index * 8 + 5]]);
        let a = u16::from_le_bytes([oam[o.index * 8], oam[o.index * 8 + 1]]);
        let bpp8 = a & 0x2000 != 0;
        let mapping_1d = dispcnt & 0x40 != 0;
        let (w, h) = (o.width, o.height);
        self.pixels.clear();
        self.pixels.resize(w * h, 0xFF00_0000);
        let step = if bpp8 { 2 } else { 1 };
        let base = (c & 0x3FF) as usize & if bpp8 && !mapping_1d { !1 } else { !0 };
        for ty in 0..h / 8 {
            for tx in 0..w / 8 {
                let tile = if mapping_1d {
                    base + (ty * (w / 8) + tx) * step
                } else {
                    base + ty * 32 + tx * step
                };
                let addr = 0x10000 + (tile * 32) % 0x8000;
                draw_gba_tile(&mut self.pixels, w, tx * 8, ty * 8, vram, palette, addr, bpp8, 256, o.palette, false, false, false);
            }
        }
    }

    fn render_gb(&mut self, vram: &[u8], palette: &[u16; 64], oam: &[u8], cgb: bool, o: &ObjInfo) {
        let attr = oam[o.index * 4 + 3];
        let bank = if cgb && attr & 0x08 != 0 { 0x2000 } else { 0 };
        let colors = gb_palette(palette, 8 + o.palette);
        let (w, h) = (8, o.height);
        self.pixels.clear();
        self.pixels.resize(w * h, 0xFF00_0000);
        let tile = if h == 16 { o.tile & !1 } else { o.tile };
        for ty in 0..h / 8 {
            draw_gb_tile(&mut self.pixels, w, 0, ty * 8, vram, bank + (tile + ty) * 16, &colors, false, false, false);
        }
    }
}

impl ToolWindow for SpriteView {
    fn title(&self) -> &'static str {
        "Sprites"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        egui::Window::new(self.title()).open(open).default_size([620.0, 460.0]).show(ctx, |ui| {
            let Some(s) = tc.session.as_deref_mut() else {
                no_game(ui);
                return;
            };
            let (objs, is_gba) = match &s.console {
                Console::Gba(g) => (gba_objs(&g.video.oam), true),
                Console::Gb(g) => {
                    let tall = g.memory.io[0x40] & 0x04 != 0;
                    (gb_objs(&g.video.oam, tall, g.model.is_cgb()), false)
                }
            };
            self.selected = self.selected.min(objs.len() - 1);
            let o = &objs[self.selected];
            match &s.console {
                Console::Gba(g) => self.render_gba(&g.video.vram, &g.video.palette, &g.video.oam, g.memory.io[0], o),
                Console::Gb(g) => self.render_gb(&g.video.vram, &g.video.palette, &g.video.oam, g.model.is_cgb(), o),
            }
            let (w, h) = (o.width.max(1), o.height.max(1));
            if self.pixels.len() < w * h {
                self.pixels.resize(w * h, 0xFF00_0000);
            }
            let tex = upload_rgba(ctx, &mut self.tex, "sprite_view", &self.pixels, w, h);
            let mut export = false;

            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(250.0);
                    egui::ScrollArea::vertical().id_salt("objlist").max_height(400.0).show(ui, |ui| {
                        for info in &objs {
                            let text = format!(
                                "#{:3} {:>4},{:>4} {}x{}",
                                info.index, info.x, info.y, info.width, info.height
                            );
                            let mut rt = egui::RichText::new(text).monospace();
                            if !info.visible {
                                rt = rt.weak();
                            }
                            if ui.selectable_label(self.selected == info.index, rt).clicked() {
                                self.selected = info.index;
                            }
                        }
                    });
                });
                ui.separator();
                ui.vertical(|ui| {
                    show_image(ui, &tex, w, h, self.scale);
                    ui.add(egui::Slider::new(&mut self.scale, 1.0..=8.0).step_by(1.0).text("Magnification"));
                    egui::Grid::new("objinfo").num_columns(2).show(ui, |ui| {
                        let row = |ui: &mut egui::Ui, k: &str, v: String| {
                            ui.label(k);
                            ui.label(v);
                            ui.end_row();
                        };
                        row(ui, "Index", o.index.to_string());
                        row(ui, "Position", format!("{}, {}", o.x, o.y));
                        row(ui, "Size", format!("{}×{}", o.width, o.height));
                        row(ui, "Tile", if is_gba { format!("{} (0x{:03X})", o.tile, o.tile) } else { format!("{} (0x{:02X})", o.tile, o.tile) });
                        row(ui, "Palette", o.palette.to_string());
                        row(ui, "Priority", o.priority.to_string());
                        row(ui, "Flags", if o.flags.is_empty() { "—".into() } else { o.flags.clone() });
                        if is_gba {
                            row(ui, "Address", format!("0x{:08X}", 0x0700_0000 + o.index * 8));
                        } else {
                            row(ui, "Address", format!("0x{:04X}", 0xFE00 + o.index * 4));
                        }
                    });
                    if ui.button("Export PNG...").clicked() {
                        export = true;
                    }
                });
            });
            if export {
                let pixels = self.pixels[..w * h].to_vec();
                let name = format!("sprite-{}.png", self.selected);
                export_png(tc, &name, &pixels, w, h);
            }
        });
    }

    fn on_session_changed(&mut self) {
        self.tex = None;
        self.selected = 0;
    }
}
