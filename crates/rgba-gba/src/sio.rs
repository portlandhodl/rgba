// Copyright (c) 2013-2015 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gba/sio.c and include/mgba/internal/gba/sio.h.
// GBASIODriver vtable becomes an enum; the GBP (Game Boy Player) virtual
// controller state lives here too (sio/gbp.c is included below).

use rgba_core::timing::Timing;
use rgba_core::{mlog, Level};

use crate::gba::{EventId, Gba};
use crate::io::*;

pub const GBA_SIO_CYCLES_PER_TRANSFER: [[i32; 4]; 4] = [
    [31976, 63427, 94884, 125829],
    [8378, 16241, 24104, 31457],
    [5750, 10998, 16241, 20972],
    [3140, 5755, 8376, 10486],
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SioMode {
    Normal8 = 0,
    Normal32 = 1,
    Multi = 2,
    JoyBus = 3,
    Gpio = 8,
    Uart = 0xC,
    Invalid = -1,
}

impl SioMode {
    fn from_bits(bits: u16) -> SioMode {
        // ((rcnt & 0xC000) | (siocnt & 0x3000)) >> 12
        let mode = bits;
        if mode < 8 {
            match mode & 0x3 {
                0 => SioMode::Normal8,
                1 => SioMode::Normal32,
                2 => SioMode::Multi,
                _ => SioMode::JoyBus,
            }
        } else {
            match mode & 0xC {
                8 => SioMode::Gpio,
                _ => SioMode::Uart,
            }
        }
    }
    fn name(self) -> &'static str {
        match self {
            SioMode::Normal8 => "NORMAL8",
            SioMode::Normal32 => "NORMAL32",
            SioMode::Multi => "MULTI",
            SioMode::JoyBus => "JOYBUS",
            SioMode::Gpio => "GPIO",
            SioMode::Uart => "UART",
            SioMode::Invalid => "(unknown)",
        }
    }
}

// JOY command/status bits
pub const JOY_CMD_RESET: u8 = 0xFF;
pub const JOY_CMD_POLL: u8 = 0x00;
pub const JOY_CMD_TRANS: u8 = 0x14;
pub const JOY_CMD_RECV: u8 = 0x15;

pub const JOYSTAT_TRANS: u16 = 0x8;
pub const JOYSTAT_RECV: u16 = 0x2;
pub const JOYCNT_RESET: u16 = 1;
pub const JOYCNT_RECV: u16 = 2;
pub const JOYCNT_TRANS: u16 = 4;

/// SIO peripheral driver (a vtable of fns in the C; here: enum-free struct
/// with optional behavior — GB Player is the only implemented driver).
pub struct Sio {
    pub mode: SioMode,
    pub rcnt: u16,
    pub siocnt: u16,
    /// GBP (Game Boy Player) virtual pad state (gbp.c: txData register
    /// interface). Ported below.
    pub gbp: GbPlayer,
}

/// GBASIOPlayer state (sio/gbp.c) — the Game Boy Player virtual controller.
pub struct GbPlayer {
    pub tx_data: u32,
    pub state: i32,
    pub clock: i32,
    pub t_started: i32,
    pub t_p: i32,
}

impl GbPlayer {
    pub fn new() -> Self {
        GbPlayer {
            tx_data: 0,
            state: 0,
            clock: 0,
            t_started: 0,
            t_p: 0,
        }
    }
}

impl Default for Sio {
    fn default() -> Self {
        Self::new()
    }
}

impl Sio {
    pub fn new() -> Self {
        Sio {
            mode: SioMode::Invalid,
            rcnt: 0x8000,
            siocnt: 0,
            gbp: GbPlayer::new(),
        }
    }
}

impl Gba {
    /// GBASIOReset
    pub fn sio_reset(&mut self) {
        self.sio.rcnt = 0x8000;
        self.sio.siocnt = 0;
        self.sio.mode = SioMode::Invalid;
        self.sio_switch_mode();
    }

    fn sio_switch_mode(&mut self) {
        let mode_bits = ((self.sio.rcnt & 0xC000) | (self.sio.siocnt & 0x3000)) >> 12;
        let new_mode = SioMode::from_bits(mode_bits);
        if new_mode != self.sio.mode {
            mlog!(
                Level::Debug,
                rgba_core::log::GBA_SIO,
                "Switching mode from {} to {}",
                self.sio.mode.name(),
                new_mode.name()
            );
            self.sio.mode = new_mode;
            match new_mode {
                SioMode::Multi => {
                    // TODO: device id passthrough when a driver exists
                }
                _ => {}
            }
        }
    }

    /// GBASIOWriteRCNT
    pub fn sio_write_rcnt(&mut self, value: u16) {
        self.sio.rcnt &= 0x1FF;
        self.sio.rcnt |= value & 0xC000;
        self.sio_switch_mode();
        // No driver → the RCNT default retain:
        if self.sio.mode == SioMode::Gpio {
            self.sio.rcnt &= 0xC000;
            self.sio.rcnt |= value & 0x1FF;
        } else {
            self.sio.rcnt &= 0xC00F;
            self.sio.rcnt |= value & 0x1F0;
        }
    }

    /// _startTransfer: schedule the transfer-complete event.
    fn sio_start_transfer(&mut self) {
        let cycles =
            self.sio_transfer_cycles(self.sio.mode, self.sio.siocnt, 0);
        self.deschedule(EventId::Sio);
        self.schedule(EventId::Sio, cycles);
    }

    /// GBASIOWriteSIOCNT
    pub fn sio_write_siocnt(&mut self, mut value: u16) {
        if (value ^ self.sio.siocnt) & 0x3000 != 0 {
            self.sio.siocnt = value & 0x3000;
            self.sio_switch_mode();
        }
        let id = 0;
        let connected = 0;
        match self.sio.mode {
            SioMode::Multi => {
                value &= 0xFF83;
                // SC appears to float high in multi mode without a transfer
                self.sio.rcnt |= 1;

                if value & 0x80 != 0 && self.sio.siocnt & 0x80 == 0 {
                    if id == 0 {
                        self.memory.io[(GBA_REG_SIOMULTI0 >> 1) as usize] = 0xFFFF;
                        self.memory.io[(GBA_REG_SIOMULTI1 >> 1) as usize] = 0xFFFF;
                        self.memory.io[(GBA_REG_SIOMULTI2 >> 1) as usize] = 0xFFFF;
                        self.memory.io[(GBA_REG_SIOMULTI3 >> 1) as usize] = 0xFFFF;
                        self.sio.rcnt &= !1;
                        self.sio_start_transfer();
                    }
                }
            }
            SioMode::Normal8 | SioMode::Normal32 => {
                if value & 1 != 0 {
                    self.sio.rcnt |= 1;
                }
                if value & 0x80 != 0 && self.sio.siocnt & 0x80 == 0 {
                    self.sio_start_transfer();
                }
            }
            _ => {}
        }
        // No driver: dummy driver behavior
        match self.sio.mode {
            SioMode::Normal8 | SioMode::Normal32 => {
                value |= 0x8; // GBASIONormalFillSi
            }
            SioMode::Multi => {
                value |= 0x4; // GBASIOMultiplayerFillReady
            }
            _ => {}
        }
        self.sio.siocnt = value;
        let _ = connected;
    }

    /// GBASIOWriteRegister
    pub fn sio_write_register(&mut self, address: u32, mut value: u16) -> u16 {
        let mut handled = true;
        match self.sio.mode {
            SioMode::JoyBus | SioMode::Normal8 | SioMode::Normal32 | SioMode::Multi
            | SioMode::Uart => {
                match address {
                    // JOYCNT is handled identically in all modes
                    GBA_REG_JOYCNT => {
                        value = (value & 0x0040)
                            | (self.memory.io[(GBA_REG_JOYCNT >> 1) as usize] & !(value & 0x7) & !0x0040);
                    }
                    _ => {
                        handled = false;
                    }
                }
            }
            _ => {
                mlog!(
                    Level::Stub,
                    rgba_core::log::GBA_SIO,
                    "Write: Unhandled {} {:03X} <- {:04X}",
                    self.sio.mode.name(),
                    address,
                    value
                );
                handled = false;
            }
        }
        if !handled {
            value = self.memory.io[(address >> 1) as usize];
        }
        value
    }

    /// GBASIOTransferCycles
    pub fn sio_transfer_cycles(&self, mode: SioMode, siocnt: u16, connected: usize) -> i32 {
        if connected >= super::gba::DMA_COUNT {
            return 0;
        }
        match mode {
            SioMode::Multi => {
                GBA_SIO_CYCLES_PER_TRANSFER[((siocnt >> 14) & 0x3) as usize][connected]
                // [Baud] index: GBASIOMultiplayerGetBaud
            }
            SioMode::Normal8 => {
                let sc = if siocnt & 1 != 0 { 2048 } else { 256 };
                (8 * 0x1000000 / (sc * 1024)) as i32
            }
            SioMode::Normal32 => {
                let sc = if siocnt & 1 != 0 { 2048 } else { 256 };
                (32 * 0x1000000 / (sc * 1024)) as i32
            }
            _ => {
                mlog!(Level::Stub, rgba_core::log::GBA_SIO, "No cycle count implemented for mode {}", mode.name());
                0
            }
        }
    }

    /// _sioFinish
    pub fn sio_complete_event(&mut self, _timing: &mut Timing, cycles_late: u32) {
        match self.sio.mode {
            SioMode::Multi => {
                // dummy all-ones data (driver path)
                let data = [0xFFFFu16; 4];
                self.sio_multiplayer_finish(data, cycles_late);
            }
            SioMode::Normal8 => {
                self.sio.siocnt &= !0x80;
                self.memory.io[(GBA_REG_SIOMLT_SEND >> 1) as usize] = 0xFF;
                if self.sio.siocnt & 0x4000 != 0 {
                    self.raise_irq(7);
                }
            }
            SioMode::Normal32 => {
                self.sio.siocnt &= !0x80;
                self.memory.io[(GBA_REG_SIODATA32_LO >> 1) as usize] = 0xFFFF;
                self.memory.io[(GBA_REG_SIODATA32_HI >> 1) as usize] = 0xFFFF;
                if self.sio.siocnt & 0x4000 != 0 {
                    self.raise_irq(7);
                }
            }
            _ => {
                mlog!(
                    Level::Stub,
                    rgba_core::log::GBA_SIO,
                    "No dummy finish implemented for mode {}",
                    self.sio.mode.name()
                );
            }
        }
    }

    /// GBASIOMultiplayerFinishTransfer
    fn sio_multiplayer_finish(&mut self, data: [u16; 4], _cycles_late: u32) {
        let id = 0;
        self.memory.io[(GBA_REG_SIOMULTI0 >> 1) as usize] = data[0];
        self.memory.io[(GBA_REG_SIOMULTI1 >> 1) as usize] = data[1];
        self.memory.io[(GBA_REG_SIOMULTI2 >> 1) as usize] = data[2];
        self.memory.io[(GBA_REG_SIOMULTI3 >> 1) as usize] = data[3];

        self.sio.siocnt &= !0x80;
        self.sio.siocnt &= !0x0C;
        self.sio.siocnt |= (id << 2) as u16;

        self.sio.rcnt |= 1;
        if self.sio.siocnt & 0x4000 != 0 {
            self.raise_irq(7);
        }
    }

    /// GBASIOJOYSendCommand (subset — GBP not attached: behave "no player").
    pub fn sio_joy_send_command(&mut self, command: u8, data: &mut [u8]) -> usize {
        match command {
            JOY_CMD_RESET => {
                self.memory.io[(GBA_REG_JOYCNT >> 1) as usize] |= JOYCNT_RESET;
                if self.memory.io[(GBA_REG_JOYCNT >> 1) as usize] & 0x40 != 0 {
                    self.raise_irq(7);
                }
                data[0] = 0x00;
                data[1] = 0x04;
                data[2] = self.memory.io[(GBA_REG_JOYSTAT >> 1) as usize] as u8;
                3
            }
            JOY_CMD_POLL => {
                data[0] = 0x00;
                data[1] = 0x04;
                data[2] = self.memory.io[(GBA_REG_JOYSTAT >> 1) as usize] as u8;
                3
            }
            JOY_CMD_RECV => {
                self.memory.io[(GBA_REG_JOYCNT >> 1) as usize] |= JOYCNT_RECV;
                self.memory.io[(GBA_REG_JOYSTAT >> 1) as usize] |= JOYSTAT_RECV;
                self.memory.io[(GBA_REG_JOY_RECV_LO >> 1) as usize] =
                    (data[0] as u16) | ((data[1] as u16) << 8);
                self.memory.io[(GBA_REG_JOY_RECV_HI >> 1) as usize] =
                    (data[2] as u16) | ((data[3] as u16) << 8);
                data[0] = self.memory.io[(GBA_REG_JOYSTAT >> 1) as usize] as u8;
                if self.memory.io[(GBA_REG_JOYCNT >> 1) as usize] & 0x40 != 0 {
                    self.raise_irq(7);
                }
                1
            }
            JOY_CMD_TRANS => {
                data[0] = self.memory.io[(GBA_REG_JOY_TRANS_LO >> 1) as usize] as u8;
                data[1] = (self.memory.io[(GBA_REG_JOY_TRANS_LO >> 1) as usize] >> 8) as u8;
                data[2] = self.memory.io[(GBA_REG_JOY_TRANS_HI >> 1) as usize] as u8;
                data[3] = (self.memory.io[(GBA_REG_JOY_TRANS_HI >> 1) as usize] >> 8) as u8;
                data[4] = self.memory.io[(GBA_REG_JOYSTAT >> 1) as usize] as u8;
                self.memory.io[(GBA_REG_JOYCNT >> 1) as usize] |= JOYCNT_TRANS;
                self.memory.io[(GBA_REG_JOYSTAT >> 1) as usize] &= !JOYSTAT_TRANS;
                if self.memory.io[(GBA_REG_JOYCNT >> 1) as usize] & 0x40 != 0 {
                    self.raise_irq(7);
                }
                5
            }
            _ => 0,
        }
    }
}

impl Gba {
    pub fn debug_write(&mut self, _value: u16) {}
    pub fn sync_savedata(&mut self) {}
}
