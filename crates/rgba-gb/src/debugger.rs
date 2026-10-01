// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Ported from mgba/src/sm83/debugger/debugger.c,
// mgba/src/sm83/debugger/memory-debugger.c, and the GB-side glue from
// mgba/src/gb/core.c (register access, raw memory ops, identifier lookup,
// debugger attach/detach) and mgba/src/gb/gb.c (illegal-op entry).
//
// The C `mDebuggerPlatform` vtable maps to `impl DebugConsole for Gb`
// (generic surface used by debugger modules) plus inherent `Gb` methods.
// The memory "shim" is a flag-checked hook in `load8`/`store8` instead of
// C's function-pointer swap (equivalent semantics, cheaper).

use rgba_debugger::access_logger::{AccessLoggerCore, CoreMemoryBlock};
use rgba_debugger::debugger::*;
use rgba_debugger::parser;
use rgba_debugger::DebugConsole;

use crate::cpu::decoder;
use crate::gb::Gb;
use crate::io::GB_IO_REGISTER_NAMES;

/// mGBA's SM83Debugger platform state. Owned by `Gb::debugger`.
pub struct GbDebugger {
    pub core: Debugger,
    pub breakpoints: Vec<Breakpoint>,
    pub watchpoints: Vec<Watchpoint>,
    pub next_id: i64,
    /// Set while a watchpoint check is reading the old value, so the inner
    /// bus access doesn't recurse into the shim (C: originalMemory calls).
    pub in_watchpoint_check: bool,
    /// Active access-logger core (C: the access logger's per-region
    /// watchpoints; here the load8/store8 shim hooks record into it).
    pub access_log: Option<Box<AccessLoggerCore>>,
}

impl GbDebugger {
    fn new() -> Self {
        GbDebugger {
            core: Debugger::new(),
            breakpoints: Vec::new(),
            watchpoints: Vec::new(),
            next_id: 1,
            in_watchpoint_check: false,
            access_log: None,
        }
    }

    /// Whether the load8/store8 shim hooks have work to do — the meaning
    /// of `Gb::dbg_watchpoints_active` (C: shim installed). The shim stays
    /// installed while watchpoints exist OR an access logger is attached.
    pub fn dbg_memory_shim_active(&self) -> bool {
        !self.watchpoints.is_empty() || self.access_log.is_some()
    }

    fn destroy_breakpoint(&mut self, index: usize) {
        self.core.point_owner.remove(&self.breakpoints[index].id);
        self.breakpoints.remove(index);
    }

    fn destroy_watchpoint(&mut self, index: usize) {
        self.core.point_owner.remove(&self.watchpoints[index].id);
        self.watchpoints.remove(index);
    }

    /// SM83DebuggerCheckBreakpoints
    fn check_breakpoints(&mut self, gb: &mut Gb) {
        let mut i = 0;
        while i < self.breakpoints.len() {
            let breakpoint = &self.breakpoints[i];
            if breakpoint.disabled {
                i += 1;
                continue;
            }
            if breakpoint.address as u16 != gb.cpu.pc {
                i += 1;
                continue;
            }
            let segment = gb.current_segment(gb.cpu.pc);
            if breakpoint.segment >= 0 && breakpoint.segment != segment {
                i += 1;
                continue;
            }
            if let Some(condition) = &self.breakpoints[i].condition {
                let cond = condition.clone();
                match parser::evaluate_parse_tree(&mut self.core, gb, &cond) {
                    Some((value, seg)) if value != 0 || seg >= 0 => {}
                    _ => {
                        i += 1;
                        continue;
                    }
                }
            }
            let id = self.breakpoints[i].id;
            let address = self.breakpoints[i].address;
            let is_temporary = self.breakpoints[i].is_temporary;
            let mut info = DebuggerEntryInfo {
                address,
                segment,
                point_id: id,
                target: self.core.point_owner(id),
                ..Default::default()
            };
            self.core.enter(gb, DebuggerEntryReason::Breakpoint, Some(&mut info));
            if is_temporary {
                self.destroy_breakpoint(i);
                continue;
            }
            i += 1;
        }
    }

    /// _checkWatchpoints in memory-debugger.c. `ty` is a watchpoint_type
    /// access bit (READ/WRITE); `new_value` is the value being written.
    fn check_watchpoints(&mut self, gb: &mut Gb, address: u16, ty: u8, new_value: u8) {
        for i in 0..self.watchpoints.len() {
            let watchpoint = &self.watchpoints[i];
            if watchpoint.ty & ty == 0
                || (address as u32) < watchpoint.min_address
                || (address as u32) >= watchpoint.max_address
            {
                continue;
            }
            if watchpoint.segment >= 0
                && watchpoint.segment != gb.current_segment(address)
            {
                continue;
            }
            if watchpoint.disabled {
                continue;
            }
            if let Some(condition) = &self.watchpoints[i].condition {
                let cond = condition.clone();
                match parser::evaluate_parse_tree(&mut self.core, gb, &cond) {
                    Some((value, seg)) if value != 0 || seg >= 0 => {}
                    _ => continue,
                }
            }
            self.in_watchpoint_check = true;
            let (old_value, id, watch_ty, max_check) = {
                let w = &self.watchpoints[i];
                (
                    gb.load8(address),
                    w.id,
                    w.ty,
                    w.min_address <= address as u32 && (address as u32) < w.max_address,
                )
            };
            self.in_watchpoint_check = false;
            let _ = max_check;
            if (watch_ty & watchpoint_type::CHANGE) != 0 && new_value == old_value {
                continue;
            }
            let segment = gb.current_segment(address);
            let mut info = DebuggerEntryInfo {
                type_info: Some(EntryTypeInfo::Wp(WatchpointEntryInfo {
                    old_value: old_value as u32,
                    new_value: new_value as u32,
                    watch_type: watch_ty,
                    access_type: ty,
                    access_source: MemoryAccessSource::Unknown,
                })),
                address: address as u32,
                segment,
                width: 1,
                point_id: id,
                target: self.core.point_owner(id),
            };
            self.core
                .enter(gb, DebuggerEntryReason::Watchpoint, Some(&mut info));
        }
    }
}

impl Gb {
    /// Create + attach the debugger (mDebuggerInit + GB attach hooks).
    pub fn debugger_attach(&mut self) {
        if self.debugger.is_some() {
            return;
        }
        let mut dbg = Box::new(GbDebugger::new());
        dbg.core.debugger_attach_init(self);
        self.debugger = Some(dbg);
    }

    /// Detach and shut the debugger down (mDebuggerDeinit + detach hooks).
    pub fn debugger_detach(&mut self) {
        if let Some(mut dbg) = self.debugger.take() {
            dbg.core.debugger_attach_deinit(self);
        }
        self.dbg_watchpoints_active = false;
    }

    /// Drives the console for one frame under debugger control.
    /// Mirrors the mCoreThread loop calling mDebuggerRunFrame.
    pub fn debugger_run_frame(&mut self) {
        let Some(mut dbg) = self.debugger.take() else {
            self.run_frame();
            return;
        };
        dbg.core.run_frame(self);
        self.debugger = Some(dbg);
    }


    /// mDebuggerAttachModule (take/restore dance hidden from callers).
    pub fn debugger_attach_module(&mut self, module: Box<dyn rgba_debugger::debugger::DebuggerModule>) -> usize {
        let Some(mut dbg) = self.debugger.take() else {
            return usize::MAX;
        };
        let idx = dbg.core.attach_module(self, module);
        self.debugger = Some(dbg);
        idx
    }
    /// mDebuggerEnter (platform->entered hook runs first, like the C)
    pub fn debugger_enter(&mut self, reason: DebuggerEntryReason, mut info: DebuggerEntryInfo) {
        if let Some(mut dbg) = self.debugger.take() {
            // SM83DebuggerEnter
            self.cpu.next_event = self.cpu.cycles;
            dbg.core.enter(self, reason, Some(&mut info));
            self.debugger = Some(dbg);
        }
    }

    /// Called from the memory shim in load8 (READ watchpoint + access log).
    pub(crate) fn dbg_watch_load8(&mut self, address: u16) {
        let Some(mut dbg) = self.debugger.take() else {
            return;
        };
        if !dbg.in_watchpoint_check {
            if !dbg.watchpoints.is_empty() {
                dbg.check_watchpoints(self, address, watchpoint_type::READ, 0);
            }
            if let Some(log) = dbg.access_log.as_deref_mut() {
                let segment = self.current_segment(address);
                log.record_access(address as u32, segment, 1, watchpoint_type::READ, 0);
            }
        }
        self.debugger = Some(dbg);
    }

    /// Called from the memory shim in store8 (WRITE watchpoint + access log).
    pub(crate) fn dbg_watch_store8(&mut self, address: u16, value: u8) {
        let Some(mut dbg) = self.debugger.take() else {
            return;
        };
        if !dbg.in_watchpoint_check {
            if !dbg.watchpoints.is_empty() {
                dbg.check_watchpoints(self, address, watchpoint_type::WRITE, value);
            }
            if let Some(log) = dbg.access_log.as_deref_mut() {
                let segment = self.current_segment(address);
                log.record_access(address as u32, segment, 1, watchpoint_type::WRITE, 0);
            }
        }
        self.debugger = Some(dbg);
    }
}

impl DebugConsole for Gb {
    fn dbg_run_loop(&mut self) {
        // C: SM83Run. Our frontend-safe quantum is a full frame; the
        // debugger loop re-invokes run() until a frame elapses either way.
        self.run_frame();
    }

    fn dbg_step(&mut self) {
        // C: mCore->step = SM83Tick
        self.step();
    }

    fn dbg_frame_counter(&self) -> u32 {
        self.video.frame_counter
    }

    fn dbg_read_register(&self, name: &str) -> Option<i32> {
        // _GBCoreReadRegister (case-insensitive)
        let n = name.to_ascii_lowercase();
        let v = match n.as_str() {
            "b" => self.cpu.b as i32,
            "c" => self.cpu.c as i32,
            "d" => self.cpu.d as i32,
            "e" => self.cpu.e as i32,
            "a" => self.cpu.a as i32,
            "f" => self.cpu.f.packed as i32,
            "h" => self.cpu.h as i32,
            "l" => self.cpu.l as i32,
            "bc" => self.cpu.bc() as i32,
            "de" => self.cpu.de() as i32,
            "hl" => self.cpu.hl() as i32,
            "af" => self.cpu.af() as i32,
            "pc" => self.cpu.pc as i32,
            "sp" => self.cpu.sp as i32,
            _ => return None,
        };
        Some(v)
    }

    fn dbg_write_register(&mut self, name: &str, value: i32) -> bool {
        // _GBCoreWriteRegister (case-sensitive in C — lowercase only)
        match name {
            "b" => self.cpu.b = (value & 0xFF) as u8,
            "c" => self.cpu.c = (value & 0xFF) as u8,
            "d" => self.cpu.d = (value & 0xFF) as u8,
            "e" => self.cpu.e = (value & 0xFF) as u8,
            "h" => self.cpu.h = (value & 0xFF) as u8,
            "l" => self.cpu.l = (value & 0xFF) as u8,
            "a" => self.cpu.a = (value & 0xFF) as u8,
            "f" => self.cpu.f.packed = (value & 0xF0) as u8,
            "bc" => self.cpu.set_bc((value & 0xFFFF) as u16),
            "de" => self.cpu.set_de((value & 0xFFFF) as u16),
            "hl" => self.cpu.set_hl((value & 0xFFFF) as u16),
            "af" => {
                self.cpu.set_af((value & 0xFFFF) as u16);
                self.cpu.f.packed &= 0xF0;
            }
            "pc" => {
                self.cpu.pc = (value & 0xFFFF) as u16;
                self.set_active_region(self.cpu.pc);
            }
            "sp" => self.cpu.sp = (value & 0xFFFF) as u16,
            _ => return false,
        }
        true
    }

    fn dbg_raw_read(&mut self, address: u32, segment: i32, width: u32) -> u32 {
        // _GBCoreRawRead8/16/32 — GBView8
        let mut v = 0u32;
        for i in 0..width.min(4) {
            v |= (self.view8((address + i) as u16, segment) as u32) << (8 * i);
        }
        v
    }

    fn dbg_raw_write(&mut self, address: u32, segment: i32, width: u32, value: u32) {
        // _GBCoreRawWrite8/16/32 — GBPatch8
        for i in 0..width.min(4) {
            self.patch8((address + i) as u16, (value >> (8 * i)) as u8, None, segment);
        }
    }

    fn dbg_lookup_identifier(&mut self, name: &str) -> Option<(i32, i32)> {
        // _GBCoreLookupIdentifier: IO register names map to 0xFF00 | i
        for (i, reg) in GB_IO_REGISTER_NAMES.iter().enumerate() {
            if !reg.is_empty() && reg.eq_ignore_ascii_case(name) {
                return Some(((0xFF00 | i as u32) as i32, -1));
            }
        }
        None
    }

    fn dbg_current_segment(&self, address: u32) -> i32 {
        self.current_segment(address as u16)
    }

    // --- access logger console hooks (access-logger.c) ---

    fn dbg_list_memory_blocks_full(&self) -> Vec<CoreMemoryBlock> {
        // _GBMemoryBlocks / _GBCMemoryBlocks (gb/core.c)
        use crate::memory::*;
        use crate::video::{GB_SIZE_OAM, GB_SIZE_VRAM_BANK0};
        use rgba_debugger::access_logger::core_memory_flags as mf;
        const CART0: i32 = 0x0; // GB_REGION_CART_BANK0
        const VRAM: i32 = 0x8; // GB_REGION_VRAM
        const SRAM: i32 = 0xA; // GB_REGION_EXTERNAL_RAM
        const WRAM0: i32 = 0xC; // GB_REGION_WORKING_RAM_BANK0
        let b = |id, name, start: u16, end: u16, size: usize, flags| {
            CoreMemoryBlock::flat(id, name, start as u32, end as u32, size as u32, flags)
        };
        let mut blocks = vec![
            CoreMemoryBlock::flat(-1, "mem", 0, 0x10000, 0x10000, mf::VIRTUAL),
            CoreMemoryBlock {
                id: CART0,
                internal_name: "cart0",
                start: GB_BASE_CART_BANK0 as u32,
                end: (GB_BASE_CART_BANK0 + GB_SIZE_CART_BANK0 as u16 * 2) as u32,
                size: 0x800000,
                flags: mf::READ | mf::WORM | mf::MAPPED,
                max_segments: 511,
                segment_start: (GB_BASE_CART_BANK0 + GB_SIZE_CART_BANK0 as u16) as u32,
            },
        ];
        if self.model.is_cgb() {
            blocks.push(CoreMemoryBlock {
                id: VRAM,
                internal_name: "vram",
                start: GB_BASE_VRAM as u32,
                end: (GB_BASE_VRAM as usize + GB_SIZE_VRAM_BANK0) as u32,
                size: (GB_SIZE_VRAM_BANK0 * 2) as u32,
                flags: mf::RW | mf::MAPPED,
                max_segments: 1,
                segment_start: 0,
            });
            blocks.push(CoreMemoryBlock {
                id: SRAM,
                internal_name: "sram",
                start: GB_BASE_EXTERNAL_RAM as u32,
                end: (GB_BASE_EXTERNAL_RAM as usize + GB_SIZE_EXTERNAL_RAM) as u32,
                size: (GB_SIZE_EXTERNAL_RAM * 4) as u32,
                flags: mf::RW | mf::MAPPED,
                max_segments: 3,
                segment_start: 0,
            });
            blocks.push(CoreMemoryBlock {
                id: WRAM0,
                internal_name: "wram",
                start: GB_BASE_WORKING_RAM_BANK0 as u32,
                end: (GB_BASE_WORKING_RAM_BANK0 + GB_SIZE_WORKING_RAM_BANK0 as u16 * 2) as u32,
                size: (GB_SIZE_WORKING_RAM_BANK0 * 8) as u32,
                flags: mf::RW | mf::MAPPED,
                max_segments: 7,
                segment_start: GB_BASE_WORKING_RAM_BANK1 as u32,
            });
        } else {
            blocks.push(b(VRAM, "vram", GB_BASE_VRAM, (GB_BASE_VRAM as usize + GB_SIZE_VRAM_BANK0) as u16, GB_SIZE_VRAM_BANK0, mf::RW | mf::MAPPED));
            blocks.push(CoreMemoryBlock {
                id: SRAM,
                internal_name: "sram",
                start: GB_BASE_EXTERNAL_RAM as u32,
                end: (GB_BASE_EXTERNAL_RAM as usize + GB_SIZE_EXTERNAL_RAM) as u32,
                size: (GB_SIZE_EXTERNAL_RAM * 4) as u32,
                flags: mf::RW | mf::MAPPED,
                max_segments: 3,
                segment_start: 0,
            });
            blocks.push(b(WRAM0, "wram", GB_BASE_WORKING_RAM_BANK0, GB_BASE_WORKING_RAM_BANK0 + GB_SIZE_WORKING_RAM_BANK0 as u16 * 2, GB_SIZE_WORKING_RAM_BANK0 * 2, mf::RW | mf::MAPPED));
        }
        blocks.push(b(GB_BASE_OAM as i32, "oam", GB_BASE_OAM, GB_BASE_OAM + GB_SIZE_OAM as u16, GB_SIZE_OAM, mf::RW | mf::MAPPED));
        blocks.push(b(GB_BASE_IO as i32, "io", GB_BASE_IO, GB_BASE_IO + GB_SIZE_IO as u16, GB_SIZE_IO, mf::RW | mf::MAPPED));
        blocks.push(b(GB_BASE_HRAM as i32, "hram", GB_BASE_HRAM, GB_BASE_HRAM + GB_SIZE_HRAM as u16, GB_SIZE_HRAM, mf::RW | mf::MAPPED));
        blocks
    }

    fn dbg_set_access_log(
        &mut self,
        core: Option<Box<AccessLoggerCore>>,
    ) -> Option<Box<AccessLoggerCore>> {
        let dbg = self.debugger.as_mut()?;
        let old = std::mem::replace(&mut dbg.access_log, core);
        let active = dbg.dbg_memory_shim_active();
        self.dbg_watchpoints_active = active; // SM83DebuggerInstallMemoryShim/RemoveMemoryShim
        old
    }

    fn dbg_access_log(&mut self) -> Option<&mut AccessLoggerCore> {
        self.debugger.as_mut()?.access_log.as_deref_mut()
    }

    // --- mDebuggerPlatform (SM83Debugger*) ---

    fn dbg_has_breakpoints(&mut self) -> bool {
        self.debugger
            .as_ref()
            .map(|d| !d.breakpoints.is_empty() || !d.watchpoints.is_empty())
            .unwrap_or(false)
    }

    fn dbg_check_breakpoints(&mut self) {
        let Some(mut dbg) = self.debugger.take() else {
            return;
        };
        dbg.check_breakpoints(self);
        self.debugger = Some(dbg);
    }

    fn dbg_set_breakpoint(&mut self, owner: Option<usize>, bp: &Breakpoint) -> i64 {
        let Some(dbg) = self.debugger.as_mut() else {
            return -1;
        };
        let mut bp = bp.clone();
        bp.id = dbg.next_id;
        dbg.next_id += 1;
        if let Some(owner) = owner {
            dbg.core.point_owner.insert(bp.id, owner);
        }
        dbg.breakpoints.push(bp.clone());
        let _ = bp;
        bp.id
    }

    fn dbg_list_breakpoints(&self, owner: Option<usize>) -> Vec<Breakpoint> {
        let Some(dbg) = self.debugger.as_ref() else {
            return Vec::new();
        };
        match owner {
            Some(o) => dbg
                .breakpoints
                .iter()
                .filter(|b| dbg.core.point_owner(b.id) == Some(o))
                .cloned()
                .collect(),
            None => dbg.breakpoints.clone(),
        }
    }

    fn dbg_clear_breakpoint(&mut self, id: i64) -> bool {
        let Some(mut dbg) = self.debugger.take() else {
            return false;
        };
        let mut result = false;
        if let Some(i) = dbg.breakpoints.iter().position(|b| b.id == id) {
            dbg.destroy_breakpoint(i);
            result = true;
        }
        if !result {
            if let Some(i) = dbg.watchpoints.iter().position(|w| w.id == id) {
                dbg.destroy_watchpoint(i);
                // SM83DebuggerRemoveMemoryShim — but the access logger
                // keeps the shim active by itself.
                self.dbg_watchpoints_active = dbg.dbg_memory_shim_active();
                result = true;
            }
        }
        self.debugger = Some(dbg);
        result
    }

    fn dbg_toggle_breakpoint(&mut self, id: i64, status: bool) -> bool {
        let Some(dbg) = self.debugger.as_mut() else {
            return false;
        };
        if let Some(b) = dbg.breakpoints.iter_mut().find(|b| b.id == id) {
            b.disabled = !status;
            return true;
        }
        if let Some(w) = dbg.watchpoints.iter_mut().find(|w| w.id == id) {
            w.disabled = !status;
            return true;
        }
        false
    }

    fn dbg_set_watchpoint(&mut self, owner: Option<usize>, wp: &Watchpoint) -> i64 {
        let Some(dbg) = self.debugger.as_mut() else {
            return -1;
        };
        if dbg.watchpoints.is_empty() {
            self.dbg_watchpoints_active = true; // SM83DebuggerInstallMemoryShim
        }
        let dbg = self.debugger.as_mut().unwrap();
        let mut wp = wp.clone();
        wp.id = dbg.next_id;
        dbg.next_id += 1;
        if let Some(owner) = owner {
            dbg.core.point_owner.insert(wp.id, owner);
        }
        dbg.watchpoints.push(wp.clone());
        wp.id
    }

    fn dbg_list_watchpoints(&self, owner: Option<usize>) -> Vec<Watchpoint> {
        let Some(dbg) = self.debugger.as_ref() else {
            return Vec::new();
        };
        match owner {
            Some(o) => dbg
                .watchpoints
                .iter()
                .filter(|w| dbg.core.point_owner(w.id) == Some(o))
                .cloned()
                .collect(),
            None => dbg.watchpoints.clone(),
        }
    }

    fn dbg_trace(&mut self) -> String {
        // SM83DebuggerTrace
        let mut disassembly = String::new();
        let mut bytes_remaining = 1usize;
        let mut address = self.cpu.pc;
        let mut info = decoder::SM83InstructionInfo::default();
        while bytes_remaining > 0 {
            bytes_remaining -= 1;
            let instruction = self.dbg_raw_read(address as u32, -1, 1) as u8;
            disassembly.push_str(&format!("{:02X}", instruction));
            address = address.wrapping_add(1);
            bytes_remaining += decoder::sm83_decode(instruction, &mut info);
        }
        disassembly.push_str(": ");
        disassembly.push_str(&decoder::sm83_disassemble(&info, address));
        let segment = self.current_segment(self.cpu.pc);
        format!(
            "A: {:02X} F: {:02X} B: {:02X} C: {:02X} D: {:02X} E: {:02X} H: {:02X} L: {:02X} SP: {:04X} PC: {:02X}:{:04X} | {}",
            self.cpu.a, self.cpu.f.packed, self.cpu.b, self.cpu.c,
            self.cpu.d, self.cpu.e, self.cpu.h, self.cpu.l,
            self.cpu.sp, segment, self.cpu.pc, disassembly
        )
    }

    fn dbg_disassemble_line(
        &mut self,
        address: u32,
        segment: i32,
        _thumb: Option<bool>,
    ) -> (String, u32) {
        // _printLine (sm83/debugger/cli-debugger.c)
        let mut address = address as u16;
        let mut info = decoder::SM83InstructionInfo::default();
        let mut hexes = String::new();
        let mut bytes_remaining = 1usize;
        while bytes_remaining > 0 {
            bytes_remaining -= 1;
            let byte = self.dbg_raw_read(address as u32, segment, 1) as u8;
            hexes.push_str(&format!("{:02X}", byte));
            address = address.wrapping_add(1);
            bytes_remaining += decoder::sm83_decode(byte, &mut info);
        }
        let mut line = String::new();
        if segment >= 0 {
            line.push_str(&format!("{:02X}:", segment));
        }
        let start = address.wrapping_sub(info.opcode_size as u16);
        line.push_str(&format!(
            "{:04X}:  {}\t{}",
            start,
            hexes,
            decoder::sm83_disassemble(&info, address)
        ));
        (line, address as u32)
    }

    fn dbg_print_status(&mut self) -> String {
        // _printStatus (sm83/debugger/cli-debugger.c + gb/debugger/debugger.c)
        let mut out = String::new();
        let cpu = &self.cpu;
        out.push_str(&format!("A: {:02X}  F: {:02X}  (AF: {:04X})\n", cpu.a, cpu.f.packed, cpu.af()));
        out.push_str(&format!("B: {:02X}  C: {:02X}  (BC: {:04X})\n", cpu.b, cpu.c, cpu.bc()));
        out.push_str(&format!("D: {:02X}  E: {:02X}  (DE: {:04X})\n", cpu.d, cpu.e, cpu.de()));
        out.push_str(&format!("H: {:02X}  L: {:02X}  (HL: {:04X})\n", cpu.h, cpu.l, cpu.hl()));
        out.push_str(&format!("PC: {:04X}  SP: {:04X}\n", cpu.pc, cpu.sp));
        // _printFlags
        let f = &cpu.f;
        out.push_str(&format!(
            "F: [{}{}{}{}]\n",
            if f.z() { 'Z' } else { '-' },
            if f.n() { 'N' } else { '-' },
            if f.h() { 'H' } else { '-' },
            if f.c() { 'C' } else { '-' },
        ));
        out.push_str(&format!("T-cycle: {}\n", self.timing.global_time()));
        // GB-specific printStatus: IE/IF/IME + LCDC/STAT/LY + segments
        out.push_str(&format!(
            "IE: {:02X}  IF: {:02X}  IME: {}\n",
            self.memory.ie, self.memory.io[crate::io::GB_REG_IF as usize], self.memory.ime
        ));
        out.push_str(&format!(
            "LCDC: {:02X}  STAT: {:02X}  LY: {:02X}\n",
            self.memory.io[crate::io::GB_REG_LCDC as usize],
            self.memory.io[crate::io::GB_REG_STAT as usize] | 0x80,
            self.memory.io[crate::io::GB_REG_LY as usize],
        ));
        // NR: the C also prints "Next video mode"; we lack that event
        // accessor at the console level, so it is omitted.
        let (line, _) = self.dbg_disassemble_line(self.cpu.pc as u32, self.current_segment(self.cpu.pc), None);
        out.push_str(&line);
        out.push('\n');
        out
    }

    /// CLI `reset` command: Core::reset
    fn dbg_reset(&mut self) {
        self.sm83_reset();
    }

    fn dbg_global_time(&self) -> u64 {
        self.timing.global_time()
    }

    fn dbg_is_thumb(&self) -> bool {
        false
    }

    fn dbg_next_instruction_info(&mut self) -> DebuggerInstructionInfo {
        // SM83DebuggerNextInstructionInfo
        let mut info = DebuggerInstructionInfo {
            address: self.cpu.pc as u32,
            ..Default::default()
        };
        let opcode = self.cpu_load8(self.cpu.pc);
        info.width = decoder::sm83_instruction_length(opcode) as u32;
        info.segment = self.current_segment(self.cpu.pc);
        info.flags[0] = access_log_flags::ACCESS8;
        info.flags[1] = access_log_flags::ACCESS8;
        info.flags[2] = access_log_flags::ACCESS8;
        info.flags_ex[0] = access_log_flags_ex::EXECUTE_OPCODE;
        info.flags_ex[1] = access_log_flags_ex::EXECUTE_OPERAND;
        info.flags_ex[2] = access_log_flags_ex::EXECUTE_OPERAND;
        if info.width == 0 {
            info.width = 1;
            info.flags_ex[0] |= access_log_flags_ex::ERROR_ILLEGAL_OPCODE;
        }
        info
    }
}
