// Frame inspector (mGBA Qt FrameView.cpp): the frame broken into its
// layers. mGBA re-runs the frame through a logging renderer to capture each
// layer exactly; here each BG and OBJ layer is re-rendered independently
// from the current VRAM/OAM/palette state using the registers as they are
// now (i.e. without mid-frame raster effects, windows or blending), then
// composited by priority. Toggle layers to see what each contributes.

use eframe::egui;

use super::tile_view::{gb_palette, gb_tile_pixel, gba_color, show_image};
use super::{no_game, upload_rgba, ToolCtx, ToolWindow};
use crate::emu::Console;
use rgba_gba::video::renderers::OBJ_SIZES_TABLE;

const TRANSPARENT: u32 = 0;

struct Layer {
    name: String,
    /// Lower draws on top (GBA priority; GB: BG 3, window 2, OBJ 0).
    priority: u32,
    /// Within one priority, lower draws on top (BG index; OBJ beats BGs).
    order: u32,
    /// 0 = transparent, otherwise 0xFFRRGGBB.
    pixels: Vec<u32>,
    visible: bool,
}

pub struct FrameInspector {
    layers: Vec<Layer>,
    backdrop: u32,
    dims: (usize, usize),
    live: bool,
    captured: bool,
    /// None = composite, Some(i) = solo layer i.
    view: Option<usize>,
    scale: f32,
    tex: Option<egui::TextureHandle>,
    composite: Vec<u32>,
}

impl Default for FrameInspector {
    fn default() -> Self {
        FrameInspector {
            layers: Vec::new(),
            backdrop: 0xFF00_0000,
            dims: (240, 160),
            live: true,
            captured: false,
            view: None,
            scale: 2.0,
            tex: None,
            composite: Vec::new(),
        }
    }
}

// ---- GBA ------------------------------------------------------------------------

fn tile_index(vram: &[u8], addr: usize, bpp8: bool, sx: usize, sy: usize) -> usize {
    if bpp8 {
        vram.get(addr + sy * 8 + sx).copied().unwrap_or(0) as usize
    } else {
        let b = vram.get(addr + sy * 4 + sx / 2).copied().unwrap_or(0);
        ((b >> ((sx & 1) * 4)) & 0xF) as usize
    }
}

fn gba_text_layer(vram: &[u8], palette: &[u8], io: &[u16], bg: usize) -> Vec<u32> {
    let cnt = io[4 + bg];
    let char_base = ((cnt >> 2) & 3) as usize * 0x4000;
    let screen_base = ((cnt >> 8) & 0x1F) as usize * 0x800;
    let bpp8 = cnt & 0x80 != 0;
    let (w, h) = [(256, 256), (512, 256), (256, 512), (512, 512)][((cnt >> 14) & 3) as usize];
    let (hofs, vofs) = ((io[8 + 2 * bg] & 0x1FF) as usize, (io[9 + 2 * bg] & 0x1FF) as usize);
    let mut out = vec![TRANSPARENT; 240 * 160];
    for y in 0..160 {
        for x in 0..240 {
            let (mx, my) = ((x + hofs) % w, (y + vofs) % h);
            let (tx, ty) = (mx / 8, my / 8);
            let sb = tx / 32 + (ty / 32) * (w / 256);
            let a = (screen_base + sb * 0x800 + ((ty % 32) * 32 + tx % 32) * 2) % 0x10000;
            let e = u16::from_le_bytes([vram[a], vram[a + 1]]) as usize;
            let mut sx = mx & 7;
            let mut sy = my & 7;
            if e & 0x400 != 0 {
                sx = 7 - sx;
            }
            if e & 0x800 != 0 {
                sy = 7 - sy;
            }
            let taddr = char_base + (e & 0x3FF) * if bpp8 { 64 } else { 32 };
            if taddr >= 0x10000 {
                continue;
            }
            let idx = tile_index(vram, taddr, bpp8, sx, sy);
            if idx != 0 {
                let entry = if bpp8 { idx } else { (e >> 12) * 16 + idx };
                out[y * 240 + x] = gba_color(palette, entry);
            }
        }
    }
    out
}

fn reg28(lo: u16, hi: u16) -> i32 {
    (((hi as u32) << 16 | lo as u32) << 4) as i32 >> 4
}

fn gba_affine_layer(vram: &[u8], palette: &[u8], io: &[u16], bg: usize) -> Vec<u32> {
    let cnt = io[4 + bg];
    let char_base = ((cnt >> 2) & 3) as usize * 0x4000;
    let screen_base = ((cnt >> 8) & 0x1F) as usize * 0x800;
    let side = 128i32 << ((cnt >> 14) & 3);
    let wrap = cnt & 0x2000 != 0;
    // BG2PA.. at 0x20 (BG2) / 0x30 (BG3); BGnX/Y follow the matrix.
    let r = if bg == 2 { 0x10 } else { 0x18 };
    let (pa, pb, pc, pd) = (io[r] as i16 as i32, io[r + 1] as i16 as i32, io[r + 2] as i16 as i32, io[r + 3] as i16 as i32);
    let (x0, y0) = (reg28(io[r + 4], io[r + 5]), reg28(io[r + 6], io[r + 7]));
    let mut out = vec![TRANSPARENT; 240 * 160];
    for y in 0..160i32 {
        for x in 0..240i32 {
            let mut tx = (x0 + pa * x + pb * y) >> 8;
            let mut ty = (y0 + pc * x + pd * y) >> 8;
            if wrap {
                tx = tx.rem_euclid(side);
                ty = ty.rem_euclid(side);
            } else if tx < 0 || ty < 0 || tx >= side || ty >= side {
                continue;
            }
            let m = (screen_base + (ty / 8 * (side / 8) + tx / 8) as usize) % 0x10000;
            let taddr = char_base + vram[m] as usize * 64;
            if taddr >= 0x10000 {
                continue;
            }
            let idx = tile_index(vram, taddr, true, (tx & 7) as usize, (ty & 7) as usize);
            if idx != 0 {
                out[(y * 240 + x) as usize] = gba_color(palette, idx);
            }
        }
    }
    out
}

fn gba_bitmap_layer(vram: &[u8], palette: &[u8], dispcnt: u16) -> Vec<u32> {
    let mode = dispcnt & 7;
    let frame = if dispcnt & 0x10 != 0 { 0xA000 } else { 0 };
    let mut out = vec![TRANSPARENT; 240 * 160];
    for y in 0..160 {
        for x in 0..240 {
            out[y * 240 + x] = match mode {
                3 => {
                    let a = (y * 240 + x) * 2;
                    super::bgr555_to_argb(u16::from_le_bytes([vram[a], vram[a + 1]]))
                }
                4 => {
                    let i = vram[frame + y * 240 + x] as usize;
                    if i == 0 { TRANSPARENT } else { gba_color(palette, i) }
                }
                _ if x < 160 && y < 128 => {
                    let a = frame + (y * 160 + x) * 2;
                    super::bgr555_to_argb(u16::from_le_bytes([vram[a], vram[a + 1]]))
                }
                _ => TRANSPARENT,
            };
        }
    }
    out
}

/// OBJ layers, one per priority (index 0 drawn on top within a priority).
fn gba_obj_layers(vram: &[u8], palette: &[u8], oam: &[u8], dispcnt: u16) -> [Vec<u32>; 4] {
    let mut layers: [Vec<u32>; 4] = std::array::from_fn(|_| vec![TRANSPARENT; 240 * 160]);
    let mapping_1d = dispcnt & 0x40 != 0;
    let bitmap = (dispcnt & 7) >= 3;
    for i in (0..128).rev() {
        let r = |o: usize| u16::from_le_bytes([oam[i * 8 + o], oam[i * 8 + o + 1]]);
        let (a, b, c) = (r(0), r(2), r(4));
        let affine = a & 0x100 != 0;
        if !affine && a & 0x200 != 0 {
            continue;
        }
        if (a >> 10) & 3 >= 2 {
            continue; // OBJ window / invalid: not visible pixels
        }
        let [w, h] = OBJ_SIZES_TABLE[((a >> 14) & 3) as usize * 4 + ((b >> 14) & 3) as usize];
        if w == 0 {
            continue;
        }
        let (w, h) = (w as i32, h as i32);
        let double = affine && a & 0x200 != 0;
        let (bw, bh) = if double { (w * 2, h * 2) } else { (w, h) };
        let mut ox = ((b & 0x1FF) as i32) << 23 >> 23;
        let mut oy = (a & 0xFF) as i32;
        if oy + bh > 256 {
            oy -= 256;
        }
        if ox + bw > 512 {
            ox -= 512;
        }
        let bpp8 = a & 0x2000 != 0;
        let tile = (c & 0x3FF) as usize;
        if bitmap && tile < 512 {
            continue;
        }
        let pal = ((c >> 12) & 0xF) as usize;
        let prio = ((c >> 10) & 3) as usize;
        let (pa, pb, pc, pd) = if affine {
            let m = ((b >> 9) & 0x1F) as usize * 32;
            let g = |o: usize| i16::from_le_bytes([oam[m + o], oam[m + o + 1]]) as i32;
            (g(6), g(14), g(22), g(30))
        } else {
            (0x100, 0, 0, 0x100)
        };
        for py in 0..bh {
            let sy = oy + py;
            if !(0..160).contains(&sy) {
                continue;
            }
            for px in 0..bw {
                let sx = ox + px;
                if !(0..240).contains(&sx) {
                    continue;
                }
                let (mut tx, mut ty) = if affine {
                    let (dx, dy) = (px - bw / 2, py - bh / 2);
                    (((pa * dx + pb * dy) >> 8) + w / 2, ((pc * dx + pd * dy) >> 8) + h / 2)
                } else {
                    (px, py)
                };
                if tx < 0 || ty < 0 || tx >= w || ty >= h {
                    continue;
                }
                if !affine {
                    if b & 0x1000 != 0 {
                        tx = w - 1 - tx;
                    }
                    if b & 0x2000 != 0 {
                        ty = h - 1 - ty;
                    }
                }
                let (cx, cy) = ((tx / 8) as usize, (ty / 8) as usize);
                let step = if bpp8 { 2 } else { 1 };
                let t = if mapping_1d {
                    tile + (cy * (w as usize / 8) + cx) * step
                } else {
                    (tile & if bpp8 { !1 } else { !0 }) + cy * 32 + cx * step
                };
                let addr = 0x10000 + (t * 32) % 0x8000;
                let idx = tile_index(vram, addr, bpp8, (tx & 7) as usize, (ty & 7) as usize);
                if idx != 0 {
                    let entry = 256 + if bpp8 { idx } else { pal * 16 + idx };
                    layers[prio][(sy * 240 + sx) as usize] = gba_color(palette, entry);
                }
            }
        }
    }
    layers
}

// ---- GB -------------------------------------------------------------------------

fn gb_map_pixel(vram: &[u8], palette: &[u16; 64], lcdc: u8, map_hi: bool, cgb: bool, mx: usize, my: usize) -> (usize, u32) {
    let map_base = if map_hi { 0x1C00 } else { 0x1800 };
    let m = map_base + (my / 8) * 32 + mx / 8;
    let t = vram[m];
    let attr = if cgb { vram.get(0x2000 + m).copied().unwrap_or(0) } else { 0 };
    let addr = if lcdc & 0x10 != 0 { t as usize * 16 } else { (0x1000 + (t as i8 as i32) * 16) as usize };
    let bank = if attr & 0x08 != 0 { 0x2000 } else { 0 };
    let mut sx = mx & 7;
    let mut sy = my & 7;
    if attr & 0x20 != 0 {
        sx = 7 - sx;
    }
    if attr & 0x40 != 0 {
        sy = 7 - sy;
    }
    let v = gb_tile_pixel(vram, bank + addr, sx, sy);
    (v, gb_palette(palette, (attr & 7) as usize)[v])
}

fn gb_layers(vram: &[u8], palette: &[u16; 64], oam: &[u8], io: &[u8], cgb: bool) -> Vec<Layer> {
    let lcdc = io[0x40];
    let (scy, scx, wy, wx) = (io[0x42] as usize, io[0x43] as usize, io[0x4A] as usize, io[0x4B] as i32 - 7);
    let (w, h) = (160usize, 144usize);
    let mut bg = vec![TRANSPARENT; w * h];
    let mut win = vec![TRANSPARENT; w * h];
    let mut obj = vec![TRANSPARENT; w * h];
    for y in 0..h {
        for x in 0..w {
            let (_, c) = gb_map_pixel(vram, palette, lcdc, lcdc & 0x08 != 0, cgb, (x + scx) & 0xFF, (y + scy) & 0xFF);
            bg[y * w + x] = c;
            if lcdc & 0x20 != 0 && y >= wy && x as i32 >= wx {
                let (_, c) = gb_map_pixel(vram, palette, lcdc, lcdc & 0x40 != 0, cgb, (x as i32 - wx) as usize, y - wy);
                win[y * w + x] = c;
            }
        }
    }
    let tall = lcdc & 0x04 != 0;
    for i in (0..40).rev() {
        let (oy, ox, tile, attr) = (oam[i * 4] as i32 - 16, oam[i * 4 + 1] as i32 - 8, oam[i * 4 + 2] as usize, oam[i * 4 + 3]);
        let oh = if tall { 16 } else { 8 };
        let pal = if cgb { 8 + (attr & 7) as usize } else { 8 + ((attr >> 4) & 1) as usize };
        let colors = gb_palette(palette, pal);
        let bank = if cgb && attr & 0x08 != 0 { 0x2000 } else { 0 };
        for py in 0..oh {
            let sy = oy + py;
            if !(0..h as i32).contains(&sy) {
                continue;
            }
            for px in 0..8 {
                let sx = ox + px;
                if !(0..w as i32).contains(&sx) {
                    continue;
                }
                let tx = if attr & 0x20 != 0 { 7 - px } else { px } as usize;
                let ty = if attr & 0x40 != 0 { oh - 1 - py } else { py } as usize;
                let t = if tall { (tile & !1) + ty / 8 } else { tile };
                let v = gb_tile_pixel(vram, bank + t * 16, tx, ty & 7);
                if v != 0 {
                    obj[sy as usize * w + sx as usize] = colors[v];
                }
            }
        }
    }
    let bg_on = lcdc & 0x01 != 0 || cgb;
    vec![
        Layer { name: "Background".into(), priority: 3, order: 1, pixels: bg, visible: bg_on },
        Layer { name: "Window".into(), priority: 2, order: 0, pixels: win, visible: lcdc & 0x20 != 0 },
        Layer { name: "Objects".into(), priority: 0, order: 0, pixels: obj, visible: lcdc & 0x02 != 0 },
    ]
}

impl FrameInspector {
    fn capture(&mut self, console: &Console) {
        let prev: Vec<(String, bool)> = self.layers.iter().map(|l| (l.name.clone(), l.visible)).collect();
        match console {
            Console::Gba(g) => {
                let (vram, pal, io) = (&g.video.vram[..], &g.video.palette[..], &g.memory.io[..]);
                let dispcnt = io[0];
                let mode = dispcnt & 7;
                let mut layers = Vec::new();
                for bg in 0..4 {
                    let pixels = match (mode, bg) {
                        (0, _) | (1, 0) | (1, 1) => gba_text_layer(vram, pal, io, bg),
                        (1, 2) | (2, 2) | (2, 3) => gba_affine_layer(vram, pal, io, bg),
                        (3..=5, 2) => gba_bitmap_layer(vram, pal, dispcnt),
                        _ => continue,
                    };
                    layers.push(Layer {
                        name: format!("Background {bg}"),
                        priority: (io[4 + bg] & 3) as u32,
                        order: 1 + bg as u32,
                        pixels,
                        visible: dispcnt & (0x100 << bg) != 0,
                    });
                }
                for (p, pixels) in gba_obj_layers(vram, pal, &g.video.oam, dispcnt).into_iter().enumerate() {
                    layers.push(Layer {
                        name: format!("Objects (priority {p})"),
                        priority: p as u32,
                        order: 0,
                        pixels,
                        visible: dispcnt & 0x1000 != 0,
                    });
                }
                self.backdrop = gba_color(pal, 0);
                self.dims = (240, 160);
                self.layers = layers;
            }
            Console::Gb(g) => {
                let cgb = g.model.is_cgb();
                self.layers = gb_layers(&g.video.vram, &g.video.palette, &g.video.oam, &g.memory.io, cgb);
                self.backdrop = gb_palette(&g.video.palette, 0)[0];
                self.dims = (160, 144);
            }
        }
        // Keep the user's visibility choices across live refreshes.
        if self.captured {
            for l in &mut self.layers {
                if let Some((_, v)) = prev.iter().find(|(n, _)| *n == l.name) {
                    l.visible = *v;
                }
            }
        }
        self.captured = true;
    }

    fn compose(&mut self) {
        let (w, h) = self.dims;
        self.composite.clear();
        if let Some(i) = self.view.filter(|&i| i < self.layers.len()) {
            // Solo: transparent pixels shown as a dark checker.
            self.composite.extend(self.layers[i].pixels.iter().enumerate().map(|(n, &p)| {
                if p != TRANSPARENT {
                    p
                } else if ((n % w) / 4 + (n / w) / 4) % 2 == 0 {
                    0xFF30_3030
                } else {
                    0xFF20_2020
                }
            }));
            return;
        }
        self.composite.resize(w * h, self.backdrop);
        let mut order: Vec<usize> = (0..self.layers.len()).filter(|&i| self.layers[i].visible).collect();
        // Draw back to front: highest (priority, order) first.
        order.sort_by_key(|&i| std::cmp::Reverse((self.layers[i].priority, self.layers[i].order)));
        for i in order {
            for (d, &p) in self.composite.iter_mut().zip(&self.layers[i].pixels) {
                if p != TRANSPARENT {
                    *d = p;
                }
            }
        }
    }
}

impl ToolWindow for FrameInspector {
    fn title(&self) -> &'static str {
        "Frame inspector"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        egui::Window::new(self.title()).open(open).default_size([720.0, 420.0]).show(ctx, |ui| {
            let Some(s) = tc.session.as_deref_mut() else {
                no_game(ui);
                return;
            };
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.live, "Live");
                if ui.button("Capture").clicked() || self.live || !self.captured {
                    self.capture(&s.console);
                }
                ui.add(egui::Slider::new(&mut self.scale, 1.0..=4.0).step_by(1.0).text("Magnification"));
            });
            ui.label(
                egui::RichText::new(
                    "Layers are re-rendered from current VRAM/OAM and register state: \
                     mid-frame raster effects (HBlank scroll/palette changes), windows, \
                     blending, mosaic and GB OBJ-behind-BG priority are not reproduced.",
                )
                .weak()
                .small(),
            );
            self.compose();
            let (w, h) = self.dims;
            let tex = upload_rgba(ctx, &mut self.tex, "frame_inspector", &self.composite, w, h);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(200.0);
                    if ui.selectable_label(self.view.is_none(), "Composite").clicked() {
                        self.view = None;
                    }
                    ui.separator();
                    for (i, l) in self.layers.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut l.visible, "");
                            if ui.selectable_label(self.view == Some(i), &l.name).clicked() {
                                self.view = Some(i);
                            }
                        });
                    }
                    ui.separator();
                    ui.label(format!("Backdrop #{:06X}", self.backdrop & 0xFF_FFFF));
                });
                show_image(ui, &tex, w, h, self.scale);
            });
        });
    }

    fn on_session_changed(&mut self) {
        self.layers.clear();
        self.captured = false;
        self.view = None;
        self.tex = None;
    }
}
