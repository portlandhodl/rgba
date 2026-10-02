// Settings window — mirrors mGBA Qt's SettingsView.cpp: a page list on the
// left (Interface, Emulation, Audio, Video, BIOS, Keyboard, Controllers,
// Paths, Game Boy, Logging) and the page's options on the right. Options
// edit the live Config; "Save" (and closing the window) persists it.

use eframe::egui;

use super::{ToolCtx, ToolWindow};
use crate::config::{native_fps, Config, KEY_NAMES};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Interface,
    Emulation,
    Audio,
    Video,
    Bios,
    Keyboard,
    Controllers,
    Paths,
    GameBoy,
    Logging,
}

const PAGES: [(Page, &str); 10] = [
    (Page::Interface, "Interface"),
    (Page::Emulation, "Emulation"),
    (Page::Audio, "Audio"),
    (Page::Video, "Video"),
    (Page::Bios, "BIOS"),
    (Page::Keyboard, "Keyboard"),
    (Page::Controllers, "Controllers"),
    (Page::Paths, "Paths"),
    (Page::GameBoy, "Game Boy"),
    (Page::Logging, "Logging"),
];

const GB_MODELS: [(&str, &str); 8] = [
    ("auto", "Autodetect"),
    ("dmg", "Game Boy (DMG)"),
    ("mgb", "Game Boy Pocket (MGB)"),
    ("sgb", "Super Game Boy (SGB)"),
    ("sgb2", "Super Game Boy 2 (SGB2)"),
    ("cgb", "Game Boy Color (CGB)"),
    ("agb", "Game Boy Advance (AGB)"),
    ("scgb", "Super Game Boy Color (SCGB)"),
];

const LOG_LEVELS: [(rgba_core::log::Level, &str); 7] = [
    (rgba_core::log::Level::Fatal, "Fatal"),
    (rgba_core::log::Level::Error, "Error"),
    (rgba_core::log::Level::Warn, "Warning"),
    (rgba_core::log::Level::Info, "Info"),
    (rgba_core::log::Level::Debug, "Debug"),
    (rgba_core::log::Level::Stub, "Stub"),
    (rgba_core::log::Level::GameError, "Game error"),
];

pub struct SettingsView {
    page: Page,
    /// Keyboard page: index of the button waiting for a key press.
    capturing: Option<usize>,
    was_open: bool,
    saved_notice: bool,
}

impl Default for SettingsView {
    fn default() -> Self {
        SettingsView { page: Page::Interface, capturing: None, was_open: false, saved_notice: false }
    }
}

fn next_load_note(ui: &mut egui::Ui) {
    ui.small("Takes effect the next time a game is loaded.");
}

fn restart_note(ui: &mut egui::Ui) {
    ui.small("Takes effect after restarting rgba.");
}

fn path_row(ui: &mut egui::Ui, label: &str, path: &mut Option<std::path::PathBuf>, filters: &[(&str, &[&str])], dir: bool) {
    ui.horizontal(|ui| {
        ui.label(label);
        let text = path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "(not set)".into());
        ui.add(egui::Label::new(egui::RichText::new(text).monospace()).truncate());
    });
    ui.horizontal(|ui| {
        if ui.button("Browse…").clicked() {
            let mut d = rfd::FileDialog::new().set_title(label);
            for (n, e) in filters {
                d = d.add_filter(*n, e);
            }
            if let Some(cur) = path.as_ref().and_then(|p| p.parent()) {
                d = d.set_directory(cur);
            }
            let picked = if dir { d.pick_folder() } else { d.pick_file() };
            if let Some(p) = picked {
                *path = Some(p);
            }
        }
        if ui.add_enabled(path.is_some(), egui::Button::new("Clear")).clicked() {
            *path = None;
        }
    });
}

impl SettingsView {
    fn interface(&mut self, ui: &mut egui::Ui, cfg: &mut Config) {
        ui.checkbox(&mut cfg.show_fps, "Show FPS in title bar and overlay");
        ui.checkbox(&mut cfg.pause_on_focus_loss, "Pause when the window is inactive");
        ui.checkbox(&mut cfg.hide_menu, "Hide menu bar")
            .on_hover_text("Alt+M or right-click the game to bring it back");
    }

    fn emulation(&mut self, ui: &mut egui::Ui, cfg: &mut Config) {
        ui.heading("Fast forward");
        ui.horizontal(|ui| {
            let mut unbounded = cfg.ff_speed <= 0.0;
            if ui.checkbox(&mut unbounded, "Unbounded").changed() {
                cfg.ff_speed = if unbounded { -1.0 } else { 2.0 };
            }
            if !unbounded {
                ui.add(egui::Slider::new(&mut cfg.ff_speed, 1.5..=10.0).step_by(0.5).suffix("×"));
            }
        });
        ui.checkbox(&mut cfg.ff_mute, "Mute while fast forwarding");
        ui.separator();
        ui.heading("Rewind");
        ui.checkbox(&mut cfg.rewind_enable, "Enable rewind");
        ui.add_enabled_ui(cfg.rewind_enable, |ui| {
            ui.horizontal(|ui| {
                ui.label("Rewind history:");
                ui.add(egui::DragValue::new(&mut cfg.rewind_capacity).range(1..=10000).suffix(" states"));
            });
            ui.horizontal(|ui| {
                ui.label("Record a state every");
                ui.add(egui::DragValue::new(&mut cfg.rewind_interval).range(1..=600).suffix(" frames"));
            });
        });
        next_load_note(ui);
        ui.separator();
        ui.heading("Other");
        ui.checkbox(&mut cfg.idle_optimization, "Idle loop optimization");
        ui.horizontal(|ui| {
            ui.label("Autofire period:");
            ui.add(egui::DragValue::new(&mut cfg.autofire_period).range(1..=60).suffix(" frames"));
        });
        ui.checkbox(&mut cfg.use_bios, "Use BIOS file if found");
        ui.checkbox(&mut cfg.skip_bios, "Skip BIOS intro");
        next_load_note(ui);
    }

    fn audio(&mut self, ui: &mut egui::Ui, cfg: &mut Config) {
        ui.horizontal(|ui| {
            ui.label("Volume:");
            let mut pct = (cfg.volume * 100.0).round();
            if ui.add(egui::Slider::new(&mut pct, 0.0..=100.0).suffix("%")).changed() {
                cfg.volume = pct / 100.0;
            }
        });
        ui.checkbox(&mut cfg.mute, "Mute");
        ui.horizontal(|ui| {
            ui.label("Sample rate:");
            egui::ComboBox::from_id_salt("sample_rate")
                .selected_text(format!("{} Hz", cfg.sample_rate))
                .show_ui(ui, |ui| {
                    for r in [22050, 32000, 44100, 48000, 96000] {
                        ui.selectable_value(&mut cfg.sample_rate, r, format!("{r} Hz"));
                    }
                });
        });
        restart_note(ui);
    }

    fn video(&mut self, ui: &mut egui::Ui, cfg: &mut Config) {
        ui.horizontal(|ui| {
            ui.label("Frameskip:");
            ui.add(egui::DragValue::new(&mut cfg.frameskip).range(0..=10));
        });
        next_load_note(ui);
        ui.checkbox(&mut cfg.lock_aspect, "Lock aspect ratio");
        ui.checkbox(&mut cfg.integer_scaling, "Force integer scaling");
        ui.checkbox(&mut cfg.bilinear, "Bilinear filtering");
        ui.checkbox(&mut cfg.interframe_blending, "Interframe blending");
        ui.horizontal(|ui| {
            ui.label("FPS target:");
            let native = (cfg.fps_target - native_fps()).abs() < 1e-6;
            let mut use_native = native;
            if ui.checkbox(&mut use_native, "Native (59.7275)").changed() {
                cfg.fps_target = if use_native { native_fps() } else { 60.0 };
            }
            if !use_native {
                ui.add(egui::DragValue::new(&mut cfg.fps_target).range(1.0..=480.0).speed(0.5).suffix(" fps"));
            }
        });
    }

    fn bios(&mut self, ui: &mut egui::Ui, cfg: &mut Config) {
        let bios_filters: &[(&str, &[&str])] = &[("BIOS images", &["bin", "rom", "gba", "gb", "gbc"]), ("All files", &["*"])];
        path_row(ui, "Game Boy Advance BIOS", &mut cfg.gba_bios, bios_filters, false);
        ui.separator();
        path_row(ui, "Game Boy BIOS", &mut cfg.gb_bios, bios_filters, false);
        ui.separator();
        path_row(ui, "Game Boy Color BIOS", &mut cfg.gbc_bios, bios_filters, false);
        ui.separator();
        ui.checkbox(&mut cfg.use_bios, "Use BIOS file if found");
        ui.checkbox(&mut cfg.skip_bios, "Skip BIOS intro");
        next_load_note(ui);
    }

    fn keyboard(&mut self, ui: &mut egui::Ui, cfg: &mut Config) {
        // Capture the next key press for the selected button.
        if let Some(idx) = self.capturing {
            let pressed = ui.ctx().input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Key { key, pressed: true, repeat: false, .. } => Some(*key),
                    _ => None,
                })
            });
            if let Some(k) = pressed {
                if k != egui::Key::Escape {
                    cfg.keys[idx] = k.name().to_string();
                }
                self.capturing = None;
            }
        }
        egui::Grid::new("keymap").num_columns(2).striped(true).show(ui, |ui| {
            for (i, name) in KEY_NAMES.iter().enumerate() {
                ui.label(*name);
                let label = if self.capturing == Some(i) {
                    "Press a key… (Esc cancels)".to_string()
                } else {
                    cfg.keys.get(i).cloned().unwrap_or_default()
                };
                if ui.add(egui::Button::new(label).min_size(egui::vec2(160.0, 0.0))).clicked() {
                    self.capturing = Some(i);
                }
                ui.end_row();
            }
        });
        if ui.button("Reset to defaults").clicked() {
            cfg.keys = Config::default().keys;
            self.capturing = None;
        }
    }

    fn controllers(&mut self, ui: &mut egui::Ui, cfg: &mut Config) {
        egui::Grid::new("padmap").num_columns(2).striped(true).show(ui, |ui| {
            for (i, name) in KEY_NAMES.iter().enumerate() {
                ui.label(*name);
                let cur = cfg.pad_buttons.get(i).cloned().unwrap_or_default();
                egui::ComboBox::from_id_salt(("pad", i)).selected_text(cur).show_ui(ui, |ui| {
                    for b in crate::input::PAD_BUTTONS {
                        ui.selectable_value(&mut cfg.pad_buttons[i], b.to_string(), b);
                    }
                });
                ui.end_row();
            }
        });
        ui.horizontal(|ui| {
            ui.label("Analog stick deadzone:");
            ui.add(egui::Slider::new(&mut cfg.pad_deadzone, 0.05..=0.95));
        });
        ui.small("The left stick also drives the D-pad.");
        if ui.button("Reset to defaults").clicked() {
            let d = Config::default();
            cfg.pad_buttons = d.pad_buttons;
            cfg.pad_deadzone = d.pad_deadzone;
        }
    }

    fn paths(&mut self, ui: &mut egui::Ui, cfg: &mut Config) {
        path_row(ui, "Screenshots", &mut cfg.screenshot_dir, &[], true);
        ui.small("When unset, screenshots go next to the ROM.");
        ui.separator();
        ui.label("Saves and save states are stored next to the ROM.");
        ui.horizontal(|ui| {
            ui.label("Settings file:");
            ui.monospace(Config::path().display().to_string());
        });
    }

    fn game_boy(&mut self, ui: &mut egui::Ui, cfg: &mut Config) {
        ui.horizontal(|ui| {
            ui.label("Game Boy model:");
            let cur = GB_MODELS
                .iter()
                .find(|m| m.0.eq_ignore_ascii_case(&cfg.gb_model))
                .map_or(cfg.gb_model.clone(), |m| m.1.to_string());
            egui::ComboBox::from_id_salt("gb_model").selected_text(cur).show_ui(ui, |ui| {
                for (id, label) in GB_MODELS {
                    ui.selectable_value(&mut cfg.gb_model, id.to_string(), label);
                }
            });
        });
        next_load_note(ui);
    }

    fn logging(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Maximum log level:");
            let cur = rgba_core::log::max_level();
            let mut sel = cur;
            let name = LOG_LEVELS.iter().find(|l| l.0 == cur).map_or("?", |l| l.1);
            egui::ComboBox::from_id_salt("log_level").selected_text(name).show_ui(ui, |ui| {
                for (lvl, label) in LOG_LEVELS {
                    ui.selectable_value(&mut sel, lvl, label);
                }
            });
            if sel != cur {
                rgba_core::log::set_max_level(sel);
            }
        });
        ui.small("Messages at or above this level reach Tools → View logs.");
    }
}

impl ToolWindow for SettingsView {
    fn title(&self) -> &'static str {
        "Settings"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        self.was_open = true;
        let mut save = false;
        egui::Window::new(self.title())
            .open(open)
            .default_size([560.0, 420.0])
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(110.0);
                        for (p, name) in PAGES {
                            if ui.selectable_label(self.page == p, name).clicked() {
                                self.page = p;
                                self.capturing = None;
                            }
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                            let cfg = &mut *tc.config;
                            match self.page {
                                Page::Interface => self.interface(ui, cfg),
                                Page::Emulation => self.emulation(ui, cfg),
                                Page::Audio => self.audio(ui, cfg),
                                Page::Video => self.video(ui, cfg),
                                Page::Bios => self.bios(ui, cfg),
                                Page::Keyboard => self.keyboard(ui, cfg),
                                Page::Controllers => self.controllers(ui, cfg),
                                Page::Paths => self.paths(ui, cfg),
                                Page::GameBoy => self.game_boy(ui, cfg),
                                Page::Logging => self.logging(ui),
                            }
                        });
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        save = true;
                    }
                    if self.saved_notice {
                        ui.small("Saved.");
                    }
                });
            });
        // Live-apply settings the running game reads from the session.
        if let Some(s) = tc.session.as_deref_mut() {
            s.set_frameskip(tc.config.frameskip);
        }
        if save {
            tc.config.save();
            self.saved_notice = true;
        }
        if !*open && self.was_open {
            // Closing the window persists the settings.
            tc.config.save();
            self.was_open = false;
            self.saved_notice = false;
            self.capturing = None;
        }
    }
}
