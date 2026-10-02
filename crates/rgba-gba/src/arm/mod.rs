// Copyright (c) 2013-2014 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/include/mgba/internal/arm/arm.h and mgba/src/arm/arm.c.

pub mod isa_arm;
pub mod isa_thumb;
pub mod decoder;

use crate::gba::Gba;

pub const ARM_SP: usize = 13;
pub const ARM_LR: usize = 14;
pub const ARM_PC: usize = 15;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Debug, Default)]
pub enum ExecutionMode {
    #[default]
    Arm = 0,
    Thumb = 1,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Debug)]
pub enum PrivilegeMode {
    User = 0x10,
    Fiq = 0x11,
    Irq = 0x12,
    Supervisor = 0x13,
    Abort = 0x17,
    Undefined = 0x1B,
    System = 0x1F,
}

impl PrivilegeMode {
    pub fn bank(self) -> usize {
        match self {
            PrivilegeMode::User | PrivilegeMode::System => 0, // BANK_NONE
            PrivilegeMode::Fiq => 1,
            PrivilegeMode::Irq => 2,
            PrivilegeMode::Supervisor => 3,
            PrivilegeMode::Abort => 4,
            PrivilegeMode::Undefined => 5,
        }
    }
    pub fn has_spsr(self) -> bool {
        self != PrivilegeMode::System && self != PrivilegeMode::User
    }

    /// Reinterpret the CPSR priv bits (debugger use).
    pub fn from_bits(bits: u8) -> PrivilegeMode {
        match bits {
            0x10 => PrivilegeMode::User,
            0x11 => PrivilegeMode::Fiq,
            0x12 => PrivilegeMode::Irq,
            0x13 => PrivilegeMode::Supervisor,
            0x17 => PrivilegeMode::Abort,
            0x1B => PrivilegeMode::Undefined,
            _ => PrivilegeMode::System,
        }
    }
}

pub const WORD_SIZE_ARM: i32 = 4;
pub const WORD_SIZE_THUMB: i32 = 2;

pub const BASE_RESET: u32 = 0x00000000;
pub const BASE_UNDEF: u32 = 0x00000004;
pub const BASE_SWI: u32 = 0x00000008;
pub const BASE_PABT: u32 = 0x0000000C;
pub const BASE_DABT: u32 = 0x00000010;
pub const BASE_IRQ: u32 = 0x00000018;
pub const BASE_FIQ: u32 = 0x0000001C;

pub const PSR_USER_MASK: u32 = 0xF0000000;
pub const PSR_PRIV_MASK: u32 = 0x000000CF;
pub const PSR_STATE_MASK: u32 = 0x00000020;

pub const LSM_IA: i32 = 0;
pub const LSM_IB: i32 = 1;
pub const LSM_DA: i32 = 2;
pub const LSM_DB: i32 = 3;

/// union PSR as a packed i32.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Psr {
    pub packed: i32,
}

impl Psr {
    #[inline]
    pub fn control(&self) -> u8 {
        self.packed as u8
    }
    #[inline]
    pub fn flags(&self) -> u8 {
        (self.packed >> 24) as u8
    }
    #[inline]
    pub fn n(&self) -> bool {
        self.packed < 0
    }
    #[inline]
    pub fn set_n(&mut self, v: bool) {
        if v {
            self.packed |= 0x80000000u32 as i32;
        } else {
            self.packed &= 0x7FFFFFFF;
        }
    }
    #[inline]
    pub fn z(&self) -> bool {
        self.packed & 0x40000000 != 0
    }
    #[inline]
    pub fn set_z(&mut self, v: bool) {
        if v {
            self.packed |= 0x40000000;
        } else {
            self.packed &= !0x40000000;
        }
    }
    #[inline]
    pub fn c(&self) -> bool {
        self.packed & 0x20000000 != 0
    }
    #[inline]
    pub fn set_c(&mut self, v: bool) {
        if v {
            self.packed |= 0x20000000;
        } else {
            self.packed &= !0x20000000;
        }
    }
    #[inline]
    pub fn v(&self) -> bool {
        self.packed & 0x10000000 != 0
    }
    #[inline]
    pub fn set_v(&mut self, v: bool) {
        if v {
            self.packed |= 0x10000000;
        } else {
            self.packed &= !0x10000000;
        }
    }
    #[inline]
    pub fn i(&self) -> bool {
        self.packed & 0x80 != 0
    }
    #[inline]
    pub fn set_i(&mut self, v: bool) {
        if v {
            self.packed |= 0x80;
        } else {
            self.packed &= !0x80;
        }
    }
    #[inline]
    pub fn f(&self) -> bool {
        self.packed & 0x40 != 0
    }
    #[inline]
    pub fn t(&self) -> bool {
        self.packed & 0x20 != 0
    }
    #[inline]
    pub fn set_t(&mut self, v: bool) {
        if v {
            self.packed |= 0x20;
        } else {
            self.packed &= !0x20;
        }
    }
    #[inline]
    pub fn priv_bits(&self) -> u8 {
        (self.packed & 0x1F) as u8
    }
    #[inline]
    pub fn priv_mode(&self) -> PrivilegeMode {
        match self.priv_bits() {
            0x10 => PrivilegeMode::User,
            0x11 => PrivilegeMode::Fiq,
            0x12 => PrivilegeMode::Irq,
            0x13 => PrivilegeMode::Supervisor,
            0x17 => PrivilegeMode::Abort,
            0x1B => PrivilegeMode::Undefined,
            _ => PrivilegeMode::System,
        }
    }
}

pub struct ArmCore {
    pub gprs: [i32; 16],
    pub cpsr: Psr,
    pub spsr: Psr,

    pub cycles: i32,
    pub next_event: i32,
    pub halted: i32,

    pub banked_registers: [[i32; 7]; 6],
    pub banked_spsrs: [i32; 6],

    pub shifter_operand: i32,
    pub shifter_carry_out: i32,

    pub prefetch: [u32; 2],
    pub execution_mode: ExecutionMode,
    pub privilege_mode: PrivilegeMode,

    /// Cached fetch window (set in memory.rs's set_active_region).
    pub active_region: crate::memory::ActiveMemoryRegion,
    pub active_mask: u32,
    pub active_seq_cycles32: i32,
    pub active_seq_cycles16: i32,
    pub active_nonseq_cycles32: i32,
    pub active_nonseq_cycles16: i32,
}

const CONDITION_LUT: [u16; 16] = [
    0xF0F0, // EQ
    0x0F0F, // NE
    0xCCCC, // CS
    0x3333, // CC
    0xFF00, // MI
    0x00FF, // PL
    0xAAAA, // VS
    0x5555, // VC
    0x0C0C, // HI
    0xF3F3, // LS
    0xAA55, // GE
    0x55AA, // LT
    0x0A05, // GT
    0xF5FA, // LE
    0xFFFF, // AL
    0x0000, // NV
];

impl ArmCore {
    pub fn new() -> Self {
        ArmCore {
            gprs: [0; 16],
            cpsr: Psr::default(),
            spsr: Psr::default(),
            cycles: 0,
            next_event: 0,
            halted: 0,
            banked_registers: [[0; 7]; 6],
            banked_spsrs: [0; 6],
            shifter_operand: 0,
            shifter_carry_out: 0,
            prefetch: [0; 2],
            execution_mode: ExecutionMode::Arm,
            privilege_mode: PrivilegeMode::System,
            active_region: crate::memory::ActiveMemoryRegion::None,
            active_mask: 0,
            active_seq_cycles32: 0,
            active_seq_cycles16: 0,
            active_nonseq_cycles32: 0,
            active_nonseq_cycles16: 0,
        }
    }

    #[inline]
    pub fn test_condition(&self, condition: u32) -> bool {
        // cpu->cpsr.flags >> 4: NZCV in bits 31..28.
        let flags = (self.cpsr.packed as u32) >> 28;
        CONDITION_LUT[(condition & 0xF) as usize] & (1 << flags) != 0
    }
}

impl Default for ArmCore {
    fn default() -> Self {
        Self::new()
    }
}

impl Gba {
    /// ARMSetPrivilegeMode
    pub fn arm_set_privilege_mode(&mut self, mode: PrivilegeMode) {
        if mode == self.cpu.privilege_mode {
            return;
        }
        let new_bank = mode.bank();
        let old_bank = self.cpu.privilege_mode.bank();
        if new_bank != old_bank {
            if mode == PrivilegeMode::Fiq || self.cpu.privilege_mode == PrivilegeMode::Fiq {
                let old_fiq_bank = (old_bank == 1) as usize;
                let new_fiq_bank = (new_bank == 1) as usize;
                for i in 0..5 {
                    self.cpu.banked_registers[old_fiq_bank][2 + i] = self.cpu.gprs[8 + i];
                }
                for i in 0..5 {
                    self.cpu.gprs[8 + i] = self.cpu.banked_registers[new_fiq_bank][2 + i];
                }
            }
            self.cpu.banked_registers[old_bank][0] = self.cpu.gprs[ARM_SP];
            self.cpu.banked_registers[old_bank][1] = self.cpu.gprs[ARM_LR];
            self.cpu.gprs[ARM_SP] = self.cpu.banked_registers[new_bank][0];
            self.cpu.gprs[ARM_LR] = self.cpu.banked_registers[new_bank][1];
            self.cpu.banked_spsrs[old_bank] = self.cpu.spsr.packed;
            self.cpu.spsr.packed = self.cpu.banked_spsrs[new_bank];
        }
        self.cpu.privilege_mode = mode;
    }

    /// ARMReset
    pub fn arm_reset(&mut self) {
        self.cpu.gprs = [0; 16];
        self.cpu.banked_registers = [[0; 7]; 6];
        self.cpu.banked_spsrs = [0; 6];

        self.cpu.privilege_mode = PrivilegeMode::System;
        self.cpu.cpsr.packed = PrivilegeMode::System as i32;
        self.cpu.spsr.packed = 0;

        self.cpu.shifter_operand = 0;
        self.cpu.shifter_carry_out = 0;

        self.cpu.execution_mode = ExecutionMode::Thumb;
        self.arm_set_mode(ExecutionMode::Arm);
        self.arm_write_pc();

        self.cpu.cycles = 0;
        self.cpu.next_event = 0;
        self.cpu.halted = 0;

        self.irq_reset();
    }

    /// _ARMSetMode
    pub fn arm_set_mode(&mut self, execution_mode: ExecutionMode) {
        if execution_mode == self.cpu.execution_mode {
            return;
        }
        self.cpu.execution_mode = execution_mode;
        match execution_mode {
            ExecutionMode::Arm => {
                self.cpu.cpsr.set_t(false);
                self.cpu.active_mask &= !2;
            }
            ExecutionMode::Thumb => {
                self.cpu.cpsr.set_t(true);
                self.cpu.active_mask |= 2;
            }
        }
        self.cpu.next_event = self.cpu.cycles;
    }

    /// _ARMReadCPSR
    pub fn arm_read_cpsr(&mut self) {
        self.arm_set_mode(if self.cpu.cpsr.t() { ExecutionMode::Thumb } else { ExecutionMode::Arm });
        self.arm_set_privilege_mode(self.cpu.cpsr.priv_mode());
        self.irq_read_cpsr();
    }

    /// ARMRaiseIRQ
    pub fn arm_raise_irq(&mut self) {
        if self.cpu.cpsr.i() {
            return;
        }
        let cpsr = self.cpu.cpsr;
        let instruction_width = if self.cpu.execution_mode == ExecutionMode::Thumb {
            WORD_SIZE_THUMB
        } else {
            WORD_SIZE_ARM
        };
        self.arm_set_privilege_mode(PrivilegeMode::Irq);
        self.cpu.cpsr.packed = (self.cpu.cpsr.packed & !0x1F) | PrivilegeMode::Irq as i32;
        self.cpu.gprs[ARM_LR] = self.cpu.gprs[ARM_PC] - instruction_width + WORD_SIZE_ARM;
        self.cpu.gprs[ARM_PC] = BASE_IRQ as i32;
        self.arm_set_mode(ExecutionMode::Arm);
        let cost = self.arm_write_pc();
        self.cpu.cycles += cost;
        self.cpu.spsr = cpsr;
        self.cpu.cpsr.set_i(true);
        self.cpu.halted = 0;
    }

    /// ARMRaiseSWI
    pub fn arm_raise_swi(&mut self) {
        let cpsr = self.cpu.cpsr;
        let instruction_width = if self.cpu.execution_mode == ExecutionMode::Thumb {
            WORD_SIZE_THUMB
        } else {
            WORD_SIZE_ARM
        };
        self.arm_set_privilege_mode(PrivilegeMode::Supervisor);
        self.cpu.cpsr.packed = (self.cpu.cpsr.packed & !0x1F) | PrivilegeMode::Supervisor as i32;
        self.cpu.gprs[ARM_LR] = self.cpu.gprs[ARM_PC] - instruction_width;
        self.cpu.gprs[ARM_PC] = BASE_SWI as i32;
        self.arm_set_mode(ExecutionMode::Arm);
        let cost = self.arm_write_pc();
        self.cpu.cycles += cost;
        self.cpu.spsr = cpsr;
        self.cpu.cpsr.set_i(true);
    }

    /// ARMRaiseUndefined
    pub fn arm_raise_undefined(&mut self) {
        let cpsr = self.cpu.cpsr;
        let instruction_width = if self.cpu.execution_mode == ExecutionMode::Thumb {
            WORD_SIZE_THUMB
        } else {
            WORD_SIZE_ARM
        };
        self.arm_set_privilege_mode(PrivilegeMode::Undefined);
        self.cpu.cpsr.packed = (self.cpu.cpsr.packed & !0x1F) | PrivilegeMode::Undefined as i32;
        self.cpu.gprs[ARM_LR] = self.cpu.gprs[ARM_PC] - instruction_width;
        self.cpu.gprs[ARM_PC] = BASE_UNDEF as i32;
        self.arm_set_mode(ExecutionMode::Arm);
        let cost = self.arm_write_pc();
        self.cpu.cycles += cost;
        self.cpu.spsr = cpsr;
        self.cpu.cpsr.set_i(true);
    }

    /// ARMWritePC (isa-inlines.h): refill the prefetch pipeline.
    pub fn arm_write_pc(&mut self) -> i32 {
        let mut pc = (self.cpu.gprs[ARM_PC] & -WORD_SIZE_THUMB) as u32;
        self.set_active_region(pc);
        self.cpu.prefetch[0] = self.active_region_load32(pc & self.cpu.active_mask);
        pc = pc.wrapping_add(WORD_SIZE_ARM as u32);
        self.cpu.prefetch[1] = self.active_region_load32(pc & self.cpu.active_mask);
        self.cpu.gprs[ARM_PC] = pc as i32;
        2 + self.cpu.active_nonseq_cycles32 + self.cpu.active_seq_cycles32
    }

    /// ThumbWritePC
    pub fn thumb_write_pc(&mut self) -> i32 {
        let mut pc = (self.cpu.gprs[ARM_PC] & -WORD_SIZE_THUMB) as u32;
        self.set_active_region(pc);
        self.cpu.prefetch[0] = self.active_region_load16(pc & self.cpu.active_mask);
        pc = pc.wrapping_add(WORD_SIZE_THUMB as u32);
        self.cpu.prefetch[1] = self.active_region_load16(pc & self.cpu.active_mask);
        self.cpu.gprs[ARM_PC] = pc as i32;
        2 + self.cpu.active_nonseq_cycles16 + self.cpu.active_seq_cycles16
    }

    /// ARMStep
    pub fn arm_step(&mut self) {
        let opcode = self.cpu.prefetch[0];
        self.cpu.prefetch[0] = self.cpu.prefetch[1];
        self.cpu.gprs[ARM_PC] += WORD_SIZE_ARM;
        let fetch_addr = self.cpu.gprs[ARM_PC] as u32 & self.cpu.active_mask;
        self.cpu.prefetch[1] = self.active_region_load32(fetch_addr);

        let condition = opcode >> 28;
        if condition != 0xE {
            if !self.cpu.test_condition(condition) {
                self.cpu.cycles += 1 + self.cpu.active_seq_cycles32; // ARM_PREFETCH_CYCLES
                return;
            }
        }
        let idx = (((opcode >> 16) & 0xFF0) | ((opcode >> 4) & 0x00F)) as usize;
        let instruction = isa_arm::ARM_INSTRUCTION_TABLE[idx];
        instruction(self, opcode);
    }

    /// ThumbStep
    pub fn thumb_step(&mut self) {
        let opcode = self.cpu.prefetch[0];
        self.cpu.prefetch[0] = self.cpu.prefetch[1];
        self.cpu.gprs[ARM_PC] += WORD_SIZE_THUMB;
        let fetch_addr = self.cpu.gprs[ARM_PC] as u32 & self.cpu.active_mask;
        self.cpu.prefetch[1] = self.active_region_load16(fetch_addr);
        let instruction = isa_thumb::THUMB_INSTRUCTION_TABLE[(opcode >> 6) as usize];
        instruction(self, opcode as u16);
    }

    /// ARMRun
    pub fn arm_run(&mut self) {
        while self.cpu.cycles >= self.cpu.next_event {
            self.process_events();
        }
        if self.cpu.execution_mode == ExecutionMode::Thumb {
            self.thumb_step();
        } else {
            self.arm_step();
        }
        while self.cpu.cycles >= self.cpu.next_event {
            self.process_events();
        }
    }

    /// ARMRunLoop
    pub fn arm_run_loop(&mut self) {
        if self.cpu.execution_mode == ExecutionMode::Thumb {
            while self.cpu.cycles < self.cpu.next_event {
                self.thumb_step();
            }
        } else {
            while self.cpu.cycles < self.cpu.next_event {
                self.arm_step();
            }
        }
        self.process_events();
    }

    /// ARMRunFake
    pub fn arm_run_fake(&mut self, opcode: u32) {
        if self.cpu.execution_mode == ExecutionMode::Arm {
            self.cpu.gprs[ARM_PC] -= WORD_SIZE_ARM;
        } else {
            self.cpu.gprs[ARM_PC] -= WORD_SIZE_THUMB;
        }
        self.cpu.prefetch[1] = self.cpu.prefetch[0];
        self.cpu.prefetch[0] = opcode;
    }
}
