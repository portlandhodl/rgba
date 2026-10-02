// Map viewer (mGBA Qt MapView.cpp): a background layer's whole tilemap.
// GBA: text BGs (size from BGCNT), affine BG2/BG3 in modes 1/2, and the
// mode 3/4/5 bitmaps. GB: the BG or window map selected by LCDC. The visible
// 240x160 / 160x144 viewport is outlined for scrolled (text/GB) layers.

use eframe::egui;

use super::tile_view::{draw_gb_tile, draw_gba_tile, export_png, gb_palette, gba_color, hovered_pixel, show_image};
use super::{bgr555_to_argb, no_game, upload_rgba, ToolCtx, ToolWindow};
use crate::emu::Console;

pub struct MapView {
    layer: usize,
    scale: f32,
    show_viewport: bool,
    tex: Option<egui::TextureHandle>,
    pixels: Vec<u32>,
}

impl Default for MapView {
    fn default() -> Self {
        MapView { layer: 0, scale: 1.0, show_viewport: true, tex: None, pixels: Vec::new() }
    }
}

/// What was rendered, for the info line and the viewport overlay.
struct Rendered {
    w: usize,
    h: usize,
    /// Scroll origin of the visible screen, when meaningful.
    viewport: Option<(usize, usize, usize, usize)>,
    info: String,
    /// Describes the map entry under a map pixel (hover text).
    hover: Option<Box<dyn Fn(usize, usize) -> String>>,
}

#[derive(Clone, Copy, PartialEq)]
enum GbaBgKind {
    Text,
    Affine,
    Bitmap(u16),
    Off,
}

fn gba_bg_kind(mode: u16, bg: usize) -> GbaBgKind {
    match (mode, bg) {
        (0, _) | (1, 0) | (1, 1) => GbaBgKind::Text,
        (1, 2) | (2, 2) | (2, 3) => GbaBgKind::Affine,
        (3..=5, 2) => GbaBgKind::Bitmap(mode),
        _ => GbaBgKind::Off,
    }
}

impl MapView {
    fn render_gba(&mut self, vram: &[u8], palette: &[u8], io: &[u16]) -> Rendered {
        let dispcnt = io[0];
        let mode = dispcnt & 7;
        let bg = self.layer.min(3);
        let cnt = io[4 + bg];
        let char_base = ((cnt >> 2) & 3) as usize * 0x4000;
        let screen_base = ((cnt >> 8) & 0x1F) as usize * 0x800;
        let size = ((cnt >> 14) & 3) as usize;
        let enabled = dispcnt & (0x100 << bg) != 0;
        let state = if enabled { "" } else { " (disabled in DISPCNT)" };
        match gba_bg_kind(mode, bg) {
            GbaBgKind::Text => {
                let (w, h) = [(256, 256), (512, 256), (256, 512), (512, 512)][size];
                let bpp8 = cnt & 0x80 != 0;
                self.pixels.clear();
                self.pixels.resize(w * h, 0xFF00_0000);
                for ty in 0..h / 8 {
                    for tx in 0..w / 8 {
                        let sb = tx / 32 + (ty / 32) * (w / 256);
                        let addr = screen_base + sb * 0x800 + ((ty % 32) * 32 + tx % 32) * 2;
                        let e = u16::from_le_bytes([vram[addr % 0x10000], vram[(addr + 1) % 0x10000]]) as usize;
                        let tile = e & 0x3FF;
                        let taddr = char_base + tile * if bpp8 { 64 } else { 32 };
                        if taddr < 0x10000 {
                            draw_gba_tile(&mut self.pixels, w, tx * 8, ty * 8, vram, palette, taddr, bpp8, 0, e >> 12, e & 0x400 != 0, e & 0x800 != 0, false);
                        }
                    }
                }
                let (hofs, vofs) = ((io[8 + 2 * bg] & 0x1FF) as usize, (io[9 + 2 * bg] & 0x1FF) as usize);
                let vram_owned: Vec<u8> = vram[screen_base.min(0x10000)..(screen_base + 0x2000).min(0x10000)].to_vec();
                Rendered {
                    w,
                    h,
                    viewport: Some((hofs % w, vofs % h, 240, 160)),
                    info: format!(
                        "BG{bg}: text {w}×{h}, {}, char base 0x{:08X}, screen base 0x{:08X}, priority {}, scroll ({hofs}, {vofs}){state}",
                        if bpp8 { "256 colors" } else { "16 colors" },
                        0x0600_0000 + char_base,
                        0x0600_0000 + screen_base,
                        cnt & 3
                    ),
                    hover: Some(Box::new(move |x, y| {
                        let (tx, ty) = (x / 8, y / 8);
                        let sb = tx / 32 + (ty / 32) * (w / 256);
                        let off = sb * 0x800 + ((ty % 32) * 32 + tx % 32) * 2;
                        let e = vram_owned.get(off..off + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
                        format!(
                            "Tile ({tx}, {ty}) at 0x{:08X}: tile {} palette {}{}{}",
                            0x0600_0000 + screen_base + off,
                            e & 0x3FF,
                            e >> 12,
                            if e & 0x400 != 0 { " hflip" } else { "" },
                            if e & 0x800 != 0 { " vflip" } else { "" }
                        )
                    })),
                }
            }
            GbaBgKind::Affine => {
                let side = 128 << size;
                let tiles = side / 8;
                self.pixels.clear();
                self.pixels.resize(side * side, 0xFF00_0000);
                for ty in 0..tiles {
                    for tx in 0..tiles {
                        let tile = vram[(screen_base + ty * tiles + tx) % 0x10000] as usize;
                        let taddr = char_base + tile * 64;
                        if taddr < 0x10000 {
                            draw_gba_tile(&mut self.pixels, side, tx * 8, ty * 8, vram, palette, taddr, true, 0, 0, false, false, false);
                        }
                    }
                }
                Rendered {
                    w: side,
                    h: side,
                    viewport: None,
                    info: format!(
                        "BG{bg}: affine {side}×{side}, char base 0x{:08X}, screen base 0x{:08X}, {}{state}",
                        0x0600_0000 + char_base,
                        0x0600_0000 + screen_base,
                        if cnt & 0x2000 != 0 { "wraps" } else { "no wrap" }
                    ),
                    hover: None,
                }
            }
            GbaBgKind::Bitmap(m) => {
                let frame = if dispcnt & 0x10 != 0 { 0xA000 } else { 0 };
                let (w, h) = if m == 5 { (160, 128) } else { (240, 160) };
                self.pixels.clear();
                self.pixels.resize(w * h, 0xFF00_0000);
                for y in 0..h {
                    for x in 0..w {
                        self.pixels[y * w + x] = match m {
                            3 => {
                                let a = (y * 240 + x) * 2;
                                bgr555_to_argb(u16::from_le_bytes([vram[a], vram[a + 1]]))
                            }
                            4 => gba_color(palette, vram[frame + y * 240 + x] as usize),
                            _ => {
                                let a = frame + (y * 160 + x) * 2;
                                bgr555_to_argb(u16::from_le_bytes([vram[a], vram[a + 1]]))
                            }
                        };
                    }
                }
                Rendered {
                    w,
                    h,
                    viewport: None,
                    info: format!(
                        "BG2: mode {m} bitmap {w}×{h}{}{state}",
                        if m == 3 { String::new() } else { format!(", frame {}", frame / 0xA000) }
                    ),
                    hover: None,
                }
            }
            GbaBgKind::Off => {
                self.pixels.clear();
                self.pixels.resize(8 * 8, 0xFF00_0000);
                Rendered { w: 8, h: 8, viewport: None, info: format!("BG{bg} does not exist in mode {mode}."), hover: None }
            }
        }
    }

    fn render_gb(&mut self, vram: &[u8], palette: &[u16; 64], io: &[u8], cgb: bool) -> Rendered {
        let lcdc = io[0x40];
        let window = self.layer == 1;
        let map_hi = if window { lcdc & 0x40 != 0 } else { lcdc & 0x08 != 0 };
        let map_base = if map_hi { 0x1C00 } else { 0x1800 };
        let unsigned = lcdc & 0x10 != 0;
        let (w, h) = (256, 256);
        self.pixels.clear();
        self.pixels.resize(w * h, 0xFF00_0000);
        for ty in 0..32 {
            for tx in 0..32 {
                let m = map_base + ty * 32 + tx;
                let t = vram[m];
                let attr = if cgb { vram.get(0x2000 + m).copied().unwrap_or(0) } else { 0 };
                let addr = if unsigned { t as usize * 16 } else { (0x1000 + (t as i8 as i32) * 16) as usize };
                let bank = if attr & 0x08 != 0 { 0x2000 } else { 0 };
                let colors = gb_palette(palette, (attr & 7) as usize);
                draw_gb_tile(&mut self.pixels, w, tx * 8, ty * 8, vram, bank + addr, &colors, attr & 0x20 != 0, attr & 0x40 != 0, false);
            }
        }
        let (scx, scy) = (io[0x43] as usize, io[0x42] as usize);
        let (wx, wy) = (io[0x4B], io[0x4A]);
        let enabled = if window { lcdc & 0x20 != 0 } else { lcdc & 0x01 != 0 || cgb };
        Rendered {
            w,
            h,
            viewport: (!window).then_some((scx, scy, 160, 144)),
            info: format!(
                "{}: map 0x{:04X}, tiles at 0x{}, {}{}",
                if window { "Window" } else { "Background" },
                0x8000 + map_base,
                if unsigned { "8000 (unsigned)" } else { "8800 (signed)" },
                if window { format!("WX {wx} WY {wy}") } else { format!("scroll ({scx}, {scy})") },
                if enabled { "" } else { " (disabled in LCDC)" }
            ),
            hover: Some(Box::new(move |x, y| {
                format!("Tile ({}, {}) at 0x{:04X}", x / 8, y / 8, 0x8000 + map_base + (y / 8) * 32 + x / 8)
            })),
        }
    }
}

/// Outline the visible screen on a wrapping map (split at the map edges).
fn draw_viewport(painter: &egui::Painter, origin: egui::Pos2, scale: f32, map: (usize, usize), vp: (usize, usize, usize, usize)) {
    let (mw, mh) = (map.0 as f32, map.1 as f32);
    let (x0, y0, vw, vh) = (vp.0 as f32, vp.1 as f32, vp.2 as f32, vp.3 as f32);
    let stroke = egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 64, 64));
    for (sx, sw) in [(x0, (mw - x0).min(vw)), (0.0, (x0 + vw - mw).max(0.0))] {
        for (sy, sh) in [(y0, (mh - y0).min(vh)), (0.0, (y0 + vh - mh).max(0.0))] {
            if sw <= 0.0 || sh <= 0.0 {
                continue;
            }
            let r = egui::Rect::from_min_size(origin + egui::vec2(sx * scale, sy * scale), egui::vec2(sw * scale, sh * scale));
            painter.rect_stroke(r, 0.0, stroke, egui::StrokeKind::Inside);
        }
    }
}

impl ToolWindow for MapView {
    fn title(&self) -> &'static str {
        "Maps"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        egui::Window::new(self.title()).open(open).default_size([560.0, 620.0]).show(ctx, |ui| {
            let Some(s) = tc.session.as_deref_mut() else {
                no_game(ui);
                return;
            };
            let is_gba = matches!(s.console, Console::Gba(_));
            ui.horizontal(|ui| {
                if is_gba {
                    for i in 0..4 {
                        ui.selectable_value(&mut self.layer, i, format!("BG{i}"));
                    }
                } else {
                    self.layer = self.layer.min(1);
                    ui.selectable_value(&mut self.layer, 0, "Background");
                    ui.selectable_value(&mut self.layer, 1, "Window");
                }
                ui.separator();
                ui.checkbox(&mut self.show_viewport, "Show viewport");
                ui.add(egui::Slider::new(&mut self.scale, 1.0..=4.0).step_by(1.0).text("Magnification"));
            });
            let r = match &s.console {
                Console::Gba(g) => self.render_gba(&g.video.vram, &g.video.palette, &g.memory.io),
                Console::Gb(g) => {
                    let cgb = g.model.is_cgb();
                    self.render_gb(&g.video.vram, &g.video.palette, &g.memory.io, cgb)
                }
            };
            ui.label(&r.info);
            let tex = upload_rgba(ctx, &mut self.tex, "map_view", &self.pixels, r.w, r.h);
            let mut hover = String::from(" ");
            egui::ScrollArea::both().max_height(ui.available_height() - 40.0).show(ui, |ui| {
                let resp = show_image(ui, &tex, r.w, r.h, self.scale);
                if self.show_viewport {
                    if let Some(vp) = r.viewport {
                        draw_viewport(ui.painter(), resp.rect.min, self.scale, (r.w, r.h), vp);
                    }
                }
                if let (Some((x, y)), Some(f)) = (hovered_pixel(&resp, self.scale), r.hover.as_ref()) {
                    if x < r.w && y < r.h {
                        hover = f(x, y);
                    }
                }
            });
            let mut export = false;
            ui.horizontal(|ui| {
                ui.label(hover);
                export = ui.button("Export PNG...").clicked();
            });
            if export {
                let pixels = self.pixels.clone();
                export_png(tc, "map.png", &pixels, r.w, r.h);
            }
        });
    }

    fn on_session_changed(&mut self) {
        self.tex = None;
    }
}
