// `--debug`: mGBA's CLI debugger on stdin/stdout (cli-el-backend without
// line editing). While attached, frames run under `debugger_run_frame`, and
// the console blocks the UI whenever the debugger is in its prompt.

use rgba_debugger::cli::{CliBackend, CliDebugger};

use crate::emu::{Console, Session};

struct StdioBackend;

impl CliBackend for StdioBackend {
    fn printf(&mut self, text: &str) {
        use std::io::Write;
        print!("{text}");
        let _ = std::io::stdout().flush();
    }
    fn poll(&mut self, _timeout_ms: i32) -> i32 {
        1
    }
    fn readline(&mut self) -> Option<String> {
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    }
}

pub fn attach(s: &mut Session) {
    match &mut s.console {
        Console::Gba(g) => {
            g.debugger_attach();
            g.debugger_attach_module(Box::new(CliDebugger::new(StdioBackend)));
        }
        Console::Gb(g) => {
            g.debugger_attach();
            g.debugger_attach_module(Box::new(CliDebugger::new(StdioBackend)));
        }
    }
    println!("Debugger CLI active on this terminal.");
}
