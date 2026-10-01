// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Ported from mgba/src/debugger/gdb-stub.c and
// mgba/include/mgba/internal/debugger/gdb-stub.h
//
// GDB remote-serial-protocol debugger module for the ARM7TDMI (GBA). The C
// socket calls (mgba-util/socket.h: SocketOpenTCP/SocketListen/SocketAccept/
// SocketSetBlocking/SocketSend/SocketRecv) map to std::net::TcpListener and
// nonblocking std::net::TcpStream here.
//
// Structural differences from the C:
// - `struct mDebuggerModule d` sub-object -> `DebuggerModule` trait impl;
//   the `stub->d.p` back-pointer to `mDebugger` arrives as the `_debugger`
//   argument of each hook (in trait-hooked contexts the stub's own module
//   slot is temporarily None, so a re-entrant `Debugger::enter` skips it,
//   like the C's target=self filter).
// - Received bytes are buffered (`line`); a message is parsed only once it is
//   complete. The C processes whatever SocketRecv returned and NAKs packets
//   split across TCP frames; buffering is the same protocol, minus that bug.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use crate::debugger::{
    watchpoint_type, Breakpoint, BreakpointType, DebugConsole, Debugger, DebuggerEntryInfo,
    DebuggerEntryReason, DebuggerModule, DebuggerState, DebuggerType, EntryTypeInfo, Watchpoint,
};

pub const GDB_STUB_MAX_LINE: usize = 1400;
pub const GDB_STUB_INTERVAL: i32 = 32;

// signal.h values used in stop replies (Win32 fallback in the C for SIGTRAP).
const SIGINT: u8 = 2;
const SIGILL: u8 = 4;
const SIGTRAP: u8 = 5;

// include/mgba/internal/gba/memory.h (the stub is ARM/GBA-specific, like the C)
const GBA_SIZE_BIOS: u32 = 0x4000;

// Mach-O cputype/cpusubtype for qHostInfo (enum in gdb-stub.c)
const MACH_O_ARM: u32 = 12;
const MACH_O_ARM_V4T: u32 = 5;

// enum GDBError
const GDB_BAD_ARGUMENTS: u8 = 0x06;
const GDB_UNSUPPORTED_COMMAND: u8 = 0x07;

pub const TARGET_XML: &str = "<target version=\"1.0\">\
<architecture>armv4t</architecture>\
<osabi>none</osabi>\
<feature name=\"org.gnu.gdb.arm.core\">\
<reg name=\"r0\" bitsize=\"32\" type=\"uint32\"/>\
<reg name=\"r1\" bitsize=\"32\" type=\"uint32\"/>\
<reg name=\"r2\" bitsize=\"32\" type=\"uint32\"/>\
<reg name=\"r3\" bitsize=\"32\" type=\"uint32\"/>\
<reg name=\"r4\" bitsize=\"32\" type=\"uint32\"/>\
<reg name=\"r5\" bitsize=\"32\" type=\"uint32\"/>\
<reg name=\"r6\" bitsize=\"32\" type=\"uint32\"/>\
<reg name=\"r7\" bitsize=\"32\" type=\"uint32\"/>\
<reg name=\"r8\" bitsize=\"32\" type=\"uint32\"/>\
<reg name=\"r9\" bitsize=\"32\" type=\"uint32\"/>\
<reg name=\"r10\" bitsize=\"32\" type=\"uint32\"/>\
<reg name=\"r11\" bitsize=\"32\" type=\"uint32\"/>\
<reg name=\"r12\" bitsize=\"32\" type=\"uint32\"/>\
<reg name=\"sp\" bitsize=\"32\" type=\"data_ptr\"/>\
<reg name=\"lr\" bitsize=\"32\"/>\
<reg name=\"pc\" bitsize=\"32\" type=\"code_ptr\"/>\
<flags id=\"cpsr_flags\" size=\"4\">\
<field name=\"N\" start=\"31\" end=\"31\"/>\
<field name=\"Z\" start=\"30\" end=\"30\"/>\
<field name=\"C\" start=\"29\" end=\"29\"/>\
<field name=\"V\" start=\"28\" end=\"28\"/>\
<field name=\"I\" start=\"7\" end=\"7\"/>\
<field name=\"F\" start=\"6\" end=\"6\"/>\
<field name=\"T\" start=\"5\" end=\"5\"/>\
<field name=\"M\" start=\"0\" end=\"4\"/>\
</flags>\
<reg name=\"cpsr\" bitsize=\"32\" regnum=\"25\" type=\"cpsr_flags\"/>\
</feature>\
</target>";

/// enum GDBStubAckState
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AckState {
    Pending = 0,
    Received,
    NakReceived,
    Off,
}

/// enum GDBWatchpointsBehvaior (sic)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WatchpointsBehavior {
    StandardLogic = 0,
    OverrideLogic,
    OverrideLogicAnyWrite,
}

pub struct GdbStub {
    // --- mDebuggerModule state ---
    paused: bool,
    needs_callback: bool,
    /// Slot index in `Debugger::modules`; owner token for the stub's
    /// breakpoints/watchpoints (C: `&stub->d`). Set via `set_module_index`.
    index: Option<usize>,

    // --- sockets (C: Socket socket, Socket connection) ---
    listener: Option<TcpListener>,
    connection: Option<TcpStream>,
    /// Test-support mode: `feed_input` stands in for a client connection.
    test_connection: bool,

    // --- protocol state ---
    /// Buffered incoming bytes (C: stub->line).
    line: Vec<u8>,
    /// Payload of the reply currently being built (C: stub->outgoing), sent
    /// by `send_message`.
    outgoing: Vec<u8>,
    /// Framed bytes queued for the client (socket send / `take_output`).
    send_queue: Vec<u8>,
    memory_map_xml: String,
    line_ack: AckState,
    until_poll: i32,
    supports_swbreak: bool,
    supports_hwbreak: bool,
    watchpoints_behavior: WatchpointsBehavior,
}

// ---------------------------------------------------------------------------
// Hex helpers (_hex2int, _int2hex8, _int2hex32, _readHex)

/// _hex2int: at most `max_digits` of value, digits and lowercase a-f only;
/// parsing stops at the first other character.
fn hex2int(hex: &[u8], max_digits: usize) -> u32 {
    let mut value: u32 = 0;
    let mut digits = max_digits;
    for &c in hex {
        if digits == 0 {
            break;
        }
        digits -= 1;
        let mut letter = c.wrapping_sub(b'0');
        if letter > 9 {
            letter = c.wrapping_sub(b'a');
            if letter > 5 {
                break;
            }
            value = value.wrapping_mul(0x10).wrapping_add(letter as u32 + 10);
        } else {
            value = value.wrapping_mul(0x10).wrapping_add(letter as u32);
        }
    }
    value
}

fn int2hex8(value: u8, out: &mut Vec<u8>) {
    const LANGUAGE: &[u8; 16] = b"0123456789abcdef";
    out.push(LANGUAGE[(value >> 4) as usize]);
    out.push(LANGUAGE[(value & 0xF) as usize]);
}

/// _int2hex32: registers are serialized as four bytes in target (little
/// endian) byte order, two hex chars per byte.
fn int2hex32(value: u32, out: &mut Vec<u8>) {
    for byte in value.to_le_bytes() {
        int2hex8(byte, out);
    }
}

/// Inverse of `_int2hex32` used by the `P` packet (C: hex2int + LOAD_32BE,
/// which byte-swaps the parsed value).
fn hex2int32_le(hex: &[u8]) -> u32 {
    hex2int(hex, 8).swap_bytes()
}

/// _readHex: parse up to 8 hex digits, stopping at ',', ':' or '='.
/// Returns (value, bytes consumed).
fn read_hex(input: &[u8]) -> (u32, usize) {
    let mut i = 0;
    while i < 8 && i < input.len() {
        if input[i] == b',' || input[i] == b':' || input[i] == b'=' {
            break;
        }
        i += 1;
    }
    (hex2int(&input[..i], i), i)
}

/// "E%02x"
fn err_reply(code: u8) -> Vec<u8> {
    let mut out = vec![b'E'];
    int2hex8(code, &mut out);
    out
}

/// "S%02x" / "T%02x"
fn sig_reply(prefix: u8, sig: u8) -> Vec<u8> {
    let mut out = vec![prefix];
    int2hex8(sig, &mut out);
    out
}

fn starts_with(b: &[u8], prefix: &[u8]) -> bool {
    b.len() >= prefix.len() && &b[..prefix.len()] == prefix
}

// ---------------------------------------------------------------------------

impl GdbStub {
    /// GDBStubCreate.
    pub fn create() -> Box<Self> {
        Box::new(GdbStub {
            paused: false,
            needs_callback: false,
            index: None,
            listener: None,
            connection: None,
            test_connection: false,
            line: Vec::new(),
            outgoing: Vec::new(),
            send_queue: Vec::new(),
            memory_map_xml: String::new(),
            line_ack: AckState::Pending,
            until_poll: GDB_STUB_INTERVAL,
            supports_swbreak: false,
            supports_hwbreak: false,
            watchpoints_behavior: WatchpointsBehavior::StandardLogic,
        })
    }

    /// GDBStubListen: bind a TCP listener on 127.0.0.1:`port` (the C takes an
    /// Address; mGBA's frontends bind the loopback interface).
    pub fn listen(&mut self, port: u16, behavior: WatchpointsBehavior) -> std::io::Result<()> {
        if self.listener.is_some() {
            // C: if (!SOCKET_FAILED(stub->socket)) GDBStubShutdown(stub)
            self.shutdown();
        }
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        listener.set_nonblocking(true)?;
        self.listener = Some(listener);
        self.watchpoints_behavior = behavior;
        self.memory_map_xml.clear();
        Ok(())
    }

    /// GDBStubHangup. The C also calls mDebuggerUpdatePaused; the enclosing
    /// Debugger poll recomputes the state from the module flags instead.
    pub fn hangup(&mut self) {
        self.outgoing = b"W00".to_vec();
        self.send_message();
        self.flush_output();
        self.connection = None;
        self.needs_callback = false;
        self.paused = false;
    }

    /// GDBStubShutdown.
    pub fn shutdown(&mut self) {
        self.hangup();
        self.listener = None;
    }

    /// Change the watchpoints behavior. In the C this is a GDBStubListen
    /// parameter; exposed separately here so it can be changed on a live
    /// stub (and by tests).
    pub fn set_watchpoints_behavior(&mut self, behavior: WatchpointsBehavior) {
        self.watchpoints_behavior = behavior;
    }

    // --- test-support API (no socket involved) ---

    /// Inject bytes as if received from the GDB client, then process them
    /// against `console`. Marks the stub as having a (virtual) connection.
    pub fn feed_input(
        &mut self,
        debugger: &mut Debugger,
        console: &mut dyn DebugConsole,
        bytes: &[u8],
    ) {
        self.test_connection = true;
        self.line.extend_from_slice(bytes);
        self.process_buffered(debugger, console);
        self.flush_output();
    }

    /// Drain bytes that would have been sent to the GDB client.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.send_queue)
    }

    /// Whether a GDB client is attached (C: connection != INVALID_SOCKET).
    pub fn is_connected(&self) -> bool {
        self.connection.is_some()
    }

    /// Address the listener is bound to (useful with port 0 / embedders).
    pub fn local_addr(&self) -> Option<std::net::SocketAddr> {
        self.listener.as_ref().and_then(|l| l.local_addr().ok())
    }

    // --- internal socket I/O ---

    fn has_connection(&self) -> bool {
        self.connection.is_some() || self.test_connection
    }

    /// Bytes queued for the client -> socket (no socket in test mode).
    fn flush_output(&mut self) {
        if self.send_queue.is_empty() || self.connection.is_none() {
            return;
        }
        let mut conn = self.connection.take().unwrap();
        match conn.write(&self.send_queue) {
            Ok(n) => {
                self.send_queue.drain(..n);
                let _ = conn.flush();
                self.connection = Some(conn);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                self.connection = Some(conn);
            }
            Err(_) => {
                drop(conn);
                self.hangup();
            }
        }
    }

    fn ack(&mut self) {
        self.send_queue.push(b'+');
        self.flush_output();
    }

    fn nak(&mut self) {
        // mLOG(DEBUGGER, WARN, "Packet error")
        self.send_queue.push(b'-');
        self.flush_output();
    }

    /// _sendMessage: frame `outgoing` as `$payload#xx` and queue it.
    fn send_message(&mut self) {
        if self.line_ack != AckState::Off {
            self.line_ack = AckState::Pending;
        }
        let mut checksum: u8 = 0;
        let mut frame = Vec::with_capacity(self.outgoing.len() + 4);
        frame.push(b'$');
        for &b in &self.outgoing {
            checksum = checksum.wrapping_add(b);
            frame.push(b);
        }
        frame.push(b'#');
        int2hex8(checksum, &mut frame);
        // mLOG(DEBUGGER, DEBUG, "> %s", ...)
        self.send_queue.extend_from_slice(&frame);
        self.flush_output();
    }

    fn error(&mut self, code: u8) {
        self.outgoing = err_reply(code);
        self.send_message();
    }

    // --- GDBStubUpdate ---

    /// One nonblocking poll iteration: accept a pending connection, read
    /// available bytes, parse complete messages. Returns false when there is
    /// no listener or the connection was lost (mirrors GDBStubUpdate).
    pub fn update(&mut self, debugger: &mut Debugger, console: &mut dyn DebugConsole) -> bool {
        if self.listener.is_none() && !self.test_connection {
            self.needs_callback = false;
            self.paused = false;
            return false;
        }
        self.flush_output();

        if !self.has_connection() {
            let pending = match &self.listener {
                Some(l) => l.accept().ok().map(|(s, _)| s),
                None => None,
            };
            match pending {
                Some(stream) => {
                    if stream.set_nonblocking(true).is_err() {
                        self.hangup();
                        return false;
                    }
                    let _ = stream.set_nodelay(true); // SocketSetTCPPush(conn, 1)
                    self.connection = Some(stream);
                    // C: mDebuggerEnter(ENTER_ATTACHED, NULL) pauses+notifies
                    // all modules. With the stub mid-hook its slot is None, so
                    // this skips it (its own Attached hook is a no-op), and we
                    // pause it directly.
                    debugger.enter(console, DebuggerEntryReason::Attached, None);
                    self.paused = true;
                }
                None => return false,
            }
        }

        if self.connection.is_some() {
            let mut conn = self.connection.take().unwrap();
            let mut buf = [0u8; GDB_STUB_MAX_LINE];
            match conn.read(&mut buf) {
                Ok(0) => {
                    drop(conn);
                    self.hangup(); // connection lost
                    return false;
                }
                Ok(n) => {
                    // mLOG(DEBUGGER, DEBUG, "< %s", ...)
                    self.line.extend_from_slice(&buf[..n]);
                    self.connection = Some(conn);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    self.connection = Some(conn);
                }
                Err(_) => {
                    drop(conn);
                    self.hangup();
                    return false;
                }
            }
        }

        self.process_buffered(debugger, console);
        self.flush_output();
        true
    }

    /// Parse buffered messages until an incomplete packet remains.
    fn process_buffered(&mut self, debugger: &mut Debugger, console: &mut dyn DebugConsole) {
        while let Some(n) = self.parse_message(debugger, console) {
            self.line.drain(..n);
        }
    }

    /// _parseGDBMessage on the buffered input. Returns bytes consumed, or
    /// None if a complete message is not available yet.
    fn parse_message(
        &mut self,
        debugger: &mut Debugger,
        console: &mut dyn DebugConsole,
    ) -> Option<usize> {
        let first = *self.line.first()?;
        match first {
            b'+' => {
                self.line_ack = AckState::Received;
                Some(1)
            }
            b'-' => {
                self.line_ack = AckState::NakReceived;
                Some(1)
            }
            0x03 => {
                // Ctrl-C: mDebuggerEnter(ENTER_MANUAL, info.target=self).
                // Net effects on the target module (this stub) are mirrored:
                // pause + own entered() stop reply.
                self.paused = true;
                self.entered_hook(console, DebuggerEntryReason::Manual, None);
                Some(1)
            }
            b'$' => {
                let hash = self.line.iter().position(|&b| b == b'#')?;
                if self.line.len() < hash + 3 {
                    return None; // checksum digits not here yet
                }
                let mut checksum: u8 = 0;
                for &b in &self.line[1..hash] {
                    checksum = checksum.wrapping_add(b);
                }
                let network_checksum = hex2int(&self.line[hash + 1..hash + 3], 2) as u8;
                if network_checksum != checksum {
                    // mLOG(DEBUGGER, WARN, "Checksum error...")
                    self.nak();
                    return Some(hash + 3);
                }
                self.ack();
                let payload: Vec<u8> = self.line[1..hash].to_vec();
                self.dispatch(debugger, console, &payload);
                Some(hash + 3)
            }
            _ => {
                self.nak();
                Some(1)
            }
        }
    }

    /// _parseGDBMessage command dispatch; `payload` is the packet body
    /// without `$`/`#xx` (the C keeps the trailing `#xx` in the string; the
    /// handlers' `#`-suffixed patterns are matched without it here).
    fn dispatch(&mut self, debugger: &mut Debugger, console: &mut dyn DebugConsole, payload: &[u8]) {
        if payload.is_empty() {
            self.error(GDB_UNSUPPORTED_COMMAND);
            return;
        }
        let message_type = payload[0];
        let message = &payload[1..];
        match message_type {
            b'?' => {
                self.outgoing = sig_reply(b'S', SIGINT);
                self.send_message();
            }
            b'c' => self.do_continue(message),
            b'G' => self.write_gprs(console, message),
            b'g' => self.read_gprs(console),
            b'H' | b'T' => {
                // This is faked because we only have one thread
                self.outgoing = b"OK".to_vec();
                self.send_message();
            }
            b'M' => self.write_memory(console, message),
            b'm' => self.read_memory(console, message),
            b'P' => self.write_register(console, message),
            b'p' => self.read_register(console, message),
            b'Q' => self.process_q_write_command(message),
            b'q' => self.process_q_read_command(console, message),
            b's' => self.step(console),
            b'V' => self.process_v_write_command(message),
            b'v' => self.process_v_read_command(debugger, console, message),
            b'X' => self.write_memory_binary(console, message),
            b'Z' => self.set_point(console, message),
            b'z' => self.clear_point(console, message),
            _ => self.error(GDB_UNSUPPORTED_COMMAND),
        }
    }

    // --- command handlers (same names as the C statics) ---

    fn do_continue(&mut self, _message: &[u8]) {
        // _continue (TODO: the C ignores the optional address argument)
        self.until_poll = GDB_STUB_INTERVAL;
        self.paused = false;
        // mDebuggerModuleSetNeedsCallback
        self.needs_callback = true;
    }

    fn read_pc(console: &dyn DebugConsole) -> i32 {
        // _readPC: architectural PC = r15 - pipeline offset
        let pc = console.dbg_read_register("r15").unwrap_or(0);
        pc - if console.dbg_is_thumb() { 2 } else { 4 }
    }

    fn read_gprs(&mut self, console: &mut dyn DebugConsole) {
        let mut out = Vec::with_capacity(17 * 8);
        // General purpose registers r0..r14
        for r in 0..15 {
            let name = format!("r{r}");
            let value = console.dbg_read_register(&name).unwrap_or(0) as u32;
            int2hex32(value, &mut out);
        }
        // Program counter
        int2hex32(Self::read_pc(console) as u32, &mut out);
        // CPU status
        let cpsr = console.dbg_read_register("cpsr").unwrap_or(0) as u32;
        int2hex32(cpsr, &mut out);
        self.outgoing = out;
        self.send_message();
    }

    fn write_gprs(&mut self, console: &mut dyn DebugConsole, message: &[u8]) {
        // NB: the C parses the eight hex digits per register without the
        // byte-swap the `P` handler applies (g/G are asymmetric upstream).
        let mut read_address = message;
        for r in 0..16 {
            let value = hex2int(read_address, 8);
            console.dbg_write_register(&format!("r{r}"), value as i32);
            read_address = &read_address[8.min(read_address.len())..];
        }
        // C: ARMWritePC/ThumbWritePC to reload the pipeline after the r15
        // write — not reachable through the generic DebugConsole interface.
        self.outgoing = b"OK".to_vec();
        self.send_message();
    }

    fn read_register(&mut self, console: &mut dyn DebugConsole, message: &[u8]) {
        let (reg, _) = read_hex(message);
        let value: u32;
        if reg < 15 {
            value = console
                .dbg_read_register(&format!("r{reg}"))
                .unwrap_or(0) as u32;
        } else if reg == 15 {
            value = Self::read_pc(console) as u32;
        } else if reg == 0x19 {
            value = console.dbg_read_register("cpsr").unwrap_or(0) as u32;
        } else {
            self.outgoing = Vec::new();
            self.send_message();
            return;
        }
        let mut out = Vec::with_capacity(8);
        int2hex32(value, &mut out);
        self.outgoing = out;
        self.send_message();
    }

    fn write_register(&mut self, console: &mut dyn DebugConsole, message: &[u8]) {
        let (reg, i) = read_hex(message);
        let read_address = &message[(i + 1).min(message.len())..];
        let value = hex2int32_le(read_address);

        if reg <= 15 {
            console.dbg_write_register(&format!("r{reg}"), value as i32);
            // C reloads the pipeline for r15 (see write_gprs).
        } else if reg == 0x19 {
            console.dbg_write_register("cpsr", value as i32);
        } else {
            self.outgoing = Vec::new();
            self.send_message();
            return;
        }
        self.outgoing = b"OK".to_vec();
        self.send_message();
    }

    fn read_memory(&mut self, console: &mut dyn DebugConsole, message: &[u8]) {
        let (address, i) = read_hex(message);
        let read_address = &message[(i + 1).min(message.len())..];
        let (size, _) = read_hex(read_address);
        if size > 512 {
            self.error(GDB_BAD_ARGUMENTS);
            return;
        }
        // C: cpu->memory.load8 per byte; the generic interface reads through
        // the raw (no-side-effect) view instead.
        let mut out = Vec::with_capacity(size as usize * 2);
        for i in 0..size {
            let byte = console.dbg_raw_read(address.wrapping_add(i), -1, 1) as u8;
            int2hex8(byte, &mut out);
        }
        self.outgoing = out;
        self.send_message();
    }

    fn write_memory(&mut self, console: &mut dyn DebugConsole, message: &[u8]) {
        let (address, i) = read_hex(message);
        let mut read_address = &message[(i + 1).min(message.len())..];
        let (size, i) = read_hex(read_address);
        read_address = &read_address[(i + 1).min(read_address.len())..];
        if size > 512 {
            self.error(GDB_BAD_ARGUMENTS);
            return;
        }
        for i in 0..size as usize {
            let byte = hex2int(&read_address[(2 * i).min(read_address.len())..], 2) as u8;
            console.dbg_raw_write(address.wrapping_add(i as u32), -1, 1, byte as u32);
        }
        self.outgoing = b"OK".to_vec();
        self.send_message();
    }

    fn write_memory_binary(&mut self, console: &mut dyn DebugConsole, message: &[u8]) {
        let (address, i) = read_hex(message);
        let mut read_address = &message[(i + 1).min(message.len())..];
        let (size, i) = read_hex(read_address);
        read_address = &read_address[(i + 1).min(read_address.len())..];
        if size > 512 {
            self.error(GDB_BAD_ARGUMENTS);
            return;
        }
        let mut idx = 0usize;
        for i in 0..size as usize {
            let Some(&byte) = read_address.get(idx) else {
                break;
            };
            idx += 1;
            // Parse escape char
            let byte = if byte == 0x7D {
                let b = read_address.get(idx).copied().unwrap_or(0) ^ 0x20;
                idx += 1;
                b
            } else {
                byte
            };
            console.dbg_raw_write(address.wrapping_add(i as u32), -1, 1, byte as u32);
        }
        self.outgoing = b"OK".to_vec();
        self.send_message();
    }

    fn step(&mut self, console: &mut dyn DebugConsole) {
        let pc = console.dbg_read_register("r15").unwrap_or(0) as u32;
        console.dbg_step();
        if pc >= GBA_SIZE_BIOS && (console.dbg_read_register("r15").unwrap_or(0) as u32) < GBA_SIZE_BIOS
        {
            // GDB cannot cope with jumps into BIOS: skip over them by placing
            // a temporary breakpoint at PC (instruction after the jump) and
            // then continue without sending a GDB SIGTRAP
            let breakpoint = Breakpoint {
                id: -1,
                address: pc,
                segment: -1,
                ty: BreakpointType::Hardware,
                condition: None,
                disabled: false,
                is_temporary: true,
            };
            console.dbg_set_breakpoint(self.index, &breakpoint);
            self.do_continue(&[]);
            return;
        }
        self.outgoing = sig_reply(b'S', SIGTRAP);
        self.send_message();
    }

    fn set_point(&mut self, console: &mut dyn DebugConsole, message: &[u8]) {
        // _setBreakpoint; message = "<type>,<address>[,<kind>]"
        if message.len() < 3 {
            self.outgoing = Vec::new();
            self.send_message();
            return;
        }
        let ty = message[0];
        let (address, _) = read_hex(&message[2..]);
        // NB: the C ignores the kind/length; watchpoints cover 1 byte.

        let breakpoint = Breakpoint {
            id: -1,
            address,
            segment: -1,
            ty: BreakpointType::Hardware,
            condition: None,
            disabled: false,
            is_temporary: false,
        };
        let mut watchpoint = Watchpoint {
            id: -1,
            segment: -1,
            min_address: address,
            max_address: address.wrapping_add(1),
            ty: 0,
            condition: None,
            disabled: false,
        };

        match ty {
            b'0' | b'1' => {
                console.dbg_set_breakpoint(self.index, &breakpoint);
            }
            b'2' => {
                watchpoint.ty = if self.watchpoints_behavior
                    == WatchpointsBehavior::OverrideLogicAnyWrite
                {
                    watchpoint_type::WRITE
                } else {
                    watchpoint_type::WRITE_CHANGE
                };
                console.dbg_set_watchpoint(self.index, &watchpoint);
            }
            b'3' => {
                watchpoint.ty = watchpoint_type::READ;
                console.dbg_set_watchpoint(self.index, &watchpoint);
            }
            b'4' => {
                watchpoint.ty = watchpoint_type::RW;
                console.dbg_set_watchpoint(self.index, &watchpoint);
            }
            _ => {
                self.outgoing = Vec::new();
                self.send_message();
                return;
            }
        }
        self.outgoing = b"OK".to_vec();
        self.send_message();
    }

    fn clear_point(&mut self, console: &mut dyn DebugConsole, message: &[u8]) {
        // _clearBreakpoint
        if message.len() >= 3 {
            let ty = message[0];
            let (address, _) = read_hex(&message[2..]);
            match ty {
                b'0' | b'1' => {
                    for bp in console.dbg_list_breakpoints(self.index) {
                        if bp.address != address {
                            continue;
                        }
                        console.dbg_clear_breakpoint(bp.id);
                    }
                }
                b'2' | b'3' | b'4' => {
                    for wp in console.dbg_list_watchpoints(self.index) {
                        if address >= wp.min_address && address < wp.max_address {
                            console.dbg_clear_breakpoint(wp.id);
                        }
                    }
                }
                _ => {}
            }
        }
        self.outgoing = b"OK".to_vec();
        self.send_message();
    }

    fn write_host_info(&mut self) {
        self.outgoing = format!(
            "cputype:{MACH_O_ARM};cpusubtype:{MACH_O_ARM_V4T}:ostype:none;vendor:none;endian:little;ptrsize:4;"
        )
        .into_bytes();
        self.send_message();
    }

    fn process_q_supported_command(&mut self, message: &[u8]) {
        self.supports_swbreak = false;
        self.supports_hwbreak = false;
        for token in message.split(|&b| b == b';') {
            if token == b"swbreak+" {
                self.supports_swbreak = true;
            } else if token == b"hwbreak+" {
                self.supports_hwbreak = true;
            } else if token == b"swbreak-" {
                self.supports_swbreak = false;
            } else if token == b"hwbreak-" {
                self.supports_hwbreak = false;
            }
        }
        self.outgoing = b"swbreak+;hwbreak+;qXfer:features:read+;qXfer:memory-map:read+;QStartNoAckMode+".to_vec();
    }

    fn process_q_xfer_command(&mut self, params: &[u8], data: &[u8]) {
        let mut index = 0usize;
        let mut offset = 0usize;
        while index < params.len() && params[index] != b',' {
            offset <<= 4;
            offset |= hex2int(&params[index..], 1) as usize;
            index += 1;
        }
        if index >= params.len() {
            self.error(GDB_BAD_ARGUMENTS);
            return;
        }
        index += 1;
        let mut length = 0usize;
        while index < params.len() {
            length <<= 4;
            length |= hex2int(&params[index..], 1) as usize;
            index += 1;
        }
        length += 1;
        if length + 4 > GDB_STUB_MAX_LINE {
            length = GDB_STUB_MAX_LINE - 4;
        }
        if data.len() < offset {
            self.error(GDB_BAD_ARGUMENTS);
            return;
        }
        let mut out = Vec::with_capacity(length);
        if data.len() < length + offset {
            out.push(b'l');
            out.extend_from_slice(&data[offset..]);
        } else {
            out.push(b'm');
            out.extend_from_slice(&data[offset..offset + length - 1]);
        }
        self.outgoing = out;
    }

    fn generate_memory_map_xml(&mut self, console: &mut dyn DebugConsole) {
        let mut xml = String::from("<memory-map version=\"1.0\">");
        for block in console.dbg_list_memory_blocks() {
            let ty = if block.writable { "ram" } else { "rom" };
            xml.push_str(&format!(
                "<memory type=\"{ty}\" start=\"0x{:08x}\" length=\"0x{:08x}\"/>",
                block.start, block.size
            ));
        }
        xml.push_str("</memory-map>");
        self.memory_map_xml = xml;
    }

    fn process_q_read_command(&mut self, console: &mut dyn DebugConsole, message: &[u8]) {
        self.outgoing = Vec::new();
        if message == b"HostInfo" {
            self.write_host_info();
            return;
        }
        if message == b"Attached" {
            self.outgoing = b"1".to_vec();
        } else if message == b"VAttachOrWaitSupported" {
            self.outgoing = b"OK".to_vec();
        } else if message == b"C" {
            self.outgoing = b"QC1".to_vec();
        } else if message == b"fThreadInfo" {
            self.outgoing = b"m1".to_vec();
        } else if message == b"sThreadInfo" {
            self.outgoing = b"l".to_vec();
        } else if let Some(params) = strip_prefix(message, b"Xfer:features:read:target.xml:") {
            self.process_q_xfer_command(params, TARGET_XML.as_bytes());
        } else if let Some(params) = strip_prefix(message, b"Xfer:memory-map:read::") {
            if self.memory_map_xml.is_empty() {
                self.generate_memory_map_xml(console);
            }
            let xml = std::mem::take(&mut self.memory_map_xml);
            self.process_q_xfer_command(params, xml.as_bytes());
            self.memory_map_xml = xml;
        } else if let Some(params) = strip_prefix(message, b"Supported:") {
            self.process_q_supported_command(params);
        }
        self.send_message();
    }

    fn process_q_write_command(&mut self, message: &[u8]) {
        self.outgoing = Vec::new();
        if message == b"StartNoAckMode" {
            self.line_ack = AckState::Off;
            self.outgoing = b"OK".to_vec();
        }
        self.send_message();
    }

    fn process_v_write_command(&mut self, _message: &[u8]) {
        self.outgoing = Vec::new();
        self.send_message();
    }

    fn process_v_read_command(
        &mut self,
        _debugger: &mut Debugger,
        console: &mut dyn DebugConsole,
        message: &[u8],
    ) {
        self.outgoing = Vec::new();
        if starts_with(message, b"Attach") {
            self.outgoing = b"1".to_vec();
            self.paused = true;
            // C: mDebuggerEnter(ENTER_MANUAL, target=self), which overwrites
            // the "1" payload, sends "S02", and then the final send_message
            // below sends it again. Both effects are mirrored.
            self.entered_hook(console, DebuggerEntryReason::Manual, None);
        }
        self.send_message();
    }

    /// Module-hook entry dispatch shared with `entered()` (the C reaches this
    /// via _gdbStubEntered from mDebuggerEnter; the stub also invokes it
    /// internally for Ctrl-C / vAttach where the C targets itself).
    fn entered_hook(
        &mut self,
        _console: &mut dyn DebugConsole,
        reason: DebuggerEntryReason,
        info: Option<&DebuggerEntryInfo>,
    ) {
        match reason {
            DebuggerEntryReason::Manual => {
                self.outgoing = sig_reply(b'S', SIGINT);
            }
            DebuggerEntryReason::Breakpoint => {
                if self.supports_hwbreak && self.supports_swbreak && info.is_some() {
                    let hw = match info.unwrap().type_info {
                        Some(EntryTypeInfo::Bp(bp)) => bp.break_type != BreakpointType::Software,
                        _ => true,
                    };
                    let mut out = sig_reply(b'T', SIGTRAP);
                    out.push(if hw { b'h' } else { b's' });
                    out.extend_from_slice(b"wbreak:;");
                    self.outgoing = out;
                } else {
                    let mut out = sig_reply(b'S', SIGTRAP);
                    out.push(b'k');
                    self.outgoing = out;
                }
            }
            DebuggerEntryReason::Watchpoint => {
                if let Some(info) = info {
                    let watch_type = match info.type_info {
                        Some(EntryTypeInfo::Wp(wp)) => wp.watch_type,
                        _ => 0,
                    };
                    if self.watchpoints_behavior != WatchpointsBehavior::StandardLogic
                        && watch_type & watchpoint_type::WRITE != 0
                    {
                        // We send S05 instead of T05watch because it bypasses
                        // GDB's internal logic to check if the value changed
                        // and to bypass a step into by GDB. This allows to
                        // control the change logic even when using savestates
                        // which we already handle in the core debugger logic
                        self.outgoing = sig_reply(b'S', SIGTRAP);
                        self.send_message();
                        return;
                    }
                    let ty = match watch_type {
                        watchpoint_type::WRITE | watchpoint_type::WRITE_CHANGE => "watch",
                        watchpoint_type::READ => "rwatch",
                        watchpoint_type::RW => "awatch",
                        _ => "",
                    };
                    let mut out = sig_reply(b'T', SIGTRAP);
                    out.extend_from_slice(format!("{ty}:{:08x};", info.address).as_bytes());
                    self.outgoing = out;
                } else {
                    self.outgoing = sig_reply(b'S', SIGTRAP);
                }
            }
            DebuggerEntryReason::IllegalOp => {
                self.outgoing = sig_reply(b'S', SIGILL);
            }
            DebuggerEntryReason::Attached | DebuggerEntryReason::Stack => {
                return;
            }
        }
        self.send_message();
    }
}

fn strip_prefix<'a>(b: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    if starts_with(b, prefix) {
        Some(&b[prefix.len()..])
    } else {
        None
    }
}

impl DebuggerModule for GdbStub {
    fn module_type(&self) -> DebuggerType {
        DebuggerType::Gdb
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn is_paused(&self) -> bool {
        self.paused
    }

    fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    fn needs_callback(&self) -> bool {
        self.needs_callback
    }

    fn set_needs_callback(&mut self, needs: bool) {
        self.needs_callback = needs;
    }

    fn debugger_state(&self) -> DebuggerState {
        if self.paused {
            DebuggerState::Paused
        } else if self.needs_callback {
            DebuggerState::Callback
        } else {
            DebuggerState::Running
        }
    }

    fn set_module_index(&mut self, idx: usize) {
        self.index = Some(idx);
    }

    fn init(&mut self, _debugger: &mut Debugger, _console: &mut dyn DebugConsole) {
        // C: init = NULL
    }

    fn deinit(&mut self, _debugger: &mut Debugger, _console: &mut dyn DebugConsole) {
        // _gdbStubDeinit
        if self.listener.is_some() {
            self.shutdown();
        }
    }

    /// _gdbStubWait -> GDBStubUpdate(timeoutMs): poll up to `timeout_ms` for
    /// client traffic. Implemented without blocking sockets: nonblocking
    /// pump in 1ms slices.
    fn paused(
        &mut self,
        debugger: &mut Debugger,
        console: &mut dyn DebugConsole,
        timeout_ms: i32,
    ) {
        let mut remaining = timeout_ms.max(0);
        loop {
            GdbStub::update(self, debugger, console);
            if remaining <= 0 || !self.paused {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
            remaining -= 1;
        }
    }

    /// _gdbStubUpdate -> GDBStubUpdate(0)
    fn update(&mut self, debugger: &mut Debugger, console: &mut dyn DebugConsole) {
        GdbStub::update(self, debugger, console);
    }

    /// _gdbStubEntered
    fn entered(
        &mut self,
        _debugger: &mut Debugger,
        console: &mut dyn DebugConsole,
        reason: DebuggerEntryReason,
        info: Option<&DebuggerEntryInfo>,
    ) {
        self.entered_hook(console, reason, info);
    }

    /// _gdbStubPoll: poll every GDB_STUB_INTERVAL callbacks.
    fn custom(&mut self, debugger: &mut Debugger, console: &mut dyn DebugConsole) {
        self.until_poll -= 1;
        if self.until_poll > 0 {
            return;
        }
        self.until_poll = GDB_STUB_INTERVAL;
        GdbStub::update(self, debugger, console);
    }

    fn interrupt(&mut self, _debugger: &mut Debugger, _console: &mut dyn DebugConsole) {
        // C: interrupt = NULL
    }
}
