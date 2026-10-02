// Game library — mirrors mGBA Qt's LibraryController ("Add folder to
// library..." + the library list view): folders are scanned recursively for
// ROMs and listed with name / platform / size / location. mGBA keeps the
// library in an SQLite mLibrary; rgba stores the folder list and the last
// scan in `<config dir>/library.json`.
//
// Loading needs the app: double-click or "Load" sets `requested_load`; since
// tools are held as `Box<dyn ToolWindow>`, the request is also published to
// `take_requested_load()`, which the app polls each frame.

use std::path::{Path, PathBuf};

use eframe::egui;
use serde::{Deserialize, Serialize};

use super::{ToolCtx, ToolWindow};

static REQUESTED: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Take the ROM the user picked in the library, if any (poll once per frame).
pub fn take_requested_load() -> Option<PathBuf> {
    REQUESTED.lock().ok().and_then(|mut r| r.take())
}

const ROM_EXTS: [&str; 8] = ["gba", "agb", "gb", "gbc", "sgb", "cgb", "bin", "zip"];

#[derive(Clone, Serialize, Deserialize)]
pub struct LibraryEntry {
    pub path: PathBuf,
    pub name: String,
    pub platform: String,
    pub size: u64,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct LibraryStore {
    folders: Vec<PathBuf>,
    entries: Vec<LibraryEntry>,
}

fn store_path() -> PathBuf {
    crate::config::config_dir().join("library.json")
}

/// Platform from the header where we can read it cheaply (GBA: fixed byte
/// 0x96 at 0xB2; GB: Nintendo logo start at 0x104), else the extension.
fn classify(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    if !ROM_EXTS.contains(&ext.as_str()) {
        return None;
    }
    if ext == "zip" {
        return Some("Archive".into());
    }
    use std::io::Read;
    let mut head = [0u8; 0x150];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    if n >= 0xB3 && head[0xB2] == 0x96 {
        return Some("GBA".into());
    }
    if n >= 0x144 && head[0x104..0x108] == [0xCE, 0xED, 0x66, 0x66] {
        return Some(if head[0x143] & 0x80 != 0 { "GBC".into() } else { "GB".into() });
    }
    match ext.as_str() {
        "gba" | "agb" => Some("GBA".into()),
        "gb" | "sgb" => Some("GB".into()),
        "gbc" | "cgb" => Some("GBC".into()),
        _ => None, // .bin that isn't a recognizable ROM
    }
}

fn scan(dir: &Path, out: &mut Vec<LibraryEntry>, depth: u32) {
    if depth > 16 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            scan(&p, out, depth + 1);
        } else if let Some(platform) = classify(&p) {
            out.push(LibraryEntry {
                name: p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                size: e.metadata().map_or(0, |m| m.len()),
                platform,
                path: p,
            });
        }
    }
}

#[derive(Default)]
pub struct LibraryView {
    /// Set when the user asks to load an entry; the parent takes it.
    pub requested_load: Option<PathBuf>,
    store: Option<LibraryStore>,
    filter: String,
    selected: Option<PathBuf>,
    status: String,
}

impl LibraryView {
    fn store(&mut self) -> &mut LibraryStore {
        self.store.get_or_insert_with(|| {
            std::fs::read_to_string(store_path())
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        })
    }

    fn save(&mut self) {
        let p = store_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(s) = serde_json::to_string(self.store()) {
            let _ = std::fs::write(p, s);
        }
    }

    fn rescan(&mut self) {
        let folders = self.store().folders.clone();
        let mut entries = Vec::new();
        for f in &folders {
            scan(f, &mut entries, 0);
        }
        entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        entries.dedup_by(|a, b| a.path == b.path);
        self.status = format!("{} games in {} folder(s)", entries.len(), folders.len());
        self.store().entries = entries;
        self.save();
    }
}

fn human_size(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MiB", n as f64 / (1 << 20) as f64)
    } else {
        format!("{} KiB", n >> 10)
    }
}

impl ToolWindow for LibraryView {
    fn title(&self) -> &'static str {
        "Library"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, _tc: &mut ToolCtx) {
        self.show_inner(ctx, open);
        if let Some(p) = self.requested_load.take() {
            if let Ok(mut r) = REQUESTED.lock() {
                *r = Some(p);
            }
        }
    }
}

impl LibraryView {
    fn show_inner(&mut self, ctx: &egui::Context, open: &mut bool) {
        egui::Window::new("Library").open(open).default_size([620.0, 420.0]).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Add folder...").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_title("Add folder to library").pick_folder() {
                        if !self.store().folders.contains(&dir) {
                            self.store().folders.push(dir);
                        }
                        self.rescan();
                    }
                }
                if ui.button("Rescan").clicked() {
                    self.rescan();
                }
                ui.menu_button("Folders", |ui| {
                    let folders = self.store().folders.clone();
                    if folders.is_empty() {
                        ui.label("No folders yet.");
                    }
                    for f in folders {
                        ui.horizontal(|ui| {
                            ui.label(f.display().to_string());
                            if ui.small_button("Remove").clicked() {
                                self.store().folders.retain(|x| x != &f);
                                self.rescan();
                            }
                        });
                    }
                });
                ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter").desired_width(180.0));
                let can_load = self.selected.is_some();
                if ui.add_enabled(can_load, egui::Button::new("Load")).clicked() {
                    self.requested_load = self.selected.clone();
                }
            });
            if !self.status.is_empty() {
                ui.label(&self.status);
            }
            ui.separator();
            let f = self.filter.to_lowercase();
            let entries: Vec<LibraryEntry> = self
                .store()
                .entries
                .iter()
                .filter(|e| f.is_empty() || e.name.to_lowercase().contains(&f) || e.platform.to_lowercase() == f)
                .cloned()
                .collect();
            if entries.is_empty() {
                ui.label("No games. Use \"Add folder...\" to scan a folder of ROMs.");
                return;
            }
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                egui::Grid::new("library").striped(true).num_columns(4).show(ui, |ui| {
                    ui.strong("Name");
                    ui.strong("Platform");
                    ui.strong("Size");
                    ui.strong("Location");
                    ui.end_row();
                    for e in &entries {
                        let sel = self.selected.as_ref() == Some(&e.path);
                        let r = ui.selectable_label(sel, &e.name);
                        if r.clicked() {
                            self.selected = Some(e.path.clone());
                        }
                        if r.double_clicked() {
                            self.requested_load = Some(e.path.clone());
                        }
                        ui.label(&e.platform);
                        ui.label(human_size(e.size));
                        ui.label(e.path.parent().map(|p| p.display().to_string()).unwrap_or_default());
                        ui.end_row();
                    }
                });
            });
        });
    }
}
