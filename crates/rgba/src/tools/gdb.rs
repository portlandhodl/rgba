// "Start GDB server..." — mirrors mGBA Qt's GDBWindow (GDBWindow.cpp):
// local port, bind address, write-watchpoint behavior, Start/Stop, Break.
//
// The Qt core thread services the stub from its debugger loop. Here the
// window pumps it on the UI thread: `debugger_update` every UI frame (accept
// connections, read packets incl. Ctrl-C), and while GDB has the target
// stopped, one zero-timeout debugger iteration per UI frame with the app's
// frame loop held. Because the stub is only serviced while this window is
// open, closing the window stops the server (Qt keeps it running).

use eframe::egui;
use rgba_debugger::debugger::{DebuggerModule, DebuggerState};
use rgba_debugger::gdb::{GdbStub, WatchpointsBehavior};
use rgba_gba::gba::Gba;

use super::{ToolCtx, ToolWindow};
use crate::emu::Console;

pub struct GdbView {
    behavior: WatchpointsBehavior,
    /// Slot of our stub in the GBA debugger's module list.
    module: Option<usize>,
    listening: bool,
    error: Option<String>,
    we_paused: bool,
}

impl Default for GdbView {
    fn default() -> Self {
        GdbView {
            behavior: WatchpointsBehavior::StandardLogic,
            module: None,
            listening: false,
            error: None,
            we_paused: false,
        }
    }
}

fn with_stub<R>(g: &mut Gba, idx: usize, f: impl FnOnce(&mut GdbStub) -> R) -> Option<R> {
    let m = g.debugger.as_mut()?.core.modules.get_mut(idx)?.as_mut()?;
    m.as_any_mut().downcast_mut::<GdbStub>().map(f)
}

impl GdbView {
    fn start(&mut self, g: &mut Gba, port: u16) {
        self.error = None;
        if let Some(idx) = self.module {
            let behavior = self.behavior;
            match with_stub(g, idx, |s| s.listen(port, behavior)) {
                Some(Ok(())) => self.listening = true,
                Some(Err(e)) => self.error = Some(format!("Could not start GDB server: {e}")),
                None => self.module = None,
            }
            if self.module.is_some() {
                return;
            }
        }
        let mut stub = GdbStub::create();
        if let Err(e) = stub.listen(port, self.behavior) {
            self.error = Some(format!("Could not start GDB server: {e}"));
            return;
        }
        g.debugger_attach();
        let idx = g.debugger_attach_module(stub);
        if idx == usize::MAX {
            self.error = Some("Could not attach the debugger".into());
            return;
        }
        self.module = Some(idx);
        self.listening = true;
    }

    fn stop(&mut self, g: &mut Gba) {
        if let Some(idx) = self.module {
            with_stub(g, idx, |s| s.shutdown());
            if let Some(d) = g.debugger.as_mut() {
                d.core.update_paused_placeholder_safe();
            }
        }
        self.listening = false;
    }

    fn stub_paused(&self, g: &mut Gba) -> bool {
        self.module.and_then(|idx| with_stub(g, idx, |s| s.is_paused())).unwrap_or(false)
    }

    fn stub_connected(&self, g: &mut Gba) -> bool {
        self.module.and_then(|idx| with_stub(g, idx, |s| s.is_connected())).unwrap_or(false)
    }
}

/// Recompute the debugger state after changing a module flag outside a poll.
trait UpdatePaused {
    fn update_paused_placeholder_safe(&mut self);
}

impl UpdatePaused for rgba_debugger::debugger::Debugger {
    fn update_paused_placeholder_safe(&mut self) {
        // Debugger::update_paused needs a console only to iterate modules;
        // the module flags are all we need here.
        let any_paused = self.modules.iter().flatten().any(|m| m.is_paused());
        let any_cb = self.modules.iter().flatten().any(|m| m.needs_callback());
        if self.state != DebuggerState::Shutdown {
            self.state = if any_paused {
                DebuggerState::Paused
            } else if any_cb {
                DebuggerState::Callback
            } else {
                DebuggerState::Running
            };
        }
    }
}

impl ToolWindow for GdbView {
    fn title(&self) -> &'static str {
        "GDB Server"
    }

    fn on_session_changed(&mut self) {
        self.module = None;
        self.listening = false;
        self.we_paused = false;
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        let mut pause_request = None;
        let port = &mut tc.config.gdb_port;
        let gba = tc.session.as_deref_mut().and_then(|s| match &mut s.console {
            Console::Gba(g) => Some(&mut **g),
            Console::Gb(_) => None,
        });
        egui::Window::new(self.title())
            .open(open)
            .resizable(false)
            .show(ctx, |ui| {
                let Some(g) = gba else {
                    ui.label("The GDB server is available for GBA games only.");
                    return;
                };

                if self.listening {
                    // Accept connections / read packets, and service the
                    // stub while GDB holds the target.
                    g.debugger_update();
                    if self.stub_paused(g) {
                        g.debugger_run_timeout(0);
                    }
                    pause_request = Some(self.stub_paused(g));
                    ctx.request_repaint();
                }

                egui::Grid::new("gdb_settings").num_columns(2).show(ui, |ui| {
                    ui.label("Local port");
                    ui.add_enabled(!self.listening, egui::DragValue::new(port).range(1..=65535));
                    ui.end_row();
                    ui.label("Bind address");
                    ui.label("127.0.0.1");
                    ui.end_row();
                });
                ui.add_space(4.0);
                ui.label("Write watchpoints behavior");
                let before = self.behavior;
                ui.radio_value(&mut self.behavior, WatchpointsBehavior::StandardLogic, "Standard GDB");
                ui.radio_value(&mut self.behavior, WatchpointsBehavior::OverrideLogic, "Internal change detection");
                ui.radio_value(&mut self.behavior, WatchpointsBehavior::OverrideLogicAnyWrite, "Break on all writes");
                if self.behavior != before {
                    if let Some(idx) = self.module {
                        let b = self.behavior;
                        with_stub(g, idx, |s| s.set_watchpoints_behavior(b));
                    }
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let label = if self.listening { "Stop" } else { "Start" };
                    if ui.button(label).clicked() {
                        if self.listening {
                            self.stop(g);
                        } else {
                            self.start(g, *port);
                        }
                    }
                    let can_break = self.listening && self.stub_connected(g) && !self.stub_paused(g);
                    if ui.add_enabled(can_break, egui::Button::new("Break")).clicked() {
                        rgba_core::core::Core::debugger_break(g);
                    }
                });
                let status = if !self.listening {
                    "Stopped".to_string()
                } else if self.stub_connected(g) {
                    if self.stub_paused(g) { "Connected — target stopped".into() } else { "Connected — running".into() }
                } else {
                    format!("Listening on 127.0.0.1:{port}")
                };
                ui.label(status);
                if let Some(e) = &self.error {
                    ui.colored_label(egui::Color32::from_rgb(255, 110, 110), e);
                }
                ui.weak("Closing this window stops the server.");
            });

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
        if !*open {
            if let Some(Console::Gba(g)) = tc.session.as_deref_mut().map(|s| &mut s.console) {
                if self.listening {
                    self.stop(g);
                }
            }
            if self.we_paused {
                *tc.paused = false;
                self.we_paused = false;
            }
        }
    }
}
