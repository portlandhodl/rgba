// Multiplayer — mGBA Qt's "New multiplayer window" (GBAApp::newWindow +
// MultiplayerController): extra GBA cores linked to the running game with
// the lockstep link cable (rgba_gba::sio::lockstep, a port of
// gba/sio/lockstep.c).
//
// The C runs every core on its own thread and the lockstep coordinator
// blocks/wakes them. The Rust cores are single-threaded and cooperative
// (see the lockstep.rs header): linked cores must be advanced in small,
// interleaved slices so their sync points converge. `run_linked_frame`
// does exactly that — it steps the primary one `arm_run_loop` (up to its
// next event, which includes the lockstep sync event) at a time and then
// catches every peer up to the same elapsed cycle count.
//
// Two ways this window drives the peers:
// - Preferred: the app calls `MultiplayerView::run_frame_linked` *instead
//   of* the primary's own `run_frame` (cycle-interleaved, like mGBA).
// - Fallback (no app support): `on_frame` runs after the primary already
//   finished its frame, so each peer is caught up to the primary's elapsed
//   cycles afterwards. Links still work but each exchange can take up to a
//   frame longer than on hardware; latency-sensitive games may time out.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use eframe::egui;

use rgba_gba::gba::Gba;
use rgba_gba::sio::lockstep::GbaSioLockstep;
use rgba_gba::video::{VIDEO_HORIZONTAL_LENGTH, VIDEO_TOTAL_LENGTH};

use super::{ToolCtx, ToolWindow};
use crate::emu::{save_path_for, Console, LoadOptions, Session};

/// Keyboard layout for the controlled peer (core key-bit order:
/// A B Select Start Right Left Up Down R L).
const PEER_KEYS: [(egui::Key, &str); 10] = [
    (egui::Key::U, "U"),
    (egui::Key::O, "O"),
    (egui::Key::Num7, "7"),
    (egui::Key::Num8, "8"),
    (egui::Key::L, "L"),
    (egui::Key::J, "J"),
    (egui::Key::I, "I"),
    (egui::Key::K, "K"),
    (egui::Key::P, "P"),
    (egui::Key::Y, "Y"),
];

struct Peer {
    session: Session,
    player: u32,
    texture: Option<egui::TextureHandle>,
    frames: u32,
}

fn gba_of(s: &mut Session) -> Option<&mut Gba> {
    match &mut s.console {
        Console::Gba(g) => Some(g),
        _ => None,
    }
}

/// Advance `primary` by one video frame with every peer interleaved at
/// cycle granularity (the cooperative stand-in for mGBA's lockstep threads).
/// Mirrors `Gba::run_frame`'s loop and frame-length cap.
pub fn run_linked_frame(primary: &mut Gba, peers: &mut [&mut Gba]) {
    let frame = primary.video.frame_counter;
    let start = primary.current_time();
    let peer_start: Vec<i32> = peers.iter().map(|p| p.current_time()).collect();
    while primary.video.frame_counter == frame
        && primary.current_time().wrapping_sub(start) < VIDEO_TOTAL_LENGTH + VIDEO_HORIZONTAL_LENGTH
    {
        primary.arm_run_loop();
        let elapsed = primary.current_time().wrapping_sub(start);
        for (p, &t0) in peers.iter_mut().zip(&peer_start) {
            catch_up(p, t0, elapsed);
        }
    }
}

/// Run `g` until it has executed `elapsed` cycles since `t0`.
fn catch_up(g: &mut Gba, t0: i32, elapsed: i32) {
    // Bounded so a wedged peer can never hang the UI thread.
    let mut guard = 0u32;
    while g.current_time().wrapping_sub(t0) < elapsed && guard < 1_000_000 {
        g.arm_run_loop();
        guard += 1;
    }
}

pub struct MultiplayerView {
    link: Option<Rc<RefCell<GbaSioLockstep>>>,
    peers: Vec<Peer>,
    /// Peer that receives the keyboard bindings (index into `peers`).
    controlled: usize,
    keys: u32,
    /// Set by `run_frame_linked` so `on_frame` doesn't step peers twice.
    driven: bool,
    /// Fallback stepping: primary time at the previous `on_frame`.
    last_primary_time: Option<i32>,
    rom: Option<PathBuf>,
    error: String,
    flush_tick: u32,
}

impl Default for MultiplayerView {
    fn default() -> Self {
        MultiplayerView {
            link: None,
            peers: Vec::new(),
            controlled: 0,
            keys: 0,
            driven: false,
            last_primary_time: None,
            rom: None,
            error: String::new(),
            flush_tick: 0,
        }
    }
}

impl MultiplayerView {

    /// For the app: run the primary's frame with peers interleaved. Returns
    /// false (doing nothing) when no peers are linked, in which case the app
    /// runs the primary normally.
    pub fn run_frame_linked(&mut self, primary: &mut Session) -> bool {
        if self.peers.is_empty() {
            return false;
        }
        let Some(main) = gba_of(primary) else { return false };
        self.apply_keys();
        let mut peers: Vec<&mut Gba> = self.peers.iter_mut().filter_map(|p| gba_of(&mut p.session)).collect();
        run_linked_frame(main, &mut peers);
        self.driven = true;
        self.last_primary_time = Some(main.current_time());
        true
    }

    fn apply_keys(&mut self) {
        for (i, p) in self.peers.iter_mut().enumerate() {
            let k = if i == self.controlled { self.keys } else { 0 };
            p.session.core().set_keys(k);
        }
    }

    fn add_peer(&mut self, tc: &mut ToolCtx, rom: PathBuf) -> Result<(), String> {
        let Some(main) = tc.session.as_deref_mut() else {
            return Err("Start a GBA game first; it becomes player 1.".into());
        };
        if self.peers.len() >= 3 {
            return Err("At most 4 players can be linked.".into());
        }
        let player = self.peers.len() as u32 + 2;
        let opts = LoadOptions { save_path: Some(save_path_for(&rom, player)), ..Default::default() };
        let mut s = Session::open(&rom, tc.config, &opts)?;
        if gba_of(&mut s).is_none() {
            return Err("Only GBA games can be linked here.".into());
        }
        let link = match &self.link {
            Some(l) => l.clone(),
            None => {
                let l = GbaSioLockstep::new();
                let Some(g) = gba_of(main) else {
                    return Err("The running game is not a GBA game.".into());
                };
                g.sio_attach_lockstep(&l);
                self.link = Some(l.clone());
                self.last_primary_time = Some(g.current_time());
                l
            }
        };
        gba_of(&mut s).unwrap().sio_attach_lockstep(&link);
        self.peers.push(Peer { session: s, player, texture: None, frames: 0 });
        Ok(())
    }

    fn disconnect(&mut self, primary: Option<&mut Session>) {
        for p in &mut self.peers {
            p.session.flush_savedata(false);
            if let Some(g) = gba_of(&mut p.session) {
                g.sio_detach_lockstep();
            }
        }
        self.peers.clear();
        if let Some(g) = primary.and_then(gba_of) {
            if self.link.is_some() {
                g.sio_detach_lockstep();
            }
        }
        self.link = None;
        self.last_primary_time = None;
        self.controlled = 0;
    }

    fn read_keys(&mut self, ctx: &egui::Context) {
        self.keys = 0;
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        ctx.input(|i| {
            for (bit, (k, _)) in PEER_KEYS.iter().enumerate() {
                if i.key_down(*k) {
                    self.keys |= 1 << bit;
                }
            }
        });
    }
}

impl ToolWindow for MultiplayerView {
    fn title(&self) -> &'static str {
        "Multiplayer"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        self.read_keys(ctx);
        let primary_is_gba = tc.session.as_deref().map_or(false, |s| matches!(s.console, Console::Gba(_)));
        let mut add: Option<PathBuf> = None;
        let mut remove = false;
        egui::Window::new(self.title()).open(open).default_width(520.0).show(ctx, |ui| {
            if !primary_is_gba {
                ui.label("Start a GBA game first; it becomes player 1. Linked players run in this window.");
            }
            ui.horizontal(|ui| {
                if ui.button("Choose ROM…").clicked() {
                    let mut d = rfd::FileDialog::new()
                        .set_title("Select ROM for the next player")
                        .add_filter("GBA ROMs", &["gba", "agb", "bin"]);
                    if let Some(dir) = tc.session.as_ref().and_then(|s| s.rom_path.parent()) {
                        d = d.set_directory(dir);
                    }
                    if let Some(p) = d.pick_file() {
                        self.rom = Some(p);
                    }
                }
                let label = self
                    .rom
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map_or("(same game as player 1)".to_string(), |n| n.to_string_lossy().into_owned());
                ui.label(label);
            });
            ui.horizontal(|ui| {
                let next = self.peers.len() + 2;
                if ui
                    .add_enabled(primary_is_gba && self.peers.len() < 3, egui::Button::new(format!("Add player {next}")))
                    .clicked()
                {
                    add = self.rom.clone().or_else(|| tc.session.as_ref().map(|s| s.rom_path.clone()));
                }
                if ui.add_enabled(!self.peers.is_empty(), egui::Button::new("Disconnect all")).clicked() {
                    remove = true;
                }
            });
            if !self.error.is_empty() {
                ui.colored_label(egui::Color32::from_rgb(230, 120, 80), &self.error);
            }
            if self.peers.is_empty() {
                return;
            }
            ui.separator();
            let names: String = PEER_KEYS.iter().zip(crate::config::KEY_NAMES).map(|((_, k), n)| format!("{n}={k}")).collect::<Vec<_>>().join("  ");
            ui.small(format!("Keyboard for the selected player: {names}"));
            if !self.driven {
                ui.small("Peers are caught up after each player-1 frame; link transfers may be slower than on hardware.");
            }
            ui.horizontal_wrapped(|ui| {
                for (i, p) in self.peers.iter_mut().enumerate() {
                    ui.vertical(|ui| {
                        let id = gba_of(&mut p.session).and_then(|g| g.sio_lockstep_player_id());
                        let sel = self.controlled == i;
                        if ui
                            .selectable_label(sel, format!("Player {} — {} (link id {})", p.player, p.session.title(), id.map_or("-".into(), |v| v.to_string())))
                            .clicked()
                        {
                            self.controlled = i;
                        }
                        let (w, h) = p.session.core().base_video_size();
                        let tex = super::upload_rgba(ctx, &mut p.texture, &format!("mp{}", p.player), p.session.core().video_buffer(), w as usize, h as usize);
                        ui.image((tex.id(), egui::vec2(w as f32, h as f32)));
                    });
                }
            });
        });
        if let Some(rom) = add {
            match self.add_peer(tc, rom) {
                Ok(()) => self.error.clear(),
                Err(e) => self.error = e,
            }
        }
        if remove || (!*open && !self.peers.is_empty()) {
            self.disconnect(tc.session.as_deref_mut());
        }
        if !self.peers.is_empty() {
            ctx.request_repaint();
        }
    }

    fn run_frame(&mut self, primary: &mut Session) -> bool {
        self.run_frame_linked(primary)
    }

    fn on_frame(&mut self, primary: &mut Session) {
        if self.peers.is_empty() {
            return;
        }
        if !self.driven {
            // Fallback: catch peers up to the primary's elapsed cycles.
            self.apply_keys();
            if let Some(main) = gba_of(primary) {
                let now = main.current_time();
                let elapsed = self.last_primary_time.map_or(VIDEO_TOTAL_LENGTH, |t| now.wrapping_sub(t));
                self.last_primary_time = Some(now);
                for p in &mut self.peers {
                    if let Some(g) = gba_of(&mut p.session) {
                        let t0 = g.current_time();
                        // Slice the catch-up so peer-to-peer sync points
                        // still interleave between peers.
                        let mut done = 0;
                        while done < elapsed {
                            let step = (elapsed - done).min(4096);
                            done += step;
                            catch_up(g, t0, done);
                        }
                    }
                }
            }
        }
        self.driven = false;
        self.flush_tick += 1;
        for p in &mut self.peers {
            // Peers are muted: drop their audio.
            p.session.core().audio_buffer().clear();
            p.frames += 1;
            if self.flush_tick % 60 == 0 {
                p.session.flush_savedata(false);
            }
        }
    }

    fn on_session_changed(&mut self) {
        // Player 1 was replaced/closed; its link node went with it.
        self.disconnect(None);
    }
}

impl Drop for MultiplayerView {
    fn drop(&mut self) {
        for p in &mut self.peers {
            p.session.flush_savedata(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgba_core::core::Core;

    #[test]
    fn linked_cores_advance_together() {
        let ls = GbaSioLockstep::new();
        let mut a = Gba::new();
        let mut b = Gba::new();
        a.arm_reset();
        b.arm_reset();
        a.sio_attach_lockstep(&ls);
        b.sio_attach_lockstep(&ls);
        assert_eq!(ls.borrow().attached(), 2);
        let ia = a.sio_lockstep_player_id().unwrap();
        let ib = b.sio_lockstep_player_id().unwrap();
        assert_eq!(ia + ib, 1, "player ids 0 and 1");
        let (ta, tb) = (a.current_time(), b.current_time());
        for _ in 0..3 {
            let mut peers = [&mut *b];
            run_linked_frame(&mut a, &mut peers);
        }
        let da = a.current_time().wrapping_sub(ta);
        let db = b.current_time().wrapping_sub(tb);
        assert!(da >= 2 * VIDEO_TOTAL_LENGTH, "primary ran ~3 frames ({da})");
        assert!((db - da).abs() < 4096, "peer kept pace with primary ({da} vs {db})");
    }

    /// Two real games linked for 10 s of emulated time: both must keep
    /// producing frames (no wedge in the lockstep coordinator).
    #[test]
    fn linked_real_rom_keeps_running() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../Pokemon - Emerald Version (USA, Europe).gba");
        let Ok(rom) = std::fs::read(path) else { return };
        let ls = GbaSioLockstep::new();
        let mut a = Gba::new();
        let mut b = Gba::new();
        assert!(a.load_rom(rom.clone()) && b.load_rom(rom));
        a.arm_reset();
        b.arm_reset();
        a.sio_attach_lockstep(&ls);
        b.sio_attach_lockstep(&ls);
        for _ in 0..600 {
            let mut peers = [&mut *b];
            run_linked_frame(&mut a, &mut peers);
            a.audio_buffer().clear();
            b.audio_buffer().clear();
        }
        assert!(a.video.frame_counter >= 590, "primary frames {}", a.video.frame_counter);
        assert!(b.video.frame_counter >= 590, "peer frames {}", b.video.frame_counter);
    }
}
