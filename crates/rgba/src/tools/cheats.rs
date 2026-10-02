// "Cheats..." — mirrors mGBA Qt's CheatsView (CheatsView.cpp / CheatsModel):
// the running game's cheat sets with enable checkboxes and editable names,
// "Add new code" / "Add lines" with a code-type selector, Remove, and
// Load/Save of mGBA's .cheats text format (mCheatParseFile / mCheatSaveFile).

use std::io::Write;
use std::path::Path;

use eframe::egui;
use rgba_core::cheats::CheatSet;
use rgba_gb::cheats::{gb_cheat_add_line, GB_CHEAT_AUTODETECT, GB_CHEAT_GAMESHARK, GB_CHEAT_GAME_GENIE, GB_CHEAT_VBA};
use rgba_gba::cheats::{
    gba_cheat_add_line, gba_cheat_set_create, GBA_CHEAT_AUTODETECT, GBA_CHEAT_CODEBREAKER,
    GBA_CHEAT_GAMESHARK, GBA_CHEAT_PRO_ACTION_REPLAY, GBA_CHEAT_VBA,
};

use super::{no_game, ToolCtx, ToolWindow};
use crate::emu::{Console, Session};

const GBA_TYPES: [(i32, &str); 5] = [
    (GBA_CHEAT_AUTODETECT, "Autodetect"),
    (GBA_CHEAT_CODEBREAKER, "CodeBreaker"),
    (GBA_CHEAT_GAMESHARK, "GameShark"),
    (GBA_CHEAT_PRO_ACTION_REPLAY, "Pro Action Replay"),
    (GBA_CHEAT_VBA, "VBA"),
];
const GB_TYPES: [(i32, &str); 4] = [
    (GB_CHEAT_AUTODETECT, "Autodetect"),
    (GB_CHEAT_GAMESHARK, "GameShark"),
    (GB_CHEAT_GAME_GENIE, "Game Genie"),
    (GB_CHEAT_VBA, "VBA"),
];

fn sets_mut(s: &mut Session) -> &mut Vec<CheatSet> {
    match &mut s.console {
        Console::Gba(g) => &mut g.cheats.cheats,
        Console::Gb(g) => &mut g.cheats.cheats,
    }
}

/// Add `lines` (one code per line) to set `index` with the given code type.
/// Returns the number of lines rejected by the parser.
fn add_lines(s: &mut Session, index: usize, text: &str, typ: i32) -> usize {
    let mut bad = 0;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let ok = match &mut s.console {
            Console::Gba(g) => {
                let (Some(set), Some(state)) = (g.cheats.cheats.get_mut(index), g.gba_cheat_sets.get_mut(index)) else {
                    return bad;
                };
                gba_cheat_add_line(set, state, line, typ)
            }
            Console::Gb(g) => match g.cheats.cheats.get_mut(index) {
                Some(set) => gb_cheat_add_line(set, line, typ),
                None => return bad,
            },
        };
        if !ok {
            bad += 1;
        }
    }
    bad
}

fn new_set(s: &mut Session, name: &str) -> usize {
    match &mut s.console {
        Console::Gba(g) => {
            let (set, state) = gba_cheat_set_create(name);
            g.cheat_add_set(set, state);
            g.cheats.cheats.len() - 1
        }
        Console::Gb(g) => {
            g.cheats.add_set(CheatSet::new(name));
            g.cheats.cheats.len() - 1
        }
    }
}

fn remove_set(s: &mut Session, index: usize) {
    match &mut s.console {
        Console::Gba(g) => g.cheat_remove_set(index),
        Console::Gb(g) => g.cheats.remove_set(index),
    }
}

/// mCheatParseFile for a `.cheats` file: `# name` starts a set, `!disabled`
/// disables the next set, other `!` lines are engine directives (GBA), and
/// everything else is a code line. Returns the number of sets loaded.
pub fn load_cheats_file(s: &mut Session, path: &Path) -> Result<usize, String> {
    let before = sets_mut(s).len();
    match &mut s.console {
        Console::Gba(g) => match g.load_cheats(path) {
            Ok(true) => {}
            Ok(false) => return Err(format!("{}: unsupported cheats file format", path.display())),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        },
        Console::Gb(g) => {
            let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let mut current: Option<CheatSet> = None;
            let mut next_disabled = false;
            for raw in text.lines() {
                let line = raw.trim();
                if let Some(name) = line.strip_prefix('#') {
                    if let Some(set) = current.take() {
                        g.cheats.add_set(set);
                    }
                    let mut set = CheatSet::new(name.trim());
                    set.enabled = !next_disabled;
                    next_disabled = false;
                    current = Some(set);
                } else if let Some(directive) = line.strip_prefix('!') {
                    // GB sets have no engine directives; only !disabled matters.
                    if directive.trim().eq_ignore_ascii_case("disabled") {
                        next_disabled = true;
                    }
                } else {
                    if current.is_none() {
                        if line.starts_with("cheats = ") || line.starts_with('[') {
                            return Err(format!("{}: libretro/EZ-Flash cheat files are not supported", path.display()));
                        }
                        let mut set = CheatSet::new("");
                        set.enabled = !next_disabled;
                        next_disabled = false;
                        current = Some(set);
                    }
                    if !line.is_empty() {
                        gb_cheat_add_line(current.as_mut().unwrap(), line, GB_CHEAT_AUTODETECT);
                    }
                }
            }
            if let Some(set) = current.take() {
                g.cheats.add_set(set);
            }
        }
    }
    Ok(sets_mut(s).len() - before)
}

/// mCheatSaveFile: directives (`!disabled`, the GBA GameShark/PAR version),
/// then `# name` and the raw code lines of every set.
pub fn save_cheats_file(s: &mut Session, path: &Path) -> Result<(), String> {
    let mut out = Vec::new();
    let n = sets_mut(s).len();
    for i in 0..n {
        let directive = match &s.console {
            // GBACheatDumpDirectives
            Console::Gba(g) => match g.gba_cheat_sets.get(i).map_or(0, |st| st.gsa_version) {
                1 => Some("GSAv1"),
                2 => Some("GSAv1 raw"),
                3 => Some("PARv3"),
                4 => Some("PARv3 raw"),
                _ => None,
            },
            Console::Gb(_) => None,
        };
        let set = &sets_mut(s)[i];
        if !set.enabled {
            out.extend_from_slice(b"!disabled\n");
        }
        if let Some(d) = directive {
            let _ = writeln!(out, "!{d}");
        }
        let _ = writeln!(out, "# {}", set.name);
        for line in &set.lines {
            let _ = writeln!(out, "{line}");
        }
    }
    std::fs::write(path, out).map_err(|e| format!("{}: {e}", path.display()))
}

pub struct CheatsView {
    selected: Option<usize>,
    code_text: String,
    code_type: i32,
    new_name: String,
}

impl Default for CheatsView {
    fn default() -> Self {
        CheatsView { selected: None, code_text: String::new(), code_type: 0, new_name: String::new() }
    }
}

impl ToolWindow for CheatsView {
    fn title(&self) -> &'static str {
        "Cheats"
    }

    fn on_session_changed(&mut self) {
        self.selected = None;
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        egui::Window::new(self.title())
            .open(open)
            .default_size([460.0, 420.0])
            .show(ctx, |ui| {
                let Some(s) = tc.session.as_deref_mut() else {
                    no_game(ui);
                    return;
                };
                let is_gba = matches!(s.console, Console::Gba(_));
                let mut notice = None;

                ui.horizontal(|ui| {
                    if ui.button("Load...").clicked() {
                        let dir = s.rom_path.parent().map(Path::to_path_buf);
                        let mut d = rfd::FileDialog::new().set_title("Select cheats file").add_filter("Cheats", &["cheats", "cht", "txt"]);
                        if let Some(dir) = dir {
                            d = d.set_directory(dir);
                        }
                        if let Some(p) = d.pick_file() {
                            notice = Some(match load_cheats_file(s, &p) {
                                Ok(n) => format!("Loaded {n} cheat set(s)"),
                                Err(e) => e,
                            });
                        }
                    }
                    if ui.button("Save...").clicked() {
                        let name = s.rom_path.with_extension("cheats");
                        let mut d = rfd::FileDialog::new().set_title("Save cheats file").add_filter("Cheats", &["cheats"]);
                        if let (Some(dir), Some(f)) = (name.parent(), name.file_name()) {
                            d = d.set_directory(dir).set_file_name(f.to_string_lossy());
                        }
                        if let Some(p) = d.save_file() {
                            notice = Some(match save_cheats_file(s, &p) {
                                Ok(()) => format!("Saved {}", p.display()),
                                Err(e) => e,
                            });
                        }
                    }
                    let can_remove = self.selected.map_or(false, |i| i < sets_mut(s).len());
                    if ui.add_enabled(can_remove, egui::Button::new("Remove")).clicked() {
                        remove_set(s, self.selected.unwrap());
                        self.selected = None;
                    }
                });
                ui.separator();

                // Set list: [x] name (n lines)
                egui::ScrollArea::vertical()
                    .id_salt("cheat_sets")
                    .max_height(180.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let sets = sets_mut(s);
                        if sets.is_empty() {
                            ui.weak("No cheats. Enter codes below and press \"Add New Code\".");
                        }
                        for (i, set) in sets.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut set.enabled, "");
                                let sel = self.selected == Some(i);
                                let mut name = set.name.clone();
                                let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(220.0).hint_text("(unnamed)"));
                                if r.changed() {
                                    set.rename(&name);
                                }
                                if r.gained_focus() {
                                    self.selected = Some(i);
                                }
                                if ui.selectable_label(sel, format!("{} line(s)", set.lines.len())).clicked() {
                                    self.selected = Some(i);
                                }
                            });
                            if self.selected == Some(i) && !set.lines.is_empty() {
                                ui.indent(("lines", i), |ui| {
                                    for l in &set.lines {
                                        ui.monospace(l);
                                    }
                                });
                            }
                        }
                    });
                ui.separator();

                let types: &[(i32, &str)] = if is_gba { &GBA_TYPES } else { &GB_TYPES };
                if !types.iter().any(|(t, _)| *t == self.code_type) {
                    self.code_type = 0;
                }
                ui.horizontal(|ui| {
                    ui.label("Code type");
                    egui::ComboBox::from_id_salt("cheat_type")
                        .selected_text(types.iter().find(|(t, _)| *t == self.code_type).map_or("", |(_, n)| n))
                        .show_ui(ui, |ui| {
                            for (t, n) in types {
                                ui.selectable_value(&mut self.code_type, *t, *n);
                            }
                        });
                });
                ui.add(
                    egui::TextEdit::multiline(&mut self.code_text)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(5)
                        .desired_width(f32::INFINITY)
                        .hint_text("One code per line"),
                );
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.add(egui::TextEdit::singleline(&mut self.new_name).desired_width(160.0));
                    let has_code = !self.code_text.trim().is_empty();
                    if ui.add_enabled(has_code, egui::Button::new("Add New Code")).clicked() {
                        let idx = new_set(s, &self.new_name);
                        let bad = add_lines(s, idx, &self.code_text, self.code_type);
                        if sets_mut(s)[idx].lines.is_empty() {
                            remove_set(s, idx);
                            notice = Some("No valid codes were entered".into());
                        } else {
                            self.selected = Some(idx);
                            self.code_text.clear();
                            self.new_name.clear();
                            if bad > 0 {
                                notice = Some(format!("{bad} line(s) were not valid codes"));
                            }
                        }
                    }
                    let target = self.selected.filter(|&i| i < sets_mut(s).len());
                    if ui.add_enabled(has_code && target.is_some(), egui::Button::new("Add Lines")).clicked() {
                        let bad = add_lines(s, target.unwrap(), &self.code_text, self.code_type);
                        self.code_text.clear();
                        if bad > 0 {
                            notice = Some(format!("{bad} line(s) were not valid codes"));
                        }
                    }
                });
                if let Some(n) = notice {
                    tc.notices.push(n);
                }
            });
    }
}
