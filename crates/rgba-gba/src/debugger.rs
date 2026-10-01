// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Ported from mgba/src/arm/debugger/debugger.c,
// mgba/src/arm/debugger/memory-debugger.c, and the GBA-side glue from
// mgba/src/gba/core.c (register access, raw memory ops, identifier lookup)
// and mgba/src/gba/gba.c (GBAHitStub/GBAIllegal/GBABreakpoint,
// GBASetBreakpoint/GBAClearBreakpoint, GBAAttachDebugger).
//
// The C `mDebuggerPlatform` vtable maps to `impl DebugConsole for Gba`.
// The memory "shim" is a flag-checked hook at the top of `load*`/`store*`
// instead of C's function-pointer swap (same call sites, same semantics).

use rgba_debugger::access_logger::{AccessLoggerCore, CoreMemoryBlock};
use rgba_debugger::debugger::*;
use rgba_debugger::parser;
use rgba_debugger::stack_trace::{StackTrace, StackTraceMode};
use rgba_debugger::DebugConsole;

use crate::arm;
use crate::arm::decoder as armdec;
use crate::gba::Gba;
use crate::io::GBA_IO_REGISTER_NAMES;

const WORD_SIZE_ARM: i32 = 4;
const WORD_SIZE_THUMB: i32 = 2;
const ARM_PC: usize = 15;
const ARM_LR: usize = 14;
const ARM_SP: usize = 13;

// The C encodes the debugger's CPU-component index in BKPT immediate bits.
// We have no component array; a fixed id is used and the bkpt handler enters
// the debugger whenever one is attached.
const DBG_COMPONENT_ID: i32 = 0;

/// ARMDebugBreakpoint
#[derive(Clone)]
pub struct ArmDebugBreakpoint {
    pub d: Breakpoint,
    pub sw: ArmSwBreakpoint,
}

#[derive(Clone, Copy, Default)]
pub struct ArmSwBreakpoint {
    pub opcode: u32,
    pub mode: arm::ExecutionMode,
}

/// The C `ARMDebugger` platform state. Owned by `Gba::debugger`.
pub struct GbaDebugger {
    pub core: Debugger,
    pub breakpoints: Vec<ArmDebugBreakpoint>,
    pub bp_bloom: [u64; 4],
    pub sw_breakpoints: Vec<ArmDebugBreakpoint>,
    pub watchpoints: Vec<Watchpoint>,
    pub next_id: i64,
    pub stack_trace_mode: StackTraceMode,
    pub in_watchpoint_check: bool,
    /// Set while `view8/16/32` read through the bus: C's GBAView* bypass the
    /// debug shim (they call the static GBALoad*, not the vtable), so
    /// debugger raw reads must not trip watchpoints.
    pub in_view_read: bool,
    /// Hits recorded while the module core is detached for driving
    /// (placeholder installed): the running core drains these and performs
    /// the module dispatch (C dispatches synchronously from the hit path).
    pub pending_entries: Vec<(DebuggerEntryReason, DebuggerEntryInfo)>,
    /// Active access-logger core (C: the access logger's per-region
    /// watchpoints; here the load*/store* shim hooks record into it).
    pub access_log: Option<Box<AccessLoggerCore>>,
}

impl GbaDebugger {
    fn new() -> Self {
        GbaDebugger {
            core: Debugger::new(),
            breakpoints: Vec::new(),
            bp_bloom: [0; 4],
            sw_breakpoints: Vec::new(),
            watchpoints: Vec::new(),
            next_id: 1,
            stack_trace_mode: StackTraceMode::Disabled,
            in_watchpoint_check: false,
            in_view_read: false,
            pending_entries: Vec::new(),
            access_log: None,
        }
    }

    /// Whether the load*/store* shim hooks have work to do — the meaning
    /// of `Gba::dbg_watchpoints_active` (C: shim installed). The shim
    /// stays installed while watchpoints exist OR an access logger is
    /// attached.
    pub fn dbg_memory_shim_active(&self) -> bool {
        !self.watchpoints.is_empty() || self.access_log.is_some()
    }

    /// Swap the module core out so this platform struct stays reachable from
    /// the console while `Debugger::run_*` drives modules. Returns the real
    /// core; hand it back through `merge_core_after_drive`.
    fn take_core_for_drive(&mut self) -> Debugger {
        std::mem::replace(&mut self.core, Debugger::placeholder())
    }

    fn merge_core_after_drive(&mut self, core: Debugger) {
        let placeholder = std::mem::replace(&mut self.core, core);
        self.core.merge_placeholder(placeholder);
    }

    /// Module-side dispatch for platform hits (C: mDebuggerEnter). While the
    /// core is detached for driving, queue instead.
    fn dispatch_enter(
        &mut self,
        gba: &mut Gba,
        reason: DebuggerEntryReason,
        mut info: DebuggerEntryInfo,
    ) {
        if self.core.is_placeholder() {
            self.pending_entries.push((reason, info));
        } else {
            self.core.enter(gba, reason, Some(&mut info));
        }
    }

    fn rebuild_bp_bloom(&mut self) {
        self.bp_bloom = [0; 4];
        for bp in &self.breakpoints {
            if bp.d.disabled {
                continue;
            }
            let address = bp.d.address;
            for j in 0..4 {
                self.bp_bloom[j] |= 1u64 << ((address >> (4 * j + 1)) & 0x3F);
            }
        }
    }

    fn check_bp_bloom(&self, address: u32) -> bool {
        for i in 0..4 {
            if self.bp_bloom[i] & (1u64 << ((address >> (4 * i + 1)) & 0x3F)) == 0 {
                return false;
            }
        }
        true
    }

    fn destroy_breakpoint(&mut self, index: usize) {
        self.core.point_owner.remove(&self.breakpoints[index].d.id);
        self.breakpoints.remove(index);
    }

    fn destroy_sw_breakpoint(&mut self, index: usize) {
        self.core.point_owner.remove(&self.sw_breakpoints[index].d.id);
        self.sw_breakpoints.remove(index);
    }

    fn destroy_watchpoint(&mut self, index: usize) {
        self.core.point_owner.remove(&self.watchpoints[index].id);
        self.watchpoints.remove(index);
    }

    fn instruction_length(gba: &Gba) -> i32 {
        if gba.cpu.execution_mode == arm::ExecutionMode::Arm {
            WORD_SIZE_ARM
        } else {
            WORD_SIZE_THUMB
        }
    }

    fn pc_address(gba: &Gba) -> u32 {
        (gba.cpu.gprs[ARM_PC] as u32).wrapping_sub((Self::instruction_length(gba) * 2) as u32)
    }

    /// ARMDecodeCombined: decode the prefetched instruction
    /// (returns info + whether it's a "wide" (Thumb 32-bit) instruction)
    fn decode_combined(gba: &Gba) -> (armdec::ARMInstructionInfo, bool) {
        if gba.cpu.execution_mode == arm::ExecutionMode::Arm {
            (armdec::arm_decode_arm(gba.cpu.prefetch[0]), false)
        } else {
            let info = armdec::arm_decode_thumb(gba.cpu.prefetch[0] as u16);
            let info2 = armdec::arm_decode_thumb(gba.cpu.prefetch[1] as u16);
            match armdec::arm_decode_thumb_combine(&info, &info2) {
                Some(combined) => (combined, true),
                None => (info, false),
            }
        }
    }

    /// ARMDebuggerCheckBreakpoints
    fn check_breakpoints(&mut self, gba: &mut Gba) {
        let instruction_length = Self::instruction_length(gba);
        let pc = gba.cpu.gprs[ARM_PC] as u32 - instruction_length as u32;
        if !self.stack_trace_mode.is_disabled()
            && self.update_stack_trace_internal(gba, pc)
        {
            return;
        }
        if self.breakpoints.len() > 3 && !self.check_bp_bloom(pc) {
            return;
        }
        let mut i = 0;
        while i < self.breakpoints.len() {
            let bp = &self.breakpoints[i];
            if bp.d.address != pc {
                i += 1;
                continue;
            }
            if bp.d.disabled {
                i += 1;
                continue;
            }
            if let Some(cond) = bp.d.condition.clone() {
                match parser::evaluate_parse_tree(&mut self.core, gba, &cond) {
                    Some((value, seg)) if value != 0 || seg >= 0 => {}
                    _ => {
                        i += 1;
                        continue;
                    }
                }
            }
            let id = self.breakpoints[i].d.id;
            let address = self.breakpoints[i].d.address;
            let is_temporary = self.breakpoints[i].d.is_temporary;
            let info = DebuggerEntryInfo {
                address,
                type_info: Some(EntryTypeInfo::Bp(BreakpointEntryInfo {
                    opcode: 0,
                    break_type: BreakpointType::Hardware,
                })),
                point_id: id,
                target: self.core.point_owner(id),
                ..Default::default()
            };
            self.dispatch_enter(gba, DebuggerEntryReason::Breakpoint, info);
            if is_temporary {
                self.destroy_breakpoint(i);
                self.rebuild_bp_bloom();
                continue;
            }
            i += 1;
        }
    }

    /// ARMDebuggerEnter (platform entered hook)
    fn entered(
        &mut self,
        gba: &mut Gba,
        reason: DebuggerEntryReason,
        mut info: Option<&mut DebuggerEntryInfo>,
    ) {
        gba.cpu.next_event = gba.cpu.cycles;
        if reason != DebuggerEntryReason::Breakpoint {
            return;
        }
        let pc_addr = Self::pc_address(gba);
        let mut i = 0;
        while i < self.sw_breakpoints.len() {
            if self.sw_breakpoints[i].d.address != pc_addr {
                i += 1;
                continue;
            }
            if self.sw_breakpoints[i].d.ty != BreakpointType::Software {
                i += 1;
                continue;
            }
            let sw = self.sw_breakpoints[i].sw;
            let id = self.sw_breakpoints[i].d.id;
            if let Some(info) = info.as_deref_mut() {
                info.address = self.sw_breakpoints[i].d.address;
                info.point_id = id;
            }
            self.clear_software_breakpoint(gba, i);

            gba.arm_run_fake(sw.opcode);

            if self.sw_breakpoints[i].d.is_temporary {
                self.destroy_sw_breakpoint(i);
                self.rebuild_bp_bloom();
            } else {
                self.set_software_breakpoint(gba, self.sw_breakpoints[i].d.address, sw.mode);
            }
            break;
        }
    }

    /// GBASetBreakpoint: patch the instruction word with a BKPT that
    /// encodes the (fixed) component id, returning the old opcode.
    fn gba_set_breakpoint(
        gba: &mut Gba,
        address: u32,
        mode: arm::ExecutionMode,
    ) -> Option<u32> {
        if mode == arm::ExecutionMode::Arm {
            let mut value: i32 = 0xE1200070u32 as i32;
            value |= DBG_COMPONENT_ID & 0xF;
            value |= (DBG_COMPONENT_ID & 0xFFF0) << 4;
            let mut old: i32 = 0;
            gba.patch32(address, value, Some(&mut old));
            Some(old as u32)
        } else {
            let mut value: i16 = 0xBE00u16 as i16;
            value |= (DBG_COMPONENT_ID & 0xFF) as i16;
            let mut old: i16 = 0;
            gba.patch16(address, value, Some(&mut old));
            Some((old as u16) as u32)
        }
    }

    /// GBAClearBreakpoint
    fn gba_clear_breakpoint(
        gba: &mut Gba,
        address: u32,
        mode: arm::ExecutionMode,
        opcode: u32,
    ) {
        if mode == arm::ExecutionMode::Arm {
            gba.patch32(address, opcode as i32, None);
        } else {
            gba.patch16(address, opcode as i16, None);
        }
    }

    /// setSoftwareBreakpoint hook (ARMDebugger->setSoftwareBreakpoint)
    fn set_software_breakpoint(
        &mut self,
        gba: &mut Gba,
        address: u32,
        mode: arm::ExecutionMode,
    ) -> Option<u32> {
        Self::gba_set_breakpoint(gba, address, mode)
    }

    /// clearSoftwareBreakpoint hook
    fn clear_software_breakpoint(&mut self, gba: &mut Gba, index: usize) {
        let bp = self.sw_breakpoints[index].clone();
        Self::gba_clear_breakpoint(gba, bp.d.address, bp.sw.mode, bp.sw.opcode);
    }

    /// _checkWatchpoints (memory-debugger.c)
    fn check_watchpoints(
        &mut self,
        gba: &mut Gba,
        address: u32,
        ty: u8,
        new_value: u32,
        width: i32,
    ) {
        let min_address = address & !((width as u32) - 1);
        let max_address = min_address.wrapping_add(width as u32);
        for i in 0..self.watchpoints.len() {
            let wp = &self.watchpoints[i];
            if wp.ty & ty == 0 {
                continue;
            }
            if wp.min_address >= max_address || min_address >= wp.max_address {
                continue;
            }
            if wp.disabled {
                continue;
            }
            if let Some(cond) = wp.condition.clone() {
                match parser::evaluate_parse_tree(&mut self.core, gba, &cond) {
                    Some((value, seg)) if value != 0 || seg >= 0 => {}
                    _ => continue,
                }
            }
            let mut cycle_counter = 0i32;
            self.in_watchpoint_check = true;
            let old_value = match width {
                1 => gba.load8(address, &mut cycle_counter),
                2 => gba.load16(address, &mut cycle_counter),
                4 => gba.load32(address, &mut cycle_counter),
                _ => continue,
            };
            self.in_watchpoint_check = false;
            let wp = &self.watchpoints[i];
            if (wp.ty & watchpoint_type::CHANGE) != 0 && new_value == old_value {
                continue;
            }
            let id = wp.id;
            let watch_ty = wp.ty;
            let info = DebuggerEntryInfo {
                type_info: Some(EntryTypeInfo::Wp(WatchpointEntryInfo {
                    old_value,
                    new_value,
                    watch_type: watch_ty,
                    access_type: ty,
                    access_source: MemoryAccessSource::Unknown,
                })),
                address,
                segment: 0,
                width,
                point_id: id,
                target: self.core.point_owner(id),
            };
            self.dispatch_enter(gba, DebuggerEntryReason::Watchpoint, info);
        }
    }

    // --- stack-trace (ARMDebuggerUpdateStackTraceInternal) ---

    fn frame_priv(frame_regs: &[u32]) -> arm::PrivilegeMode {
        arm::PrivilegeMode::from_bits((frame_regs[16] & 0x1F) as u8)
    }

    fn snapshot_registers(gba: &Gba) -> [u32; 18] {
        let mut regs = [0u32; 18];
        for i in 0..16 {
            regs[i] = gba.cpu.gprs[i] as u32;
        }
        regs[16] = gba.cpu.cpsr.packed as u32;
        regs[17] = gba.cpu.spsr.packed as u32;
        regs
    }

    fn update_stack_trace_internal(&mut self, gba: &mut Gba, pc: u32) -> bool {
        let mut stack = std::mem::take(&mut self.core.stack_trace);
        let r = self.update_stack_trace_internal2(gba, pc, &mut stack);
        self.core.stack_trace = stack;
        r
    }

    fn update_stack_trace_internal2(
        &mut self,
        gba: &mut Gba,
        pc: u32,
        stack: &mut StackTrace,
    ) -> bool {
        let cpu = &gba.cpu;
        let current_stack = cpu.cpsr.priv_mode().bank();
        let sp = cpu.gprs[ARM_SP] as u32;

        // Check whether frames have been popped (function returned).
        if let Some(frame) = stack.get_frame(0) {
            if (frame.frame_base_address < sp)
                && current_stack == Self::frame_priv(&frame.regs).bank()
            {
                let mut should_break = self.stack_trace_mode.has_return();
                loop {
                    let f0 = stack.get_frame(0);
                    match f0 {
                        Some(_) => {
                            should_break =
                                should_break || stack.get_frame(0).unwrap().break_when_finished;
                            stack.pop();
                        }
                        None => break,
                    }
                    let f = stack.get_frame(0);
                    let done = match f {
                        Some(f) => {
                            !(f.frame_base_address < sp)
                                || current_stack != Self::frame_priv(&f.regs).bank()
                        }
                        None => true,
                    };
                    if done {
                        break;
                    }
                }
                if should_break {
                    let info = DebuggerEntryInfo {
                        address: pc,
                        type_info: Some(EntryTypeInfo::St(StackEntryInfo {
                            trace_type: StackTraceMode::BreakOnReturn,
                        })),
                        point_id: 0,
                        ..Default::default()
                    };
                    self.dispatch_enter(gba, DebuggerEntryReason::Stack, info);
                    return true;
                }
                return false;
            }
        }

        let mut interrupt = false;
        let (info, is_wide_instruction) = Self::decode_combined(gba);
        if !is_wide_instruction && info.mnemonic == armdec::ARMMnemonic::Bl {
            return false;
        }
        if !gba.cpu.test_condition(info.condition as u32) {
            return false;
        }

        if Self::mode_has_spsr(gba.cpu.cpsr.priv_mode()) {
            let ivt_base = 0x00000000u32;
            let irq_frame_is_irq = stack
                .get_frame(0)
                .map(|f| Self::mode_has_spsr(Self::frame_priv(&f.regs)))
                .unwrap_or(false);
            if ivt_base <= pc && pc < ivt_base + 0x20 && !irq_frame_is_irq {
                let regs = Self::snapshot_registers(gba);
                let frame = stack.push(pc, pc, sp, &regs);
                frame.interrupt = true;
                interrupt = true;
            }
        }

        if info.branch_type == armdec::ARM_BRANCH_NONE && !interrupt {
            return false;
        }

        let mut is_call = info.branch_type & armdec::ARM_BRANCH_LINKED != 0;
        let dest_address: u32;

        if interrupt && !is_call {
            // The stack frame was already pushed above; only check for a
            // breakpoint below if the first instruction isn't a call.
            dest_address = pc;
        } else if info.operand_format & armdec::ARM_OPERAND_MEMORY_1 != 0 {
            // ldmia ..., {..., pc} — function return.
            let reg_count = (info.op1.immediate as u32).count_ones();
            let base_address = gba.cpu.gprs[info.memory.base_reg as usize] as u32
                + ((reg_count - 1) << 2);
            let mut cycle_counter = 0i32;
            dest_address = gba.load32(base_address, &mut cycle_counter);
        } else if info.operand_format & armdec::ARM_OPERAND_IMMEDIATE_1 != 0 {
            if !is_call {
                return false;
            }
            dest_address = (info.op1.immediate as u32)
                .wrapping_add(gba.cpu.gprs[ARM_PC] as u32);
        } else if info.operand_format & armdec::ARM_OPERAND_REGISTER_1 != 0 {
            if is_call {
                dest_address = gba.cpu.gprs[info.op1.reg as usize] as u32;
            } else {
                let is_exception_return = Self::mode_has_spsr(gba.cpu.cpsr.priv_mode())
                    && info.affects_cpsr
                    && info.op1.reg as usize == ARM_PC;
                let is_mov_pc_lr = info.operand_format & armdec::ARM_OPERAND_REGISTER_2 != 0
                    && info.op1.reg as usize == ARM_PC
                    && info.op2.reg as usize == ARM_LR;
                let mut is_branch = armdec::arm_instruction_is_branch(info.mnemonic);
                let reg = (if is_branch { info.op1.reg } else { info.op2.reg }) as usize;
                let mut dest = gba.cpu.gprs[reg] as u32;
                if !is_branch
                    && info.branch_type & armdec::ARM_BRANCH_INDIRECT != 0
                    && info.op1.reg as usize == ARM_PC
                    && info.operand_format & armdec::ARM_OPERAND_MEMORY_2 != 0
                {
                    let regs16 = Self::snapshot_gprs16(gba);
                    let ptr_address =
                        armdec::arm_resolve_memory_access(&info, &regs16, pc);
                    let mut cycle_counter = 0i32;
                    dest = gba.load32(ptr_address, &mut cycle_counter);
                }
                dest_address = dest;
                if is_branch || (info.op1.reg as usize == ARM_PC && !is_mov_pc_lr) {
                    // ARMv4 has no BLX; lr-assignment + bx is the call form.
                    let mut cycle_counter = 0i32;
                    let prev_info = if gba.cpu.execution_mode == arm::ExecutionMode::Arm {
                        let op = gba.load32(pc - 4, &mut cycle_counter);
                        armdec::arm_decode_arm(op)
                    } else {
                        let op = gba.load16(pc - 2, &mut cycle_counter) as u16;
                        armdec::arm_decode_thumb(op)
                    };
                    if (prev_info.operand_format
                        & (armdec::ARM_OPERAND_REGISTER_1 | armdec::ARM_OPERAND_AFFECTED_1))
                        == (armdec::ARM_OPERAND_REGISTER_1 | armdec::ARM_OPERAND_AFFECTED_1)
                        && prev_info.op1.reg as usize == ARM_LR
                    {
                        is_call = true;
                    } else if (if is_branch { info.op1.reg } else { info.op2.reg }) as usize
                        == ARM_LR
                    {
                        is_branch = true;
                    } else if let Some(frame) = stack.get_frame(0) {
                        // Heuristic: branch back to just past the caller's
                        // return address with a matching SP = nonstandard
                        // function return.
                        if frame.frame_base_address == sp {
                            is_branch = dest_address > frame.call_address + 1
                                && dest_address <= frame.call_address + 5;
                        } else {
                            is_branch = false;
                        }
                    } else {
                        is_branch = false;
                    }
                }
                if !is_call && !is_branch && !is_exception_return && !is_mov_pc_lr {
                    return false;
                }
            }
        } else {
            // mLOG(DEBUGGER, ERROR, "Unknown branch operand in stack trace")
            return false;
        }

        if interrupt || is_call {
            if is_call {
                let instruction_length = if is_wide_instruction {
                    WORD_SIZE_ARM
                } else {
                    if gba.cpu.execution_mode == arm::ExecutionMode::Arm {
                        WORD_SIZE_ARM
                    } else {
                        WORD_SIZE_THUMB
                    }
                };
                let regs = Self::snapshot_registers(gba);
                stack.push(pc, dest_address + instruction_length as u32, sp, &regs);
            }
            if !self.stack_trace_mode.has_call() {
                return false;
            }
        } else {
            if let Some(frame) = stack.get_frame(0) {
                if current_stack == Self::frame_priv(&frame.regs).bank() {
                    stack.pop();
                }
            }
            if !self.stack_trace_mode.has_return() {
                return false;
            }
        }
        let entry_info = DebuggerEntryInfo {
            address: pc,
            type_info: Some(EntryTypeInfo::St(StackEntryInfo {
                trace_type: if interrupt || is_call {
                    StackTraceMode::BreakOnCall
                } else {
                    StackTraceMode::BreakOnReturn
                },
            })),
            point_id: 0,
            ..Default::default()
        };
        self.dispatch_enter(gba, DebuggerEntryReason::Stack, entry_info);
        true
    }

    fn mode_has_spsr(mode: arm::PrivilegeMode) -> bool {
        !matches!(mode, arm::PrivilegeMode::System | arm::PrivilegeMode::User)
    }

    fn snapshot_gprs16(gba: &Gba) -> [u32; 16] {
        let mut r = [0u32; 16];
        for i in 0..16 {
            r[i] = gba.cpu.gprs[i] as u32;
        }
        r
    }
}

impl Gba {
    /// _ARMPCAddress (address of the currently-executing instruction)
    pub(crate) fn dbg_pc_address(&self) -> u32 {
        let ilen = if self.cpu.execution_mode == arm::ExecutionMode::Arm {
            4u32
        } else {
            2u32
        };
        (self.cpu.gprs[15] as u32).wrapping_sub(ilen * 2)
    }

    /// Create + attach the debugger (mDebuggerInit + GBA attach hooks).
    pub fn debugger_attach(&mut self) {
        if self.debugger.is_some() {
            return;
        }
        let mut dbg = Box::new(GbaDebugger::new());
        dbg.core.debugger_attach_init(self);
        self.debugger = Some(dbg);
    }

    /// Detach and shut the debugger down.
    pub fn debugger_detach(&mut self) {
        if let Some(mut dbg) = self.debugger.take() {
            // Clear software breakpoints back-to-front (C: ARMDebuggerDeinit)
            while !dbg.sw_breakpoints.is_empty() {
                let i = dbg.sw_breakpoints.len() - 1;
                dbg.clear_software_breakpoint(self, i);
                dbg.sw_breakpoints.remove(i);
            }
            dbg.core.debugger_attach_deinit(self);
        }
        self.dbg_watchpoints_active = false;
    }

    /// Drives the console for one frame under debugger control.
    pub fn debugger_run_frame(&mut self) {
        let Some(dbg) = self.debugger.as_mut() else {
            self.run_frame();
            return;
        };
        let mut core = dbg.take_core_for_drive();
        core.run_frame(self);
        if let Some(dbg) = self.debugger.as_mut() {
            dbg.merge_core_after_drive(core);
        }
    }

    /// Drives the console for one debugger poll iteration
    /// (mDebuggerRunTimeout).
    pub fn debugger_run_timeout(&mut self, timeout_ms: i32) {
        let Some(dbg) = self.debugger.as_mut() else {
            return;
        };
        let mut core = dbg.take_core_for_drive();
        core.run_timeout(self, timeout_ms);
        if let Some(dbg) = self.debugger.as_mut() {
            dbg.merge_core_after_drive(core);
        }
    }

    /// mDebuggerUpdate: poll modules (e.g. the GDB stub accepting a
    /// connection) without running emulation.
    pub fn debugger_update(&mut self) {
        let Some(dbg) = self.debugger.as_mut() else {
            return;
        };
        let mut core = dbg.take_core_for_drive();
        core.update(self);
        if let Some(dbg) = self.debugger.as_mut() {
            dbg.merge_core_after_drive(core);
        }
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
            dbg.entered(self, reason, Some(&mut info));
            dbg.dispatch_enter(self, reason, info);
            self.debugger = Some(dbg);
        }
    }

    /// Debugger watchpoint callback installed at the head of loads
    /// (CREATE_WATCHPOINT_READ_SHIM; also the access-logger record point).
    pub(crate) fn dbg_watch_load(&mut self, address: u32, width: i32, _new: u32) {
        let Some(mut dbg) = self.debugger.take() else {
            return;
        };
        if !dbg.in_watchpoint_check && !dbg.in_view_read {
            if !dbg.watchpoints.is_empty() {
                dbg.check_watchpoints(self, address, watchpoint_type::READ, 0, width);
            }
            if let Some(log) = dbg.access_log.as_deref_mut() {
                log.record_access(address, 0, width.max(1) as u32, watchpoint_type::READ, 0);
            }
        }
        self.debugger = Some(dbg);
    }

    pub(crate) fn dbg_watch_store(&mut self, address: u32, width: i32, value: u32) {
        let Some(mut dbg) = self.debugger.take() else {
            return;
        };
        if !dbg.in_watchpoint_check && !dbg.in_view_read {
            if !dbg.watchpoints.is_empty() {
                dbg.check_watchpoints(self, address, watchpoint_type::WRITE, value, width);
            }
            if let Some(log) = dbg.access_log.as_deref_mut() {
                log.record_access(address, 0, width.max(1) as u32, watchpoint_type::WRITE, 0);
            }
        }
        self.debugger = Some(dbg);
    }

    /// CREATE_MULTIPLE_WATCHPOINT_SHIM (loadMultiple/storeMultiple)
    pub(crate) fn dbg_watch_multiple(&mut self, address: u32, mask: i32, direction: i32, store: bool) {
        let Some(dbg) = self.debugger.as_mut() else {
            return;
        };
        if dbg.in_watchpoint_check || dbg.in_view_read || !dbg.dbg_memory_shim_active() {
            return;
        }
        let mut dbg = self.debugger.take().unwrap();
        let popcount = (mask as u32).count_ones() as i32;
        let mut offset = 4;
        let mut base = address as i32;
        if direction & 2 != 0 { // LSM_D
            offset = -4;
            base -= (popcount << 2) - 4;
        }
        if direction & 1 != 0 { // LSM_B
            base += offset;
        }
        for i in 0..popcount {
            let ty = if store {
                watchpoint_type::WRITE
            } else {
                watchpoint_type::READ
            };
            let addr = (base + 4 * i) as u32;
            if !dbg.watchpoints.is_empty() {
                dbg.check_watchpoints(self, addr, ty, 0, 4);
            }
            if let Some(log) = dbg.access_log.as_deref_mut() {
                log.record_access(addr, 0, 4, ty, 0);
            }
        }
        self.debugger = Some(dbg);
    }
}

fn gba_psr_mode_string(priv_mode: arm::PrivilegeMode) -> &'static str {
    match priv_mode {
        arm::PrivilegeMode::User => "usr",
        arm::PrivilegeMode::Fiq => "fiq",
        arm::PrivilegeMode::Irq => "irq",
        arm::PrivilegeMode::Supervisor => "svc",
        arm::PrivilegeMode::Abort => "abt",
        arm::PrivilegeMode::Undefined => "und",
        arm::PrivilegeMode::System => "sys",
    }
}

impl Gba {
    /// _printPSR (arm/debugger/cli-debugger.c)
    fn dbg_format_psr(packed: i32) -> String {
        let p = packed as u32;
        let pm = arm::PrivilegeMode::from_bits((p & 0x1F) as u8);
        format!(
            "{:08X} [{}{}{}{}{}{}{}] ({})\n",
            p,
            if p & (1 << 31) != 0 { 'N' } else { '-' },
            if p & (1 << 30) != 0 { 'Z' } else { '-' },
            if p & (1 << 29) != 0 { 'C' } else { '-' },
            if p & (1 << 28) != 0 { 'V' } else { '-' },
            if p & (1 << 7) != 0 { 'I' } else { '-' },
            if p & (1 << 6) != 0 { 'F' } else { '-' },
            if p & (1 << 5) != 0 { 'T' } else { '-' },
            gba_psr_mode_string(pm),
        )
    }
}

impl DebugConsole for Gba {
    fn dbg_run_loop(&mut self) {
        self.run_frame();
    }

    fn dbg_step(&mut self) {
        // mCore->step = ARMStep
        self.step();
    }

    fn dbg_frame_counter(&self) -> u32 {
        self.video.frame_counter
    }

    fn dbg_read_register(&self, name: &str) -> Option<i32> {
        // _GBACoreReadRegister
        let cpu = &self.cpu;
        let (first, rest) = name.split_at(1);
        match first {
            "r" | "R" => {}
            "c" | "C" => {
                if rest.eq_ignore_ascii_case("psr") {
                    return Some(cpu.cpsr.packed);
                }
                return None;
            }
            "i" | "I" => {
                if rest.eq_ignore_ascii_case("p") {
                    return Some(cpu.gprs[12]);
                }
                return None;
            }
            "s" | "S" => {
                if rest.eq_ignore_ascii_case("p") {
                    return Some(cpu.gprs[ARM_SP]);
                }
                return None;
            }
            "l" | "L" => {
                if rest.eq_ignore_ascii_case("r") {
                    return Some(cpu.gprs[ARM_LR]);
                }
                return None;
            }
            "p" | "P" => {
                if rest.eq_ignore_ascii_case("c") {
                    return Some(cpu.gprs[ARM_PC]);
                }
                return None;
            }
            _ => return None,
        }
        // rNN
        let reg_id: u32 = rest.parse().ok()?;
        if reg_id > 15 {
            return None;
        }
        Some(cpu.gprs[reg_id as usize])
    }

    fn dbg_write_register(&mut self, name: &str, value: i32) -> bool {
        // _GBACoreWriteRegister
        let (first, rest) = name.split_at(1);
        match first {
            "r" | "R" => {}
            "c" | "C" => {
                if rest == "psr" {
                    let pc = self.cpu.gprs[ARM_PC] & -WORD_SIZE_THUMB;
                    let mode = self.cpu.execution_mode;
                    self.cpu.cpsr.packed = value & 0xF00000FFu32 as i32;
                    self.arm_read_cpsr();
                    if mode != self.cpu.execution_mode {
                        // Mode changed, flush prefetch
                        let mut cc = 0i32;
                        if self.cpu.execution_mode == arm::ExecutionMode::Arm {
                            let pc = pc & -WORD_SIZE_ARM;
                            self.cpu.prefetch[0] =
                                self.load32((pc - WORD_SIZE_ARM) as u32 & self.cpu.active_mask, &mut cc);
                        } else {
                            self.cpu.prefetch[0] =
                                self.load16((pc - WORD_SIZE_THUMB) as u32 & self.cpu.active_mask, &mut cc);
                        }
                        self.cpu.prefetch[1] = match self.cpu.execution_mode {
                            arm::ExecutionMode::Arm => self.load32(pc as u32 & self.cpu.active_mask, &mut cc),
                            arm::ExecutionMode::Thumb => self.load16(pc as u32 & self.cpu.active_mask, &mut cc),
                        };
                    }
                    return true;
                }
                return false;
            }
            "i" | "I" => {
                if rest.eq_ignore_ascii_case("p") {
                    self.cpu.gprs[12] = value;
                    return true;
                }
                return false;
            }
            "s" | "S" => {
                if rest.eq_ignore_ascii_case("p") {
                    self.cpu.gprs[ARM_SP] = value;
                    return true;
                }
                return false;
            }
            "l" | "L" => {
                if rest.eq_ignore_ascii_case("r") {
                    self.cpu.gprs[ARM_LR] = value;
                    return true;
                }
                return false;
            }
            "p" | "P" => {
                if rest.eq_ignore_ascii_case("c") {
                    self.cpu.gprs[ARM_PC] = value;
                    return true;
                }
                return false;
            }
            _ => return false,
        }
        let Ok(reg_id) = rest.parse::<u32>() else {
            return false;
        };
        if reg_id > 15 {
            return false;
        }
        self.cpu.gprs[reg_id as usize] = value;
        true
    }

    fn dbg_raw_read(&mut self, address: u32, _segment: i32, width: u32) -> u32 {
        match width {
            1 => self.view8(address) as u32,
            2 => self.view16(address) as u32,
            _ => self.view32(address),
        }
    }

    fn dbg_raw_write(&mut self, address: u32, _segment: i32, width: u32, value: u32) {
        match width {
            1 => self.patch8(address, value as i8, None),
            2 => self.patch16(address, value as i16, None),
            _ => self.patch32(address, value as i32, None),
        }
    }

    fn dbg_lookup_identifier(&mut self, name: &str) -> Option<(i32, i32)> {
        // _GBACoreLookupIdentifier: IO register names map to GBA_BASE_IO | i
        for (i, reg) in GBA_IO_REGISTER_NAMES.iter().enumerate() {
            if let Some(reg) = reg {
                if reg.eq_ignore_ascii_case(name) {
                    return Some(((crate::memory::GBA_BASE_IO | (i as u32) << 1) as i32, -1));
                }
            }
        }
        None
    }

    // --- mDebuggerPlatform (ARMDebugger*) ---

    fn dbg_has_breakpoints(&mut self) -> bool {
        self.debugger
            .as_ref()
            .map(|d| {
                !d.breakpoints.is_empty()
                    || !d.watchpoints.is_empty()
                    || !d.stack_trace_mode.is_disabled()
            })
            .unwrap_or(false)
    }

    fn dbg_check_breakpoints(&mut self) {
        let Some(mut dbg) = self.debugger.take() else {
            return;
        };
        dbg.check_breakpoints(self);
        self.debugger = Some(dbg);
    }

    /// ARMDebuggerSetSoftwareBreakpoint (via the C `mDebuggerModule`-facing
    /// software path) — also used for hardware breakpoints here.
    fn dbg_set_breakpoint(&mut self, owner: Option<usize>, bp: &Breakpoint) -> i64 {
        let Some(dbg_mut) = self.debugger.as_mut() else {
            return -1;
        };
        if bp.ty == BreakpointType::Software {
            // Route through the software patch path, mirroring what the C
            // frontends call (ARMDebuggerSetSoftwareBreakpoint).
            let mode = if bp.address & 1 != 0 {
                arm::ExecutionMode::Thumb
            } else {
                arm::ExecutionMode::Arm
            };
            let mut dbg = self.debugger.take().unwrap();
            let result = dbg
                .set_software_breakpoint(self, bp.address & !1, mode)
                .map(|opcode| {
                    let id = dbg.next_id;
                    dbg.next_id += 1;
                    dbg.sw_breakpoints.push(ArmDebugBreakpoint {
                        d: Breakpoint {
                            id,
                            address: bp.address & !1,
                            segment: -1,
                            ty: BreakpointType::Software,
                            condition: None,
                            disabled: false,
                            is_temporary: bp.is_temporary,
                        },
                        sw: ArmSwBreakpoint { opcode, mode },
                    });
                    if let Some(owner) = owner {
                        dbg.core.point_owner.insert(id, owner);
                    }
                    id
                })
                .unwrap_or(-1);
            self.debugger = Some(dbg);
            return result;
        }
        let _ = dbg_mut;
        let dbg = self.debugger.as_mut().unwrap();
        let id = dbg.next_id;
        dbg.next_id += 1;
        let mut b = bp.clone();
        b.address &= !1; // Clear Thumb bit
        b.id = id;
        dbg.breakpoints.push(ArmDebugBreakpoint {
            d: b,
            sw: ArmSwBreakpoint::default(),
        });
        if let Some(owner) = owner {
            dbg.core.point_owner.insert(id, owner);
        }
        dbg.rebuild_bp_bloom();
        if bp.ty == BreakpointType::Software {
            // C: TODO/abort() — unreachable here.
            unreachable!()
        }
        id
    }

    fn dbg_list_breakpoints(&self, owner: Option<usize>) -> Vec<Breakpoint> {
        let Some(dbg) = self.debugger.as_ref() else {
            return Vec::new();
        };
        // Merge hw+sw lists ordered by id (C interleaves both lists).
        let mut all: Vec<Breakpoint> = Vec::new();
        for b in dbg
            .breakpoints
            .iter()
            .chain(dbg.sw_breakpoints.iter())
            .map(|b| &b.d)
        {
            if let Some(o) = owner {
                if dbg.core.point_owner(b.id) != Some(o) {
                    continue;
                }
            }
            all.push(b.clone());
        }
        all.sort_by_key(|b| b.id);
        all
    }

    fn dbg_clear_breakpoint(&mut self, id: i64) -> bool {
        let Some(mut dbg) = self.debugger.take() else {
            return false;
        };
        let mut result = false;
        if let Some(i) = dbg.breakpoints.iter().position(|b| b.d.id == id) {
            dbg.destroy_breakpoint(i);
            result = true;
        }
        dbg.rebuild_bp_bloom();
        if !result {
            if let Some(i) = dbg.sw_breakpoints.iter().position(|b| b.d.id == id) {
                dbg.clear_software_breakpoint(self, i);
                dbg.destroy_sw_breakpoint(i);
                result = true;
            }
        }
        if !result {
            if let Some(i) = dbg.watchpoints.iter().position(|w| w.id == id) {
                dbg.destroy_watchpoint(i);
                // ARMDebuggerRemoveMemoryShim — but the access logger
                // keeps the shim active by itself.
                self.dbg_watchpoints_active = dbg.dbg_memory_shim_active();
                result = true;
            }
        }
        self.debugger = Some(dbg);
        result
    }

    fn dbg_toggle_breakpoint(&mut self, id: i64, status: bool) -> bool {
        let Some(mut dbg) = self.debugger.take() else {
            return false;
        };
        let mut result = false;
        if let Some(b) = dbg.breakpoints.iter_mut().find(|b| b.d.id == id) {
            b.d.disabled = !status;
            result = true;
        }
        dbg.rebuild_bp_bloom();
        if !result {
            if let Some(b) = dbg.sw_breakpoints.iter_mut().find(|b| b.d.id == id) {
                b.d.disabled = !status;
                result = true;
            }
        }
        if !result {
            if let Some(w) = dbg.watchpoints.iter_mut().find(|w| w.id == id) {
                w.disabled = !status;
                result = true;
            }
        }
        self.debugger = Some(dbg);
        result
    }

    fn dbg_set_watchpoint(&mut self, owner: Option<usize>, wp: &Watchpoint) -> i64 {
        let Some(dbg) = self.debugger.as_mut() else {
            return -1;
        };
        if dbg.watchpoints.is_empty() {
            self.dbg_watchpoints_active = true; // ARMDebuggerInstallMemoryShim
        }
        let dbg = self.debugger.as_mut().unwrap();
        let mut wp = wp.clone();
        wp.id = dbg.next_id;
        dbg.next_id += 1;
        let id = wp.id;
        if let Some(owner) = owner {
            dbg.core.point_owner.insert(id, owner);
        }
        dbg.watchpoints.push(wp);
        id
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
        // ARMDebuggerTrace
        let (info, is_wide) = GbaDebugger::decode_combined(self);
        let disassembly = if self.cpu.execution_mode == arm::ExecutionMode::Arm {
            let instruction = self.cpu.prefetch[0];
            let info = armdec::arm_decode_arm(instruction);
            format!(
                "{:08X}: {}",
                instruction,
                armdec::arm_disassemble(&info, self.cpu.gprs[ARM_PC] as u32, None)
            )
        } else {
            let instruction = self.cpu.prefetch[0] as u16;
            if is_wide {
                let _ = info;
                let i1 = armdec::arm_decode_thumb(instruction);
                let i2 = armdec::arm_decode_thumb(self.cpu.prefetch[1] as u16);
                let combined = armdec::arm_decode_thumb_combine(&i1, &i2).unwrap_or(i1);
                format!(
                    "{:04X}{:04X}: {}",
                    instruction,
                    self.cpu.prefetch[1] as u16,
                    armdec::arm_disassemble(
                        &combined,
                        self.cpu.gprs[ARM_PC] as u32,
                        None
                    )
                )
            } else {
                format!(
                    "    {:04X}: {}",
                    instruction,
                    armdec::arm_disassemble(&info, self.cpu.gprs[ARM_PC] as u32, None)
                )
            }
        };
        // ARMDebuggerFormatRegisters
        let regs = &self.cpu;
        let g = &regs.gprs;
        format!(
            "{g0:08X} {g1:08X} {g2:08X} {g3:08X} {g4:08X} {g5:08X} {g6:08X} {g7:08X} {g8:08X} {g9:08X} {g10:08X} {g11:08X} {g12:08X} {g13:08X} {g14:08X} {g15:08X} cpsr: {cpsr:08X}",
            g0 = g[0], g1 = g[1], g2 = g[2], g3 = g[3], g4 = g[4], g5 = g[5], g6 = g[6],
            g7 = g[7], g8 = g[8], g9 = g[9], g10 = g[10], g11 = g[11], g12 = g[12],
            g13 = g[13], g14 = g[14], g15 = g[15], cpsr = regs.cpsr.packed,
        ) + &format!(" | {}", disassembly)
    }

    fn dbg_disassemble_line(
        &mut self,
        address: u32,
        _segment: i32,
        thumb: Option<bool>,
    ) -> (String, u32) {
        // _printLine (arm/debugger/cli-debugger.c)
        let thumb = thumb.unwrap_or_else(|| self.cpu.execution_mode == arm::ExecutionMode::Thumb);
        let address = address & !(WORD_SIZE_THUMB as u32 - 1);
        if !thumb {
            let address = address & !(WORD_SIZE_ARM as u32 - 1);
            let instruction = self.view32(address);
            let info = armdec::arm_decode_arm(instruction);
            let line = format!(
                "{:08X}:  {:08X}\t{}",
                address,
                instruction,
                armdec::arm_disassemble(&info, address + WORD_SIZE_ARM as u32 * 2, None)
            );
            (line, address + WORD_SIZE_ARM as u32)
        } else {
            let instruction = self.view16(address);
            let instruction2 = self.view16(address + WORD_SIZE_THUMB as u32);
            let info = armdec::arm_decode_thumb(instruction);
            let info2 = armdec::arm_decode_thumb(instruction2);
            if let Some(combined) = armdec::arm_decode_thumb_combine(&info, &info2) {
                let line = format!(
                    "{:08X}:  {:04X} {:04X}\t{}",
                    address,
                    instruction,
                    instruction2,
                    armdec::arm_disassemble(
                        &combined,
                        address + WORD_SIZE_THUMB as u32 * 2,
                        None
                    )
                );
                (line, address + WORD_SIZE_THUMB as u32 * 2)
            } else {
                let line = format!(
                    "{:08X}:  {:04X}     \t{}",
                    address,
                    instruction,
                    armdec::arm_disassemble(&info, address + WORD_SIZE_THUMB as u32 * 2, None)
                );
                (line, address + WORD_SIZE_THUMB as u32)
            }
        }
    }

    fn dbg_print_status(&mut self) -> String {
        // _printStatus (arm/debugger/cli-debugger.c)
        let mut out = String::new();
        let g = &self.cpu.gprs;
        for r in (0..16).step_by(4) {
            out.push_str(&format!(
                "{}r{}: {:08X}  {}r{}: {:08X}  {}r{}: {:08X}  {}r{}: {:08X}\n",
                if r < 10 { " " } else { "" }, r, g[r] as u32,
                if r < 9 { " " } else { "" }, r + 1, g[r + 1] as u32,
                if r < 8 { " " } else { "" }, r + 2, g[r + 2] as u32,
                if r < 7 { " " } else { "" }, r + 3, g[r + 3] as u32,
            ));
        }
        out.push_str("cpsr: ");
        out.push_str(&Self::dbg_format_psr(self.cpu.cpsr.packed));
        if !matches!(
            self.cpu.cpsr.priv_mode(),
            arm::PrivilegeMode::User | arm::PrivilegeMode::System
        ) {
            out.push_str("spsr: ");
            out.push_str(&Self::dbg_format_psr(self.cpu.spsr.packed));
        }
        out.push_str(&format!("Cycle: {}\n", self.timing.global_time()));
        let instruction_length = if self.cpu.execution_mode == arm::ExecutionMode::Arm {
            WORD_SIZE_ARM
        } else {
            WORD_SIZE_THUMB
        };
        let (line, _) = self.dbg_disassemble_line(
            (self.cpu.gprs[ARM_PC] as u32).wrapping_sub(instruction_length as u32),
            -1,
            None,
        );
        out.push_str(&line);
        out.push('\n');
        out
    }

    fn dbg_global_time(&self) -> u64 {
        self.timing.global_time()
    }

    fn dbg_is_thumb(&self) -> bool {
        self.cpu.execution_mode == arm::ExecutionMode::Thumb
    }

    /// GB / GBA `reset` CLI command: Core::reset
    fn dbg_reset(&mut self) {
        self.arm_reset();
    }

    /// arm/debugger/cli-debugger.c _disassembleMode: default address is
    /// pc - instruction size (the instruction about to execute).
    fn dbg_disassemble_default_address(&self) -> u32 {
        let instruction_length = if self.cpu.execution_mode == arm::ExecutionMode::Arm {
            WORD_SIZE_ARM
        } else {
            WORD_SIZE_THUMB
        };
        (self.cpu.gprs[ARM_PC] as u32).wrapping_sub(instruction_length as u32)
    }

    /// ARMDebuggerGetStackTraceMode is registered on the ARM platform.
    fn dbg_supports_stack_trace(&self) -> bool {
        true
    }

    /// ARMCLIDebuggerCreate registers the ARM platform command table.
    fn dbg_cli_platform(&self) -> Option<&'static str> {
        Some("ARM")
    }

    fn dbg_set_software_breakpoint(
        &mut self,
        owner: Option<usize>,
        address: u32,
        thumb: bool,
    ) -> i64 {
        // ARMDebuggerSetSoftwareBreakpoint
        let mode = if thumb {
            arm::ExecutionMode::Thumb
        } else {
            arm::ExecutionMode::Arm
        };
        let Some(mut dbg) = self.debugger.take() else {
            return -1;
        };
        let result = match dbg.set_software_breakpoint(self, address, mode) {
            Some(opcode) => {
                let id = dbg.next_id;
                dbg.next_id += 1;
                dbg.sw_breakpoints.push(ArmDebugBreakpoint {
                    d: Breakpoint {
                        id,
                        address: address & !1,
                        segment: -1,
                        ty: BreakpointType::Software,
                        condition: None,
                        disabled: false,
                        is_temporary: false,
                    },
                    sw: ArmSwBreakpoint { opcode, mode },
                });
                if let Some(owner) = owner {
                    dbg.core.point_owner.insert(id, owner);
                }
                id
            }
            None => -1,
        };
        self.debugger = Some(dbg);
        result
    }

    fn dbg_get_stack_trace_mode(&self) -> StackTraceMode {
        self.debugger
            .as_ref()
            .map(|d| d.stack_trace_mode)
            .unwrap_or(StackTraceMode::Disabled)
    }

    /// While a debugger core is detached for driving, platform hits are
    /// queued in `pending_entries`; the running core drains them.
    fn dbg_take_pending_entries(&mut self) -> Vec<(DebuggerEntryReason, DebuggerEntryInfo)> {
        match self.debugger.as_mut() {
            Some(dbg) => std::mem::take(&mut dbg.pending_entries),
            None => Vec::new(),
        }
    }

    /// _GBACoreListMemoryBlocks, filtered to `mCORE_MEMORY_MAPPED` blocks
    /// like the GDB memory-map consumer does. The SRAM/EEPROM block variants
    /// are never MAPPED in the C tables, so this list is constant.
    fn dbg_list_memory_blocks(&self) -> Vec<MemoryBlockInfo> {        use crate::memory::*;
        vec![
            MemoryBlockInfo {
                start: GBA_BASE_BIOS,
                size: GBA_SIZE_BIOS as u32,
                writable: false,
            },
            MemoryBlockInfo {
                start: GBA_BASE_EWRAM,
                size: GBA_SIZE_EWRAM as u32,
                writable: true,
            },
            MemoryBlockInfo {
                start: GBA_BASE_IWRAM,
                size: GBA_SIZE_IWRAM as u32,
                writable: true,
            },
            MemoryBlockInfo {
                start: GBA_BASE_IO,
                size: GBA_SIZE_IO as u32,
                writable: true,
            },
            MemoryBlockInfo {
                start: GBA_BASE_PALETTE_RAM,
                size: GBA_SIZE_PALETTE_RAM as u32,
                writable: true,
            },
            MemoryBlockInfo {
                start: GBA_BASE_VRAM,
                size: GBA_SIZE_VRAM as u32,
                writable: true,
            },
            MemoryBlockInfo {
                start: GBA_BASE_OAM,
                size: GBA_SIZE_OAM as u32,
                writable: true,
            },
            // ROM regions are mCORE_MEMORY_READ|mCORE_MEMORY_WORM|MAPPED; the
            // C XML generator treats WORM as writable ("ram").
            MemoryBlockInfo {
                start: GBA_BASE_ROM0,
                size: GBA_SIZE_ROM0 as u32,
                writable: true,
            },
            MemoryBlockInfo {
                start: GBA_BASE_ROM1,
                size: GBA_SIZE_ROM0 as u32,
                writable: true,
            },
            MemoryBlockInfo {
                start: GBA_BASE_ROM2,
                size: GBA_SIZE_ROM0 as u32,
                writable: true,
            },
        ]
    }

    // --- access logger console hooks (access-logger.c) ---

    /// _GBACoreListMemoryBlocks with the fields the access logger needs
    /// (ids, internal names, flags). The savedata-variant tables differ
    /// only in the (unmapped) SRAM/EEPROM tail block, which the logger
    /// rejects on `mCORE_MEMORY_MAPPED`; omitted here.
    fn dbg_list_memory_blocks_full(&self) -> Vec<CoreMemoryBlock> {
        use crate::memory::*;
        use rgba_debugger::access_logger::core_memory_flags as mf;
        const BIOS: i32 = 0x0; // GBA_REGION_BIOS
        const EWRAM: i32 = 0x2; // GBA_REGION_EWRAM
        const IWRAM: i32 = 0x3; // GBA_REGION_IWRAM
        const IO: i32 = 0x4; // GBA_REGION_IO
        const PALETTE: i32 = 0x5; // GBA_REGION_PALETTE_RAM
        const VRAM: i32 = 0x6; // GBA_REGION_VRAM
        const OAM: i32 = 0x7; // GBA_REGION_OAM
        const ROM0: i32 = 0x8; // GBA_REGION_ROM0
        const ROM1: i32 = 0xA; // GBA_REGION_ROM1
        const ROM2: i32 = 0xC; // GBA_REGION_ROM2
        let b = |id, name, start: u32, size: usize, flags| {
            CoreMemoryBlock::flat(id, name, start, start + size as u32, size as u32, flags)
        };
        // C adjusts the ROM block sizes to the loaded image
        // (`memoryBlocks[i].size = gba->memory.romSize`).
        let rom_size = self.memory.rom_size.max(GBA_SIZE_ROM0);
        vec![
            b(-1, "mem", 0, 0x10000000, mf::VIRTUAL),
            b(BIOS, "bios", GBA_BASE_BIOS, GBA_SIZE_BIOS, mf::READ | mf::MAPPED),
            b(EWRAM, "wram", GBA_BASE_EWRAM, GBA_SIZE_EWRAM, mf::RW | mf::MAPPED),
            b(IWRAM, "iwram", GBA_BASE_IWRAM, GBA_SIZE_IWRAM, mf::RW | mf::MAPPED),
            b(IO, "io", GBA_BASE_IO, GBA_SIZE_IO, mf::RW | mf::MAPPED),
            b(PALETTE, "palette", GBA_BASE_PALETTE_RAM, GBA_SIZE_PALETTE_RAM, mf::RW | mf::MAPPED),
            b(VRAM, "vram", GBA_BASE_VRAM, GBA_SIZE_VRAM, mf::RW | mf::MAPPED),
            b(OAM, "oam", GBA_BASE_OAM, GBA_SIZE_OAM, mf::RW | mf::MAPPED),
            b(ROM0, "cart0", GBA_BASE_ROM0, rom_size, mf::READ | mf::WORM | mf::MAPPED),
            b(ROM1, "cart1", GBA_BASE_ROM1, rom_size, mf::READ | mf::WORM | mf::MAPPED),
            b(ROM2, "cart2", GBA_BASE_ROM2, rom_size, mf::READ | mf::WORM | mf::MAPPED),
        ]
    }

    fn dbg_set_access_log(
        &mut self,
        core: Option<Box<AccessLoggerCore>>,
    ) -> Option<Box<AccessLoggerCore>> {
        let dbg = self.debugger.as_mut()?;
        let old = std::mem::replace(&mut dbg.access_log, core);
        let active = dbg.dbg_memory_shim_active();
        self.dbg_watchpoints_active = active; // ARMDebuggerInstallMemoryShim/RemoveMemoryShim
        old
    }

    fn dbg_access_log(&mut self) -> Option<&mut AccessLoggerCore> {
        self.debugger.as_mut()?.access_log.as_deref_mut()
    }

    fn dbg_next_instruction_info(&mut self) -> DebuggerInstructionInfo {
        // ARMDebuggerNextInstructionInfo
        let mut info = DebuggerInstructionInfo {
            segment: 0,
            ..Default::default()
        };
        let width = if self.cpu.execution_mode == arm::ExecutionMode::Arm {
            WORD_SIZE_ARM
        } else {
            WORD_SIZE_THUMB
        } as u32;
        info.width = width;
        info.address = (self.cpu.gprs[ARM_PC] as u32).wrapping_sub(width);
        if self.cpu.execution_mode == arm::ExecutionMode::Arm {
            for i in 0..4 {
                info.flags[i] = access_log_flags::ACCESS32;
                info.flags_ex[i] = access_log_flags_ex::EXECUTE_ARM;
            }
        } else {
            for i in 0..2 {
                info.flags[i] = access_log_flags::ACCESS16;
                info.flags_ex[i] = access_log_flags_ex::EXECUTE_THUMB;
            }
        }
        // TODO Access types (C has the same TODO)
        info
    }

    fn dbg_set_stack_trace_mode(&mut self, mode: StackTraceMode) {
        // ARMDebuggerSetStackTraceMode
        if let Some(dbg) = self.debugger.as_mut() {
            if mode.is_disabled() && !dbg.stack_trace_mode.is_disabled() {
                dbg.core.stack_trace.clear();
            }
            dbg.stack_trace_mode = mode;
        }
    }

    fn dbg_update_stack_trace(&mut self) -> bool {
        // ARMDebuggerUpdateStackTrace
        let Some(mut dbg) = self.debugger.take() else {
            return false;
        };
        let instruction_length = GbaDebugger::instruction_length(self);
        let pc = self.cpu.gprs[ARM_PC] as u32 - instruction_length as u32;
        let r = if !dbg.stack_trace_mode.is_disabled() {
            dbg.update_stack_trace_internal(self, pc)
        } else {
            false
        };
        self.debugger = Some(dbg);
        r

    }
}
