// Frontend settings, persisted as JSON. Plays the role of mGBA's
// `mCoreConfig` + the Qt `ConfigController` (config.ini / qt.ini): window,
// video, audio, emulation and input options plus the recent-files list.
//
// Location: `<config dir>/rgba/config.json`, or next to the executable when
// a `portable.json` marker exists there (mGBA's "Make portable", which keys
// off `portable.ini`).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MAX_RECENT: usize = 10;

/// GBA buttons in core key-bit order (rgba_core::core::Key).
pub const KEY_NAMES: [&str; 10] = [
    "A", "B", "Select", "Start", "Right", "Left", "Up", "Down", "R", "L",
];

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub recent: Vec<PathBuf>,

    // File
    pub gba_bios: Option<PathBuf>,
    pub gb_bios: Option<PathBuf>,
    pub gbc_bios: Option<PathBuf>,
    pub use_bios: bool,
    pub skip_bios: bool,
    pub gb_model: String,
    /// "Use player N save game": 0 = automatic.
    pub save_player: u32,

    // Emulation
    /// Fast-forward speed multiplier; <= 0 means unbounded.
    pub ff_speed: f32,
    pub rewind_enable: bool,
    pub rewind_capacity: usize,
    pub rewind_interval: u32,
    pub idle_optimization: bool,

    // Audio/Video
    pub frame_scale: u32,
    pub lock_frame_size: bool,
    pub lock_aspect: bool,
    pub integer_scaling: bool,
    pub interframe_blending: bool,
    pub bilinear: bool,
    pub frameskip: u32,
    pub mute: bool,
    pub volume: f32,
    pub ff_mute: bool,
    pub fps_target: f64,
    pub sample_rate: i32,
    pub hide_menu: bool,
    pub screenshot_dir: Option<PathBuf>,

    // Input: keyboard key name (egui::Key::name) per GBA button.
    pub keys: Vec<String>,
    /// Gamepad button name (gilrs::Button debug name) per GBA button.
    pub pad_buttons: Vec<String>,
    pub pad_deadzone: f32,
    /// Autofire period in frames (on N frames, off N frames).
    pub autofire_period: u32,

    // Tools
    pub gdb_port: u16,
    pub log_level_mask: u32,
    pub show_fps: bool,
    pub pause_on_focus_loss: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            recent: Vec::new(),
            gba_bios: None,
            gb_bios: None,
            gbc_bios: None,
            use_bios: true,
            skip_bios: false,
            gb_model: "auto".into(),
            save_player: 0,
            ff_speed: 2.0,
            rewind_enable: false,
            rewind_capacity: 600,
            rewind_interval: 1,
            idle_optimization: true,
            frame_scale: 3,
            lock_frame_size: false,
            lock_aspect: true,
            integer_scaling: false,
            interframe_blending: false,
            bilinear: false,
            frameskip: 0,
            mute: false,
            volume: 1.0,
            ff_mute: false,
            fps_target: native_fps(),
            sample_rate: 48000,
            hide_menu: false,
            screenshot_dir: None,
            keys: ["Z", "X", "Backspace", "Enter", "ArrowRight", "ArrowLeft", "ArrowUp", "ArrowDown", "S", "A"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            pad_buttons: ["South", "East", "Select", "Start", "DPadRight", "DPadLeft", "DPadUp", "DPadDown", "RightTrigger", "LeftTrigger"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            pad_deadzone: 0.5,
            autofire_period: 2,
            gdb_port: 2345,
            log_level_mask: 0,
            show_fps: false,
            pause_on_focus_loss: false,
        }
    }
}

/// GBA_ARM7TDMI_FREQUENCY / VIDEO_TOTAL_LENGTH ("Native (59.7275)").
pub fn native_fps() -> f64 {
    16_777_216.0 / 280_896.0
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

/// Directory holding config.json (and anything else per-user).
pub fn config_dir() -> PathBuf {
    if let Some(dir) = exe_dir() {
        if dir.join("portable.json").exists() {
            return dir;
        }
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rgba")
}

impl Config {
    pub fn path() -> PathBuf {
        config_dir().join("config.json")
    }

    pub fn load() -> Config {
        let mut cfg: Config = std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let d = Config::default();
        if cfg.keys.len() != KEY_NAMES.len() {
            cfg.keys = d.keys.clone();
        }
        if cfg.pad_buttons.len() != KEY_NAMES.len() {
            cfg.pad_buttons = d.pad_buttons;
        }
        cfg.frame_scale = cfg.frame_scale.clamp(1, 8);
        cfg
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, s);
        }
    }

    pub fn push_recent(&mut self, path: &Path) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.recent.retain(|p| p != &path);
        self.recent.insert(0, path);
        self.recent.truncate(MAX_RECENT);
    }

    /// "Make portable": drop a marker next to the executable and copy the
    /// current settings there.
    pub fn make_portable(&self) -> std::io::Result<PathBuf> {
        let dir = exe_dir().ok_or_else(|| std::io::Error::other("no executable directory"))?;
        std::fs::write(dir.join("portable.json"), b"{}\n")?;
        std::fs::write(
            dir.join("config.json"),
            serde_json::to_string_pretty(self).unwrap_or_default(),
        )?;
        Ok(dir)
    }
}
