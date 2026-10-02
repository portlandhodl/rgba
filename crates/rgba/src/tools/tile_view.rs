// Tile viewer (mGBA Qt TileView.cpp / AssetTile.cpp): every character in
// VRAM decoded as an image. GBA: 4bpp or 256-color, BG charblocks or OBJ
// tiles, with a palette selector. GB: 2bpp tiles at 0x8000-0x97FF, both
// VRAM banks on CGB.
//
// Also holds the tile/palette decode helpers shared by the other graphics
// views (sprites, maps, frame inspector).

use eframe::egui;

use super::{bgr555_to_argb, no_game, upload_rgba, ToolCtx, ToolWindow};
use crate::emu::Console;

// ---- Shared decode helpers ----------------------------------------------------

/// GBA palette entry `index` (0..512: BG 0..255, OBJ 256..511) as 0xFFRRGGBB.
pub fn gba_color(palette: &[u8], index: usize) -> u32 {
    let i = (index & 0x1FF) * 2;
    bgr555_to_argb(u16::from_le_bytes([palette[i], palette[i + 1]]))
}

/// Draw one 8x8 GBA tile at (`x`, `y`) into `out` (row stride `stride`).
/// `addr` is a byte offset into VRAM; `pal_base` is the first palette entry
/// (BG 0 / OBJ 256) and `bank` the 16-color bank for 4bpp. Color 0 is
/// skipped when `transparent`, otherwise drawn as palette entry 0.
#[allow(clippy::too_many_arguments)]
pub fn draw_gba_tile(
    out: &mut [u32],
    stride: usize,
    x: usize,
    y: usize,
    vram: &[u8],
    palette: &[u8],
    addr: usize,
    bpp8: bool,
    pal_base: usize,
    bank: usize,
    hflip: bool,
    vflip: bool,
    transparent: bool,
) {
    let height = out.len() / stride;
    for ty in 0..8 {
        let sy = if vflip { 7 - ty } else { ty };
        for tx in 0..8 {
            let sx = if hflip { 7 - tx } else { tx };
            let idx = if bpp8 {
                vram.get(addr + sy * 8 + sx).copied().unwrap_or(0) as usize
            } else {
                let b = vram.get(addr + sy * 4 + sx / 2).copied().unwrap_or(0);
                ((b >> ((sx & 1) * 4)) & 0xF) as usize
            };
            let (ox, oy) = (x + tx, y + ty);
            if ox >= stride || oy >= height {
                continue;
            }
            if idx == 0 && transparent {
                continue;
            }
            let entry = if idx == 0 {
                pal_base
            } else if bpp8 {
                pal_base + idx
            } else {
                pal_base + bank * 16 + idx
            };
            out[oy * stride + ox] = gba_color(palette, entry);
        }
    }
}

/// The 2bpp color index of pixel (`sx`, `sy`) of the GB tile at `addr`.
pub fn gb_tile_pixel(vram: &[u8], addr: usize, sx: usize, sy: usize) -> usize {
    let lo = vram.get(addr + sy * 2).copied().unwrap_or(0);
    let hi = vram.get(addr + sy * 2 + 1).copied().unwrap_or(0);
    let bit = 7 - sx;
    (((lo >> bit) & 1) | (((hi >> bit) & 1) << 1)) as usize
}

/// Draw one 8x8 GB tile; `colors` maps the 2bpp value to 0xFFRRGGBB, and
/// value 0 is skipped when `transparent`.
#[allow(clippy::too_many_arguments)]
pub fn draw_gb_tile(
    out: &mut [u32],
    stride: usize,
    x: usize,
    y: usize,
    vram: &[u8],
    addr: usize,
    colors: &[u32; 4],
    hflip: bool,
    vflip: bool,
    transparent: bool,
) {
    let height = out.len() / stride;
    for ty in 0..8 {
        let sy = if vflip { 7 - ty } else { ty };
        for tx in 0..8 {
            let sx = if hflip { 7 - tx } else { tx };
            let v = gb_tile_pixel(vram, addr, sx, sy);
            if v == 0 && transparent {
                continue;
            }
            let (ox, oy) = (x + tx, y + ty);
            if ox < stride && oy < height {
                out[oy * stride + ox] = colors[v];
            }
        }
    }
}

/// GB palette `n` (0..8 BG, 8..16 OBJ) from the renderer's 64-entry palette.
pub fn gb_palette(palette: &[u16; 64], n: usize) -> [u32; 4] {
    let base = (n & 15) * 4;
    [0, 1, 2, 3].map(|i| bgr555_to_argb(palette[base + i]))
}

/// Draw a magnified image with a pixel grid-free nearest-neighbour texture
/// and return the response (hoverable).
pub fn show_image(ui: &mut egui::Ui, tex: &egui::TextureHandle, w: usize, h: usize, scale: f32) -> egui::Response {
    let size = egui::vec2(w as f32 * scale, h as f32 * scale);
    ui.add(egui::Image::from_texture((tex.id(), size)).sense(egui::Sense::click()))
}

/// Pixel coordinate under the pointer within an image response.
pub fn hovered_pixel(resp: &egui::Response, scale: f32) -> Option<(usize, usize)> {
    let p = resp.hover_pos()?;
    let rel = p - resp.rect.min;
    if rel.x < 0.0 || rel.y < 0.0 {
        return None;
    }
    Some(((rel.x / scale) as usize, (rel.y / scale) as usize))
}

/// "Export" button helper: save `pixels` as PNG via a save dialog.
pub fn export_png(tc: &mut ToolCtx, name: &str, pixels: &[u32], w: usize, h: usize) {
    if let Some(p) = rfd::FileDialog::new()
        .set_title("Export image")
        .set_file_name(name)
        .add_filter("PNG", &["png"])
        .save_file()
    {
        match crate::screenshot::save(pixels, w as u32, h as u32, &p) {
            Ok(()) => tc.notice(format!("Exported {}", p.display())),
            Err(e) => tc.notice(format!("Export failed: {e}")),
        }
    }
}

// ---- Tile view ----------------------------------------------------------------

#[derive(PartialEq, Clone, Copy)]
enum GbaRegion {
    Bg,
    Obj,
}

pub struct TileView {
    bpp8: bool,
    region: GbaRegion,
    palette: usize,
    scale: f32,
    tex: Option<egui::TextureHandle>,
    pixels: Vec<u32>,
    dims: (usize, usize),
}

impl Default for TileView {
    fn default() -> Self {
        TileView {
            bpp8: false,
            region: GbaRegion::Bg,
            palette: 0,
            scale: 2.0,
            tex: None,
            pixels: Vec::new(),
            dims: (0, 0),
        }
    }
}

const TILES_PER_ROW_GBA: usize = 32;
const TILES_PER_ROW_GB: usize = 16;

impl TileView {
    /// Decode the GBA tile sheet; returns (width, height).
    fn decode_gba(&mut self, vram: &[u8], palette: &[u8]) -> (usize, usize) {
        let (start, len) = match self.region {
            GbaRegion::Bg => (0usize, 0x10000usize),
            GbaRegion::Obj => (0x10000, 0x8000),
        };
        let tile_bytes = if self.bpp8 { 64 } else { 32 };
        let count = len / tile_bytes;
        let rows = count.div_ceil(TILES_PER_ROW_GBA);
        let (w, h) = (TILES_PER_ROW_GBA * 8, rows * 8);
        self.pixels.clear();
        self.pixels.resize(w * h, 0xFF00_0000);
        let pal_base = if self.region == GbaRegion::Obj { 256 } else { 0 };
        for t in 0..count {
            let (tx, ty) = (t % TILES_PER_ROW_GBA, t / TILES_PER_ROW_GBA);
            draw_gba_tile(
                &mut self.pixels, w, tx * 8, ty * 8, vram, palette,
                start + t * tile_bytes, self.bpp8, pal_base, self.palette, false, false, false,
            );
        }
        (w, h)
    }

    fn decode_gb(&mut self, vram: &[u8], palette: &[u16; 64], banks: usize) -> (usize, usize) {
        // 384 tiles per bank, banks stacked vertically.
        let per_bank = 384;
        let count = per_bank * banks;
        let rows = count / TILES_PER_ROW_GB;
        let (w, h) = (TILES_PER_ROW_GB * 8, rows * 8);
        self.pixels.clear();
        self.pixels.resize(w * h, 0xFF00_0000);
        let colors = gb_palette(palette, self.palette);
        for t in 0..count {
            let (tx, ty) = (t % TILES_PER_ROW_GB, t / TILES_PER_ROW_GB);
            let addr = (t / per_bank) * 0x2000 + (t % per_bank) * 16;
            draw_gb_tile(&mut self.pixels, w, tx * 8, ty * 8, vram, addr, &colors, false, false, false);
        }
        (w, h)
    }
}

impl ToolWindow for TileView {
    fn title(&self) -> &'static str {
        "Tiles"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        egui::Window::new(self.title()).open(open).default_size([560.0, 600.0]).show(ctx, |ui| {
            let Some(s) = tc.session.as_deref_mut() else {
                no_game(ui);
                return;
            };
            let is_gba = matches!(s.console, Console::Gba(_));
            ui.horizontal(|ui| {
                if is_gba {
                    ui.selectable_value(&mut self.region, GbaRegion::Bg, "BG (0x06000000)");
                    ui.selectable_value(&mut self.region, GbaRegion::Obj, "OBJ (0x06010000)");
                    ui.separator();
                    ui.checkbox(&mut self.bpp8, "256 colors");
                }
                ui.add_enabled(
                    !(is_gba && self.bpp8),
                    egui::Slider::new(&mut self.palette, 0..=15).text("Palette"),
                );
                ui.add(egui::Slider::new(&mut self.scale, 1.0..=6.0).step_by(1.0).text("Magnification"));
            });
            if !is_gba {
                ui.label("Palettes 0-7 are BG, 8-15 are OBJ.");
            }
            let (w, h, base_addr, tile_bytes, cols) = match &mut s.console {
                Console::Gba(g) => {
                    let (w, h) = self.decode_gba(&g.video.vram, &g.video.palette);
                    let base = if self.region == GbaRegion::Obj { 0x0601_0000 } else { 0x0600_0000 };
                    (w, h, base, if self.bpp8 { 64 } else { 32 }, TILES_PER_ROW_GBA)
                }
                Console::Gb(g) => {
                    let banks = if g.video.vram.len() >= 0x4000 && g.model.is_cgb() { 2 } else { 1 };
                    let (w, h) = self.decode_gb(&g.video.vram, &g.video.palette, banks);
                    (w, h, 0x8000, 16, TILES_PER_ROW_GB)
                }
            };
            self.dims = (w, h);
            let tex = upload_rgba(ctx, &mut self.tex, "tile_view", &self.pixels, w, h);
            let mut hover = String::from(" ");
            egui::ScrollArea::both().max_height(ui.available_height() - 40.0).show(ui, |ui| {
                let resp = show_image(ui, &tex, w, h, self.scale);
                if let Some((px, py)) = hovered_pixel(&resp, self.scale) {
                    if px < w && py < h {
                        let t = (py / 8) * cols + px / 8;
                        hover = if is_gba {
                            format!("Tile {t} (0x{:03X}) at 0x{:08X}", t, base_addr + t * tile_bytes)
                        } else {
                            let bank = t / 384;
                            let i = t % 384;
                            format!("Tile {i} (bank {bank}) at {}:0x{:04X}", bank, base_addr + i * tile_bytes)
                        };
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label(hover);
                if ui.button("Export PNG...").clicked() {
                    let pixels = self.pixels.clone();
                    export_png(tc, "tiles.png", &pixels, w, h);
                }
            });
        });
    }

    fn on_session_changed(&mut self) {
        self.tex = None;
    }
}
