// Copyright (c) 2013-2015 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/core/log.c (subset): leveled, categorized logging.

use std::sync::atomic::{AtomicI32, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Fatal = 0,
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Stub = 5,
    GameError = 6,
}

static MAX_LEVEL: AtomicI32 = AtomicI32::new(Level::Warn as i32);

pub fn set_max_level(level: Level) {
    MAX_LEVEL.store(level as i32, Ordering::Relaxed);
}

pub fn max_level() -> Level {
    match MAX_LEVEL.load(Ordering::Relaxed) {
        0 => Level::Fatal,
        1 => Level::Error,
        2 => Level::Warn,
        3 => Level::Info,
        4 => Level::Debug,
        5 => Level::Stub,
        _ => Level::GameError,
    }
}

/// A frontend log sink (mLogger.log): receives every message that passes the
/// level filter. When set, it replaces the default stderr output.
pub type LogSink = Box<dyn Fn(Level, &str, &str) + Send + Sync>;

static SINK: std::sync::RwLock<Option<LogSink>> = std::sync::RwLock::new(None);

pub fn set_sink(sink: Option<LogSink>) {
    if let Ok(mut s) = SINK.write() {
        *s = sink;
    }
}

/// Deliver one message (used by `mlog!`).
pub fn emit(level: Level, category: &str, message: std::fmt::Arguments) {
    if let Ok(s) = SINK.read() {
        if let Some(sink) = s.as_ref() {
            sink(level, category, &message.to_string());
            return;
        }
    }
    eprintln!("[{}] {}", category, message);
}

#[macro_export]
macro_rules! mlog {
    ($level:expr, $cat:expr, $($arg:tt)*) => {
        if $level <= $crate::log::max_level() {
            $crate::log::emit($level, $cat, format_args!($($arg)*));
        }
    };
}

// Category constants mirroring mLOG_* names (subset actually used by cores).
pub const GB: &str = "GB";
pub const GB_MEM: &str = "GB Memory";
pub const GB_VIDEO: &str = "GB Video";
pub const GB_MBC: &str = "GB MBC";
pub const GB_SIO: &str = "GB SIO";
pub const GB_AUDIO: &str = "GB Audio";
pub const GB_CHEAT: &str = "GB Cheat";
pub const GBA: &str = "GBA";
pub const GBA_MEM: &str = "GBA Memory";
pub const GBA_VIDEO: &str = "GBA Video";
pub const GBA_AUDIO: &str = "GBA Audio";
pub const GBA_SIO: &str = "GBA SIO";
pub const GBA_DMA: &str = "GBA DMA";
pub const GBA_HW: &str = "GBA Hardware";
pub const GBA_BIOS: &str = "GBA BIOS";
pub const GBA_DEBUG: &str = "GBA Debug";
pub const GBA_SAVE: &str = "GBA Savedata";
pub const GBA_CHEAT: &str = "GBA Cheat";
pub const GBA_ROM: &str = "GBA ROM";
pub const GBA_BATTLECHIP: &str = "GBA BattleChip Gate";
pub const ARM: &str = "ARM";
pub const SM83: &str = "SM83";
pub const CORE: &str = "Core";
pub const DEBUGGER: &str = "Debugger";
