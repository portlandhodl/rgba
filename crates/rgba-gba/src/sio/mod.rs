// Copyright (c) 2013-2015 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gba/sio.c and include/mgba/internal/gba/sio.h.
// The GBASIODriver vtable becomes the `SioDriver` enum (see lockstep.rs for
// the link-cable driver); the GBP (Game Boy Player) virtual controller state
// lives here too (sio/gbp.c is included below).
//
// Driver call sites mirror the C's GBASIOWriteSIOCNT/GBASIOWriteRCNT/
// _startTransfer/_sioFinish/GBASIOMultiplayerFinishTransfer; with no driver
// installed the behavior is the C's "dummy driver" path.

pub mod dolphin;
pub mod gbp;
pub mod lockstep;

use rgba_core::timing::Timing;
use rgba_core::{mlog, Level};

use crate::gba::{EventId, Gba};
use crate::io::*;
use crate::sio::lockstep::GbaSioLockstepNode;

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
    pub(crate) fn name(self) -> &'static str {
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
    /// The raw enum GBASIOMode value (as stored in lockstep event payloads).
    pub(crate) fn to_raw(self) -> i32 {
        self as i32
    }
    pub(crate) fn from_raw(raw: i32) -> SioMode {
        match raw {
            0 => SioMode::Normal8,
            1 => SioMode::Normal32,
            2 => SioMode::Multi,
            3 => SioMode::JoyBus,
            8 => SioMode::Gpio,
            0xC => SioMode::Uart,
            _ => SioMode::Invalid,
        }
    }
}

// SIOCNT/RCNT bitfield helpers (sio.h DECL_BIT/DECL_BITS):
// GBASIONormal: Sc 0, InternalSc 1, Si 2, IdleSo 3, Start 7, Length 12, Irq 14.
pub(crate) fn normal_fill_si(v: u16) -> u16 {
    v | 0x4
}
pub(crate) fn normal_clear_start(v: u16) -> u16 {
    v & !0x80
}
// GBASIOMultiplayer: Baud 0-1, Slave 2, Ready 3, Id 4-5, Error 6, Busy 7, Irq 14.
pub(crate) fn multiplayer_set_slave(v: u16, slave: bool) -> u16 {
    (v & !0x4) | (slave as u16) << 2
}
pub(crate) fn multiplayer_clear_slave(v: u16) -> u16 {
    v & !0x4
}
pub(crate) fn multiplayer_set_ready(v: u16, ready: bool) -> u16 {
    (v & !0x8) | (ready as u16) << 3
}
pub(crate) fn multiplayer_fill_ready(v: u16) -> u16 {
    v | 0x8
}
pub(crate) fn multiplayer_set_id(v: u16, id: i32) -> u16 {
    (v & !0x30) | (((id as u16) & 3) << 4)
}
pub(crate) fn multiplayer_clear_busy(v: u16) -> u16 {
    v & !0x80
}
// GBASIORegisterRCNT: Sc 0, Sd 1, Si 2, So 3, directions 4-7.
pub(crate) fn rcnt_fill_sc(v: u16) -> u16 {
    v | 0x1
}
pub(crate) fn rcnt_clear_sc(v: u16) -> u16 {
    v & !0x1
}
pub(crate) fn rcnt_set_sd(v: u16, sd: bool) -> u16 {
    (v & !0x2) | (sd as u16) << 1
}
pub(crate) fn rcnt_set_si(v: u16, si: bool) -> u16 {
    (v & !0x4) | (si as u16) << 2
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

/// struct GBASIODriver vtable, as an enum of link-cable peers (same pattern
/// as the GB core's `sio::SioDriver`).
pub enum SioDriver {
    None,
    /// The Game Boy Player driver (gbp.c). Its methods live in `super::gbp`.
    Gbp,
    /// The Dolphin link adaptor driver (dolphin.c). The socket itself lives
    /// out of the enum (it owns TCP sockets, non-Copy); node state is kept
    /// separate: driver variant presence == "installed".
    Dolphin,
    Lockstep(GbaSioLockstepNode),
}

impl Default for SioDriver {
    fn default() -> Self {
        SioDriver::None
    }
}

/// SIO peripheral state (struct GBASIO). The C's `completeEvent` slot is
/// folded into `EventId::Sio`; the GBP (Game Boy Player) virtual pad state
/// (gbp.c: txData register interface) is ported below.
pub struct Sio {
    pub mode: SioMode,
    pub rcnt: u16,
    pub siocnt: u16,
    pub driver: SioDriver,
    /// GBP (Game Boy Player) virtual pad state (gbp.c: txData register
    /// interface). Ported below.
    pub gbp: GbPlayer,
    /// Dolphin JOYBUS-over-TCP adaptor (dolphin.c). Option so it can be
    /// installed only on demand from the frontend.
    pub dolphin: Option<Box<dolphin::Dolphin>>,
}

/// GBASIOPlayer state (sio/gbp.c) — the Game Boy Player virtual controller.
pub struct GbPlayer {
    pub inputs_posted: i32,
    pub tx_position: i32,
    /// Set once the GBP screen-detect has fired: the frontend key callback
    /// is overridden by the GBP virtual controller (`_gbpRead`).
    pub key_override: bool,
    /// C: oldCallback/oldOpposingDirections tracking is folded into
    /// `key_override` (the frontend reinstalls its callback when this flips
    /// back to false at run_frame end).
    _unused: (),
}

impl GbPlayer {
    pub fn new() -> Self {
        GbPlayer {
            inputs_posted: 0,
            tx_position: 0,
            key_override: false,
            _unused: (),
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
            driver: SioDriver::None,
            gbp: GbPlayer::new(),
            dolphin: None,
        }
    }
}

impl Gba {
    /// GBASIOReset
    pub fn sio_reset(&mut self) {
        // The C resets the driver before clearing the registers.
        let driver = std::mem::take(&mut self.sio.driver);
        if let SioDriver::Lockstep(mut node) = driver {
            node.reset(self);
            self.sio.driver = SioDriver::Lockstep(node);
        }
        self.sio.rcnt = 0x8000;
        self.sio.siocnt = 0;
        self.sio.mode = SioMode::Invalid;
        self.sio_switch_mode();
    }

    /// GBASIODeinit
    pub fn sio_deinit(&mut self) {
        let driver = std::mem::take(&mut self.sio.driver);
        if let SioDriver::Lockstep(mut node) = driver {
            node.deinit(self);
        }
    }

    /// GBASIOSetDriver
    pub fn sio_set_driver(&mut self, driver: Option<SioDriver>) {
        let mut old = std::mem::take(&mut self.sio.driver);
        if let SioDriver::Lockstep(node) = &mut old {
            node.deinit(self);
        }
        drop(old);
        let Some(mut driver) = driver else {
            return;
        };
        let ok = match &mut driver {
            SioDriver::None | SioDriver::Gbp => true,
            SioDriver::Dolphin => {
                if let Some(mut d) = self.sio.dolphin.take() {
                    let r = d.init(self);
                    self.sio.dolphin = Some(d);
                    r
                } else {
                    false
                }
            }
            SioDriver::Lockstep(node) => node.init(self),
        };
        if !ok {
            if let SioDriver::Lockstep(node) = &mut driver {
                node.deinit(self);
            }
            mlog!(
                Level::Error,
                rgba_core::log::GBA_SIO,
                "Could not initialize SIO driver"
            );
            return;
        }
        self.sio.driver = driver;
    }

    /// driver->deviceId (0 with no driver)
    fn sio_driver_device_id(&self) -> i32 {
        match &self.sio.driver {
            SioDriver::None | SioDriver::Gbp | SioDriver::Dolphin => 0,
            SioDriver::Lockstep(node) => node.device_id(),
        }
    }

    /// driver->connectedDevices (0 with no driver)
    fn sio_driver_connected_devices(&self) -> i32 {
        match &self.sio.driver {
            SioDriver::None => 0,
            SioDriver::Gbp => 1,
            SioDriver::Dolphin => 1,
            SioDriver::Lockstep(node) => node.connected_devices(),
        }
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
            // driver->setMode
            let driver = std::mem::take(&mut self.sio.driver);
            let mut driver = driver;
            match &mut driver {
                SioDriver::Lockstep(node) => node.set_mode(self, new_mode),
                SioDriver::Dolphin => {
                    if let Some(d) = self.sio.dolphin.as_mut() {
                        d.set_mode(new_mode);
                    }
                }
                _ => {}
            }
            self.sio.driver = driver;

            if new_mode == SioMode::Multi {
                let id = self.sio_driver_device_id();
                self.sio.rcnt = rcnt_set_si(self.sio.rcnt, id != 0);
            }
        }
    }

    /// GBASIOWriteRCNT
    pub fn sio_write_rcnt(&mut self, value: u16) {
        self.sio.rcnt &= 0x1FF;
        self.sio.rcnt |= value & 0xC000;
        self.sio_switch_mode();
        // driver->writeRCNT (the lockstep driver returns the value verbatim)
        let value = match &mut self.sio.driver {
            SioDriver::None | SioDriver::Gbp => value,
            // Dolphin's writeRCNT leaves the value untouched (dolphin.c has
            // no writeRCNT hook).
            SioDriver::Dolphin => value,
            SioDriver::Lockstep(node) => node.write_rcnt(value),
        };
        if self.sio.mode == SioMode::Gpio {
            self.sio.rcnt = (value & 0x01FF) | (self.sio.rcnt & 0xC000);
        } else {
            self.sio.rcnt = (value & 0x01F0) | (self.sio.rcnt & 0xC00F);
        }
    }

    /// _startTransfer: schedule the transfer-complete event (unless the
    /// driver handles completion internally).
    fn sio_start_transfer(&mut self) {
        let driver = std::mem::take(&mut self.sio.driver);
        let mut started = true;
        let mut driver = driver;
        match &mut driver {
            SioDriver::None => {}
            SioDriver::Gbp => {
                started = gbp::gbp_start(self);
            }
            SioDriver::Lockstep(node) => {
                started = node.start(self);
            }
            SioDriver::Dolphin => {
                // Dolphin has no start() hook in the C.
            }
        }
        self.sio.driver = driver;
        if !started {
            // Transfer completion is handled internally to the driver
            return;
        }
        let connected = self.sio_driver_connected_devices() as usize;
        let cycles = self.sio_transfer_cycles(self.sio.mode, self.sio.siocnt, connected);
        self.deschedule(EventId::Sio);
        self.schedule(EventId::Sio, cycles);
    }

    /// GBASIOWriteSIOCNT
    pub fn sio_write_siocnt(&mut self, mut value: u16) {
        if (value ^ self.sio.siocnt) & 0x3000 != 0 {
            self.sio.siocnt = value & 0x3000;
            self.sio_switch_mode();
        }
        let mut id = 0;
        let mut connected = 0;
        // if (sio->driver) { handled = handlesMode; id = deviceId; connected = ... }
        let has_driver_write_siocnt = match &self.sio.driver {
            SioDriver::None => false,
            SioDriver::Gbp => {
                if gbp::driver::handles_mode(self.sio.mode) {
                    true
                } else {
                    false
                }
            }
            SioDriver::Lockstep(node) => {
                if node.handles_mode(self.sio.mode) {
                    id = node.device_id();
                    connected = node.connected_devices();
                    true // writeSIOCNT is always present on the lockstep driver
                } else {
                    false
                }
            }
            SioDriver::Dolphin => false, // no writeSIOCNT hook in the C
        };
        match self.sio.mode {
            SioMode::Multi => {
                value &= 0xFF83;
                value = multiplayer_set_slave(value, id != 0 || connected == 0);
                value = multiplayer_set_id(value, id);
                value |= self.sio.siocnt & 0x00FC;

                // SC appears to float high in multi mode without a transfer
                self.sio.rcnt = rcnt_fill_sc(self.sio.rcnt);

                if value & 0x80 != 0 && self.sio.siocnt & 0x80 == 0 {
                    if id == 0 {
                        self.memory.io[(GBA_REG_SIOMULTI0 >> 1) as usize] = 0xFFFF;
                        self.memory.io[(GBA_REG_SIOMULTI1 >> 1) as usize] = 0xFFFF;
                        self.memory.io[(GBA_REG_SIOMULTI2 >> 1) as usize] = 0xFFFF;
                        self.memory.io[(GBA_REG_SIOMULTI3 >> 1) as usize] = 0xFFFF;
                        self.sio.rcnt = rcnt_clear_sc(self.sio.rcnt);
                        self.sio_start_transfer();
                    }
                }
            }
            SioMode::Normal8 | SioMode::Normal32 => {
                if value & 1 != 0 {
                    self.sio.rcnt = rcnt_fill_sc(self.sio.rcnt);
                }
                if value & 0x80 != 0 && self.sio.siocnt & 0x80 == 0 {
                    self.sio_start_transfer();
                }
            }
            _ => {}
        }
        if has_driver_write_siocnt {
            let driver = std::mem::take(&mut self.sio.driver);
            let mut driver = driver;
            match &mut driver {
                SioDriver::Gbp => {
                    value = gbp::gbp_write_siocnt(self, value);
                }
                SioDriver::Lockstep(node) => {
                    value = node.write_siocnt(value);
                }
                _ => {}
            }
            self.sio.driver = driver;
        } else {
            // Dummy drivers
            match self.sio.mode {
                SioMode::Normal8 | SioMode::Normal32 => {
                    value = normal_fill_si(value);
                }
                SioMode::Multi => {
                    value = multiplayer_fill_ready(value);
                }
                _ => {}
            }
        }
        self.sio.siocnt = value;
    }

    /// GBASIOWriteRegister
    pub fn sio_write_register(&mut self, address: u32, mut value: u16) -> u16 {
        let mut handled = true;
        match self.sio.mode {
            SioMode::JoyBus
            | SioMode::Normal8
            | SioMode::Normal32
            | SioMode::Multi
            | SioMode::Uart => {
                match address {
                    // JOYCNT is handled identically in all modes
                    GBA_REG_JOYCNT => {
                        value = (value & 0x0040)
                            | (self.memory.io[(GBA_REG_JOYCNT >> 1) as usize]
                                & !(value & 0x7)
                                & !0x0040);
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
                mlog!(
                    Level::Stub,
                    rgba_core::log::GBA_SIO,
                    "No cycle count implemented for mode {}",
                    mode.name()
                );
                0
            }
        }
    }

    /// _sioFinish
    pub fn sio_complete_event(&mut self, timing: &mut Timing, cycles_late: u32) {
        match self.sio.mode {
            SioMode::Multi => {
                let mut data = [0u16; 4];
                let driver = std::mem::take(&mut self.sio.driver);
                if let SioDriver::Lockstep(mut node) = driver {
                    node.finish_multiplayer(self, timing, &mut data);
                    self.sio.driver = SioDriver::Lockstep(node);
                }
                self.sio_multiplayer_finish(data, cycles_late);
            }
            SioMode::Normal8 => {
                let mut data = 0u8;
                let driver = std::mem::take(&mut self.sio.driver);
                if let SioDriver::Lockstep(mut node) = driver {
                    data = node.finish_normal8(self, timing);
                    self.sio.driver = SioDriver::Lockstep(node);
                }
                self.sio_normal8_finish(data, cycles_late);
            }
            SioMode::Normal32 => {
                let mut data = 0u32;
                let driver = std::mem::take(&mut self.sio.driver);
                if let SioDriver::Lockstep(mut node) = driver {
                    data = node.finish_normal32(self, timing);
                    self.sio.driver = SioDriver::Lockstep(node);
                }
                self.sio_normal32_finish(data, cycles_late);
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
        let id = self.sio_driver_device_id();
        self.memory.io[(GBA_REG_SIOMULTI0 >> 1) as usize] = data[0];
        self.memory.io[(GBA_REG_SIOMULTI1 >> 1) as usize] = data[1];
        self.memory.io[(GBA_REG_SIOMULTI2 >> 1) as usize] = data[2];
        self.memory.io[(GBA_REG_SIOMULTI3 >> 1) as usize] = data[3];

        self.sio.siocnt = multiplayer_clear_busy(self.sio.siocnt);
        self.sio.siocnt = multiplayer_set_id(self.sio.siocnt, id);

        self.sio.rcnt = rcnt_fill_sc(self.sio.rcnt);
        if self.sio.siocnt & 0x4000 != 0 {
            self.raise_irq(7);
        }
    }

    /// GBASIONormal8FinishTransfer
    fn sio_normal8_finish(&mut self, data: u8, _cycles_late: u32) {
        self.sio.siocnt = normal_clear_start(self.sio.siocnt);
        // SIODATA8 shares its address with SIOMLT_SEND
        self.memory.io[(GBA_REG_SIOMLT_SEND >> 1) as usize] = data as u16;
        if self.sio.siocnt & 0x4000 != 0 {
            self.raise_irq(7);
        }
    }

    /// GBASIONormal32FinishTransfer
    fn sio_normal32_finish(&mut self, data: u32, _cycles_late: u32) {
        self.sio.siocnt = normal_clear_start(self.sio.siocnt);
        self.memory.io[(GBA_REG_SIODATA32_LO >> 1) as usize] = data as u16;
        self.memory.io[(GBA_REG_SIODATA32_HI >> 1) as usize] = (data >> 16) as u16;
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
