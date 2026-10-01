// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Tests for the GDB remote-serial-protocol stub (src/gdb.rs), driven
// through the test-support `feed_input`/`take_output` API against a mock
// ARM-ish console.

use std::collections::HashMap;

use rgba_debugger::debugger::*;
use rgba_debugger::gdb::{GdbStub, WatchpointsBehavior, GDB_STUB_MAX_LINE, TARGET_XML};
use rgba_debugger::{DebugConsole, Debugger};

// ---------------------------------------------------------------------------
// Mock console: r0..r15 + cpsr, flat bytewise memory, breakpoint tracking.

#[derive(Default)]
struct MockConsole {
    regs: [i32; 16],
    cpsr: i32,
    thumb: bool,
    mem: HashMap<u32, u8>,
    steps: u32,
    step_into_bios: bool,
    breakpoints: Vec<Breakpoint>,
    watchpoints: Vec<Watchpoint>,
    point_owner: HashMap<i64, usize>,
    cleared: Vec<i64>,
    next_id: i64,
    blocks: Vec<MemoryBlockInfo>,
}

impl MockConsole {
    fn read32(&self, address: u32) -> u32 {
        (0..4)
            .map(|i| (*self.mem.get(&(address + i)).unwrap_or(&0) as u32) << (8 * i))
            .sum()
    }
}

impl DebugConsole for MockConsole {
    fn dbg_run_loop(&mut self) {}

    fn dbg_step(&mut self) {
        self.steps += 1;
        if self.step_into_bios {
            // Pretend the stepped instruction was a BIOS call.
            self.regs[15] = 0x20;
            self.step_into_bios = false;
        }
    }

    fn dbg_frame_counter(&self) -> u32 {
        0
    }

    fn dbg_read_register(&self, name: &str) -> Option<i32> {
        if name == "cpsr" {
            return Some(self.cpsr);
        }
        name.strip_prefix('r')
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|&i| i < 16)
            .map(|i| self.regs[i])
    }

    fn dbg_write_register(&mut self, name: &str, value: i32) -> bool {
        if name == "cpsr" {
            self.cpsr = value;
            return true;
        }
        match name
            .strip_prefix('r')
            .and_then(|n| n.parse::<usize>().ok())
        {
            Some(i) if i < 16 => {
                self.regs[i] = value;
                true
            }
            _ => false,
        }
    }

    fn dbg_raw_read(&mut self, address: u32, _segment: i32, width: u32) -> u32 {
        (0..width)
            .map(|i| (*self.mem.get(&(address + i)).unwrap_or(&0) as u32) << (8 * i))
            .sum()
    }

    fn dbg_raw_write(&mut self, address: u32, _segment: i32, width: u32, value: u32) {
        for i in 0..width {
            self.mem.insert(address + i, (value >> (8 * i)) as u8);
        }
    }

    fn dbg_lookup_identifier(&mut self, _name: &str) -> Option<(i32, i32)> {
        None
    }

    fn dbg_set_breakpoint(&mut self, owner: Option<usize>, bp: &Breakpoint) -> i64 {
        self.next_id += 1;
        let id = self.next_id;
        let mut bp = bp.clone();
        bp.id = id;
        self.breakpoints.push(bp);
        if let Some(owner) = owner {
            self.point_owner.insert(id, owner);
        }
        id
    }

    fn dbg_list_breakpoints(&self, owner: Option<usize>) -> Vec<Breakpoint> {
        self.breakpoints
            .iter()
            .filter(|b| owner.map_or(true, |o| self.point_owner.get(&b.id) == Some(&o)))
            .cloned()
            .collect()
    }

    fn dbg_clear_breakpoint(&mut self, id: i64) -> bool {
        for i in 0..self.breakpoints.len() {
            if self.breakpoints[i].id == id {
                self.breakpoints.remove(i);
                self.cleared.push(id);
                return true;
            }
        }
        for i in 0..self.watchpoints.len() {
            if self.watchpoints[i].id == id {
                self.watchpoints.remove(i);
                self.cleared.push(id);
                return true;
            }
        }
        false
    }

    fn dbg_toggle_breakpoint(&mut self, _id: i64, _status: bool) -> bool {
        false
    }

    fn dbg_set_watchpoint(&mut self, owner: Option<usize>, wp: &Watchpoint) -> i64 {
        self.next_id += 1;
        let id = self.next_id;
        let mut wp = wp.clone();
        wp.id = id;
        self.watchpoints.push(wp);
        if let Some(owner) = owner {
            self.point_owner.insert(id, owner);
        }
        id
    }

    fn dbg_list_watchpoints(&self, owner: Option<usize>) -> Vec<Watchpoint> {
        self.watchpoints
            .iter()
            .filter(|w| owner.map_or(true, |o| self.point_owner.get(&w.id) == Some(&o)))
            .cloned()
            .collect()
    }

    fn dbg_trace(&mut self) -> String {
        String::new()
    }

    fn dbg_is_thumb(&self) -> bool {
        self.thumb
    }

    fn dbg_list_memory_blocks(&self) -> Vec<MemoryBlockInfo> {
        self.blocks.clone()
    }
}

// ---------------------------------------------------------------------------
// Harness: real Debugger + one GdbStub module + MockConsole.

struct Harness {
    dbg: Debugger,
    console: MockConsole,
}

fn pkt(payload: &str) -> Vec<u8> {
    pkt_bytes(payload.as_bytes())
}

fn pkt_bytes(payload: &[u8]) -> Vec<u8> {
    let checksum = payload.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    let mut out = b"$".to_vec();
    out.extend_from_slice(payload);
    out.push(b'#');
    out.extend_from_slice(format!("{checksum:02x}").as_bytes());
    out
}

/// Expected client-visible bytes for a command: ack + framed reply.
fn acked(payload: &str) -> Vec<u8> {
    let mut out = b"+".to_vec();
    out.extend_from_slice(&pkt(payload));
    out
}

impl Harness {
    fn new() -> Self {
        let mut h = Harness {
            dbg: Debugger::new(),
            console: MockConsole::default(),
        };
        h.dbg.debugger_attach_init(&mut h.console);
        let idx = h.dbg.attach_module(&mut h.console, GdbStub::create());
        assert_eq!(idx, 0);
        h
    }

    fn stub(&mut self) -> &mut GdbStub {
        self.dbg.modules[0]
            .as_mut()
            .unwrap()
            .as_any_mut()
            .downcast_mut::<GdbStub>()
            .unwrap()
    }

    /// Take the module out of the Debugger slot so `&mut Debugger` can be
    /// passed alongside it (the same dance `for_each_module` performs).
    fn with_stub<R>(
        &mut self,
        f: impl FnOnce(&mut GdbStub, &mut Debugger, &mut MockConsole) -> R,
    ) -> R {
        let mut module = self.dbg.modules[0].take().unwrap();
        let stub = module.as_any_mut().downcast_mut::<GdbStub>().unwrap();
        let r = f(stub, &mut self.dbg, &mut self.console);
        self.dbg.modules[0] = Some(module);
        r
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.with_stub(|s, d, c| s.feed_input(d, c, bytes));
    }

    fn output(&mut self) -> Vec<u8> {
        self.stub().take_output()
    }

    /// Send one packet built from `payload`, return everything the stub sent.
    fn send(&mut self, payload: &str) -> Vec<u8> {
        self.feed(&pkt(payload));
        self.output()
    }

    fn send_raw(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.feed(bytes);
        self.output()
    }

    fn entered(&mut self, reason: DebuggerEntryReason, info: Option<&DebuggerEntryInfo>) -> Vec<u8> {
        self.with_stub(|s, d, c| {
            s.entered(d, c, reason, info);
            s.take_output()
        })
    }
}

fn out_str(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

// ---------------------------------------------------------------------------

#[test]
fn stop_reason_query() {
    let mut h = Harness::new();
    let out = h.send("?");
    assert_eq!(out, acked("S02"), "{}", out_str(&out));
}

#[test]
fn unknown_command_is_unsupported_error() {
    let mut h = Harness::new();
    let out = h.send("k"); // kill: not implemented by the C either
    assert_eq!(out, acked("E07"), "{}", out_str(&out));
    let out = h.send("D"); // detach: likewise
    assert_eq!(out, acked("E07"), "{}", out_str(&out));
}

#[test]
fn checksum_error_is_nak() {
    let mut h = Harness::new();
    // '?' with a deliberately wrong checksum
    let out = h.send_raw(b"$?#00");
    assert_eq!(out, b"-");
}

#[test]
fn garbage_byte_is_nak() {
    let mut h = Harness::new();
    let out = h.send_raw(b"x");
    assert_eq!(out, b"-");
}

#[test]
fn split_packet_waits_for_completion() {
    let mut h = Harness::new();
    let p = pkt("?");
    let split = p.len() - 2;
    h.feed(&p[..split]);
    assert_eq!(h.output(), b"", "no reply before the packet is complete");
    h.feed(&p[split..]);
    assert_eq!(h.output(), acked("S02"));
}

#[test]
fn thread_packets_are_faked_ok() {
    let mut h = Harness::new();
    assert_eq!(h.send("Hg0"), acked("OK"),);
    assert_eq!(h.send("T00"), acked("OK"),);
}

#[test]
fn memory_write_then_read_roundtrip() {
    let mut h = Harness::new();
    assert_eq!(h.send("M1000,4:0102a0ff"), acked("OK"));
    assert_eq!(h.console.read32(0x1000), 0xffa00201);

    let out = h.send("m1000,4");
    assert_eq!(out, acked("0102a0ff"), "{}", out_str(&out));

    // size > 512 rejected
    assert_eq!(h.send("m1000,201"), acked("E06"));
    assert_eq!(h.send("M1000,201:00"), acked("E06"));
}

#[test]
fn binary_memory_write_unescapes() {
    let mut h = Harness::new();
    // Write 0x23 ('#', escaped as 0x7d 0x03) and 'A' to 0x1000.
    let mut payload = b"X1000,2:".to_vec();
    payload.extend_from_slice(&[0x7D, 0x03, b'A']);
    let out = h.send_raw(&pkt_bytes(&payload));
    assert_eq!(out, acked("OK"));
    assert_eq!(h.console.mem[&0x1000], 0x23);
    assert_eq!(h.console.mem[&0x1001], b'A');

    assert_eq!(h.send("X1000,201:"), acked("E06"));
}

#[test]
fn read_gprs_format() {
    let mut h = Harness::new();
    for r in 0..16 {
        h.console.regs[r] = 0x11223300 + r as i32;
    }
    h.console.cpsr = 0x1F;

    let out = h.send("g");
    let mut expected = String::new();
    // r0..r14, each as four little-endian bytes
    for r in 0..15 {
        expected.push_str(&format!("{:08x}", (0x11223300u32 + r as u32).swap_bytes()));
    }
    // pc = r15 - 4 (ARM pipeline adjustment)
    expected.push_str(&format!("{:08x}", (0x1122330Fu32 - 4).swap_bytes()));
    // cpsr
    expected.push_str(&format!("{:08x}", 0x1Fu32.swap_bytes()));
    assert_eq!(out, acked(&expected), "{}", out_str(&out));
}

#[test]
fn read_pc_adjusts_for_thumb_pipeline() {
    let mut h = Harness::new();
    h.console.regs[15] = 0x08000104;
    // pf = r15; Thumb: -2, ARM: -4, serialized little-endian
    h.console.thumb = true;
    let out = h.send("pf");
    assert_eq!(out, acked("02010008"), "{}", out_str(&out));
    h.console.thumb = false;
    let out = h.send("pf");
    assert_eq!(out, acked("00010008"), "{}", out_str(&out));
}

#[test]
fn read_write_single_register() {
    let mut h = Harness::new();
    // p0 reads r0
    h.console.regs[3] = 0xDEADBEEFu32 as i32;
    let out = h.send("p3");
    assert_eq!(out, acked("efbeadde"), "{}", out_str(&out));

    // P writes with byte swap; cpsr is register 0x19
    assert_eq!(h.send("P0=78563412"), acked("OK"));
    assert_eq!(h.console.regs[0], 0x12345678);
    assert_eq!(h.send("P19=1f000000"), acked("OK"));
    assert_eq!(h.console.cpsr, 0x1F);

    // Out-of-range register: empty reply
    let out = h.send("p1a");
    assert_eq!(out, acked(""), "{}", out_str(&out));
    let out = h.send("P1a=00000000");
    assert_eq!(out, acked(""), "{}", out_str(&out));
}

#[test]
fn write_gprs_sets_r0_through_r15() {
    let mut h = Harness::new();
    let mut payload = String::from("G");
    for r in 0..16 {
        payload.push_str(&format!("{:08x}", 0x100u32 + r as u32));
    }
    assert_eq!(h.send(&payload), acked("OK"));
    for r in 0..16 {
        assert_eq!(h.console.regs[r], 0x100 + r as i32, "r{r}");
    }
}

#[test]
fn supported_packet_negotiates_break_kinds() {
    let mut h = Harness::new();
    let out = h.send("qSupported:swbreak+;hwbreak+;multiprocess-");
    assert_eq!(
        out,
        acked("swbreak+;hwbreak+;qXfer:features:read+;qXfer:memory-map:read+;QStartNoAckMode+"),
        "{}",
        out_str(&out)
    );

    // Now that both break kinds are supported, breakpoint stops are T05.
    let info = DebuggerEntryInfo {
        address: 0x08000010,
        type_info: Some(EntryTypeInfo::Bp(BreakpointEntryInfo {
            opcode: 0,
            break_type: BreakpointType::Hardware,
        })),
        ..Default::default()
    };
    let out = h.entered(DebuggerEntryReason::Breakpoint, Some(&info));
    assert_eq!(out, pkt("T05hwbreak:;"), "{}", out_str(&out));

    let info = DebuggerEntryInfo {
        type_info: Some(EntryTypeInfo::Bp(BreakpointEntryInfo {
            opcode: 0,
            break_type: BreakpointType::Software,
        })),
        ..Default::default()
    };
    let out = h.entered(DebuggerEntryReason::Breakpoint, Some(&info));
    assert_eq!(out, pkt("T05swbreak:;"), "{}", out_str(&out));

    // Fresh stub without negotiation falls back to S05k.
    let mut h2 = Harness::new();
    let out = h2.entered(DebuggerEntryReason::Breakpoint, Some(&info));
    assert_eq!(out, pkt("S05k"), "{}", out_str(&out));
}

#[test]
fn entered_replies_for_each_reason() {
    let mut h = Harness::new();
    assert_eq!(h.entered(DebuggerEntryReason::Manual, None), pkt("S02"));
    assert_eq!(h.entered(DebuggerEntryReason::IllegalOp, None), pkt("S04"));
    assert_eq!(h.entered(DebuggerEntryReason::Attached, None), b"");
    assert_eq!(h.entered(DebuggerEntryReason::Stack, None), b"");
    // Watchpoint without info
    assert_eq!(h.entered(DebuggerEntryReason::Watchpoint, None), pkt("S05"));
}

#[test]
fn watchpoint_stop_reply_by_type() {
    let mut h = Harness::new();
    let mk = |ty: u8| DebuggerEntryInfo {
        address: 0x02000100,
        type_info: Some(EntryTypeInfo::Wp(WatchpointEntryInfo {
            old_value: 0,
            new_value: 1,
            watch_type: ty,
            access_type: 1,
            access_source: MemoryAccessSource::Program,
        })),
        ..Default::default()
    };
    assert_eq!(
        h.entered(DebuggerEntryReason::Watchpoint, Some(&mk(watchpoint_type::READ))),
        pkt("T05rwatch:02000100;")
    );
    assert_eq!(
        h.entered(DebuggerEntryReason::Watchpoint, Some(&mk(watchpoint_type::WRITE))),
        pkt("T05watch:02000100;")
    );
    assert_eq!(
        h.entered(DebuggerEntryReason::Watchpoint, Some(&mk(watchpoint_type::RW))),
        pkt("T05awatch:02000100;")
    );
}

#[test]
fn watchpoint_override_behavior_sends_plain_trap_for_writes() {
    let mut h = Harness::new();
    h.stub().set_watchpoints_behavior(WatchpointsBehavior::OverrideLogic);
    let info = DebuggerEntryInfo {
        address: 0x02000100,
        type_info: Some(EntryTypeInfo::Wp(WatchpointEntryInfo {
            old_value: 0,
            new_value: 1,
            watch_type: watchpoint_type::WRITE,
            access_type: 1,
            access_source: MemoryAccessSource::Program,
        })),
        ..Default::default()
    };
    assert_eq!(
        h.entered(DebuggerEntryReason::Watchpoint, Some(&info)),
        pkt("S05")
    );
}

#[test]
fn set_and_clear_hardware_breakpoint() {
    let mut h = Harness::new();
    assert_eq!(h.send("Z0,8000,4"), acked("OK"));
    assert_eq!(h.console.breakpoints.len(), 1);
    let bp = &h.console.breakpoints[0];
    assert_eq!(bp.address, 0x8000);
    assert_eq!(bp.ty, BreakpointType::Hardware);
    // Owned by the stub module (slot 0)
    assert_eq!(h.console.point_owner.get(&bp.id), Some(&0));
    let id = bp.id;

    // Unknown Z type: empty reply
    assert_eq!(h.send("Z5,8000,4"), acked(""));

    assert_eq!(h.send("z0,8000,4"), acked("OK"));
    assert_eq!(h.console.cleared, vec![id]);
    assert!(h.console.breakpoints.is_empty());
}

#[test]
fn z1_also_installs_hardware_breakpoint() {
    let mut h = Harness::new();
    assert_eq!(h.send("Z1,8000,4"), acked("OK"));
    assert_eq!(h.console.breakpoints[0].ty, BreakpointType::Hardware);
}

#[test]
fn set_watchpoints_map_gdb_kinds() {
    let mut h = Harness::new();
    // Standard logic: Z2 -> WRITE_CHANGE
    assert_eq!(h.send("Z2,2000100,4"), acked("OK"));
    assert_eq!(h.send("Z3,2000200,4"), acked("OK"));
    assert_eq!(h.send("Z4,2000300,4"), acked("OK"));
    let wps = &h.console.watchpoints;
    assert_eq!(wps.len(), 3);
    assert_eq!(wps[0].ty, watchpoint_type::WRITE_CHANGE);
    assert_eq!(wps[1].ty, watchpoint_type::READ);
    assert_eq!(wps[2].ty, watchpoint_type::RW);
    // 1-byte range regardless of the GDB kind field
    assert_eq!((wps[0].min_address, wps[0].max_address), (0x2000100, 0x2000101));

    // Clear the rwatch one (z3 matches the address inside a range)
    let id = wps[1].id;
    assert_eq!(h.send("z3,2000200,4"), acked("OK"));
    assert_eq!(h.console.cleared, vec![id]);
    assert_eq!(h.console.watchpoints.len(), 2);
}

#[test]
fn any_write_behavior_maps_z2_to_plain_write() {
    let mut h = Harness::new();
    h.stub()
        .set_watchpoints_behavior(WatchpointsBehavior::OverrideLogicAnyWrite);
    assert_eq!(h.send("Z2,2000100,4"), acked("OK"));
    assert_eq!(h.console.watchpoints[0].ty, watchpoint_type::WRITE);
}

#[test]
fn query_packets() {
    let mut h = Harness::new();
    assert_eq!(h.send("qAttached"), acked("1"));
    assert_eq!(h.send("qC"), acked("QC1"));
    assert_eq!(h.send("qfThreadInfo"), acked("m1"));
    assert_eq!(h.send("qsThreadInfo"), acked("l"));
    assert_eq!(h.send("qVAttachOrWaitSupported"), acked("OK"));
    assert_eq!(
        h.send("qHostInfo"),
        acked("cputype:12;cpusubtype:5:ostype:none;vendor:none;endian:little;ptrsize:4;")
    );
}

#[test]
fn target_xml_xfer() {
    let mut h = Harness::new();
    // Small chunk from the front: 'm' + exactly `length` bytes
    let out = h.send("qXfer:features:read:target.xml:0,10");
    let mut expected = b"+$m".to_vec();
    expected.extend_from_slice(&TARGET_XML.as_bytes()[..0x10]);
    expected.extend_from_slice(b"#");
    let checksum = format!(
        "{:02x}",
        {
            let mut c = 0u8;
            for b in
                std::iter::once(b'm').chain(TARGET_XML.as_bytes()[..0x10].iter().copied())
            {
                c = c.wrapping_add(b);
            }
            c
        }
    );
    expected.extend_from_slice(checksum.as_bytes());
    assert_eq!(out, expected, "{}", out_str(&out));

    // Whole thing in one shot: 'l' + the rest
    let out = h.send("qXfer:features:read:target.xml:0,556");
    let mut payload = b"l".to_vec();
    payload.extend_from_slice(TARGET_XML.as_bytes());
    assert_eq!(out, acked(&String::from_utf8(payload).unwrap()));

    // Past the end of the data: E06 — sent twice, mirroring the C (once
    // from _error inside _processQXferCommand and again from the trailing
    // _sendMessage in _processQReadCommand).
    let off = format!("{:x}", TARGET_XML.len() + 1);
    let out = h.send(&format!("qXfer:features:read:target.xml:{off},10"));
    let mut expected = acked("E06");
    expected.extend_from_slice(&pkt("E06"));
    assert_eq!(out, expected, "{}", out_str(&out));
}

#[test]
fn memory_map_xfer_uses_console_block_list() {
    let mut h = Harness::new();
    h.console.blocks = vec![
        MemoryBlockInfo {
            start: 0x02000000,
            size: 0x00040000,
            writable: true,
        },
        MemoryBlockInfo {
            start: 0x00000000,
            size: 0x00004000,
            writable: false,
        },
    ];
    let out = h.send("qXfer:memory-map:read::0,556");
    let expected_payload = concat!(
        "l<memory-map version=\"1.0\">",
        "<memory type=\"ram\" start=\"0x02000000\" length=\"0x00040000\"/>",
        "<memory type=\"rom\" start=\"0x00000000\" length=\"0x00004000\"/>",
        "</memory-map>"
    );
    assert_eq!(out, acked(expected_payload), "{}", out_str(&out));
}

#[test]
fn no_ack_mode_still_acks_like_the_c() {
    let mut h = Harness::new();
    assert_eq!(h.send("QStartNoAckMode"), acked("OK"));
    // The C stub's _ack is unconditional; QStartNoAckMode only flips the
    // (otherwise unused) lineAck state. Mirror it.
    assert_eq!(h.send("?"), acked("S02"));
}

#[test]
fn continue_sets_callback_state() {
    let mut h = Harness::new();
    h.stub().set_paused(true);
    let out = h.send("c");
    assert_eq!(out, b"+", "ack but no stop reply for continue");
    assert!(!h.stub().is_paused());
    assert!(h.stub().needs_callback());
}

#[test]
fn ctrl_c_pauses_and_reports_sigint() {
    let mut h = Harness::new();
    h.stub().set_needs_callback(true);
    let out = h.send_raw(b"\x03");
    // No ack for Ctrl-C, just the stop reply
    assert_eq!(out, pkt("S02"), "{}", out_str(&out));
    assert!(h.stub().is_paused());
}

#[test]
fn vattach_pauses_and_reports_like_the_c_double_send() {
    let mut h = Harness::new();
    let out = h.send("vAttach");
    let mut expected = b"+".to_vec();
    expected.extend_from_slice(&pkt("S02"));
    expected.extend_from_slice(&pkt("S02"));
    assert_eq!(out, expected, "{}", out_str(&out));
    assert!(h.stub().is_paused());
}

#[test]
fn single_step_replies_trap() {
    let mut h = Harness::new();
    h.console.regs[15] = 0x08000000;
    let out = h.send("s");
    assert_eq!(out, acked("S05"), "{}", out_str(&out));
    assert_eq!(h.console.steps, 1);
}

#[test]
fn single_step_into_bios_sets_temporary_breakpoint_and_continues() {
    let mut h = Harness::new();
    h.console.regs[15] = 0x08000000;
    h.console.step_into_bios = true;
    let out = h.send("s");
    assert_eq!(out, b"+", "ack but no reply until the BIOS call returns");
    assert_eq!(h.console.steps, 1);
    // Temporary hw breakpoint at the pre-step r15 (instruction after the call)
    assert_eq!(h.console.breakpoints.len(), 1);
    let bp = &h.console.breakpoints[0];
    assert_eq!(bp.address, 0x08000000);
    assert!(bp.is_temporary);
    assert_eq!(bp.ty, BreakpointType::Hardware);
    assert_eq!(h.console.point_owner.get(&bp.id), Some(&0));
    // And execution continues (callback scheduled, not paused)
    assert!(!h.stub().is_paused());
    assert!(h.stub().needs_callback());
}

#[test]
fn hangup_sends_exit_reply() {
    let mut h = Harness::new();
    h.stub().hangup();
    assert_eq!(h.output(), pkt("W00"));
}

#[test]
fn listen_binds_loopback() {
    let mut stub = GdbStub::create();
    stub.listen(0, WatchpointsBehavior::StandardLogic)
        .expect("bind 127.0.0.1:0");
    stub.shutdown();
}

#[test]
fn poll_without_connection_produces_nothing() {
    let mut h = Harness::new();
    // No listener, no feed_input: update reports "not connected".
    assert!(!h.with_stub(|s, d, c| s.update(d, c)));
    assert_eq!(h.output(), b"");
}

#[test]
fn max_line_constant_matches_c() {
    assert_eq!(GDB_STUB_MAX_LINE, 1400);
}

#[test]
fn m_hex_alpha_bytes() {
    let mut h = Harness::new();
    assert_eq!(h.send("M2000000,4:0badf00d"), acked("OK"));
    // M data is a byte sequence: 0x0b 0xad 0xf0 0x0d
    assert_eq!(h.console.read32(0x2000000), 0x0df0ad0b);
}
