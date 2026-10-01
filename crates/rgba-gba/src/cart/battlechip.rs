// Copyright (c) 2013-2018 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gba/extra/battlechip.c (in this 0.11.0-dev snapshot
// struct GBASIOBattlechipGate and enum GBASIOBattleChipGateFlavor live in
// include/mgba/gba/interface.h, not a separate extra/battlechip.h).
//
// In the C, the gate is a GBASIODriver installed on the link port via
// mCore::setPeripheral(mPERIPH_GBA_LINK_PORT) (see Qt's
// CoreController::attachBattleChipGate) — it is a serial peripheral, not a
// HW_* cartridge device, so there is no HW_BATTLECHIP_GATE bit. Here the
// vtable is folded into `sio::SioDriver::Battlechip`:
//
//   C vtable entry          → Rust
//   init                    → BattleChipGate::init
//   writeSIOCNT             → BattleChipGate::write_siocnt
//   handlesMode             → BattleChipGate::handles_mode
//   connectedDevices        → BattleChipGate::connected_devices
//   finishMultiplayer       → BattleChipGate::finish_multiplayer
//   (unset hooks: deinit/reset/setMode/deviceId/writeRCNT/start/
//    finishNormal8/finishNormal32 — see the SioDriver::Battlechip arms in
//    sio/mod.rs, which take the same "no hook" path as the C null checks)

use rgba_core::{mlog, Level};

use crate::gba::Gba;
use crate::io::GBA_REG_SIOMLT_SEND;
use crate::sio::{SioDriver, SioMode};

// State machine states (battlechip.c enum). SYNC intentionally counts
// down while the game has not sent the 0x8FFF handshake, exactly like the
// C's `--gate->state`.
const BATTLECHIP_STATE_SYNC: i32 = -1;
const BATTLECHIP_STATE_COMMAND: i32 = 0;
const BATTLECHIP_STATE_UNK_0: i32 = 1;
const BATTLECHIP_STATE_UNK_1: i32 = 2;
const BATTLECHIP_STATE_DATA_0: i32 = 3;
const BATTLECHIP_STATE_DATA_1: i32 = 4;
const BATTLECHIP_STATE_ID: i32 = 5;
const BATTLECHIP_STATE_UNK_2: i32 = 6;
const BATTLECHIP_STATE_UNK_3: i32 = 7;
const BATTLECHIP_STATE_END: i32 = 8;

// Handshake/reply words (battlechip.c enum).
pub const BATTLECHIP_OK: u16 = 0xFFC6;
pub const PROGRESS_GATE_OK: u16 = 0xFFC7;
pub const BEAST_LINK_GATE_OK: u16 = 0xFFC4;
pub const BEAST_LINK_GATE_US_OK: u16 = 0xFF00;
pub const BATTLECHIP_CONTINUE: u16 = 0xFFFF;

/// The games' resync commands (EXE 5/6 and EXE 4): receiving one of these
/// outside the COMMAND state forces the gate back to SYNC.
const RESYNC_COMMANDS: [u16; 7] = [0xA380, 0xA390, 0xA3A0, 0xA3B0, 0xA3C0, 0xA3D0, 0xA6C0];

/// enum GBASIOBattleChipGateFlavor (gba/interface.h). Which gate model to
/// emulate; it only changes the "ok" handshake word.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BattlechipFlavor {
    /// GBA_FLAVOR_BATTLECHIP_GATE (the default in GBASIOBattlechipGateCreate)
    #[default]
    BattlechipGate,
    ProgressGate,
    BeastLinkGate,
    BeastLinkGateUs,
}

impl BattlechipFlavor {
    fn ok_code(self) -> u16 {
        match self {
            // C: GBA_FLAVOR_BATTLECHIP_GATE and the switch default
            BattlechipFlavor::BattlechipGate => BATTLECHIP_OK,
            BattlechipFlavor::ProgressGate => PROGRESS_GATE_OK,
            BattlechipFlavor::BeastLinkGate => BEAST_LINK_GATE_OK,
            BattlechipFlavor::BeastLinkGateUs => BEAST_LINK_GATE_US_OK,
        }
    }
}

/// struct GBASIOBattlechipGate. Its GBASIODriver member is folded into
/// `SioDriver::Battlechip`; attach with `Gba::attach_battlechip_gate`.
pub struct BattleChipGate {
    /// Inserted BattleChip id (0 = no chip). Mirrors the Qt frontend poking
    /// `m_battlechip.chipId` via setBattleChipId.
    pub chip_id: u16,
    /// Gate model; mirrors setBattleChipFlavor.
    pub flavor: BattlechipFlavor,
    data: [u16; 2],
    state: i32,
}

impl BattleChipGate {
    /// GBASIOBattlechipGateCreate (state/data are zeroed here; they are
    /// (re)initialized by `init` when the driver is installed, as in the C).
    pub fn new() -> Self {
        BattleChipGate {
            chip_id: 0,
            flavor: BattlechipFlavor::default(),
            data: [0; 2],
            state: 0,
        }
    }

    /// Insert a BattleChip (frontend convenience == setting `chip_id`).
    pub fn insert_chip(&mut self, chip_id: u16) {
        self.chip_id = chip_id;
    }

    /// Remove the BattleChip (chip id 0).
    pub fn remove_chip(&mut self) {
        self.chip_id = 0;
    }

    /// GBASIOBattlechipGateInit
    pub(crate) fn init(&mut self) -> bool {
        self.state = BATTLECHIP_STATE_SYNC;
        self.data[0] = 0x00FE;
        self.data[1] = 0xFFFE;
        true
    }

    /// GBASIOBattlechipGateWriteSIOCNT: force the SI/ready lines the gate
    /// drives (clears 0xC, sets 0x8).
    pub(crate) fn write_siocnt(&self, mut value: u16) -> u16 {
        value &= !0xC;
        value |= 0x8;
        value
    }

    /// GBASIOBattlechipGateHandlesMode
    pub(crate) fn handles_mode(&self, mode: SioMode) -> bool {
        matches!(mode, SioMode::Normal32 | SioMode::Multi)
    }

    /// GBASIOBattlechipGateConnectedDevices
    pub(crate) fn connected_devices(&self) -> i32 {
        1
    }

    /// GBASIOBattlechipGateFinishMultiplayer: one completed MULTI transfer
    /// is one phase of the gate protocol; `data` is the outgoing SIOMULTI
    /// block (`data[1]` is the word the game receives in SIOMULTI1).
    pub(crate) fn finish_multiplayer(&mut self, gba: &mut Gba, data: &mut [u16; 4]) {
        let cmd = gba.memory.io[(GBA_REG_SIOMLT_SEND >> 1) as usize];
        let mut reply = 0xFFFFu16;

        mlog!(
            Level::Debug,
            rgba_core::log::GBA_BATTLECHIP,
            "Game: {:04X} ({})",
            cmd,
            self.state
        );

        let ok = self.flavor.ok_code();

        if self.state != BATTLECHIP_STATE_COMMAND && RESYNC_COMMANDS.contains(&cmd) {
            // Resync if needed
            mlog!(Level::Debug, rgba_core::log::GBA_BATTLECHIP, "Resync detected");
            self.state = BATTLECHIP_STATE_SYNC;
        }

        match self.state {
            BATTLECHIP_STATE_SYNC => {
                if cmd != 0x8FFF {
                    self.state -= 1;
                }
                // Fall through in the C: SYNC shares COMMAND's reply
                reply = ok;
            }
            BATTLECHIP_STATE_COMMAND => {
                reply = ok;
            }
            BATTLECHIP_STATE_UNK_0 | BATTLECHIP_STATE_UNK_1 => {
                reply = 0xFFFF;
            }
            BATTLECHIP_STATE_DATA_0 => {
                reply = self.data[0];
                self.data[0] = self.data[0].wrapping_add(3) & 0x00FF;
            }
            BATTLECHIP_STATE_DATA_1 => {
                reply = self.data[1];
                self.data[1] = self.data[1].wrapping_sub(3) | 0xFC00;
            }
            BATTLECHIP_STATE_ID => {
                reply = self.chip_id;
            }
            BATTLECHIP_STATE_UNK_2 | BATTLECHIP_STATE_UNK_3 => {
                reply = 0;
            }
            BATTLECHIP_STATE_END => {
                reply = ok;
                self.state = BATTLECHIP_STATE_SYNC;
            }
            // Off the end of the enum the C has no case: reply stays
            // 0xFFFF (only reachable via the SYNC countdown going 0 → -1).
            _ => {}
        }

        mlog!(
            Level::Debug,
            rgba_core::log::GBA_BATTLECHIP,
            "Gate: {:04X} ({})",
            reply,
            self.state
        );
        self.state += 1;

        data[0] = cmd;
        data[1] = reply;
        data[2] = 0xFFFF;
        data[3] = 0xFFFF;
    }
}

impl Default for BattleChipGate {
    fn default() -> Self {
        Self::new()
    }
}

impl Gba {
    /// CoreController::attachBattleChipGate: install the gate as the link
    /// port (SIO) driver.
    pub fn attach_battlechip_gate(&mut self, gate: BattleChipGate) {
        self.sio_set_driver(Some(SioDriver::Battlechip(gate)));
    }

    /// CoreController::detachBattleChipGate.
    pub fn detach_battlechip_gate(&mut self) {
        self.sio_set_driver(None);
    }
}
