// Tool windows (mGBA Qt's Tools menu views: log view, cheats, overrides,
// debugger console, game state views, ...). Each tool is an egui window that
// gets the running session for the duration of one UI frame.

use eframe::egui;

use crate::config::Config;
use crate::emu::Session;
use crate::logbuf::LogBuffer;

pub mod about;
pub mod access_log;
pub mod cheats;
pub mod debugger_console;
pub mod frame_inspector;
pub mod gdb;
pub mod io_viewer;
pub mod library;
pub mod log_view;
pub mod map_view;
pub mod memory_search;
pub mod memory_view;
pub mod multiplayer;
pub mod overrides;
pub mod palette_view;
pub mod peripherals;
pub mod record;
pub mod rom_info;
pub mod save_converter;
pub mod sensors;
pub mod settings;
pub mod sprite_view;
pub mod tile_view;

/// What a tool may touch during `show`.
pub struct ToolCtx<'a> {
    /// The running game, if any.
    pub session: Option<&'a mut Session>,
    pub config: &'a mut Config,
    /// Emulation pause flag (tools like the debugger console pause/resume).
    pub paused: &'a mut bool,
    pub log: &'a LogBuffer,
    /// Short status messages to show in the main window.
    pub notices: &'a mut Vec<String>,
}

impl ToolCtx<'_> {
    pub fn notice(&mut self, msg: impl Into<String>) {
        self.notices.push(msg.into());
    }
}

pub trait ToolWindow {
    /// Window title (also its egui id).
    fn title(&self) -> &'static str;
    /// Draw the window. `open` is the window's close-button state.
    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx);
    /// Called once per emulated frame while the window is open (for tools
    /// that pump work every frame, e.g. the GDB server).
    fn on_frame(&mut self, _session: &mut Session) {}
    /// Drive the session's frame yourself (e.g. linked multiplayer cores
    /// stepping in lockstep). Return true if the frame was run; the app then
    /// skips its own run_frame. Called even while the window is closed.
    fn run_frame(&mut self, _session: &mut Session) -> bool {
        false
    }
    /// The game was closed or replaced; drop per-game state.
    fn on_session_changed(&mut self) {}
}

/// Identifiers for every tool window, in menu order.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ToolId {
    About,
    RomInfo,
    SaveConverter,
    Dolphin,
    BugReport,
    BattleChip,
    GbPrinter,
    LogView,
    Overrides,
    Sensors,
    Cheats,
    Settings,
    DebuggerConsole,
    Gdb,
    PaletteView,
    SpriteView,
    TileView,
    MapView,
    FrameInspector,
    MemoryView,
    MemorySearch,
    IoViewer,
    AccessLog,
    Library,
    Multiplayer,
    RecordAv,
    RecordGif,
}

pub struct Tools {
    pub windows: Vec<(ToolId, Box<dyn ToolWindow>, bool)>,
}

impl Tools {
    pub fn new() -> Tools {
        let w: Vec<(ToolId, Box<dyn ToolWindow>)> = vec![
            (ToolId::About, Box::new(about::About::default())),
            (ToolId::RomInfo, Box::new(rom_info::RomInfo::default())),
            (ToolId::SaveConverter, Box::new(save_converter::SaveConverter::default())),
            (ToolId::Dolphin, Box::new(peripherals::DolphinConnector::default())),
            (ToolId::BugReport, Box::new(about::BugReport::default())),
            (ToolId::BattleChip, Box::new(peripherals::BattleChipView::default())),
            (ToolId::GbPrinter, Box::new(peripherals::GbPrinterView::default())),
            (ToolId::LogView, Box::new(log_view::LogView::default())),
            (ToolId::Overrides, Box::new(overrides::OverridesView::default())),
            (ToolId::Sensors, Box::new(sensors::SensorView::default())),
            (ToolId::Cheats, Box::new(cheats::CheatsView::default())),
            (ToolId::Settings, Box::new(settings::SettingsView::default())),
            (ToolId::DebuggerConsole, Box::new(debugger_console::DebuggerConsole::default())),
            (ToolId::Gdb, Box::new(gdb::GdbView::default())),
            (ToolId::PaletteView, Box::new(palette_view::PaletteView::default())),
            (ToolId::SpriteView, Box::new(sprite_view::SpriteView::default())),
            (ToolId::TileView, Box::new(tile_view::TileView::default())),
            (ToolId::MapView, Box::new(map_view::MapView::default())),
            (ToolId::FrameInspector, Box::new(frame_inspector::FrameInspector::default())),
            (ToolId::MemoryView, Box::new(memory_view::MemoryView::default())),
            (ToolId::MemorySearch, Box::new(memory_search::MemorySearch::default())),
            (ToolId::IoViewer, Box::new(io_viewer::IoViewer::default())),
            (ToolId::AccessLog, Box::new(access_log::AccessLogView::default())),
            (ToolId::Library, Box::new(library::LibraryView::default())),
            (ToolId::Multiplayer, Box::new(multiplayer::MultiplayerView::default())),
            (ToolId::RecordAv, Box::new(record::RecordView::av())),
            (ToolId::RecordGif, Box::new(record::RecordView::gif())),
        ];
        Tools { windows: w.into_iter().map(|(id, t)| (id, t, false)).collect() }
    }

    pub fn open(&mut self, id: ToolId) {
        if let Some(w) = self.windows.iter_mut().find(|w| w.0 == id) {
            w.2 = true;
        }
    }

    pub fn show_all(&mut self, ctx: &egui::Context, tc: &mut ToolCtx) {
        for (_, tool, open) in self.windows.iter_mut() {
            if *open {
                tool.show(ctx, open, tc);
            }
        }
    }

    /// Let a tool drive this frame (see ToolWindow::run_frame).
    pub fn run_frame(&mut self, session: &mut Session) -> bool {
        self.windows.iter_mut().any(|(_, tool, _)| tool.run_frame(session))
    }

    pub fn on_frame(&mut self, session: &mut Session) {
        for (_, tool, open) in self.windows.iter_mut() {
            if *open {
                tool.on_frame(session);
            }
        }
    }

    pub fn session_changed(&mut self) {
        for (_, tool, _) in self.windows.iter_mut() {
            tool.on_session_changed();
        }
    }
}

// ---- Shared helpers for tool implementations --------------------------------

/// Upload 0xFFRRGGBB pixels as an egui texture (nearest filtering), reusing
/// `slot` when present.
pub fn upload_rgba(
    ctx: &egui::Context,
    slot: &mut Option<egui::TextureHandle>,
    name: &str,
    pixels: &[u32],
    width: usize,
    height: usize,
) -> egui::TextureHandle {
    let mut rgba = Vec::with_capacity(width * height * 4);
    for &p in &pixels[..width * height] {
        rgba.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8]);
    }
    let image = egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba);
    match slot {
        Some(t) => {
            t.set(image, egui::TextureOptions::NEAREST);
            t.clone()
        }
        None => {
            let t = ctx.load_texture(name, image, egui::TextureOptions::NEAREST);
            *slot = Some(t.clone());
            t
        }
    }
}

/// BGR555 → 0xFFRRGGBB with mGBA's 5→8-bit expansion (M_RGB5_TO_RGB8).
pub fn bgr555_to_argb(c: u16) -> u32 {
    let r = (c & 0x1F) as u32;
    let g = ((c >> 5) & 0x1F) as u32;
    let b = ((c >> 10) & 0x1F) as u32;
    let x = |v: u32| (v << 3) | (v >> 2);
    0xFF00_0000 | (x(r) << 16) | (x(g) << 8) | x(b)
}

/// "No game loaded" placeholder used by tools that need a session.
pub fn no_game(ui: &mut egui::Ui) {
    ui.label("No game is running.");
}
