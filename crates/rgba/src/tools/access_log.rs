// "Log memory accesses..." — mirrors mGBA Qt's MemoryAccessLogView.cpp +
// MemoryAccessLogController.cpp over rgba_debugger::access_logger: choose a
// log file, Load (optionally loading an existing mAL\1 file) / Unload,
// region checkboxes, Start / Stop, and "Export ROM snapshot" (the shadow
// file of what the game actually read from cart0).
//
// Wiring differs from the C: the logger is driven directly by this window
// instead of being registered as a debugger module. Starting installs the
// logger core into the console (`dbg_set_access_log`, which needs the
// console's debugger state, so the debugger is attached first) and the
// memory-shim hooks record every load/store from then on, also during a
// plain `run_frame`. Instruction-execution flags come from the debugger's
// per-step callback, which only runs under `debugger_run_frame`; see
// `wants_debugger_run_loop`.
//
// The log is written to disk on Stop/Unload and every AUTOSAVE_FRAMES while
// logging (the C keeps it live in an mmap instead).

use std::path::PathBuf;

use eframe::egui;
use rgba_debugger::access_logger::{
    access_log_region_flags, core_memory_flags as mf, AccessLogger, MAL_PLATFORM_GB,
    MAL_PLATFORM_GBA,
};

use super::memory_view::{dbg, long_name};
use super::{no_game, ToolCtx, ToolWindow};
use crate::emu::{Console, Session};

const AUTOSAVE_FRAMES: u32 = 600;

struct RegionBox {
    internal: &'static str,
    label: String,
    checked: bool,
    /// Region id inside the logger once watched.
    mapped: Option<i32>,
}

pub struct AccessLogView {
    path: Option<PathBuf>,
    log_extra: bool,
    load_existing: bool,
    logger: Option<AccessLogger>,
    active: bool,
    regions: Vec<RegionBox>,
    frames: u32,
    status: String,
}

impl Default for AccessLogView {
    fn default() -> Self {
        AccessLogView {
            path: None,
            log_extra: false,
            load_existing: true,
            logger: None,
            active: false,
            regions: Vec::new(),
            frames: 0,
            status: String::new(),
        }
    }
}

fn attach(c: &mut Console) {
    match c {
        Console::Gba(g) => g.debugger_attach(),
        Console::Gb(g) => g.debugger_attach(),
    }
}

impl AccessLogView {
    /// True while logging: instruction-execution flags are only recorded if
    /// the app drives frames with `debugger_run_frame` (data accesses are
    /// recorded either way). The app loop does not consult this yet.
    #[allow(dead_code)]
    pub fn wants_debugger_run_loop(&self) -> bool {
        self.active
    }

    fn flags(&self) -> u64 {
        if self.log_extra {
            access_log_region_flags::HAS_EX_BLOCK
        } else {
            0
        }
    }

    /// MemoryAccessLogController::listRegions — mapped, non-virtual blocks.
    fn list_regions(c: &mut Console) -> Vec<RegionBox> {
        let gba = matches!(c, Console::Gba(_));
        dbg(c)
            .dbg_list_memory_blocks_full()
            .into_iter()
            .filter(|b| b.flags & mf::MAPPED != 0 && b.flags & mf::VIRTUAL == 0)
            .map(|b| {
                let ln = long_name(b.internal_name, gba);
                RegionBox {
                    internal: b.internal_name,
                    label: if ln.is_empty() { b.internal_name.to_string() } else { ln.to_string() },
                    checked: false,
                    mapped: None,
                }
            })
            .collect()
    }

    fn save(&mut self) {
        let (Some(path), Some(logger)) = (&self.path, &self.logger) else { return };
        if let Some(bytes) = logger.save_bytes() {
            match std::fs::write(path, bytes) {
                Ok(()) => self.status = format!("Saved {}", path.display()),
                Err(e) => self.status = format!("Write failed: {e}"),
            }
        }
    }

    /// MemoryAccessLogController::load
    fn load(&mut self, c: &mut Console) -> bool {
        let Some(path) = self.path.clone() else {
            self.status = "Choose a log file first".into();
            return false;
        };
        let platform = if matches!(c, Console::Gba(_)) { MAL_PLATFORM_GBA } else { MAL_PLATFORM_GB };
        let mut logger = AccessLogger::new(platform);
        let existing = if self.load_existing { std::fs::read(&path).ok() } else { None };
        if !logger.open_bytes(existing.as_deref(), true, !self.load_existing) {
            self.status = "Could not open access log (wrong platform or corrupt file?)".into();
            return false;
        }
        self.logger = Some(logger);
        for r in &mut self.regions {
            r.mapped = None;
        }
        self.status = format!("Loaded {}", path.display());
        true
    }

    fn unload(&mut self, c: &mut Console) {
        if self.active {
            self.stop(c);
        }
        self.save();
        if let Some(mut l) = self.logger.take() {
            l.close(dbg(c));
        }
        for r in &mut self.regions {
            r.mapped = None;
        }
    }

    /// Watch every checked-but-unmapped region. The logger core must be
    /// module-owned (not started) for this.
    fn watch_pending(&mut self, c: &mut Console) {
        let flags = self.flags();
        let Some(logger) = self.logger.as_mut() else { return };
        for r in self.regions.iter_mut().filter(|r| r.checked && r.mapped.is_none()) {
            let id = logger.watch_memory_block_name(dbg(c), r.internal, flags);
            if id >= 0 {
                r.mapped = Some(id);
            }
        }
    }

    /// MemoryAccessLogController::start
    fn start(&mut self, c: &mut Console) {
        if self.logger.is_none() && !self.load(c) {
            return;
        }
        attach(c);
        self.watch_pending(c);
        let Some(logger) = self.logger.as_mut() else { return };
        if logger.start(dbg(c)) {
            self.active = true;
            self.frames = 0;
            self.status = "Logging".into();
        } else {
            self.status = "This console cannot host an access logger".into();
        }
    }

    fn stop(&mut self, c: &mut Console) {
        if let Some(logger) = self.logger.as_mut() {
            logger.stop(dbg(c));
        }
        self.active = false;
        self.save();
    }

    /// MemoryAccessLogController::exportFile — shadow copy of cart0.
    fn export(&mut self, c: &mut Console) {
        let Some(id) = self.regions.iter().find(|r| r.internal == "cart0").and_then(|r| r.mapped) else {
            self.status = "Watch the Game Pak region and log something first".into();
            return;
        };
        let Some(logger) = &self.logger else { return };
        let console = dbg(c);
        let Some(bytes) = logger.create_shadow_file(id, 0, |addr, seg| console.dbg_raw_read(addr, seg, 1) as u8) else {
            self.status = "Nothing to export".into();
            return;
        };
        if let Some(p) = rfd::FileDialog::new().set_title("Export ROM snapshot").set_file_name("snapshot.gba").save_file() {
            self.status = match std::fs::write(&p, bytes) {
                Ok(()) => format!("Exported {}", p.display()),
                Err(e) => format!("Write failed: {e}"),
            };
        }
    }
}

impl ToolWindow for AccessLogView {
    fn title(&self) -> &'static str {
        "Memory access logging"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        egui::Window::new(self.title()).open(open).default_width(380.0).show(ctx, |ui| {
            let Some(s) = tc.session.as_deref_mut() else {
                no_game(ui);
                return;
            };
            let title = s.title();
            let c = &mut s.console;
            if self.regions.is_empty() {
                self.regions = Self::list_regions(c);
            }
            let loaded = self.logger.is_some();

            ui.horizontal(|ui| {
                ui.label("Log file");
                let name = self.path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "(none)".into());
                ui.add_enabled(!loaded, egui::Label::new(name).truncate());
                if ui.add_enabled(!loaded, egui::Button::new("Browse")).clicked() {
                    let default = format!("{title}.mal");
                    if let Some(p) = rfd::FileDialog::new()
                        .set_title("Select access log file")
                        .add_filter("Memory access logs", &["mal"])
                        .set_file_name(default)
                        .save_file()
                    {
                        self.path = Some(p);
                    }
                }
            });
            ui.add_enabled(!loaded, egui::Checkbox::new(&mut self.log_extra, "Log additional information (uses 3× space)"));
            ui.add_enabled(!loaded, egui::Checkbox::new(&mut self.load_existing, "Load existing file if present"));
            ui.horizontal(|ui| {
                if ui.add_enabled(!loaded, egui::Button::new("Load")).clicked() {
                    self.load(c);
                }
                if ui.add_enabled(loaded && !self.active, egui::Button::new("Unload")).clicked() {
                    self.unload(c);
                }
            });

            ui.separator();
            ui.label("Regions");
            let mut added = false;
            for r in &mut self.regions {
                // Watched regions can't be unwatched (Qt disables them).
                let locked = r.mapped.is_some();
                let before = r.checked;
                ui.add_enabled(!locked, egui::Checkbox::new(&mut r.checked, &r.label));
                if r.checked && !before {
                    added = true;
                }
            }
            if added && self.active {
                // Our logger only accepts new regions while stopped.
                if let Some(logger) = self.logger.as_mut() {
                    logger.stop(dbg(c));
                }
                self.watch_pending(c);
                if let Some(logger) = self.logger.as_mut() {
                    logger.start(dbg(c));
                }
            }

            ui.separator();
            ui.horizontal(|ui| {
                if ui.add_enabled(loaded && !self.active, egui::Button::new("Export ROM snapshot")).clicked() {
                    self.export(c);
                }
                if ui.add_enabled(!self.active && self.path.is_some(), egui::Button::new("Start")).clicked() {
                    self.start(c);
                }
                if ui.add_enabled(self.active, egui::Button::new("Stop")).clicked() {
                    self.stop(c);
                }
            });
            if self.active {
                ui.label(
                    egui::RichText::new(
                        "Recording data reads/writes. Instruction execution is only recorded when frames run under the debugger.",
                    )
                    .weak(),
                );
            }
            if !self.status.is_empty() {
                ui.label(egui::RichText::new(&self.status).weak());
            }
        });
    }

    fn on_frame(&mut self, session: &mut Session) {
        if !self.active {
            return;
        }
        self.frames += 1;
        if self.frames < AUTOSAVE_FRAMES {
            return;
        }
        self.frames = 0;
        // The core lives in the console while started; snapshot it there.
        let bytes = dbg(&mut session.console).dbg_access_log().map(|core| core.serialize());
        if let (Some(path), Some(bytes)) = (&self.path, bytes) {
            let _ = std::fs::write(path, bytes);
        }
    }

    fn on_session_changed(&mut self) {
        // The installed logger core went away with the old console; anything
        // since the last autosave is lost.
        *self = AccessLogView {
            path: self.path.take(),
            log_extra: self.log_extra,
            load_existing: self.load_existing,
            ..AccessLogView::default()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_bus_store_without_debugger_loop() {
        let mut c = Console::Gba(rgba_gba::gba::Gba::new());
        let mut v = AccessLogView::default();
        v.path = Some(std::env::temp_dir().join("rgba_access_log_test.mal"));
        v.load_existing = false;
        v.regions = AccessLogView::list_regions(&mut c);
        v.regions.iter_mut().find(|r| r.internal == "wram").unwrap().checked = true;
        v.start(&mut c);
        assert!(v.active, "{}", v.status);
        if let Console::Gba(g) = &mut c {
            let mut cyc = 0;
            g.store32(0x0200_0010, 0x55, &mut cyc);
        }
        v.stop(&mut c);
        let logger = v.logger.as_ref().unwrap();
        let (r, off) = logger.get_region(0x0200_0010, -1).unwrap();
        let core = logger.access_log().unwrap();
        assert_ne!(core.regions[r].block[off], 0, "store was not recorded");
        let bytes = logger.save_bytes().unwrap();
        assert!(bytes.starts_with(b"mAL\x01"));
        let _ = std::fs::remove_file(v.path.as_ref().unwrap());
    }
}
