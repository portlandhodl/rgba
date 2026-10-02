// The main window: menu bar, game display, input, frame pacing. Mirrors
// mGBA Qt's Window.cpp (menus/actions) + CoreController (run loop, states,
// rewind, fast forward) on a single thread — the cores are not Send.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Key, KeyboardShortcut, Modifiers};

use rgba_core::core::Platform;

use crate::audio::Audio;
use crate::config::{native_fps, Config};
use crate::emu::{LoadOptions, Session};
use crate::logbuf::LogBuffer;
use crate::tools::{ToolCtx, ToolId, Tools};

/// Startup options from the command line.
#[derive(Default)]
pub struct Startup {
    pub rom: Option<PathBuf>,
    pub patch: Option<PathBuf>,
    pub cheats: Option<PathBuf>,
    pub video_log: Option<PathBuf>,
    pub gbp: bool,
    pub stdio_debugger: bool,
    /// `--no-sync`: run as fast as possible.
    pub unthrottled: bool,
}

const GBA_BUTTONS: u32 = 10;

#[derive(Clone, Copy, PartialEq)]
enum StateDialog {
    Load,
    Save,
}

pub struct App {
    cfg: Config,
    session: Option<Session>,
    audio: Option<Audio>,
    log: LogBuffer,
    tools: Tools,

    texture: Option<egui::TextureHandle>,
    blend_prev: Vec<u32>,
    blend_out: Vec<u32>,

    paused: bool,
    frame_advance: bool,
    ff_toggle: bool,
    ff_held: bool,
    rewind_held: bool,
    accum: f64,
    last_tick: Instant,
    frames_run: u64,
    fps: f64,
    fps_window: (Instant, u64),
    dirty_frames: u32,

    autofire_mask: u32,
    autofire_phase: u32,
    gs_button: bool,
    gilrs: Option<gilrs::Gilrs>,

    fullscreen: bool,
    menu_height: f32,
    notices: Vec<(String, Instant)>,
    pending_notices: Vec<String>,
    error: Option<String>,
    state_dialog: Option<StateDialog>,
    state_thumbs: Vec<Option<egui::TextureHandle>>,
    archive_pick: Option<(PathBuf, Vec<String>)>,
    placement_open: bool,
    stdio_debugger: bool,
    startup_video_log: Option<PathBuf>,
    force_gbp: bool,
    pending_resize: Option<u32>,
    unthrottled: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext, cfg: Config, startup: Startup) -> App {
        let log = LogBuffer::default();
        log.install();
        let audio = match Audio::new(cfg.sample_rate) {
            Ok(a) => Some(a),
            Err(e) => {
                eprintln!("audio unavailable: {e}");
                None
            }
        };
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let mut app = App {
            cfg,
            session: None,
            audio,
            log,
            tools: Tools::new(),
            texture: None,
            blend_prev: Vec::new(),
            blend_out: Vec::new(),
            paused: false,
            frame_advance: false,
            ff_toggle: false,
            ff_held: false,
            rewind_held: false,
            accum: 0.0,
            last_tick: Instant::now(),
            frames_run: 0,
            fps: 0.0,
            fps_window: (Instant::now(), 0),
            dirty_frames: 0,
            autofire_mask: 0,
            autofire_phase: 0,
            gs_button: false,
            gilrs: gilrs::Gilrs::new().ok(),
            fullscreen: false,
            menu_height: 0.0,
            notices: Vec::new(),
            pending_notices: Vec::new(),
            error: None,
            state_dialog: None,
            state_thumbs: Vec::new(),
            archive_pick: None,
            placement_open: false,
            stdio_debugger: startup.stdio_debugger,
            startup_video_log: startup.video_log,
            force_gbp: startup.gbp,
            pending_resize: None,
            unthrottled: startup.unthrottled,
        };
        if let Some(rom) = startup.rom {
            let opts = LoadOptions { patch: startup.patch, force_gbp: startup.gbp, ..Default::default() };
            app.load_rom_with(&rom, opts);
            if let (Some(c), Some(s)) = (startup.cheats, app.session.as_mut()) {
                match crate::tools::cheats::load_cheats_file(s, &c) {
                    Ok(n) => app.pending_notices.push(format!("Loaded {n} cheat set(s)")),
                    Err(e) => app.error = Some(e),
                }
            }
        }
        app.pending_resize = Some(app.cfg.frame_scale);
        app
    }

    // ---- Game lifecycle -----------------------------------------------------

    fn load_rom_with(&mut self, path: &Path, opts: LoadOptions) {
        if crate::archive::is_archive(path) {
            match crate::archive::list_roms(path) {
                Ok(names) if names.len() == 1 => {
                    self.load_from_archive(path, &names[0], opts);
                }
                Ok(names) if names.is_empty() => {
                    self.error = Some(format!("No ROMs found in {}", path.display()));
                }
                Ok(names) => self.archive_pick = Some((path.to_path_buf(), names)),
                Err(e) => self.error = Some(e),
            }
            return;
        }
        self.close_game();
        match Session::open(path, &self.cfg, &opts) {
            Ok(s) => self.start_session(s, path),
            Err(e) => self.error = Some(e),
        }
    }

    fn load_from_archive(&mut self, archive: &Path, entry: &str, opts: LoadOptions) {
        match crate::archive::extract_to_cache(archive, entry) {
            Ok(rom) => {
                self.close_game();
                // Keep saves/states next to the archive, named after the entry.
                let save = opts.save_path.clone().or_else(|| {
                    Some(archive.with_file_name(Path::new(entry).with_extension("sav").file_name()?))
                });
                let opts = LoadOptions { save_path: save, ..opts };
                match Session::open(&rom, &self.cfg, &opts) {
                    Ok(mut s) => {
                        s.rom_path = archive.with_file_name(Path::new(entry).file_name().unwrap_or_default());
                        self.start_session(s, archive);
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn start_session(&mut self, mut s: Session, recent: &Path) {
        crate::tools::overrides::apply_saved_overrides(&mut s);
        s.set_solar_level(0);
        if let Some(vl) = self.startup_video_log.take() {
            let r = match &mut s.console {
                crate::emu::Console::Gba(g) => g.start_video_log(&vl),
                crate::emu::Console::Gb(g) => g.start_video_log(&vl),
            };
            match r {
                Ok(()) => s.video_log = true,
                Err(e) => self.error = Some(format!("video log: {e}")),
            }
        }
        if self.stdio_debugger {
            crate::stdio_debugger::attach(&mut s);
        }
        self.cfg.push_recent(recent);
        self.cfg.save();
        self.session = Some(s);
        self.tools.session_changed();
        self.paused = false;
        self.accum = 0.0;
        self.blend_prev.clear();
        if let Some(a) = &mut self.audio {
            a.clear();
        }
    }

    fn close_game(&mut self) {
        if let Some(mut s) = self.session.take() {
            s.flush_savedata(false);
            if s.video_log {
                match &mut s.console {
                    crate::emu::Console::Gba(g) => g.end_video_log(),
                    crate::emu::Console::Gb(g) => g.end_video_log(),
                }
            }
        }
        self.tools.session_changed();
        self.texture = None;
        if let Some(a) = &mut self.audio {
            a.clear();
        }
    }

    fn reset(&mut self) {
        if let Some(s) = &mut self.session {
            s.flush_savedata(false);
            s.core().reset();
            if self.cfg.skip_bios {
                if let crate::emu::Console::Gba(g) = &mut s.console {
                    if g.has_bios {
                        g.skip_bios();
                    }
                }
            }
            self.notice("Reset");
        }
    }

    fn notice(&mut self, msg: impl Into<String>) {
        self.pending_notices.push(msg.into());
    }

    fn report<T>(&mut self, r: Result<T, String>, ok: impl Into<String>) {
        match r {
            Ok(_) => self.notice(ok),
            Err(e) => self.error = Some(e),
        }
    }

    // ---- File dialogs -------------------------------------------------------

    fn rom_dir(&self) -> Option<PathBuf> {
        self.session
            .as_ref()
            .and_then(|s| s.rom_path.parent().map(Path::to_path_buf))
            .or_else(|| self.cfg.recent.first().and_then(|p| p.parent().map(Path::to_path_buf)))
    }

    fn pick(&self, title: &str, filters: &[(&str, &[&str])]) -> Option<PathBuf> {
        let mut d = rfd::FileDialog::new().set_title(title);
        for (name, exts) in filters {
            d = d.add_filter(*name, exts);
        }
        if let Some(dir) = self.rom_dir() {
            d = d.set_directory(dir);
        }
        d.pick_file()
    }

    fn pick_save(&self, title: &str, name: &str, filters: &[(&str, &[&str])]) -> Option<PathBuf> {
        let mut d = rfd::FileDialog::new().set_title(title).set_file_name(name);
        for (n, exts) in filters {
            d = d.add_filter(*n, exts);
        }
        if let Some(dir) = self.rom_dir() {
            d = d.set_directory(dir);
        }
        d.save_file()
    }

    fn select_rom(&mut self) {
        if let Some(p) = self.pick("Select ROM", ROM_FILTERS) {
            self.load_rom_with(&p, LoadOptions { force_gbp: self.force_gbp, ..Default::default() });
        }
    }

    fn select_rom_in_archive(&mut self) {
        if let Some(p) = self.pick("Select archive", &[("Archives", &["zip"])]) {
            match crate::archive::list_roms(&p) {
                Ok(names) if names.is_empty() => self.error = Some("No ROMs in archive".into()),
                Ok(names) => self.archive_pick = Some((p, names)),
                Err(e) => self.error = Some(e),
            }
        }
    }

    fn select_patch(&mut self) {
        let Some(rom) = self.session.as_ref().map(|s| s.rom_path.clone()) else { return };
        if let Some(p) = self.pick("Select patch", &[("Patches", &["ips", "ups", "bps"])]) {
            // mGBA applies a patch on the next (re)load of the game.
            self.load_rom_with(&rom, LoadOptions { patch: Some(p), force_gbp: self.force_gbp, ..Default::default() });
            self.notice("Patch applied (game restarted)");
        }
    }

    fn boot_bios(&mut self) {
        let Some(bios) = self.cfg.gba_bios.clone() else {
            self.error = Some("Set a GBA BIOS in Settings → BIOS first.".into());
            return;
        };
        self.close_game();
        match Session::boot_bios(&bios, &self.cfg) {
            Ok(s) => {
                self.session = Some(s);
                self.tools.session_changed();
                self.paused = false;
            }
            Err(e) => self.error = Some(e),
        }
    }

    // ---- Frame loop ---------------------------------------------------------

    fn fast_forward(&self) -> bool {
        self.ff_toggle || self.ff_held
    }

    fn gather_keys(&mut self, ctx: &egui::Context) -> u32 {
        let mut keys = 0u32;
        if !ctx.egui_wants_keyboard_input() {
            ctx.input(|i| {
                for (bit, name) in self.cfg.keys.iter().enumerate() {
                    if let Some(k) = Key::from_name(name) {
                        if i.key_down(k) && !i.modifiers.ctrl && !i.modifiers.alt {
                            keys |= 1 << bit;
                        }
                    }
                }
            });
        }
        if let Some(g) = &mut self.gilrs {
            while g.next_event().is_some() {}
            for (_, pad) in g.gamepads() {
                for (bit, name) in self.cfg.pad_buttons.iter().enumerate() {
                    if let Some(b) = crate::input::pad_button(name) {
                        if pad.is_pressed(b) {
                            keys |= 1 << bit;
                        }
                    }
                }
                let dz = self.cfg.pad_deadzone;
                let x = pad.value(gilrs::Axis::LeftStickX);
                let y = pad.value(gilrs::Axis::LeftStickY);
                if x > dz { keys |= 1 << 4; }
                if x < -dz { keys |= 1 << 5; }
                if y > dz { keys |= 1 << 6; }
                if y < -dz { keys |= 1 << 7; }
            }
        }
        // Autofire: held buttons toggle every `autofire_period` frames.
        let period = self.cfg.autofire_period.max(1);
        if (self.autofire_phase / period) % 2 == 1 {
            keys &= !self.autofire_mask;
        } else {
            keys |= self.autofire_mask;
        }
        keys & ((1 << GBA_BUTTONS) - 1)
    }

    fn run_one_frame(&mut self, keys: u32) {
        let muted = self.cfg.mute || (self.fast_forward() && self.cfg.ff_mute);
        let Some(s) = self.session.as_mut() else { return };
        s.core().set_keys(keys);
        if self.tools.run_frame(s) {
            // A tool (linked multiplayer) drove the frame.
        } else if s.core().debugger_attached() {
            s.core().debugger_run_frame();
        } else {
            s.core().run_frame();
        }
        s.rewind_tick(self.cfg.rewind_interval);
        self.tools.on_frame(s);
        let rate = s.core().audio_sample_rate();
        if let Some(a) = &mut self.audio {
            a.pump(s.core().audio_buffer(), rate, self.cfg.volume, muted);
        } else {
            s.core().audio_buffer().clear();
        }
        self.autofire_phase = self.autofire_phase.wrapping_add(1);
        self.frames_run += 1;
        self.dirty_frames += 1;
        // Flush the battery save shortly after the game stops writing it
        // (mSavedataClean's dirt-age window).
        if self.dirty_frames >= 60 {
            self.dirty_frames = 0;
            s.flush_savedata(false);
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f64().min(0.25);
        self.last_tick = now;
        if self.session.is_none() {
            return;
        }
        let keys = self.gather_keys(ctx);
        if let Some(s) = &mut self.session {
            if let crate::emu::Console::Gba(g) = &mut s.console {
                g.cheats.press_button(self.gs_button);
            }
        }

        if self.rewind_held && !self.paused {
            // Rewind (held): step back one history entry per displayed frame.
            if let Some(s) = &mut self.session {
                if !s.rewind_step(1) {
                    self.rewind_held = false;
                }
                s.core().audio_buffer().clear();
            }
            self.accum = 0.0;
            return;
        }

        if self.paused {
            if self.frame_advance {
                self.frame_advance = false;
                self.run_one_frame(keys);
            }
            self.accum = 0.0;
            return;
        }

        let ff = self.fast_forward() || self.unthrottled;
        let speed = if self.unthrottled { -1.0 } else if ff { self.cfg.ff_speed } else { 1.0 };
        if ff && speed <= 0.0 {
            // Unbounded: as many frames as fit in most of a display frame.
            let budget = Duration::from_millis(14);
            let start = Instant::now();
            while start.elapsed() < budget {
                self.run_one_frame(keys);
            }
            self.accum = 0.0;
            return;
        }
        let target = if self.cfg.fps_target > 0.0 { self.cfg.fps_target } else { native_fps() };
        self.accum += dt * target * speed as f64;
        // Don't try to catch up after a stall (dialogs, window drags).
        let cap = 4.0 * speed.max(1.0) as f64;
        if self.accum > cap {
            self.accum = cap;
        }
        while self.accum >= 1.0 {
            self.accum -= 1.0;
            self.run_one_frame(keys);
        }
    }

    // ---- Hotkeys ------------------------------------------------------------

    fn shortcut(ctx: &egui::Context, m: Modifiers, k: Key) -> bool {
        ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)))
    }

    fn handle_hotkeys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let c = Modifiers::COMMAND;
        let sh = Modifiers::SHIFT;
        let none = Modifiers::NONE;
        if Self::shortcut(ctx, c, Key::O) { self.select_rom(); }
        if Self::shortcut(ctx, c, Key::Q) { ctx.send_viewport_cmd(egui::ViewportCommand::Close); }
        if Self::shortcut(ctx, c, Key::R) { self.reset(); }
        if Self::shortcut(ctx, c, Key::P) { self.toggle_pause(); }
        if Self::shortcut(ctx, c, Key::N) { self.next_frame(); }
        if Self::shortcut(ctx, c, Key::F) { self.toggle_fullscreen(ctx); }
        if Self::shortcut(ctx, c, Key::B) { self.step_back(); }
        if Self::shortcut(ctx, sh, Key::Tab) { self.ff_toggle = !self.ff_toggle; }
        if Self::shortcut(ctx, none, Key::F12) { self.screenshot(); }
        if Self::shortcut(ctx, none, Key::F10) { self.open_state_dialog(ctx, StateDialog::Load); }
        if Self::shortcut(ctx, sh, Key::F10) { self.open_state_dialog(ctx, StateDialog::Save); }
        if Self::shortcut(ctx, none, Key::F11) { self.undo_load(); }
        if Self::shortcut(ctx, sh, Key::F11) { self.undo_save(); }
        if self.fullscreen && Self::shortcut(ctx, none, Key::Escape) { self.toggle_fullscreen(ctx); }
        if self.cfg.hide_menu && Self::shortcut(ctx, Modifiers::ALT, Key::M) {
            self.cfg.hide_menu = false;
        }
        const FKEYS: [Key; 9] = [Key::F1, Key::F2, Key::F3, Key::F4, Key::F5, Key::F6, Key::F7, Key::F8, Key::F9];
        for (i, k) in FKEYS.iter().enumerate() {
            if Self::shortcut(ctx, sh, *k) { self.save_slot(i as u32 + 1); }
            if Self::shortcut(ctx, none, *k) { self.load_slot(i as u32 + 1); }
        }
        let (tab, tilde, back) = ctx.input(|i| {
            (
                i.key_down(Key::Tab) && !i.modifiers.shift,
                i.key_pressed(Key::Backtick) && i.modifiers.shift,
                i.key_down(Key::Backtick) && !i.modifiers.shift,
            )
        });
        self.ff_held = tab;
        if tilde {
            self.rewind_once();
        }
        if self.cfg.rewind_enable {
            self.rewind_held = back;
        }
    }

    fn toggle_pause(&mut self) {
        if self.session.is_some() {
            self.paused = !self.paused;
            if let Some(a) = &mut self.audio {
                a.clear();
            }
        }
    }

    fn next_frame(&mut self) {
        if self.session.is_some() {
            self.paused = true;
            self.frame_advance = true;
        }
    }

    fn rewind_once(&mut self) {
        let interval = self.cfg.rewind_interval.max(1) as usize;
        if let Some(s) = &mut self.session {
            // "Rewind": go back ~one second of history.
            let steps = (60 / interval).max(1);
            if !s.rewind_step(steps) {
                self.notice("Rewind history is empty (enable rewind in Settings)");
            }
        }
    }

    fn step_back(&mut self) {
        if let Some(s) = &mut self.session {
            self.paused = true;
            if !s.rewind_step(1) {
                self.pending_notices.push("Rewind history is empty (enable rewind in Settings)".into());
            }
        }
    }

    fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        self.fullscreen = !self.fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
    }

    fn set_frame_size(&mut self, scale: u32) {
        self.cfg.frame_scale = scale;
        self.pending_resize = Some(scale);
    }

    fn screenshot(&mut self) {
        let Some(s) = &mut self.session else { return };
        let (w, h) = s.core().base_video_size();
        let path = crate::screenshot::next_screenshot_path(&s.rom_path, self.cfg.screenshot_dir.as_deref());
        let r = crate::screenshot::save(s.core().video_buffer(), w, h, &path);
        self.report(r, format!("Screenshot saved to {}", path.display()));
    }

    fn save_slot(&mut self, slot: u32) {
        let Some(s) = &mut self.session else { return };
        let (w, h) = s.core().base_video_size();
        let thumb = crate::screenshot::encode_png(s.core().video_buffer(), w, h).ok();
        let r = s.save_slot(slot);
        if r.is_ok() {
            if let Some(png) = thumb {
                let _ = std::fs::write(s.state_path(slot).with_extension(format!("ss{slot}.png")), png);
            }
        }
        self.report(r, format!("State {slot} saved"));
    }

    fn load_slot(&mut self, slot: u32) {
        let Some(s) = &mut self.session else { return };
        let r = s.load_slot(slot);
        if let Some(a) = &mut self.audio {
            a.clear();
        }
        self.report(r, format!("State {slot} loaded"));
    }

    fn undo_load(&mut self) {
        if let Some(s) = &mut self.session {
            let r = s.undo_load();
            self.report(r, "Load state undone");
        }
    }

    fn undo_save(&mut self) {
        if let Some(s) = &mut self.session {
            let r = s.undo_save();
            self.report(r, "Save state undone");
        }
    }

    fn open_state_dialog(&mut self, ctx: &egui::Context, which: StateDialog) {
        let Some(s) = &self.session else { return };
        self.state_thumbs = (1..=9)
            .map(|slot| {
                let png = std::fs::read(s.state_path(slot).with_extension(format!("ss{slot}.png"))).ok()?;
                let img = crate::input::decode_png(&png)?;
                Some(ctx.load_texture(format!("state{slot}"), img, egui::TextureOptions::NEAREST))
            })
            .collect();
        self.state_dialog = Some(which);
        self.paused = true;
    }

    // ---- Menus --------------------------------------------------------------

    fn menu_item(ui: &mut egui::Ui, enabled: bool, label: &str, shortcut: &str) -> bool {
        let mut b = egui::Button::new(label);
        if !shortcut.is_empty() {
            b = b.shortcut_text(shortcut);
        }
        ui.add_enabled(enabled, b).clicked()
    }

    fn unavailable(ui: &mut egui::Ui, label: &str, why: &str) {
        ui.add_enabled(false, egui::Button::new(label)).on_disabled_hover_text(why);
    }

    fn menus(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let game = self.session.is_some();
        let is_gba = self.session.as_ref().map_or(false, |s| s.console.platform() == Platform::Gba);
        let is_gb = self.session.as_ref().map_or(false, |s| s.console.platform() == Platform::Gb);

        ui.menu_button("File", |ui| {
            if Self::menu_item(ui, true, "Load ROM...", "Ctrl+O") { self.select_rom(); }
            if Self::menu_item(ui, true, "Load ROM in archive...", "") { self.select_rom_in_archive(); }
            if Self::menu_item(ui, true, "Add folder to library...", "") { self.tools.open(ToolId::Library); }
            ui.menu_button("Save games", |ui| {
                if Self::menu_item(ui, game, "Load alternate save game...", "") { self.load_alternate_save(false); }
                if Self::menu_item(ui, game, "Load temporary save game...", "") { self.load_alternate_save(true); }
                ui.separator();
                if Self::menu_item(ui, true, "Convert save game...", "") { self.tools.open(ToolId::SaveConverter); }
                if Self::menu_item(ui, is_gba, "Import GameShark Save...", "") { self.import_sharkport(); }
                if Self::menu_item(ui, is_gba, "Export GameShark Save...", "") { self.export_sharkport(); }
                ui.separator();
                let mut p = self.cfg.save_player;
                ui.radio_value(&mut p, 0, "Automatically determine");
                for i in 1..=4 {
                    ui.radio_value(&mut p, i, format!("Use player {i} save game"));
                }
                if p != self.cfg.save_player {
                    self.cfg.save_player = p;
                    self.cfg.save();
                    if let Some(rom) = self.session.as_ref().map(|s| s.rom_path.clone()) {
                        self.load_rom_with(&rom, LoadOptions::default());
                    }
                }
            });
            ui.separator();
            if Self::menu_item(ui, game, "Load patch...", "") { self.select_patch(); }
            if Self::menu_item(ui, true, "Boot BIOS", "") { self.boot_bios(); }
            if Self::menu_item(ui, is_gba, "Scan e-Reader dotcodes...", "") { self.scan_ereader(); }
            if Self::menu_item(ui, game, "ROM info...", "") { self.tools.open(ToolId::RomInfo); }
            ui.menu_button("Recent", |ui| {
                let recent = self.cfg.recent.clone();
                for p in &recent {
                    let label = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if ui.button(label).on_hover_text(p.display().to_string()).clicked() {
                        self.load_rom_with(p, LoadOptions { force_gbp: self.force_gbp, ..Default::default() });
                    }
                }
                ui.separator();
                if Self::menu_item(ui, !recent.is_empty(), "Clear", "") {
                    self.cfg.recent.clear();
                    self.cfg.save();
                }
            });
            ui.separator();
            if Self::menu_item(ui, game, "Load state", "F10") { self.open_state_dialog(&ctx, StateDialog::Load); }
            if Self::menu_item(ui, game, "Load state file...", "") { self.load_state_file(); }
            if Self::menu_item(ui, game, "Save state", "Shift+F10") { self.open_state_dialog(&ctx, StateDialog::Save); }
            if Self::menu_item(ui, game, "Save state file...", "") { self.save_state_file(); }
            ui.menu_button("Quick load", |ui| {
                if Self::menu_item(ui, game, "Load recent", "") {
                    let slot = self.session.as_ref().map_or(1, |s| s.last_slot);
                    self.load_slot(slot);
                }
                ui.separator();
                let can_undo = self.session.as_ref().map_or(false, |s| s.backup_load.is_some());
                if Self::menu_item(ui, can_undo, "Undo load state", "F11") { self.undo_load(); }
                ui.separator();
                for i in 1..=9 {
                    if Self::menu_item(ui, game, &format!("State {i}"), &format!("F{i}")) { self.load_slot(i); }
                }
            });
            ui.menu_button("Quick save", |ui| {
                if Self::menu_item(ui, game, "Save recent", "") {
                    let slot = self.session.as_ref().map_or(1, |s| s.last_slot);
                    self.save_slot(slot);
                }
                ui.separator();
                let can_undo = self.session.as_ref().map_or(false, |s| s.backup_save.is_some());
                if Self::menu_item(ui, can_undo, "Undo save state", "Shift+F11") { self.undo_save(); }
                ui.separator();
                for i in 1..=9 {
                    if Self::menu_item(ui, game, &format!("State {i}"), &format!("Shift+F{i}")) { self.save_slot(i); }
                }
            });
            ui.separator();
            if Self::menu_item(ui, true, "New multiplayer window", "") { self.tools.open(ToolId::Multiplayer); }
            if Self::menu_item(ui, true, "Connect to Dolphin...", "") { self.tools.open(ToolId::Dolphin); }
            ui.separator();
            if Self::menu_item(ui, true, "Report bug...", "") { self.tools.open(ToolId::BugReport); }
            ui.separator();
            if Self::menu_item(ui, true, "About...", "") { self.tools.open(ToolId::About); }
            if Self::menu_item(ui, true, "Exit", "Ctrl+Q") { ctx.send_viewport_cmd(egui::ViewportCommand::Close); }
        });

        ui.menu_button("Emulation", |ui| {
            if Self::menu_item(ui, game, "Reset", "Ctrl+R") { self.reset(); }
            if Self::menu_item(ui, game, "Shutdown", "") { self.close_game(); }
            ui.separator();
            if Self::menu_item(ui, game, "Replace ROM...", "") { self.replace_rom(); }
            if Self::menu_item(ui, game, "Yank game pak", "") { self.yank(); }
            ui.separator();
            let mut paused = self.paused;
            if ui.add_enabled(game, egui::Checkbox::new(&mut paused, "Pause")).clicked() {
                self.toggle_pause();
            }
            if Self::menu_item(ui, game, "Next frame", "Ctrl+N") { self.next_frame(); }
            ui.separator();
            ui.add_enabled(false, egui::Button::new("Fast forward (held)").shortcut_text("Tab"));
            let mut ff = self.ff_toggle;
            if ui.add_enabled(game, egui::Checkbox::new(&mut ff, "Fast forward")).on_hover_text("Shift+Tab").clicked() {
                self.ff_toggle = ff;
            }
            ui.menu_button("Fast forward speed", |ui| {
                let mut sp = self.cfg.ff_speed;
                ui.radio_value(&mut sp, -1.0, "Unbounded");
                ui.separator();
                for i in 2..=10 {
                    ui.radio_value(&mut sp, i as f32, format!("{i}×"));
                }
                if sp != self.cfg.ff_speed {
                    self.cfg.ff_speed = sp;
                    self.cfg.save();
                }
            });
            if Self::menu_item(ui, true, "Increase fast forward speed", "") {
                self.cfg.ff_speed = if self.cfg.ff_speed <= 0.0 { -1.0 } else if self.cfg.ff_speed >= 10.0 { -1.0 } else { self.cfg.ff_speed + 1.0 };
            }
            if Self::menu_item(ui, true, "Decrease fast forward speed", "") {
                self.cfg.ff_speed = if self.cfg.ff_speed <= 0.0 { 10.0 } else { (self.cfg.ff_speed - 1.0).max(2.0) };
            }
            ui.separator();
            ui.add_enabled(false, egui::Button::new("Rewind (held)").shortcut_text("`"))
                .on_disabled_hover_text("Hold ` (enable rewind in Settings → Emulation)");
            if Self::menu_item(ui, game, "Rewind", "~") { self.rewind_once(); }
            if Self::menu_item(ui, game, "Step backwards", "Ctrl+B") { self.step_back(); }
            ui.separator();
            ui.menu_button("Solar sensor", |ui| {
                let level = self.session.as_ref().map_or(0, |s| s.solar_level);
                if Self::menu_item(ui, game, "Increase solar level", "") { self.set_solar(level + 1); }
                if Self::menu_item(ui, game, "Decrease solar level", "") { self.set_solar(level - 1); }
                if Self::menu_item(ui, game, "Brightest solar level", "") { self.set_solar(10); }
                if Self::menu_item(ui, game, "Darkest solar level", "") { self.set_solar(0); }
                ui.separator();
                for i in 0..=10 {
                    let mut l = level;
                    if ui.add_enabled(game, egui::RadioButton::new(l == i, format!("Brightness {i}"))).clicked() {
                        l = i;
                        self.set_solar(l);
                    }
                }
            });
            Self::unavailable(ui, "Load camera image...", "Game Boy Camera image input is not ported yet");
            if Self::menu_item(ui, is_gb, "Game Boy Printer...", "") { self.tools.open(ToolId::GbPrinter); }
            if Self::menu_item(ui, is_gba, "BattleChip Gate...", "") { self.tools.open(ToolId::BattleChip); }
        });

        ui.menu_button("Audio/Video", |ui| {
            ui.menu_button("Frame size", |ui| {
                for i in 1..=8 {
                    let mut sel = self.cfg.frame_scale == i;
                    if ui.radio(sel, format!("{i}×")).clicked() {
                        sel = true;
                        let _ = sel;
                        self.set_frame_size(i);
                    }
                }
                ui.separator();
                if Self::menu_item(ui, true, "Toggle fullscreen", "Ctrl+F") { self.toggle_fullscreen(&ctx); }
                ui.checkbox(&mut self.cfg.lock_frame_size, "Lock frame size");
            });
            ui.checkbox(&mut self.cfg.lock_aspect, "Lock aspect ratio");
            ui.checkbox(&mut self.cfg.integer_scaling, "Force integer scaling");
            ui.checkbox(&mut self.cfg.interframe_blending, "Interframe blending");
            ui.checkbox(&mut self.cfg.bilinear, "Bilinear filtering");
            ui.menu_button("Frameskip", |ui| {
                let mut fs = self.cfg.frameskip;
                for i in 0..=10 {
                    ui.radio_value(&mut fs, i, i.to_string());
                }
                if fs != self.cfg.frameskip {
                    self.cfg.frameskip = fs;
                    if let Some(s) = &mut self.session {
                        s.set_frameskip(fs);
                    }
                }
            });
            ui.separator();
            ui.checkbox(&mut self.cfg.mute, "Mute");
            ui.menu_button("FPS target", |ui| {
                let mut t = self.cfg.fps_target;
                for fps in [15.0, 30.0, 45.0] {
                    ui.radio_value(&mut t, fps, format!("{fps}"));
                }
                ui.radio_value(&mut t, native_fps(), "Native (59.7275)");
                for fps in [60.0, 90.0, 120.0, 240.0] {
                    ui.radio_value(&mut t, fps, format!("{fps}"));
                }
                self.cfg.fps_target = t;
            });
            ui.separator();
            if Self::menu_item(ui, game, "Take screenshot", "F12") { self.screenshot(); }
            if Self::menu_item(ui, game, "Record A/V...", "") { self.tools.open(ToolId::RecordAv); }
            if Self::menu_item(ui, game, "Record GIF/WebP/APNG...", "") { self.tools.open(ToolId::RecordGif); }
            ui.separator();
            ui.menu_button("Video layers", |ui| {
                if let Some(s) = &mut self.session {
                    for (i, name) in s.layer_names().iter().enumerate() {
                        let mut on = s.layer_enabled(i).unwrap_or(true);
                        if ui.checkbox(&mut on, *name).changed() {
                            s.set_layer_enabled(i, on);
                        }
                    }
                } else {
                    ui.add_enabled(false, egui::Label::new("No game loaded"));
                }
            });
            ui.menu_button("Audio channels", |ui| {
                if let Some(s) = &mut self.session {
                    for (i, name) in s.channel_names().iter().enumerate() {
                        let mut on = s.channel_enabled(i);
                        if ui.checkbox(&mut on, *name).changed() {
                            s.set_channel_enabled(i, on);
                        }
                    }
                } else {
                    ui.add_enabled(false, egui::Label::new("No game loaded"));
                }
            });
            if Self::menu_item(ui, is_gba, "Adjust layer placement...", "") { self.placement_open = true; }
            ui.separator();
            ui.checkbox(&mut self.cfg.hide_menu, "Hide menu").on_hover_text("Alt+M or right-click the game to bring it back");
        });

        ui.menu_button("Tools", |ui| {
            if Self::menu_item(ui, true, "View logs...", "") { self.tools.open(ToolId::LogView); }
            if Self::menu_item(ui, true, "Game overrides...", "") { self.tools.open(ToolId::Overrides); }
            if Self::menu_item(ui, true, "Game Pak sensors...", "") { self.tools.open(ToolId::Sensors); }
            if Self::menu_item(ui, game, "Cheats...", "") { self.tools.open(ToolId::Cheats); }
            Self::unavailable(ui, "Scripting...", "mGBA's Lua scripting engine is not ported yet");
            Self::unavailable(ui, "Create forwarder...", "Forwarders are 3DS/Vita homebrew wrappers built from mGBA's own binaries; not applicable to rgba");
            ui.separator();
            if Self::menu_item(ui, true, "Settings...", "") { self.tools.open(ToolId::Settings); }
            if Self::menu_item(ui, true, "Make portable", "") {
                let r = self.cfg.make_portable().map_err(|e| e.to_string());
                self.report(r, "Settings now stored next to the executable");
            }
            ui.separator();
            if Self::menu_item(ui, game, "Open debugger console...", "") { self.tools.open(ToolId::DebuggerConsole); }
            if Self::menu_item(ui, is_gba, "Start GDB server...", "") { self.tools.open(ToolId::Gdb); }
            ui.separator();
            ui.menu_button("Game state views", |ui| {
                if Self::menu_item(ui, game, "View palette...", "") { self.tools.open(ToolId::PaletteView); }
                if Self::menu_item(ui, game, "View sprites...", "") { self.tools.open(ToolId::SpriteView); }
                if Self::menu_item(ui, game, "View tiles...", "") { self.tools.open(ToolId::TileView); }
                if Self::menu_item(ui, game, "View map...", "") { self.tools.open(ToolId::MapView); }
                if Self::menu_item(ui, game, "Frame inspector...", "") { self.tools.open(ToolId::FrameInspector); }
                if Self::menu_item(ui, game, "View memory...", "") { self.tools.open(ToolId::MemoryView); }
                if Self::menu_item(ui, game, "Search memory...", "") { self.tools.open(ToolId::MemorySearch); }
                if Self::menu_item(ui, game, "View I/O registers...", "") { self.tools.open(ToolId::IoViewer); }
                if Self::menu_item(ui, game, "Log memory accesses...", "") { self.tools.open(ToolId::AccessLog); }
            });
            ui.separator();
            Self::unavailable(ui, "Convert e-Reader card image to raw...", "Needs dotcode image scanning, which is not ported yet");
            ui.separator();
            let logging = self.session.as_ref().map_or(false, |s| s.video_log);
            if Self::menu_item(ui, game && !logging, "Record debug video log...", "") { self.start_video_log(); }
            if Self::menu_item(ui, logging, "Stop debug video log", "") { self.stop_video_log(); }
        });
    }

    // ---- Menu actions that need dialogs -------------------------------------

    fn load_alternate_save(&mut self, temporary: bool) {
        let Some(rom) = self.session.as_ref().map(|s| s.rom_path.clone()) else { return };
        if let Some(p) = self.pick("Select save game", &[("Save games", &["sav", "sa1", "sa2", "sa3", "sa4"]), ("All files", &["*"])]) {
            let opts = LoadOptions { save_path: Some(p), temporary_save: temporary, force_gbp: self.force_gbp, ..Default::default() };
            self.load_rom_with(&rom, opts);
        }
    }

    fn import_sharkport(&mut self) {
        let Some(p) = self.pick("Select GameShark save", &[("GameShark saves", &["sps", "xps", "gsv"])]) else { return };
        let Some(s) = &mut self.session else { return };
        let Ok(bytes) = std::fs::read(&p) else {
            self.error = Some(format!("Could not read {}", p.display()));
            return;
        };
        let gsv = p.extension().map_or(false, |e| e.eq_ignore_ascii_case("gsv"));
        let ok = match s.console.gba() {
            Some(g) if gsv => g.savedata_import_gsv(&bytes, false),
            Some(g) => g.savedata_import_sharkport(&bytes, false),
            None => false,
        };
        if ok {
            s.flush_savedata(true);
            self.notice("GameShark save imported");
        } else {
            self.error = Some("Not a valid GameShark save for this game".into());
        }
    }

    fn export_sharkport(&mut self) {
        let name = self.session.as_ref().map(|s| format!("{}.sps", s.title())).unwrap_or_default();
        let Some(p) = self.pick_save("Export GameShark save", &name, &[("SharkPort", &["sps"])]) else { return };
        let Some(g) = self.session.as_mut().and_then(|s| s.console.gba()) else { return };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        match g.savedata_export_sharkport(now) {
            Some(bytes) => {
                let r = std::fs::write(&p, bytes).map_err(|e| e.to_string());
                self.report(r, format!("Exported {}", p.display()));
            }
            None => self.error = Some("This game has no save data to export".into()),
        }
    }

    fn scan_ereader(&mut self) {
        let Some(p) = self.pick("Select e-Reader dotcode", &[("e-Reader dotcodes", &["raw", "bin", "bmp"])]) else { return };
        let Some(g) = self.session.as_mut().and_then(|s| s.console.gba()) else { return };
        match std::fs::read(&p) {
            Ok(bytes) => {
                g.ereader_queue_card(&bytes);
                self.notice("e-Reader card queued");
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn load_state_file(&mut self) {
        if let Some(p) = self.pick("Select save state", &[("Save states", &["ss0", "ss1", "ss2", "ss3", "ss4", "ss5", "ss6", "ss7", "ss8", "ss9"]), ("All files", &["*"])]) {
            if let Some(s) = &mut self.session {
                let r = s.load_state_file(&p);
                self.report(r, "State loaded");
            }
        }
    }

    fn save_state_file(&mut self) {
        let name = self.session.as_ref().map(|s| format!("{}.ss0", s.title())).unwrap_or_default();
        if let Some(p) = self.pick_save("Save state", &name, &[("Save states", &["ss0"])]) {
            if let Some(s) = &mut self.session {
                let r = s.save_state_file(&p);
                self.report(r, "State saved");
            }
        }
    }

    fn replace_rom(&mut self) {
        let Some(p) = self.pick("Select replacement ROM", ROM_FILTERS) else { return };
        let Some(s) = &mut self.session else { return };
        let r = s.replace_rom(&p);
        self.report(r, "ROM replaced");
    }

    fn yank(&mut self) {
        if let Some(s) = &mut self.session {
            s.yank();
            self.notice("Game pak yanked");
        }
    }

    fn set_solar(&mut self, level: i32) {
        if let Some(s) = &mut self.session {
            s.set_solar_level(level);
            let l = s.solar_level;
            self.notice(format!("Solar level {l}"));
        }
    }

    fn start_video_log(&mut self) {
        let name = self.session.as_ref().map(|s| format!("{}.mvl", s.title())).unwrap_or_default();
        let Some(p) = self.pick_save("Record debug video log", &name, &[("Video logs", &["mvl"])]) else { return };
        let Some(s) = &mut self.session else { return };
        let r = match &mut s.console {
            crate::emu::Console::Gba(g) => g.start_video_log(&p),
            crate::emu::Console::Gb(g) => g.start_video_log(&p),
        };
        if r.is_ok() {
            s.video_log = true;
        }
        self.report(r.map_err(|e| e.to_string()), "Recording video log");
    }

    fn stop_video_log(&mut self) {
        if let Some(s) = &mut self.session {
            match &mut s.console {
                crate::emu::Console::Gba(g) => g.end_video_log(),
                crate::emu::Console::Gb(g) => g.end_video_log(),
            }
            s.video_log = false;
            self.notice("Video log saved");
        }
    }

    // ---- Display ------------------------------------------------------------

    fn upload_frame(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.session else { return };
        let (w, h) = s.core().base_video_size();
        let (w, h) = (w as usize, h as usize);
        let src = &s.core().video_buffer()[..w * h];
        let pixels: &[u32] = if self.cfg.interframe_blending {
            // Average with the previous frame (mGBA's interframe blending
            // shader), which also hides GBA flicker transparency.
            if self.blend_prev.len() != src.len() {
                self.blend_prev = src.to_vec();
            }
            self.blend_out.resize(src.len(), 0);
            for i in 0..src.len() {
                let (a, b) = (src[i], self.blend_prev[i]);
                self.blend_out[i] = 0xFF00_0000 | (((a & 0xFEFEFE) >> 1) + ((b & 0xFEFEFE) >> 1));
            }
            self.blend_prev.copy_from_slice(src);
            &self.blend_out
        } else {
            src
        };
        let mut rgba = Vec::with_capacity(w * h * 4);
        for &p in pixels {
            rgba.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, 255]);
        }
        let image = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
        let opts = if self.cfg.bilinear { egui::TextureOptions::LINEAR } else { egui::TextureOptions::NEAREST };
        match &mut self.texture {
            Some(t) => t.set(image, opts),
            None => self.texture = Some(ctx.load_texture("screen", image, opts)),
        }
    }

    fn display_rect(&self, avail: egui::Rect, w: f32, h: f32) -> egui::Rect {
        let mut size = avail.size();
        if self.cfg.lock_aspect || self.cfg.integer_scaling {
            let scale = (size.x / w).min(size.y / h);
            let scale = if self.cfg.integer_scaling { scale.floor().max(1.0) } else { scale };
            size = egui::vec2(w * scale, h * scale);
        }
        egui::Rect::from_center_size(avail.center(), size)
    }

    fn game_area(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(avail, egui::Sense::click());
        ui.painter().rect_filled(avail, 0.0, egui::Color32::BLACK);
        if let (Some(t), Some(s)) = (&self.texture, &self.session) {
            let (w, h) = s.console.core_ref().base_video_size();
            let rect = self.display_rect(avail, w as f32, h as f32);
            ui.painter().image(t.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        } else {
            ui.painter().text(
                avail.center(),
                egui::Align2::CENTER_CENTER,
                "File → Load ROM… (Ctrl+O), or drop a ROM here",
                egui::FontId::proportional(16.0),
                egui::Color32::GRAY,
            );
        }
        // Right-click: the full menu as a context menu (needed when hidden).
        resp.context_menu(|ui| {
            if self.cfg.hide_menu && ui.button("Show menu bar").clicked() {
                self.cfg.hide_menu = false;
            }
            self.menus(ui);
        });
    }

    fn overlays(&mut self, ctx: &egui::Context) {
        // Notices (bottom-left), errors (modal).
        let now = Instant::now();
        for n in self.pending_notices.drain(..) {
            self.notices.push((n, now));
        }
        self.notices.retain(|(_, t)| now - *t < Duration::from_secs(3));
        if !self.notices.is_empty() {
            egui::Area::new(egui::Id::new("notices"))
                .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(8.0, -8.0))
                .interactable(false)
                .show(ctx, |ui| {
                    for (n, _) in &self.notices {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.label(n);
                        });
                    }
                });
        }
        if self.cfg.show_fps || self.fast_forward() || self.paused {
            let mut text = String::new();
            if self.paused { text.push_str("Paused  "); }
            if self.fast_forward() { text.push_str("Fast forward  "); }
            if self.cfg.show_fps { text.push_str(&format!("{:.1} fps", self.fps)); }
            if self.session.is_some() && !text.is_empty() {
                egui::Area::new(egui::Id::new("status"))
                    .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-8.0, -8.0))
                    .interactable(false)
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| { ui.label(text.trim_end()); });
                    });
            }
        }
        if let Some(err) = self.error.clone() {
            egui::Window::new("Error")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label(err);
                    if ui.button("OK").clicked() {
                        self.error = None;
                    }
                });
        }
        if let Some((archive, names)) = self.archive_pick.clone() {
            let mut close = false;
            egui::Window::new("Select ROM in archive").collapsible(false).show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    for n in &names {
                        if ui.button(n).clicked() {
                            self.load_from_archive(&archive, n, LoadOptions { force_gbp: self.force_gbp, ..Default::default() });
                            close = true;
                        }
                    }
                });
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
            if close {
                self.archive_pick = None;
            }
        }
        if let Some(which) = self.state_dialog {
            self.state_dialog_ui(ctx, which);
        }
        if self.placement_open {
            self.placement_ui(ctx);
        }
    }

    /// The F10 / Shift+F10 state picker (Qt LoadSaveState): 9 slots with
    /// thumbnails.
    fn state_dialog_ui(&mut self, ctx: &egui::Context, which: StateDialog) {
        let mut chosen = None;
        let mut close = false;
        let title = if which == StateDialog::Load { "Load state" } else { "Save state" };
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::Grid::new("slots").spacing([8.0, 8.0]).show(ui, |ui| {
                    for slot in 1..=9u32 {
                        let exists = self.session.as_ref().map_or(false, |s| s.state_path(slot).exists());
                        ui.vertical(|ui| {
                            let size = egui::vec2(120.0, 80.0);
                            let thumb = self.state_thumbs.get(slot as usize - 1).cloned().flatten();
                            let r = match thumb {
                                Some(t) => ui.add(egui::Button::image(egui::Image::new(&t).fit_to_exact_size(size))),
                                None => ui.add_sized(size, egui::Button::new(if exists { "(no preview)" } else { "Empty" })),
                            };
                            ui.label(format!("State {slot}"));
                            let enabled = which == StateDialog::Save || exists;
                            if r.clicked() && enabled {
                                chosen = Some(slot);
                            }
                        });
                        if slot % 3 == 0 {
                            ui.end_row();
                        }
                    }
                });
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    close = true;
                }
            });
        if let Some(slot) = chosen {
            if which == StateDialog::Load { self.load_slot(slot); } else { self.save_slot(slot); }
            close = true;
        }
        if close {
            self.state_dialog = None;
            self.paused = false;
        }
    }

    /// "Adjust layer placement" (Qt PlacementControl): the renderer's OBJ /
    /// BG offsets.
    fn placement_ui(&mut self, ctx: &egui::Context) {
        let mut open = self.placement_open;
        egui::Window::new("Adjust layer placement").open(&mut open).show(ctx, |ui| {
            let Some(g) = self.session.as_mut().and_then(|s| s.console.gba()) else {
                ui.label("GBA only.");
                return;
            };
            let sw = &mut g.video.sw;
            let mut changed = false;
            egui::Grid::new("placement").show(ui, |ui| {
                ui.label("Objects");
                changed |= ui.add(egui::DragValue::new(&mut sw.obj_offset_x).prefix("x ")).changed();
                changed |= ui.add(egui::DragValue::new(&mut sw.obj_offset_y).prefix("y ")).changed();
                ui.end_row();
                for i in 0..4 {
                    ui.label(format!("Background {i}"));
                    changed |= ui.add(egui::DragValue::new(&mut sw.bg[i].offset_x).prefix("x ")).changed();
                    changed |= ui.add(egui::DragValue::new(&mut sw.bg[i].offset_y).prefix("y ")).changed();
                    ui.end_row();
                }
            });
            if changed {
                sw.oam_dirty = true;
                sw.scanline_dirty = [0xFFFF_FFFF; 5];
            }
        });
        self.placement_open = open;
    }
}

pub const ROM_FILTERS: &[(&str, &[&str])] = &[
    ("Game Boy Advance / Game Boy ROMs", &["gba", "agb", "bin", "gb", "gbc", "sgb", "cgb", "zip"]),
    ("All files", &["*"]),
];

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Drag and drop a ROM onto the window.
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if let Some(p) = dropped.first() {
            self.load_rom_with(p, LoadOptions { force_gbp: self.force_gbp, ..Default::default() });
        }

        if let Some(p) = crate::tools::library::take_requested_load() {
            self.load_rom_with(&p, LoadOptions { force_gbp: self.force_gbp, ..Default::default() });
        }

        self.handle_hotkeys(&ctx);
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if self.cfg.pause_on_focus_loss && !focused {
            self.last_tick = Instant::now();
        } else {
            self.tick(&ctx);
        }
        self.upload_frame(&ctx);

        // FPS counter
        let (t0, n0) = self.fps_window;
        if t0.elapsed() >= Duration::from_secs(1) {
            self.fps = (self.frames_run - n0) as f64 / t0.elapsed().as_secs_f64();
            self.fps_window = (Instant::now(), self.frames_run);
        }

        if !self.cfg.hide_menu && !self.fullscreen {
            let r = egui::Panel::top("menu").show(ui, |ui| {
                egui::MenuBar::new().ui(ui, |ui| self.menus(ui));
            });
            self.menu_height = r.response.rect.height();
        } else {
            self.menu_height = 0.0;
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
            .show(ui, |ui| self.game_area(ui));

        // Tool windows.
        let mut paused = self.paused;
        let mut notices = Vec::new();
        {
            let mut tc = ToolCtx {
                session: self.session.as_mut(),
                config: &mut self.cfg,
                paused: &mut paused,
                log: &self.log,
                notices: &mut notices,
            };
            self.tools.show_all(&ctx, &mut tc);
        }
        self.paused = paused;
        self.pending_notices.extend(notices);
        self.overlays(&ctx);

        if let Some(scale) = self.pending_resize.take() {
            if !self.fullscreen {
                let (w, h) = self.session.as_ref().map_or((240, 160), |s| s.console.core_ref().base_video_size());
                let size = egui::vec2((w * scale) as f32, (h * scale) as f32 + self.menu_height.max(if self.cfg.hide_menu { 0.0 } else { 24.0 }));
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            }
        }
        let title = match &self.session {
            Some(s) if self.cfg.show_fps => format!("rgba - {} ({:.1} fps)", s.title(), self.fps),
            Some(s) => format!("rgba - {}", s.title()),
            None => "rgba".to_string(),
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        if self.session.is_some() && !self.paused {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn on_exit(&mut self) {
        self.close_game();
        self.cfg.save();
    }
}
