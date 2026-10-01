// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Ported from mgba/src/gba/sio/dolphin.c — the Dolphin (GameCube emulator)
// connector: a JOYBUS SIO driver bridged over two TCP sockets (clock +
// data). std::net replaces the C's socket abstraction (core/socket.c).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};

use rgba_core::mlog;

use crate::gba::{EventId, Gba, GBA_ARM7TDMI_FREQUENCY};
use crate::sio::SioMode;

pub const BITS_PER_SECOND: i32 = 115200; // C: "This is wrong, but we need to maintain compat"
pub const CYCLES_PER_BIT: i32 = (GBA_ARM7TDMI_FREQUENCY / BITS_PER_SECOND as u32) as i32;
pub const CLOCK_GRAIN: i32 = CYCLES_PER_BIT * 8;
pub const CLOCK_WAIT_MS: i32 = 500;

pub const DOLPHIN_CLOCK_PORT: u16 = 49420;
pub const DOLPHIN_DATA_PORT: u16 = 54970;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DolphinState {
    WaitForFirstClock = 0,
    WaitForClock,
    WaitForCommand,
}

/// struct GBASIODolphin.
pub struct Dolphin {
    data: Option<TcpStream>,
    clock: Option<TcpStream>,
    active: bool,
    state: DolphinState,
    clock_slice: i32,
}

impl Dolphin {
    /// GBASIODolphinCreate
    pub fn new() -> Self {
        Dolphin {
            data: None,
            clock: None,
            active: false,
            state: DolphinState::WaitForFirstClock,
            clock_slice: 0,
        }
    }

    /// GBASIODolphinConnect
    pub fn connect(&mut self, address: &str, data_port: u16, clock_port: u16) -> std::io::Result<()> {
        self.data = None;
        self.clock = None;
        let data_port = if data_port == 0 { DOLPHIN_DATA_PORT } else { data_port };
        let clock_port = if clock_port == 0 { DOLPHIN_CLOCK_PORT } else { clock_port };

        let data_addr: SocketAddr = format!("{}:{}", address, data_port).parse()
            .map_err(|e: std::net::AddrParseError| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let clock_addr: SocketAddr = format!("{}:{}", address, clock_port).parse()
            .map_err(|e: std::net::AddrParseError| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

        let data = TcpStream::connect(data_addr)?;
        let clock = match TcpStream::connect(clock_addr) {
            Ok(c) => c,
            Err(e) => {
                self.data = None;
                return Err(e);
            }
        };
        data.set_nonblocking(true)?;
        clock.set_nonblocking(true)?;
        data.set_nodelay(true)?;
        self.data = Some(data);
        self.clock = Some(clock);
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.data.is_some()
    }

    /// GBASIODolphinInit
    pub fn init(&mut self, gba: &mut Gba) -> bool {
        self.clock_slice = 0;
        self.state = DolphinState::WaitForFirstClock;
        self.reset(gba);
        true
    }

    /// GBASIODolphinReset
    pub fn reset(&mut self, gba: &mut Gba) {
        self.active = false;
        self.flush();
        gba.deschedule(EventId::SioDolphin);
        gba.schedule(EventId::SioDolphin, 0);
    }

    /// GBASIODolphinSetMode
    pub fn set_mode(&mut self, mode: SioMode) {
        self.active = mode == SioMode::JoyBus;
    }

    fn flush(&mut self) {
        let mut buffer = [0u8; 32];
        if let Some(clock) = &mut self.clock {
            while clock.read(&mut buffer).map(|n| n == 32).unwrap_or(false) {}
        }
        if let Some(data) = &mut self.data {
            while data.read(&mut buffer).map(|n| n == 32).unwrap_or(false) {}
        }
    }

    /// GBASIODolphinProcessEvents (dispatched from EventId::SioDolphin)
    pub fn process_events(&mut self, gba: &mut Gba, cycles_late: i32) {
        if self.data.is_none() {
            return;
        }
        self.clock_slice -= cycles_late;

        let mut next_event = CLOCK_GRAIN;
        loop {
            match self.state {
                DolphinState::WaitForFirstClock => {
                    self.clock_slice = 0;
                    self.state = DolphinState::WaitForClock;
                    continue;
                }
                DolphinState::WaitForClock => {
                    if self.clock_slice < 0 {
                        // SocketPoll with CLOCK_WAIT — we poll
                        // nonblocking; no wait (mGBA's blocking poll only
                        // delays the emulator, which a frontend can't do
                        // anyway without a thread).
                    }
                    let mut buf = [0u8; 4];
                    let got = match &mut self.clock {
                        Some(c) => c.read(&mut buf).unwrap_or(0),
                        None => 0,
                    };
                    if got == 4 {
                        let slice = i32::from_be_bytes(buf);
                        self.clock_slice += slice;
                        self.state = DolphinState::WaitForCommand;
                        next_event = 0;
                        continue;
                    }
                    break;
                }
                DolphinState::WaitForCommand => {
                    if self.clock_slice < -(crate::video::VIDEO_TOTAL_LENGTH as i32) * 4 {
                        // Would SocketPoll(1, &data, 0, 0, CLOCK_WAIT)
                    }
                    match self.process_command(gba, cycles_late) {
                        Some(n) => {
                            self.state = DolphinState::WaitForClock;
                            next_event = n;
                            break;
                        }
                        None => break,
                    }
                }
            }
        }
        self.clock_slice -= next_event;
        gba.schedule(EventId::SioDolphin, next_event.max(0));
    }

    /// _processCommand
    fn process_command(&mut self, gba: &mut Gba, cycles_late: i32) -> Option<i32> {
        // Does not include the stop bits due to compatibility reasons
        let mut bits_on_line = 8;
        let mut buffer = [0u8; 6];
        let gotten = match &mut self.data {
            Some(d) => d.read(&mut buffer[0..1]).unwrap_or(0),
            None => 0,
        };
        if gotten < 1 {
            return None;
        }

        match buffer[0] {
            JOY_RESET | JOY_POLL => {
                bits_on_line += 24;
            }
            JOY_RECV => {
                let gotten = match &mut self.data {
                    Some(d) => d.read_exact_or_wouldblock(&mut buffer[1..5]).unwrap_or(0),
                    None => 0,
                };
                if gotten < 4 {
                    return None;
                }
                mlog!(
                    rgba_core::log::Level::Debug,
                    rgba_core::log::GBA_SIO,
                    "DOL recv: {:02X}{:02X}{:02X}{:02X}",
                    buffer[1], buffer[2], buffer[3], buffer[4]
                );
                bits_on_line += 40;
            }
            JOY_TRANS => {
                bits_on_line += 40;
            }
            _ => {}
        }

        if !self.active {
            return Some(0);
        }

        let sent = gba.sio_joy_send_command(buffer[0], &mut buffer[1..]);
        if let Some(d) = &mut self.data {
            let _ = d.write_all(&buffer[1..1 + sent]);
        }
        Some(bits_on_line * CYCLES_PER_BIT - cycles_late)
    }
}

impl Default for Dolphin {
    fn default() -> Self {
        Self::new()
    }
}

// JOYBUS commands (mgba include/mgba/internal/gba/sio.h enum GBASIOJOYCommand)
pub const JOY_RESET: u8 = 0xFF;
pub const JOY_POLL: u8 = 0x00;
pub const JOY_TRANS: u8 = 0x01;
pub const JOY_RECV: u8 = 0x14;

/// Read that tolerates WouldBlock mid-buffer (SocketRecv in C).
trait ReadPartial {
    fn read_exact_or_wouldblock(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;
}
impl ReadPartial for TcpStream {
    fn read_exact_or_wouldblock(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            match self.read(&mut buf[n..]) {
                Ok(0) => break,
                Ok(m) => n += m,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }
        Ok(n)
    }
}
