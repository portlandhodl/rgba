// "Open debugger console..." — mirrors mGBA Qt's DebuggerConsole +
// DebuggerConsoleController: mGBA's CLI debugger with a GUI backend instead
// of a TTY.
//
// The Qt controller runs the core on its own thread and blocks that thread
// in readline until a line is entered. Here the core runs on the UI thread,
// so the backend never blocks: `poll` reports "no input" when the queue is
// empty. While the CLI is paused this window pauses the app's frame loop and
// pumps one debugger iteration per UI frame (`run_timeout(0)`), which runs
// any queued commands and returns. Typing while the game runs breaks in
// first (DebuggerConsoleController::enterLine).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use eframe::egui;
use rgba_debugger::cli::{CliBackend, CliDebugger};
use rgba_debugger::debugger::DebuggerState;

use super::{no_game, ToolCtx, ToolWindow};
use crate::emu::{Console, Session};

const MAX_OUTPUT: usize = 256 * 1024;

#[derive(Default)]
struct Shared {
    output: String,
    input: VecDeque<String>,
    history: Vec<String>,
    completion: String,
}

/// CLIDebuggerBackend for the window (the Qt controller's backend).
struct GuiBackend(Rc<RefCell<Shared>>);

impl CliBackend for GuiBackend {
    fn printf(&mut self, text: &str) {
        let mut s = self.0.borrow_mut();
        s.output.push_str(text);
        if s.output.len() > MAX_OUTPUT {
            let cut = s.output.len() - MAX_OUTPUT / 2;
            let cut = (cut..s.output.len()).find(|&i| s.output.is_char_boundary(i)).unwrap_or(0);
            s.output.drain(..cut);
        }
    }
    fn poll(&mut self, _timeout_ms: i32) -> i32 {
        // Never block the UI thread: 0 = timed out, try again next frame.
        (!self.0.borrow().input.is_empty()) as i32
    }
    fn readline(&mut self) -> Option<String> {
        self.0.borrow_mut().input.pop_front()
    }
    fn line_append(&mut self, line: &str) {
        self.0.borrow_mut().completion.push_str(line);
    }
    fn history_last(&mut self) -> &str {
        // The trait hands out a borrow of the backend; history lives in the
        // shared cell, so `\n` (repeat last command) is resolved by the
        // window before queueing instead.
        ""
    }
    fn history_append(&mut self, line: &str) {
        self.0.borrow_mut().history.push(line.to_string());
    }
}

pub struct DebuggerConsole {
    shared: Rc<RefCell<Shared>>,
    /// Slot of our CLI module in the debugger, once attached.
    module: Option<usize>,
    line: String,
    history_pos: Option<usize>,
    /// We paused the app's frame loop (and must resume it).
    we_paused: bool,
}

impl Default for DebuggerConsole {
    fn default() -> Self {
        DebuggerConsole {
            shared: Rc::default(),
            module: None,
            line: String::new(),
            history_pos: None,
            we_paused: false,
        }
    }
}

fn debugger_state(s: &Session) -> Option<DebuggerState> {
    match &s.console {
        Console::Gba(g) => g.debugger.as_ref().map(|d| d.core.state),
        Console::Gb(g) => g.debugger.as_ref().map(|d| d.core.state),
    }
}

/// One debugger poll iteration (mDebuggerRunTimeout) with a zero timeout.
fn pump(s: &mut Session) {
    match &mut s.console {
        Console::Gba(g) => g.debugger_run_timeout(0),
        Console::Gb(g) => {
            if let Some(mut dbg) = g.debugger.take() {
                dbg.core.run_timeout(&mut **g, 0);
                g.debugger = Some(dbg);
            }
        }
    }
}

fn with_cli<R>(s: &mut Session, idx: usize, f: impl FnOnce(&mut CliDebugger) -> R) -> Option<R> {
    let modules = match &mut s.console {
        Console::Gba(g) => &mut g.debugger.as_mut()?.core.modules,
        Console::Gb(g) => &mut g.debugger.as_mut()?.core.modules,
    };
    let m = modules.get_mut(idx)?.as_mut()?;
    m.as_any_mut().downcast_mut::<CliDebugger>().map(f)
}

impl DebuggerConsole {
    fn attach(&mut self, s: &mut Session) {
        let backend = GuiBackend(self.shared.clone());
        let idx = match &mut s.console {
            Console::Gba(g) => {
                g.debugger_attach();
                g.debugger_attach_module(Box::new(CliDebugger::new(backend)))
            }
            Console::Gb(g) => {
                g.debugger_attach();
                g.debugger_attach_module(Box::new(CliDebugger::new(backend)))
            }
        };
        if idx != usize::MAX {
            self.module = Some(idx);
            self.shared.borrow_mut().output.push_str("Debugger attached. Type \"help\" for commands, or press Break.\n");
        }
    }

    fn cli_paused(&self, s: &mut Session) -> bool {
        let Some(idx) = self.module else { return false };
        use rgba_debugger::debugger::DebuggerModule;
        with_cli(s, idx, |c| c.is_paused()).unwrap_or(false)
    }

    fn submit(&mut self, s: &mut Session) {
        let mut line = std::mem::take(&mut self.line);
        if line.trim().is_empty() {
            // Bare Enter repeats the last command (the libedit backend's "\n").
            match self.shared.borrow().history.last() {
                Some(last) => line = last.clone(),
                None => return,
            }
        }
        {
            let mut sh = self.shared.borrow_mut();
            sh.output.push_str(&format!("> {line}\n"));
            sh.input.push_back(line);
        }
        self.history_pos = None;
        // enterLine: typing while running breaks into the debugger first.
        if debugger_state(s) == Some(DebuggerState::Running) && !self.cli_paused(s) {
            s.core().debugger_break();
        }
    }

    fn tab_complete(&mut self, s: &mut Session) {
        let Some(idx) = self.module else { return };
        // Complete the first token only (CLIDebuggerTabComplete works on the
        // command table).
        if self.line.contains(char::is_whitespace) {
            return;
        }
        let token = self.line.clone();
        self.shared.borrow_mut().completion.clear();
        with_cli(s, idx, |c| c.tab_complete(&token, true));
        let add = std::mem::take(&mut self.shared.borrow_mut().completion);
        self.line.push_str(&add);
    }
}

impl ToolWindow for DebuggerConsole {
    fn title(&self) -> &'static str {
        "Debugger"
    }

    fn on_session_changed(&mut self) {
        self.module = None;
        self.we_paused = false;
        self.shared.borrow_mut().input.clear();
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        let mut pause_request = None;
        egui::Window::new(self.title())
            .open(open)
            .default_size([560.0, 380.0])
            .show(ctx, |ui| {
                let Some(s) = tc.session.as_deref_mut() else {
                    no_game(ui);
                    return;
                };
                if self.module.is_none() {
                    self.attach(s);
                }

                // Keep the debugger serviced while it holds the CPU.
                let mut paused = self.cli_paused(s);
                if paused {
                    pump(s);
                    paused = self.cli_paused(s);
                }
                pause_request = Some(paused);

                let state = debugger_state(s);
                ui.horizontal(|ui| {
                    let label = match state {
                        Some(DebuggerState::Paused) => "Paused",
                        Some(DebuggerState::Running) => "Running",
                        Some(DebuggerState::Callback) => "Tracing",
                        Some(DebuggerState::Shutdown) => "Detached",
                        _ => "—",
                    };
                    ui.label(format!("State: {label}"));
                    if ui.add_enabled(!paused, egui::Button::new("Break")).clicked() {
                        s.core().debugger_break();
                    }
                    if ui.add_enabled(paused, egui::Button::new("Continue")).clicked() {
                        self.shared.borrow_mut().input.push_back("continue".into());
                    }
                    if ui.add_enabled(paused, egui::Button::new("Step")).clicked() {
                        self.shared.borrow_mut().input.push_back("next".into());
                    }
                    if ui.button("Clear").clicked() {
                        self.shared.borrow_mut().output.clear();
                    }
                });
                ui.separator();

                let input_h = ui.spacing().interact_size.y + 8.0;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .max_height(ui.available_height() - input_h)
                    .show(ui, |ui| {
                        let out = self.shared.borrow().output.clone();
                        ui.add(
                            egui::Label::new(egui::RichText::new(out).monospace())
                                .wrap_mode(egui::TextWrapMode::Extend),
                        );
                    });

                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.line)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .lock_focus(true)
                        .hint_text("command"),
                );
                let (enter, up, down, tab) = ui.input(|i| {
                    (
                        i.key_pressed(egui::Key::Enter),
                        i.key_pressed(egui::Key::ArrowUp),
                        i.key_pressed(egui::Key::ArrowDown),
                        i.key_pressed(egui::Key::Tab),
                    )
                });
                if r.lost_focus() && enter {
                    self.submit(s);
                    r.request_focus();
                } else if r.has_focus() {
                    let hist = self.shared.borrow().history.clone();
                    if up && !hist.is_empty() {
                        let p = self.history_pos.map_or(hist.len() - 1, |p| p.saturating_sub(1));
                        self.history_pos = Some(p);
                        self.line = hist[p].clone();
                    } else if down {
                        match self.history_pos {
                            Some(p) if p + 1 < hist.len() => {
                                self.history_pos = Some(p + 1);
                                self.line = hist[p + 1].clone();
                            }
                            _ => {
                                self.history_pos = None;
                                self.line.clear();
                            }
                        }
                    } else if tab {
                        self.tab_complete(s);
                    }
                }
                // Queued input while paused is consumed on the next pump.
                if paused || !self.shared.borrow().input.is_empty() {
                    ctx.request_repaint();
                }
            });

        // Hold the app's frame loop while the CLI owns the CPU.
        match pause_request {
            Some(true) if !*tc.paused => {
                *tc.paused = true;
                self.we_paused = true;
            }
            Some(false) if self.we_paused => {
                *tc.paused = false;
                self.we_paused = false;
            }
            _ => {}
        }
        if !*open && self.we_paused {
            // Closing the window doesn't detach (like Qt), but don't leave
            // the game frozen with nobody to type "continue".
            if let Some(s) = tc.session.as_deref_mut() {
                if self.cli_paused(s) {
                    self.shared.borrow_mut().input.push_back("continue".into());
                    pump(s);
                }
            }
            *tc.paused = false;
            self.we_paused = false;
        }
    }
}
