// "View logs..." — mirrors mGBA Qt's LogView (LogView.cpp): level filter
// checkboxes, a line limit, Clear and Save, over the captured core log.

use eframe::egui;
use rgba_core::log::Level;

use super::{ToolCtx, ToolWindow};
use crate::logbuf::LogLine;

const LEVELS: [(Level, &str); 7] = [
    (Level::Fatal, "Fatal"),
    (Level::Error, "Error"),
    (Level::Warn, "Warning"),
    (Level::Info, "Info"),
    (Level::Debug, "Debug"),
    (Level::Stub, "Stub"),
    (Level::GameError, "Game error"),
];

pub struct LogView {
    /// Shown levels, indexed like LEVELS.
    shown: [bool; 7],
    max_lines: usize,
    /// Whether this window changed the core's log level (so closing
    /// restores the default).
    raised_level: bool,
    was_open: bool,
}

impl Default for LogView {
    fn default() -> Self {
        // LogView's defaults: warnings, errors, fatal.
        LogView {
            shown: [true, true, true, false, false, false, false],
            max_lines: 2000,
            raised_level: false,
            was_open: false,
        }
    }
}

impl LogView {
    fn shown(&self, level: Level) -> bool {
        LEVELS
            .iter()
            .position(|(l, _)| *l == level)
            .map_or(false, |i| self.shown[i])
    }

    /// The core filters on a single max level, so let through everything up
    /// to the most verbose level that is checked.
    fn apply_core_level(&mut self) {
        let max = LEVELS
            .iter()
            .zip(self.shown.iter())
            .filter(|(_, on)| **on)
            .map(|((l, _), _)| *l)
            .max()
            .unwrap_or(Level::Fatal);
        rgba_core::log::set_max_level(max.max(Level::Warn));
        self.raised_level = true;
    }

    fn restore_core_level(&mut self) {
        if self.raised_level {
            rgba_core::log::set_max_level(Level::Warn);
            self.raised_level = false;
        }
    }

    fn format_line(l: &LogLine) -> String {
        let name = LEVELS.iter().find(|(lv, _)| *lv == l.level).map_or("?", |(_, n)| n);
        format!("[{}] {}: {}", name.to_uppercase(), l.category, l.message)
    }
}

impl ToolWindow for LogView {
    fn title(&self) -> &'static str {
        "Logs"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        if !self.was_open {
            self.apply_core_level();
            self.was_open = true;
        }
        let mut changed = false;
        egui::Window::new(self.title())
            .open(open)
            .default_size([560.0, 360.0])
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (i, (_, name)) in LEVELS.iter().enumerate() {
                        changed |= ui.checkbox(&mut self.shown[i], *name).changed();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Max lines");
                    ui.add(egui::DragValue::new(&mut self.max_lines).range(10..=crate::logbuf::MAX_LINES));
                    if ui.button("Clear").clicked() {
                        tc.log.clear();
                    }
                    if ui.button("Save...").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_title("Save log")
                            .set_file_name("rgba.log")
                            .save_file()
                        {
                            let text: String = tc
                                .log
                                .snapshot()
                                .iter()
                                .filter(|l| self.shown(l.level))
                                .map(|l| Self::format_line(l) + "\n")
                                .collect();
                            match std::fs::write(&path, text) {
                                Ok(()) => tc.notice(format!("Log saved to {}", path.display())),
                                Err(e) => tc.notice(format!("Could not save log: {e}")),
                            }
                        }
                    }
                });
                ui.separator();
                let lines: Vec<LogLine> = tc
                    .log
                    .snapshot()
                    .into_iter()
                    .filter(|l| self.shown(l.level))
                    .collect();
                let skip = lines.len().saturating_sub(self.max_lines);
                let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show_rows(ui, row_h, lines.len() - skip, |ui, range| {
                        for l in &lines[skip + range.start..skip + range.end] {
                            let color = match l.level {
                                Level::Fatal | Level::Error => egui::Color32::from_rgb(255, 110, 110),
                                Level::Warn => egui::Color32::from_rgb(255, 200, 90),
                                Level::GameError => egui::Color32::from_rgb(220, 140, 255),
                                _ => ui.visuals().text_color(),
                            };
                            ui.label(egui::RichText::new(Self::format_line(l)).monospace().color(color));
                        }
                    });
            });
        if changed {
            self.apply_core_level();
        }
        if !*open {
            self.restore_core_level();
            self.was_open = false;
        }
    }
}
