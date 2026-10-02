// Captured core log (the Qt LogController): a bounded ring of messages fed
// by rgba_core's log sink, read by the "View logs" window.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use rgba_core::log::Level;

pub const MAX_LINES: usize = 10_000;

#[derive(Clone)]
pub struct LogLine {
    pub level: Level,
    pub category: String,
    pub message: String,
}

#[derive(Clone, Default)]
pub struct LogBuffer {
    lines: Arc<Mutex<VecDeque<LogLine>>>,
}

impl LogBuffer {
    /// Route rgba_core's `mlog!` output into this buffer (and still echo to
    /// stderr, like mGBA's default logger alongside the log view).
    pub fn install(&self) {
        let lines = self.lines.clone();
        rgba_core::log::set_sink(Some(Box::new(move |level, category, message| {
            eprintln!("[{category}] {message}");
            if let Ok(mut l) = lines.lock() {
                if l.len() >= MAX_LINES {
                    l.pop_front();
                }
                l.push_back(LogLine {
                    level,
                    category: category.to_string(),
                    message: message.to_string(),
                });
            }
        })));
    }

    pub fn snapshot(&self) -> Vec<LogLine> {
        self.lines.lock().map(|l| l.iter().cloned().collect()).unwrap_or_default()
    }

    pub fn clear(&self) {
        if let Ok(mut l) = self.lines.lock() {
            l.clear();
        }
    }
}
