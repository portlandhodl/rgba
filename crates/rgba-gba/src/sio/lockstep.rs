// Copyright (c) 2013-2024 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gba/sio/lockstep.c and
// include/mgba/internal/gba/sio/lockstep.h (+ include/mgba/core/lockstep.h's
// mLockstepUser vtable, and the SIO-driver half of _GBACoreSaveExtraState/
// _GBACoreLoadExtraState from mgba/src/gba/core.c).
//
// DESIGN (mirrors the GB port in rgba-gb/src/sio.rs): the C is built for two
// emulator threads sharing a coordinator behind a mutex, with a
// `struct mLockstepUser` vtable (sleep/wake/requestedId/playerIdChanged) that
// the Qt frontend implements as thread blocking/resume. Here everything is
// single-threaded and cooperative:
//
// - `GbaSioLockstep` (the coordinator) is shared between the linked `Gba`s
//   through an `Rc<RefCell<GbaSioLockstep>>`; each `GbaSioLockstepNode` (the
//   C's GBASIOLockstepDriver) holds a Weak handle plus its `lockstep_id`.
// - The mutex/Table types become the plain HashMap; `attachedPlayers` and the
//   renumbering on attach/detach (`_reconfigPlayers`) are kept exactly.
// - sleep/wake are bookkeeping flags + the C's CPU nudges
//   (`cpu->nextEvent = 0; GBAInterrupt()`: early_exit + Timing::interrupt);
//   the "parked" core simply stops having useful lockstep work until its
//   priority-0x80 lockstep event fires again, so a harness that alternates
//   stepping the linked `Gba`s makes progress like the C's threads.
// - requestedId is a static per-player field (default MAX_GBAS-1, as in the C
//   when the vtable hook is NULL); playerIdChanged is a frontend UI hook and
//   is not modeled (query `node.player_id()` instead).
// - The per-player event queue keeps the C's `buffer[MAX_LOCKSTEP_EVENTS]`
//   cap semantics: a Vec sorted by ascending timestamp; enqueue with a full
//   queue logs "No free events" and drops the event (matching mASSERT_LOG +
//   the effective drop in the C).
// - The only cross-core peek the C performs that we cannot is
//   `_reconfigPlayers' 1-player case reading the *remaining* player's live
//   mTimingCurrentTime; the node records a `proxy_time` sample of its own
//   Gba at every entry point and the coordinator uses that (identical when
//   cores are detached while paused, the only case frontends hit).
// - The driver's savestate (`struct GBASIOLockstepSerializedState`, 0x1F0
//   bytes) is exposed through `Gba::sio_save_extra_state`/`sio_load_extra_state`,
//   byte-compatible with the C, prefixed with the 4-byte DRIVER_ID exactly
//   like core.c's extdata slot EXTDATA_SUBSYSTEM_START+GBA_SUBSYSTEM_SIO_DRIVER.
//   It is NOT part of the main 0x61000 GBASerializedState (as in the C).
//
// The driver's timing event (`_lockstepEvent`, priority 0x80) is folded into
// `EventId::SioLockstep` and dispatched from `Gba::process_event`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use rgba_core::timing::Timing;
use rgba_core::{mlog, Level};

use crate::gba::{EventId, Gba};
use crate::io::*;
use crate::sio::{
    multiplayer_clear_slave, multiplayer_set_id, multiplayer_set_ready, multiplayer_set_slave,
    rcnt_set_sd, SioDriver, SioMode,
};

/// DRIVER_ID ("Lock" little-endian) — the extdata type tag from core.c.
pub const SIO_LOCKSTEP_DRIVER_ID: u32 = 0x6B636F4C;
const DRIVER_STATE_VERSION: u32 = 1;

pub const LOCKSTEP_INTERVAL: i32 = 4096;
pub const UNLOCKED_INTERVAL: i32 = 4096;
pub const HARD_SYNC_INTERVAL: i32 = 0x80000;
pub const MAX_LOCKSTEP_EVENTS: usize = 8;

fn target(p: i32) -> u32 {
    1u32 << p
}
const TARGET_ALL: u32 = 0xF;
const TARGET_SECONDARY: u32 = TARGET_ALL & !0x1; // all but the primary

/// enum GBASIOLockstepEventType (values must match the C for savestates).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum GbaSioLockstepEventType {
    Attach = 0,
    Detach = 1,
    HardSync = 2,
    ModeSet = 3,
    TransferStart = 4,
}

/// struct GBASIOLockstepEvent. The C union { mode; finishCycle; } is a
/// single i32 payload here, interpreted by event type.
#[derive(Clone, Copy, Debug)]
pub struct GbaSioLockstepEvent {
    pub event_type: GbaSioLockstepEventType,
    pub timestamp: i32,
    pub player_id: i32,
    /// union { enum GBASIOMode mode; int32_t finishCycle; } (raw i32 either way)
    pub payload: i32,
}

/// struct GBASIOLockstepPlayer (the driver back-pointer is replaced by the
/// node's `lockstep_id` key + per-call `&mut Gba`).
pub struct GbaSioLockstepPlayer {
    pub player_id: i32,
    mode: SioMode,
    other_modes: [SioMode; MAX_GBAS],
    asleep: bool,
    cycle_offset: i32,
    queue: Vec<GbaSioLockstepEvent>,
    data_received: bool,
    /// mLockstepUser.requestedId, sampled into a plain field at player
    /// creation (the C re-asks the frontend on every reconfig).
    requested_id: i32,
    /// Port modeling aid: newest current-time sample of this player's Gba
    /// (see module comment).
    proxy_time: i32,
}

/// struct GBASIOLockstepCoordinator. Shared between linked `Gba`s via
/// `Rc<RefCell<...>>`.
pub struct GbaSioLockstep {
    players: HashMap<u32, GbaSioLockstepPlayer>,
    next_id: u32,
    attached_players: [u32; MAX_GBAS],
    n_attached: usize,
    waiting: u32,
    transfer_active: bool,
    transfer_mode: SioMode,
    cycle: i32,
    next_hard_sync: i32,
    multi_data: [u16; 4],
    normal_data: [u32; 4],
}

/// MAX_GBAS (include/mgba/internal/gba/sio.h)
pub const MAX_GBAS: usize = 4;

impl GbaSioLockstep {
    /// GBASIOLockstepCoordinatorInit (+ mLockstepUser defaults).
    pub fn new() -> Rc<RefCell<GbaSioLockstep>> {
        Rc::new(RefCell::new(GbaSioLockstep {
            players: HashMap::new(),
            next_id: 0,
            attached_players: [0; MAX_GBAS],
            n_attached: 0,
            waiting: 0,
            transfer_active: false,
            transfer_mode: SioMode::Invalid,
            cycle: 0,
            next_hard_sync: 0,
            multi_data: [0; 4],
            normal_data: [0; 4],
        }))
    }

    /// GBASIOLockstepCoordinatorAttach: the node holds the Weak handle; the
    /// player slot is allocated on the node's init/reset (C: driver->reset).
    pub fn attach_node(coordinator: &Rc<RefCell<GbaSioLockstep>>) -> GbaSioLockstepNode {
        GbaSioLockstepNode {
            coordinator: Rc::downgrade(coordinator),
            lockstep_id: 0,
            requested_id: MAX_GBAS as i32 - 1,
        }
    }

    /// GBASIOLockstepCoordinatorAttached (TableSize)
    pub fn attached(&self) -> usize {
        self.players.len()
    }

    /// Number of player slots currently in use (coordinator->nAttached).
    pub fn n_attached(&self) -> usize {
        self.n_attached
    }

    // -- internal helpers -------------------------------------------------

    /// GBASIOLockstepTime
    fn player_time(&self, lockstep_id: u32) -> i32 {
        let clock = self.players[&lockstep_id].proxy_time;
        clock.wrapping_sub(self.players[&lockstep_id].cycle_offset)
    }

    /// _verifyAwake (mASSERT_DEBUG)
    fn verify_awake(&self) {
        let mut asleep = 0;
        for i in 0..self.n_attached {
            if self.attached_players[i] == 0 {
                continue;
            }
            if let Some(player) = self.players.get(&self.attached_players[i]) {
                asleep += player.asleep as i32;
            }
        }
        debug_assert!(asleep == 0 || asleep < self.n_attached as i32);
    }

    /// _abortTransfer
    fn abort_transfer(&mut self, active_id: u32) {
        mlog!(
            Level::Debug,
            rgba_core::log::GBA_SIO,
            "Aborting in-progress transfer"
        );
        // TODO: Do we need to clean this up better?
        self.transfer_active = false;
        self.waiting = 0;
        let player_id = self.players[&active_id].player_id;
        if player_id != 0 {
            let runner = self.attached_players[0];
            if runner != 0 {
                self.player_wake(runner);
            }
        } else {
            self.wake_players();
        }
    }

    /// _removePlayer. `now` is the detaching node's current time.
    fn remove_player(&mut self, lockstep_id: u32, now: i32) {
        let player = &self.players[&lockstep_id];
        let player_id = player.player_id;
        let event = GbaSioLockstepEvent {
            event_type: GbaSioLockstepEventType::Detach,
            player_id,
            timestamp: now.wrapping_sub(player.cycle_offset),
            payload: 0,
        };
        self.enqueue_event(&event, TARGET_ALL & !target(player_id));

        self.waiting = 0;
        self.transfer_active = false;

        self.players.remove(&lockstep_id);
        self.reconfig_players();

        let runner = self.attached_players[0];
        if runner != 0 {
            self.player_wake(runner);
        }
        self.verify_awake();
    }

    /// _reconfigPlayers
    fn reconfig_players(&mut self) {
        let players = self.players.len();
        self.attached_players = [0; MAX_GBAS];
        if players == 0 {
            mlog!(
                Level::Warn,
                rgba_core::log::GBA_SIO,
                "Reconfiguring player IDs with no players attached somehow?"
            );
        } else if players == 1 {
            let (&p0, player) = self.players.iter_mut().next().expect("1 player");
            self.attached_players[0] = p0;
            // The C reads the remaining player's live mTimingCurrentTime;
            // we use its most recent sample (see module comment).
            self.cycle = player.proxy_time;
            self.next_hard_sync = HARD_SYNC_INTERVAL;

            if player.player_id != 0 {
                player.player_id = 0;
                // C: user->playerIdChanged hook (frontend UI); not modeled.
            }
            if !self.transfer_active {
                self.transfer_mode = player.mode;
            }
        } else {
            // UIntList playerPreferences[MAX_GBAS]
            let mut preferences: [Vec<u32>; MAX_GBAS] = Default::default();

            // Collect the first four players' requested player IDs
            let mut seen = 0usize;
            for (&pid, player) in self.players.iter() {
                let mut requested = player.requested_id;
                if requested < 0 {
                    continue;
                }
                if requested >= MAX_GBAS as i32 {
                    requested = MAX_GBAS as i32 - 1;
                }
                preferences[requested as usize].push(pid);
                seen += 1;
                if seen >= MAX_GBAS {
                    break;
                }
            }

            // Now sort each requested player ID to figure out who gets which ID
            seen = 0;
            for i in 0..MAX_GBAS {
                for j in 0..=i {
                    while !preferences[j].is_empty() && seen < MAX_GBAS {
                        let pid = preferences[j].remove(0);
                        let player = match self.players.get_mut(&pid) {
                            Some(p) => p,
                            None => {
                                mlog!(
                                    Level::Error,
                                    rgba_core::log::GBA_SIO,
                                    "Player list appears to have changed unexpectedly. PID {} missing.",
                                    pid
                                );
                                continue;
                            }
                        };
                        self.attached_players[seen] = pid;
                        if player.player_id != seen as i32 {
                            player.player_id = seen as i32;
                            // C: user->playerIdChanged hook; not modeled.
                        }
                        seen += 1;
                    }
                }
            }
        }

        let mut n_attached = 0;
        for i in 0..MAX_GBAS {
            let pid = self.attached_players[i];
            if pid == 0 {
                continue;
            }
            if !self.players.contains_key(&pid) {
                self.attached_players[i] = 0;
            } else {
                n_attached += 1;
            }
        }
        self.n_attached = n_attached;
    }

    /// _enqueueEvent (sorted insert; logs and drops on a full queue)
    fn enqueue_event(&mut self, event: &GbaSioLockstepEvent, target_mask: u32) {
        mlog!(
            Level::Debug,
            rgba_core::log::GBA_SIO,
            "Enqueuing event of type {:X} from {} for target {:X} at timestamp {:X}",
            event.event_type as u32,
            event.player_id,
            target_mask,
            event.timestamp
        );
        for i in 0..self.n_attached {
            if target_mask & (1 << i) == 0 {
                continue;
            }
            let pid = self.attached_players[i];
            let Some(player) = self.players.get_mut(&pid) else {
                continue;
            };
            if player.queue.len() >= MAX_LOCKSTEP_EVENTS {
                // mASSERT_LOG(player->freeList, "No free events") — the C
                // then uses a NULL event; the observable effect we keep is
                // log + drop.
                mlog!(Level::Error, rgba_core::log::GBA_SIO, "No free events");
                continue;
            }
            let pos = player
                .queue
                .iter()
                .position(|next| event.timestamp.wrapping_sub(next.timestamp) < 0)
                .unwrap_or(player.queue.len());
            player.queue.insert(pos, *event);
        }
    }

    /// _setData
    fn set_data(&mut self, gba: &Gba, id: i32) {
        if id < 0 || id as usize >= MAX_GBAS {
            return;
        }
        let id = id as usize;
        match self.transfer_mode {
            SioMode::Multi => {
                self.multi_data[id] = gba.memory.io[(GBA_REG_SIOMLT_SEND >> 1) as usize];
            }
            SioMode::Normal8 => {
                // SIODATA8 shares its address with SIOMLT_SEND (0x12A)
                self.normal_data[id] = gba.memory.io[(GBA_REG_SIOMLT_SEND >> 1) as usize] as u32;
            }
            SioMode::Normal32 => {
                self.normal_data[id] = gba.memory.io[(GBA_REG_SIODATA32_LO >> 1) as usize] as u32;
                self.normal_data[id] |=
                    (gba.memory.io[(GBA_REG_SIODATA32_HI >> 1) as usize] as u32) << 16;
            }
            _ => {
                mlog!(
                    Level::Warn,
                    rgba_core::log::GBA_SIO,
                    "Unsupported mode {} in lockstep",
                    self.transfer_mode.to_raw()
                );
            }
        }
    }

    /// _setReady (the siocnt/rcnt pokes hit the *active* player's own Gba)
    fn set_ready(&mut self, gba: &mut Gba, active_id: u32, player_id: i32, mode: SioMode) {
        debug_assert!(player_id >= 0 && (player_id as usize) < MAX_GBAS);
        if player_id < 0 || player_id as usize >= MAX_GBAS {
            return;
        }
        let Some(active) = self.players.get_mut(&active_id) else {
            return;
        };
        active.other_modes[player_id as usize] = mode;
        let mut ready = true;
        for i in 0..self.n_attached {
            if active.other_modes[i] != active.mode {
                ready = false;
                break;
            }
        }
        if active.mode == SioMode::Multi {
            gba.sio.siocnt = multiplayer_set_ready(gba.sio.siocnt, ready);
            gba.sio.rcnt = rcnt_set_sd(gba.sio.rcnt, ready);
        }
    }

    /// _advanceCycle
    fn advance_cycle(&mut self, active_id: u32, now: i32) {
        let player = &self.players[&active_id];
        let new_cycle = now.wrapping_sub(player.cycle_offset);
        debug_assert!(new_cycle.wrapping_sub(self.cycle) >= 0);
        self.next_hard_sync = self
            .next_hard_sync
            .wrapping_sub(new_cycle.wrapping_sub(self.cycle));
        self.cycle = new_cycle;
    }

    /// _untilNextSync
    fn until_next_sync(&self, active_id: u32, now: i32) -> i32 {
        let player = &self.players[&active_id];
        let time = now.wrapping_sub(player.cycle_offset);
        let cycle = self.cycle.wrapping_sub(time);
        if player.player_id == 0 {
            if self.n_attached < 2 {
                cycle.wrapping_add(UNLOCKED_INTERVAL)
            } else {
                cycle.wrapping_add(LOCKSTEP_INTERVAL)
            }
        } else {
            cycle
        }
    }

    /// GBASIOLockstepCoordinatorWaitOnPlayers
    fn wait_on_players(&mut self, gba: &mut Gba, timing: &mut Timing, active_id: u32) {
        let player_id = self.players[&active_id].player_id;
        // mASSERT_LOG checks ("Multiplayer desynchronized: ...")
        if self.waiting != 0 || self.players[&active_id].asleep || player_id != 0 {
            mlog!(
                Level::Error,
                rgba_core::log::GBA_SIO,
                "Multiplayer desynchronized: {} (player {})",
                if self.waiting != 0 {
                    "coordinator still waiting"
                } else if self.players[&active_id].asleep {
                    "player asleep"
                } else {
                    "invalid player attempting to coordinate"
                },
                player_id
            );
        }
        if self.n_attached < 2 {
            return;
        }

        let now = timing.current_time();
        self.advance_cycle(active_id, now);
        mlog!(
            Level::Debug,
            rgba_core::log::GBA_SIO,
            "Primary waiting for players to ack"
        );
        self.waiting = ((1u32 << self.n_attached) - 1) & !target(player_id);
        self.player_sleep(gba, timing, active_id);
        self.wake_players();

        self.verify_awake();
    }

    /// GBASIOLockstepCoordinatorWakePlayers
    fn wake_players(&mut self) {
        for i in 1..self.n_attached {
            let pid = self.attached_players[i];
            if pid == 0 {
                continue;
            }
            self.player_wake(pid);
        }
    }

    /// GBASIOLockstepPlayerWake — the C also resumes the player's thread via
    /// user->wake; in the cooperative model the flag is all there is.
    fn player_wake(&mut self, lockstep_id: u32) {
        let Some(player) = self.players.get_mut(&lockstep_id) else {
            return;
        };
        if !player.asleep {
            return;
        }
        player.asleep = false;
    }

    /// GBASIOLockstepCoordinatorAckPlayer
    fn ack_player(&mut self, gba: &mut Gba, timing: &mut Timing, active_id: u32) {
        let player_id = self.players[&active_id].player_id;
        if player_id == 0 {
            return;
        }
        self.waiting &= !target(player_id);
        if self.waiting == 0 {
            mlog!(
                Level::Debug,
                rgba_core::log::GBA_SIO,
                "All players acked, waking primary"
            );
            if self.transfer_active {
                for i in 0..self.n_attached {
                    let pid = self.attached_players[i];
                    if pid == 0 {
                        continue;
                    }
                    if let Some(player) = self.players.get_mut(&pid) {
                        player.data_received = true;
                    }
                }
                self.transfer_active = false;
            }
            let runner = self.attached_players[0];
            if runner != 0 {
                self.player_wake(runner);
            }
        }
        self.player_sleep(gba, timing, active_id);
    }

    /// GBASIOLockstepPlayerSleep
    fn player_sleep(&mut self, gba: &mut Gba, timing: &mut Timing, lockstep_id: u32) {
        let Some(player) = self.players.get_mut(&lockstep_id) else {
            return;
        };
        if player.asleep {
            return;
        }
        player.asleep = true;
        // C: user->sleep (thread block — no-op here), then nudge the CPU:
        gba.cpu.next_event = 0;
        // GBAInterrupt(gba)
        gba.early_exit = true;
        timing.interrupt();
    }

    /// _hardSync
    fn hard_sync(&mut self, gba: &mut Gba, timing: &mut Timing, active_id: u32) {
        debug_assert_eq!(self.players[&active_id].player_id, 0);
        let event = GbaSioLockstepEvent {
            event_type: GbaSioLockstepEventType::HardSync,
            player_id: 0,
            timestamp: self.player_time(active_id),
            payload: 0,
        };
        self.enqueue_event(&event, TARGET_SECONDARY);
        self.wait_on_players(gba, timing, active_id);
    }
}

/// struct GBASIOLockstepDriver, owned by its `Gba` inside
/// `SioDriver::Lockstep`. The C's `mTimingEvent event` slot becomes
/// `EventId::SioLockstep` on the owning Gba.
pub struct GbaSioLockstepNode {
    coordinator: Weak<RefCell<GbaSioLockstep>>,
    lockstep_id: u32,
    /// mLockstepUser.requestedId as a plain field (set before install).
    pub requested_id: i32,
}

fn event_type_from_u32(v: u32) -> GbaSioLockstepEventType {
    match v {
        1 => GbaSioLockstepEventType::Detach,
        2 => GbaSioLockstepEventType::HardSync,
        3 => GbaSioLockstepEventType::ModeSet,
        4 => GbaSioLockstepEventType::TransferStart,
        _ => GbaSioLockstepEventType::Attach,
    }
}

// _modeEnumToInt / _modeIntToEnum (3-bit serialized mode encoding)
fn mode_enum_to_int(mode: SioMode) -> u32 {
    match mode {
        SioMode::Multi => 1,
        SioMode::Normal8 => 2,
        SioMode::Normal32 => 3,
        SioMode::Gpio => 4,
        SioMode::Uart => 5,
        SioMode::JoyBus => 6,
        SioMode::Invalid => 0,
    }
}

fn mode_int_to_enum(mode: u32) -> SioMode {
    const MODES: [SioMode; 8] = [
        SioMode::Invalid,
        SioMode::Multi,
        SioMode::Normal8,
        SioMode::Normal32,
        SioMode::Gpio,
        SioMode::Uart,
        SioMode::JoyBus,
        SioMode::Invalid,
    ];
    MODES[(mode & 7) as usize]
}

fn get_u32(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

fn put_u32(out: &mut [u8], off: usize, v: u32) {
    out[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_u16(out: &mut [u8], off: usize, v: u16) {
    out[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

// GBASIOLockstepSerializedFlags bit accessors (flag field is u32)
fn flags_get(flags: u32, start: u32, n: u32) -> u32 {
    (flags >> start) & ((1 << n) - 1)
}
fn flags_set(flags: u32, start: u32, n: u32, v: u32) -> u32 {
    (flags & !(((1 << n) - 1) << start)) | ((v & ((1 << n) - 1)) << start)
}

/// sizeof(struct GBASIOLockstepSerializedState)
pub const SIO_LOCKSTEP_SERIALIZED_SIZE: usize = 0x1F0;

impl GbaSioLockstepNode {
    /// Builder for the C's user->requestedId hook before install.
    pub fn with_requested_id(mut self, id: i32) -> Self {
        self.requested_id = id;
        self
    }

    /// GBASIOLockstepDriverDeviceId; None when the node has no coordinator.
    pub fn player_id(&self) -> Option<i32> {
        let rc = self.coordinator.upgrade()?;
        let ls = rc.borrow();
        let player = ls.players.get(&self.lockstep_id)?;
        Some(player.player_id.max(0))
    }

    /// Shared coordinator state accessor for tests/frontends.
    pub fn coordinator(&self) -> Option<Rc<RefCell<GbaSioLockstep>>> {
        self.coordinator.upgrade()
    }

    // -- GBASIODriver vtable methods ----------------------------------------

    /// GBASIOLockstepDriverInit
    pub(crate) fn init(&mut self, gba: &mut Gba) -> bool {
        gba.with_timing(|g, t| self.reset_impl(g, t));
        true
    }

    /// GBASIOLockstepDriverDeinit (also GBASIOLockstepCoordinatorDetach's
    /// player-removal half; the frontend's detach is `Gba::sio_detach_lockstep`).
    pub(crate) fn deinit(&mut self, gba: &mut Gba) {
        gba.with_timing(|_g, t| {
            if let Some(rc) = self.coordinator.upgrade() {
                let mut guard = rc.borrow_mut();
                let ls = &mut *guard;
                if ls.players.contains_key(&self.lockstep_id) {
                    let now = t.current_time();
                    ls.remove_player(self.lockstep_id, now);
                }
            }
            t.deschedule(EventId::SioLockstep.into());
            self.lockstep_id = 0;
        });
    }

    /// GBASIOLockstepDriverReset
    pub(crate) fn reset(&mut self, gba: &mut Gba) {
        gba.with_timing(|g, t| self.reset_impl(g, t));
    }

    fn reset_impl(&mut self, gba: &mut Gba, timing: &mut Timing) {
        let Some(rc) = self.coordinator.upgrade() else {
            return;
        };
        let mut guard = rc.borrow_mut();
        let ls = &mut *guard;
        let now = timing.current_time();
        if self.lockstep_id == 0 {
            // First reset on this node: allocate a player.
            let player = GbaSioLockstepPlayer {
                player_id: -1,
                mode: gba.sio.mode,
                other_modes: [SioMode::Invalid; MAX_GBAS],
                asleep: false,
                cycle_offset: 0,
                queue: Vec::new(),
                data_received: false,
                requested_id: self.requested_id,
                proxy_time: now,
            };
            let id = loop {
                if ls.next_id == u32::MAX {
                    ls.next_id = 0;
                }
                ls.next_id = ls.next_id.wrapping_add(1);
                let id = ls.next_id;
                if !ls.players.contains_key(&id) {
                    self.lockstep_id = id;
                    break id;
                }
            };
            ls.players.insert(id, player);
            ls.reconfig_players();
            let (player_id, timestamp) = {
                let player = ls.players.get_mut(&id).expect("just inserted");
                player.cycle_offset = now.wrapping_sub(ls.cycle);
                (player.player_id, now.wrapping_sub(player.cycle_offset))
            };
            if player_id != 0 {
                let event = GbaSioLockstepEvent {
                    event_type: GbaSioLockstepEventType::Attach,
                    player_id,
                    timestamp,
                    payload: 0,
                };
                ls.enqueue_event(&event, TARGET_ALL & !target(player_id));
            }
        } else {
            let Some(player) = ls.players.get_mut(&self.lockstep_id) else {
                return;
            };
            player.cycle_offset = now.wrapping_sub(ls.cycle);
            player.proxy_time = now;
        }

        let player_id = ls.players[&self.lockstep_id].player_id;
        if ls.transfer_active {
            ls.abort_transfer(self.lockstep_id);
            if let Some(player) = ls.players.get_mut(&self.lockstep_id) {
                player.asleep = false;
            }
        }
        if player_id == 0 && ls.n_attached > 1 {
            ls.waiting = 0;
            // We will immediately go back to sleep when the initial mode gets
            // set, so we need to clear this here to avoid an assert later.
            if let Some(player) = ls.players.get_mut(&self.lockstep_id) {
                player.asleep = false;
            }
            ls.wake_players();
        }

        if timing.is_scheduled(EventId::SioLockstep.into()) {
            return;
        }

        let player_mode = ls.players[&self.lockstep_id].mode;
        ls.set_ready(gba, self.lockstep_id, player_id, player_mode);
        let next_event = if ls.players.len() == 1 {
            ls.cycle = now;
            LOCKSTEP_INTERVAL
        } else {
            let transfer_mode = ls.transfer_mode;
            ls.set_ready(gba, self.lockstep_id, 0, transfer_mode);
            ls.until_next_sync(self.lockstep_id, now)
        };
        drop(guard);
        timing.schedule(
            EventId::SioLockstep.into(),
            EventId::SioLockstep.priority(),
            next_event,
        );
    }

    /// GBASIOLockstepDriverSetMode
    pub(crate) fn set_mode(&mut self, gba: &mut Gba, mode: SioMode) {
        gba.with_timing(|g, t| {
            let Some(rc) = self.coordinator.upgrade() else {
                return;
            };
            let mut guard = rc.borrow_mut();
            let ls = &mut *guard;
            let now = t.current_time();
            let Some(player) = ls.players.get_mut(&self.lockstep_id) else {
                return;
            };
            player.proxy_time = now;
            if mode == player.mode {
                return;
            }
            mlog!(
                Level::Debug,
                rgba_core::log::GBA_SIO,
                "Switching mode from {} to {}",
                player.mode.to_raw(),
                mode.to_raw()
            );
            player.mode = mode;
            let player_id = player.player_id;
            let event = GbaSioLockstepEvent {
                event_type: GbaSioLockstepEventType::ModeSet,
                player_id,
                timestamp: now.wrapping_sub(player.cycle_offset),
                payload: mode.to_raw(),
            };
            if player_id == 0 {
                ls.transfer_mode = mode;
                ls.wait_on_players(g, t, self.lockstep_id);
            }
            ls.set_ready(g, self.lockstep_id, player_id, mode);
            ls.enqueue_event(&event, TARGET_ALL & !target(player_id));
        });
    }

    /// GBASIOLockstepDriverHandlesMode
    pub(crate) fn handles_mode(&self, _mode: SioMode) -> bool {
        true
    }

    /// GBASIOLockstepDriverConnectedDevices
    pub(crate) fn connected_devices(&self) -> i32 {
        if self.lockstep_id == 0 {
            return 0;
        }
        let Some(rc) = self.coordinator.upgrade() else {
            return 0;
        };
        let ls = rc.borrow();
        ls.n_attached as i32 - 1
    }

    /// GBASIOLockstepDriverDeviceId
    pub(crate) fn device_id(&self) -> i32 {
        self.player_id().unwrap_or(0)
    }

    /// GBASIOLockstepDriverWriteSIOCNT
    pub(crate) fn write_siocnt(&mut self, value: u16) -> u16 {
        mlog!(
            Level::Debug,
            rgba_core::log::GBA_SIO,
            "Lockstep: SIOCNT <- {:04X}",
            value
        );
        value
    }

    /// GBASIOLockstepDriverWriteRCNT
    pub(crate) fn write_rcnt(&mut self, value: u16) -> u16 {
        mlog!(
            Level::Debug,
            rgba_core::log::GBA_SIO,
            "Lockstep: RCNT <- {:04X}",
            value
        );
        value
    }

    /// GBASIOLockstepDriverStart
    pub(crate) fn start(&mut self, gba: &mut Gba) -> bool {
        gba.with_timing(|g, t| {
            let Some(rc) = self.coordinator.upgrade() else {
                return false;
            };
            let mut guard = rc.borrow_mut();
            let ls = &mut *guard;
            let now = t.current_time();
            if ls.transfer_active {
                mlog!(
                    Level::GameError,
                    rgba_core::log::GBA_SIO,
                    "Transfer restarted unexpectedly"
                );
                return false;
            }
            if ls.n_attached < 2 {
                mlog!(
                    Level::Debug,
                    rgba_core::log::GBA_SIO,
                    "Attempted to start transfer with no secondary players"
                );
                return false;
            }
            let Some(player) = ls.players.get_mut(&self.lockstep_id) else {
                return false;
            };
            player.proxy_time = now;
            if player.player_id != 0 {
                mlog!(
                    Level::Debug,
                    rgba_core::log::GBA_SIO,
                    "Secondary player attempted to start transfer"
                );
                return false;
            }
            mlog!(
                Level::Debug,
                rgba_core::log::GBA_SIO,
                "Transfer starting at {:08X}",
                ls.cycle
            );
            ls.multi_data = [0xFFFF; 4];
            ls.set_data(g, 0);

            let timestamp = now.wrapping_sub(ls.players[&self.lockstep_id].cycle_offset);
            let transfer_cycles = g.sio_transfer_cycles(
                ls.players[&self.lockstep_id].mode,
                g.sio.siocnt,
                ls.n_attached - 1,
            );
            let event = GbaSioLockstepEvent {
                event_type: GbaSioLockstepEventType::TransferStart,
                timestamp,
                player_id: 0,
                payload: timestamp.wrapping_add(transfer_cycles),
            };
            ls.enqueue_event(&event, TARGET_SECONDARY);
            ls.wait_on_players(g, t, self.lockstep_id);
            ls.transfer_active = true;
            true
        })
    }

    /// GBASIOLockstepDriverFinishMultiplayer
    pub(crate) fn finish_multiplayer(
        &mut self,
        gba: &mut Gba,
        timing: &mut Timing,
        data: &mut [u16; 4],
    ) {
        let Some(rc) = self.coordinator.upgrade() else {
            return;
        };
        let mut guard = rc.borrow_mut();
        let ls = &mut *guard;
        if ls.transfer_mode != SioMode::Multi {
            return;
        }
        let now = timing.current_time();
        let Some(player) = ls.players.get_mut(&self.lockstep_id) else {
            return;
        };
        player.proxy_time = now;
        if !player.data_received {
            mlog!(
                Level::Warn,
                rgba_core::log::GBA_SIO,
                "MULTI did not receive data. Are we running behind?"
            );
            *data = [0xFFFF; 4];
        } else {
            mlog!(
                Level::Debug,
                rgba_core::log::GBA_SIO,
                "MULTI transfer finished: {:04X} {:04X} {:04X} {:04X}",
                ls.multi_data[0],
                ls.multi_data[1],
                ls.multi_data[2],
                ls.multi_data[3]
            );
            *data = ls.multi_data;
        }
        player.data_received = false;
        let player_id = player.player_id;
        if player_id == 0 {
            ls.hard_sync(gba, timing, self.lockstep_id);
        }
    }

    /// GBASIOLockstepDriverFinishNormal8
    pub(crate) fn finish_normal8(&mut self, gba: &mut Gba, timing: &mut Timing) -> u8 {
        let mut data = 0xFF;
        let Some(rc) = self.coordinator.upgrade() else {
            return data;
        };
        let mut guard = rc.borrow_mut();
        let ls = &mut *guard;
        if ls.transfer_mode == SioMode::Normal8 {
            let now = timing.current_time();
            if let Some(player) = ls.players.get_mut(&self.lockstep_id) {
                player.proxy_time = now;
                if player.player_id > 0 {
                    if !player.data_received {
                        mlog!(
                            Level::Warn,
                            rgba_core::log::GBA_SIO,
                            "NORMAL did not receive data. Are we running behind?"
                        );
                    } else {
                        data = ls.normal_data[player.player_id as usize - 1] as u8;
                        mlog!(
                            Level::Debug,
                            rgba_core::log::GBA_SIO,
                            "NORMAL8 transfer finished: {:02X}",
                            data
                        );
                    }
                }
                player.data_received = false;
                let player_id = player.player_id;
                if player_id == 0 {
                    ls.hard_sync(gba, timing, self.lockstep_id);
                }
            }
        }
        data
    }

    /// GBASIOLockstepDriverFinishNormal32
    pub(crate) fn finish_normal32(&mut self, gba: &mut Gba, timing: &mut Timing) -> u32 {
        let mut data = 0xFFFFFFFF;
        let Some(rc) = self.coordinator.upgrade() else {
            return data;
        };
        let mut guard = rc.borrow_mut();
        let ls = &mut *guard;
        if ls.transfer_mode == SioMode::Normal32 {
            let now = timing.current_time();
            if let Some(player) = ls.players.get_mut(&self.lockstep_id) {
                player.proxy_time = now;
                if player.player_id > 0 {
                    if !player.data_received {
                        mlog!(
                            Level::Warn,
                            rgba_core::log::GBA_SIO,
                            "Did not receive data. Are we running behind?"
                        );
                    } else {
                        data = ls.normal_data[player.player_id as usize - 1];
                        mlog!(
                            Level::Debug,
                            rgba_core::log::GBA_SIO,
                            "NORMAL32 transfer finished: {:08X}",
                            data
                        );
                    }
                }
                player.data_received = false;
                let player_id = player.player_id;
                if player_id == 0 {
                    ls.hard_sync(gba, timing, self.lockstep_id);
                }
            }
        }
        data
    }

    // -- savestate (GBASIOLockstepDriverSaveState / LoadState) --------------

    /// GBASIOLockstepDriverSaveState: the 0x1F0-byte
    /// GBASIOLockstepSerializedState payload (without core.c's DRIVER_ID tag).
    pub(crate) fn save_state(&mut self, gba: &mut Gba) -> Option<Vec<u8>> {
        let rc = self.coordinator.upgrade()?;
        let mut out = vec![0u8; SIO_LOCKSTEP_SERIALIZED_SIZE];

        put_u32(&mut out, 0x00, DRIVER_STATE_VERSION);

        // driver.nextEvent: C stores event.when - mTimingCurrentTime. Here
        // the descheduled event serializes as i32::MAX (same convention as
        // hw.sioNextEvent in serialize.rs); the EventScheduled flag guards it.
        let next_event = gba.until(EventId::SioLockstep);
        put_u32(&mut out, 0x10, next_event as u32);

        let ls = rc.borrow();
        let player = ls.players.get(&self.lockstep_id)?;

        let mut flags: u32 = 0;
        put_u32(&mut out, 0x30, player.player_id as u32);
        put_u32(&mut out, 0x34, player.cycle_offset as u32);
        flags = flags_set(flags, 7, 1, player.asleep as u32);
        flags = flags_set(flags, 8, 1, player.data_received as u32);
        flags = flags_set(flags, 0, 3, mode_enum_to_int(player.mode));
        let scheduled = gba.is_scheduled(EventId::SioLockstep);
        flags = flags_set(flags, 9, 1, scheduled as u32);

        for i in 0..MAX_GBAS {
            flags = flags_set(
                flags,
                10 + 3 * i as u32,
                3,
                mode_enum_to_int(player.other_modes[i]),
            );
        }

        let num_events = player.queue.len().min(MAX_LOCKSTEP_EVENTS);
        for (i, event) in player.queue.iter().take(MAX_LOCKSTEP_EVENTS).enumerate() {
            let base = 0x40 + i * 0x30;
            let eflags = event.event_type as u32; // Type at bits 0..3
            put_u32(&mut out, base, event.timestamp as u32);
            put_u32(&mut out, base + 4, event.player_id as u32);
            match event.event_type {
                GbaSioLockstepEventType::ModeSet | GbaSioLockstepEventType::TransferStart => {
                    put_u32(&mut out, base + 0x20, event.payload as u32);
                }
                _ => {}
            }
            put_u32(&mut out, base + 8, eflags);
        }
        flags = flags_set(flags, 3, 4, num_events as u32);

        if player.player_id == 0 {
            put_u32(&mut out, 0x1C0, ls.cycle as u32);
            put_u32(&mut out, 0x1C4, ls.waiting);
            put_u32(&mut out, 0x1C8, ls.next_hard_sync as u32);
            for i in 0..4 {
                put_u16(&mut out, 0x1D8 + i * 2, ls.multi_data[i]);
                put_u32(&mut out, 0x1E0 + i * 4, ls.normal_data[i]);
            }
            flags = flags_set(flags, 28, 3, mode_enum_to_int(ls.transfer_mode));
            flags = flags_set(flags, 31, 1, ls.transfer_active as u32);
        }
        drop(ls);

        put_u32(&mut out, 0x04, flags);
        Some(out)
    }

    /// GBASIOLockstepDriverLoadState
    pub(crate) fn load_state(&mut self, gba: &mut Gba, data: &[u8]) -> bool {
        if data.len() != SIO_LOCKSTEP_SERIALIZED_SIZE {
            mlog!(
                Level::Warn,
                rgba_core::log::GBA_SIO,
                "Incorrect state size: expected {:X}, got {:X}",
                SIO_LOCKSTEP_SERIALIZED_SIZE,
                data.len()
            );
            return false;
        }
        let version = get_u32(data, 0x00);
        if version > DRIVER_STATE_VERSION {
            mlog!(
                Level::Warn,
                rgba_core::log::GBA_SIO,
                "Invalid or too new save state: expected {}, got {}",
                DRIVER_STATE_VERSION,
                version
            );
            return false;
        }
        let Some(rc) = self.coordinator.upgrade() else {
            return false;
        };
        let mut guard = rc.borrow_mut();
        let ls = &mut *guard;

        let check = get_u32(data, 0x30) as i32;
        let Some(player) = ls.players.get_mut(&self.lockstep_id) else {
            mlog!(
                Level::Warn,
                rgba_core::log::GBA_SIO,
                "State is for an unknown player"
            );
            return false;
        };
        if check != player.player_id {
            mlog!(
                Level::Warn,
                rgba_core::log::GBA_SIO,
                "State is for different player: expected {}, got {}",
                player.player_id,
                check
            );
            return false;
        }

        let flags = get_u32(data, 0x04);
        player.cycle_offset = get_u32(data, 0x34) as i32;
        player.data_received = flags_get(flags, 8, 1) != 0;
        player.mode = mode_int_to_enum(flags_get(flags, 0, 3));
        for i in 0..MAX_GBAS {
            player.other_modes[i] = mode_int_to_enum(flags_get(flags, 10 + 3 * i as u32, 3));
        }
        player.proxy_time = gba.current_time();

        if flags_get(flags, 9, 1) != 0 {
            let when = get_u32(data, 0x10) as i32;
            gba.schedule(EventId::SioLockstep, when);
        }

        // The C's user->sleep/wake on asleep transitions are thread ops; in
        // the cooperative model only the flag matters.
        player.asleep = flags_get(flags, 7, 1) != 0;

        player.queue.clear();
        let num_events = flags_get(flags, 3, 4).min(MAX_LOCKSTEP_EVENTS as u32);
        for i in 0..num_events as usize {
            let base = 0x40 + i * 0x30;
            let eflags = get_u32(data, base + 8);
            let event_type = event_type_from_u32(flags_get(eflags, 0, 3));
            let payload = match event_type {
                GbaSioLockstepEventType::ModeSet | GbaSioLockstepEventType::TransferStart => {
                    get_u32(data, base + 0x20) as i32
                }
                _ => 0,
            };
            player.queue.push(GbaSioLockstepEvent {
                event_type,
                timestamp: get_u32(data, base) as i32,
                player_id: get_u32(data, base + 4) as i32,
                payload,
            });
        }

        if player.player_id == 0 {
            ls.cycle = get_u32(data, 0x1C0) as i32;
            ls.waiting = get_u32(data, 0x1C4);
            ls.next_hard_sync = get_u32(data, 0x1C8) as i32;
            for i in 0..4 {
                ls.multi_data[i] =
                    u16::from_le_bytes([data[0x1D8 + i * 2], data[0x1D8 + i * 2 + 1]]);
                ls.normal_data[i] = get_u32(data, 0x1E0 + i * 4);
            }
            ls.transfer_mode = mode_int_to_enum(flags_get(flags, 28, 3));
            ls.transfer_active = flags_get(flags, 31, 1) != 0;
        }
        drop(guard);
        // mTimingInterrupt
        gba.timing.interrupt();
        true
    }

    // -- _lockstepEvent (the priority-0x80 timing callback) ------------------

    /// _lockstepEvent: process this node's event queue.
    fn process_event(&mut self, gba: &mut Gba, timing: &mut Timing, cycles_late: u32) {
        let Some(rc) = self.coordinator.upgrade() else {
            return;
        };
        let mut guard = rc.borrow_mut();
        let ls = &mut *guard;
        let now = timing.current_time();
        {
            let Some(player) = ls.players.get_mut(&self.lockstep_id) else {
                return;
            };
            player.proxy_time = now;
            if !(0..4).contains(&player.player_id) {
                mlog!(
                    Level::Error,
                    rgba_core::log::GBA_SIO,
                    "Invalid multiplayer ID {}",
                    player.player_id
                );
            }
        }
        let player_id = ls.players[&self.lockstep_id].player_id;
        let time = now.wrapping_sub(ls.players[&self.lockstep_id].cycle_offset);

        let mut was_detach = false;
        if let Some(head) = ls.players[&self.lockstep_id].queue.first() {
            if head.event_type == GbaSioLockstepEventType::Detach {
                mlog!(
                    Level::Debug,
                    rgba_core::log::GBA_SIO,
                    "Player {} detached at timestamp {:X}, picking up the pieces",
                    head.player_id,
                    head.timestamp
                );
                was_detach = true;
            }
        }
        if player_id == 0 && time.wrapping_sub(ls.cycle) >= 0 {
            // We are the clock owner; advance the shared clock.
            ls.advance_cycle(self.lockstep_id, now);
            if !ls.transfer_active {
                ls.wake_players();
            }
            if ls.next_hard_sync < 0 {
                if ls.waiting == 0 {
                    ls.hard_sync(gba, timing, self.lockstep_id);
                }
                ls.next_hard_sync = ls.next_hard_sync.wrapping_add(HARD_SYNC_INTERVAL);
            }
        }

        let mut next_event = ls.until_next_sync(self.lockstep_id, now);
        loop {
            let Some(player) = ls.players.get(&self.lockstep_id) else {
                break;
            };
            let Some(&event) = player.queue.first() else {
                break;
            };
            if event.timestamp.wrapping_sub(time) > 0 {
                break;
            }
            ls.players
                .get_mut(&self.lockstep_id)
                .expect("player")
                .queue
                .remove(0);
            let mut reply = GbaSioLockstepEvent {
                event_type: GbaSioLockstepEventType::Attach,
                player_id,
                timestamp: time,
                payload: 0,
            };
            mlog!(
                Level::Debug,
                rgba_core::log::GBA_SIO,
                "Got event of type {:X} from {} at timestamp {:X}",
                event.event_type as u32,
                event.player_id,
                event.timestamp
            );
            match event.event_type {
                GbaSioLockstepEventType::Attach => {
                    ls.set_ready(gba, self.lockstep_id, event.player_id, SioMode::Invalid);
                    if player_id == 0 {
                        gba.sio.siocnt = multiplayer_clear_slave(gba.sio.siocnt);
                    }
                    reply.payload = ls.players[&self.lockstep_id].mode.to_raw();
                    reply.event_type = GbaSioLockstepEventType::ModeSet;
                    ls.enqueue_event(&reply, target(event.player_id));
                }
                GbaSioLockstepEventType::HardSync => {
                    ls.ack_player(gba, timing, self.lockstep_id);
                }
                GbaSioLockstepEventType::TransferStart => {
                    ls.set_data(gba, player_id);
                    next_event = event
                        .payload
                        .wrapping_sub(time)
                        .wrapping_sub(cycles_late as i32);
                    gba.sio.siocnt |= 0x80;
                    timing.deschedule(EventId::Sio.into());
                    timing.schedule(EventId::Sio.into(), EventId::Sio.priority(), next_event);
                    ls.ack_player(gba, timing, self.lockstep_id);
                }
                GbaSioLockstepEventType::ModeSet => {
                    let event_mode = SioMode::from_raw(event.payload);
                    if ls.transfer_active && ls.players[&self.lockstep_id].mode != event_mode {
                        mlog!(
                            Level::Debug,
                            rgba_core::log::GBA_SIO,
                            "Switching modes while transfer is active"
                        );
                        ls.abort_transfer(self.lockstep_id);
                    }
                    ls.set_ready(gba, self.lockstep_id, event.player_id, event_mode);
                    if event.player_id == 0 {
                        ls.ack_player(gba, timing, self.lockstep_id);
                    }
                }
                GbaSioLockstepEventType::Detach => {
                    ls.set_ready(gba, self.lockstep_id, event.player_id, SioMode::Invalid);
                    let my_mode = ls.players[&self.lockstep_id].mode;
                    ls.set_ready(gba, self.lockstep_id, player_id, my_mode);
                    reply.payload = my_mode.to_raw();
                    reply.event_type = GbaSioLockstepEventType::ModeSet;
                    ls.enqueue_event(&reply, !target(event.player_id));
                    if my_mode == SioMode::Multi {
                        gba.sio.siocnt = multiplayer_set_id(gba.sio.siocnt, player_id);
                        gba.sio.siocnt = multiplayer_set_slave(
                            gba.sio.siocnt,
                            player_id != 0 || ls.n_attached < 2,
                        );
                    }
                    was_detach = true;
                }
            }
            // The C returns the event to the freelist here.
        }
        if let Some(head) = ls.players[&self.lockstep_id].queue.first() {
            let until = head.timestamp.wrapping_sub(time);
            if until < next_event {
                next_event = until;
            }
        }

        let queue_empty = ls.players[&self.lockstep_id].queue.is_empty();
        if player_id != 0 && next_event <= LOCKSTEP_INTERVAL {
            if queue_empty || was_detach {
                ls.player_sleep(gba, timing, self.lockstep_id);
                // XXX: Is there a better way to gain sync lock at the beginning?
                if next_event < 4 {
                    next_event = 4;
                }
                ls.verify_awake();
            }
        }
        drop(guard);

        debug_assert!(next_event > 0);
        timing.schedule(
            EventId::SioLockstep.into(),
            EventId::SioLockstep.priority(),
            next_event,
        );
    }
}

impl Gba {
    /// Dispatch entry for the lockstep node event (`_lockstepEvent`).
    pub fn sio_lockstep_event(&mut self, timing: &mut Timing, cycles_late: u32) {
        let driver = std::mem::take(&mut self.sio.driver);
        let SioDriver::Lockstep(mut node) = driver else {
            self.sio.driver = driver;
            return;
        };
        node.process_event(self, timing, cycles_late);
        self.sio.driver = SioDriver::Lockstep(node);
    }

    /// Convenience: GBASIOLockstepCoordinatorAttach + GBASIOSetDriver on this
    /// core (the Qt frontend's attachGame order).
    pub fn sio_attach_lockstep(&mut self, coordinator: &Rc<RefCell<GbaSioLockstep>>) {
        let node = GbaSioLockstep::attach_node(coordinator);
        self.sio_set_driver(Some(SioDriver::Lockstep(node)));
    }

    /// Detach any lockstep driver (GBASIOSetDriver(sio, NULL); the node
    /// deinit performs GBASIOLockstepCoordinatorDetach's player removal).
    pub fn sio_detach_lockstep(&mut self) {
        self.sio_set_driver(None);
    }

    /// Query the installed lockstep node, if any.
    pub fn sio_lockstep_player_id(&self) -> Option<i32> {
        match &self.sio.driver {
            SioDriver::Lockstep(node) => node.player_id(),
            SioDriver::None | SioDriver::Gbp | SioDriver::Dolphin => None,
        }
    }

    /// _GBACoreSaveExtraState, SIO-driver half: DRIVER_ID tag +
    /// GBASIOLockstepSerializedState for the installed lockstep node.
    pub fn sio_save_extra_state(&mut self) -> Option<Vec<u8>> {
        let driver = std::mem::take(&mut self.sio.driver);
        let SioDriver::Lockstep(mut node) = driver else {
            self.sio.driver = driver;
            return None;
        };
        let out = node.save_state(self).map(|payload| {
            let mut v = Vec::with_capacity(payload.len() + 4);
            v.extend_from_slice(&SIO_LOCKSTEP_DRIVER_ID.to_le_bytes());
            v.extend_from_slice(&payload);
            v
        });
        self.sio.driver = SioDriver::Lockstep(node);
        out
    }

    /// _GBACoreLoadExtraState, SIO-driver half. State for a different driver
    /// type is ignored (as in the C); a driver-id match loads the payload.
    pub fn sio_load_extra_state(&mut self, data: &[u8]) -> bool {
        let driver = std::mem::take(&mut self.sio.driver);
        let SioDriver::Lockstep(mut node) = driver else {
            // No driver: the C skips the extdata item entirely.
            self.sio.driver = driver;
            return true;
        };
        let mut ok = true;
        if data.len() > 4 {
            let id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            if id == SIO_LOCKSTEP_DRIVER_ID {
                ok = node.load_state(self, &data[4..]);
            }
        } else if !data.is_empty() {
            ok = false;
        }
        self.sio.driver = SioDriver::Lockstep(node);
        ok
    }
}
