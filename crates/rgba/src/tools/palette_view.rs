// Palette viewer (mGBA Qt PaletteView.cpp): BG and OBJ palettes as 16x16
// swatch grids (GBA) or 8 palettes x 4 colors (GB/CGB). Clicking a swatch
// shows its index, raw BGR555 value and 8-bit RGB; the palettes can be
// exported as a JASC .pal file.

use eframe::egui;

use super::{bgr555_to_argb, no_game, ToolCtx, ToolWindow};
use crate::emu::Console;

#[derive(Default)]
pub struct PaletteView {
    /// Selected entry: (is_obj, index within that half).
    selected: Option<(bool, usize)>,
}

fn swatch(ui: &mut egui::Ui, argb: u32, selected: bool, size: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    let c = egui::Color32::from_rgb((argb >> 16) as u8, (argb >> 8) as u8, argb as u8);
    ui.painter().rect_filled(rect, 0.0, c);
    if selected {
        ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(2.0, egui::Color32::WHITE), egui::StrokeKind::Inside);
    }
    resp
}

/// Draw a grid of `count` colors, `cols` per row; returns a clicked index.
fn grid(ui: &mut egui::Ui, id: &str, colors: &[u16], cols: usize, sel: Option<usize>) -> Option<usize> {
    let mut clicked = None;
    egui::Grid::new(id).spacing([1.0, 1.0]).show(ui, |ui| {
        for (i, &c) in colors.iter().enumerate() {
            if swatch(ui, bgr555_to_argb(c), sel == Some(i), 14.0).clicked() {
                clicked = Some(i);
            }
            if i % cols == cols - 1 {
                ui.end_row();
            }
        }
    });
    clicked
}

/// JASC-PAL text for a list of BGR555 colors (PaletteView::exportPalette).
fn jasc(colors: &[u16]) -> String {
    let mut s = format!("JASC-PAL\r\n0100\r\n{}\r\n", colors.len());
    for &c in colors {
        let argb = bgr555_to_argb(c);
        s += &format!("{} {} {}\r\n", (argb >> 16) & 0xFF, (argb >> 8) & 0xFF, argb & 0xFF);
    }
    s
}

impl ToolWindow for PaletteView {
    fn title(&self) -> &'static str {
        "Palette"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        egui::Window::new(self.title()).open(open).resizable(false).show(ctx, |ui| {
            let Some(s) = tc.session.as_deref_mut() else {
                no_game(ui);
                return;
            };
            // (bg, obj, columns, is_gba)
            let (bg, obj, cols, is_gba): (Vec<u16>, Vec<u16>, usize, bool) = match &s.console {
                Console::Gba(g) => {
                    let p: Vec<u16> = g.video.palette[..0x400]
                        .chunks_exact(2)
                        .map(|b| u16::from_le_bytes([b[0], b[1]]))
                        .collect();
                    (p[..256].to_vec(), p[256..512].to_vec(), 16, true)
                }
                Console::Gb(g) => (g.video.palette[..32].to_vec(), g.video.palette[32..64].to_vec(), 4, false),
            };
            let mut export: Option<Vec<u16>> = None;
            ui.horizontal_top(|ui| {
                for (is_obj, colors, name) in [(false, &bg, "Background"), (true, &obj, "Objects")] {
                    ui.vertical(|ui| {
                        ui.label(name);
                        let sel = self.selected.filter(|s| s.0 == is_obj).map(|s| s.1);
                        if let Some(i) = grid(ui, name, colors, cols, sel) {
                            self.selected = Some((is_obj, i));
                        }
                        if ui.button("Export palette...").clicked() {
                            export = Some(colors.clone());
                        }
                    });
                    ui.add_space(12.0);
                }
            });
            ui.separator();
            if let Some((is_obj, i)) = self.selected {
                let colors = if is_obj { &obj } else { &bg };
                if let Some(&c) = colors.get(i) {
                    let argb = bgr555_to_argb(c);
                    let (r5, g5, b5) = (c & 0x1F, (c >> 5) & 0x1F, (c >> 10) & 0x1F);
                    let addr = if is_gba {
                        format!("0x{:08X}", 0x0500_0000 + (if is_obj { 0x200 } else { 0 }) + i * 2)
                    } else {
                        format!("{} palette {} color {}", if is_obj { "OBJ" } else { "BG" }, i / 4, i % 4)
                    };
                    ui.horizontal(|ui| {
                        swatch(ui, argb, false, 32.0);
                        ui.vertical(|ui| {
                            ui.label(format!(
                                "Index {}{}   {addr}",
                                if is_obj { "OBJ " } else { "BG " },
                                i
                            ));
                            ui.label(format!("Value 0x{c:04X}   5-bit RGB ({r5}, {g5}, {b5})"));
                            ui.label(format!(
                                "8-bit RGB ({}, {}, {})   #{:06X}",
                                (argb >> 16) & 0xFF,
                                (argb >> 8) & 0xFF,
                                argb & 0xFF,
                                argb & 0xFF_FFFF
                            ));
                        });
                    });
                }
            } else {
                ui.label("Click a color for details.");
            }
            if let Some(colors) = export {
                if let Some(p) = rfd::FileDialog::new()
                    .set_title("Export palette")
                    .set_file_name("palette.pal")
                    .add_filter("JASC palette", &["pal"])
                    .save_file()
                {
                    match std::fs::write(&p, jasc(&colors)) {
                        Ok(()) => tc.notice(format!("Exported {}", p.display())),
                        Err(e) => tc.notice(format!("Export failed: {e}")),
                    }
                }
            }
        });
    }
}
