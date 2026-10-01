// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/include/mgba/internal/sm83/sm83.h and mgba/src/sm83/sm83.c.
//
// The SM83 core is microcoded: each M-cycle is one call of the current
// `instruction` handler, which updates `execution_state` to schedule the next
// M-cycle's bus activity. The tick engine reproduces SM83Tick/SM83Run from
// sm83.c, including the mid-M-cycle event polling.

pub mod decoder;
pub mod isa;

use crate::gb::Gb;

pub const SM83_CORE_FETCH: i32 = 3;
pub const SM83_CORE_IDLE_0: i32 = 0;
pub const SM83_CORE_IDLE_1: i32 = 1;
pub const SM83_CORE_EXECUTE: i32 = 2;
pub const SM83_CORE_MEMORY_LOAD: i32 = 7;
pub const SM83_CORE_MEMORY_STORE: i32 = 11;
pub const SM83_CORE_READ_PC: i32 = 15;
pub const SM83_CORE_STALL: i32 = 19;
pub const SM83_CORE_OP2: i32 = 23;
pub const SM83_CORE_HALT_BUG: i32 = 27;

pub type Instruction = fn(&mut Gb);

/// Packed flag register, matching mGBA's union FlagRegister layout
/// (z=bit7, n=bit6, h=bit5, c=bit4, low nibble unused).
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct FlagRegister {
    pub packed: u8,
}

impl FlagRegister {
    pub const Z: u8 = 0x80;
    pub const N: u8 = 0x40;
    pub const H: u8 = 0x20;
    pub const C: u8 = 0x10;

    #[inline]
    pub fn z(&self) -> bool {
        self.packed & Self::Z != 0
    }
    #[inline]
    pub fn n(&self) -> bool {
        self.packed & Self::N != 0
    }
    #[inline]
    pub fn h(&self) -> bool {
        self.packed & Self::H != 0
    }
    #[inline]
    pub fn c(&self) -> bool {
        self.packed & Self::C != 0
    }
    #[inline]
    pub fn set_z(&mut self, v: bool) {
        self.packed = if v { self.packed | Self::Z } else { self.packed & !Self::Z } & 0xF0;
    }
    #[inline]
    pub fn set_n(&mut self, v: bool) {
        self.packed = if v { self.packed | Self::N } else { self.packed & !Self::N } & 0xF0;
    }
    #[inline]
    pub fn set_h(&mut self, v: bool) {
        self.packed = if v { self.packed | Self::H } else { self.packed & !Self::H } & 0xF0;
    }
    #[inline]
    pub fn set_c(&mut self, v: bool) {
        self.packed = if v { self.packed | Self::C } else { self.packed & !Self::C } & 0xF0;
    }
}

pub struct Sm83 {
    pub f: FlagRegister,
    pub a: u8,
    pub c: u8,
    pub b: u8,
    pub e: u8,
    pub d: u8,
    pub l: u8,
    pub h: u8,
    pub sp: u16,
    pub pc: u16,

    pub index: u16,

    pub t_multiplier: i32,
    pub cycles: i32,
    pub next_event: i32,
    pub execution_state: i32,
    pub halted: bool,

    pub bus: u8,
    pub condition: bool,
    pub instruction: Instruction,

    pub irq_pending: bool,
}

impl Sm83 {
    pub fn new() -> Self {
        Sm83 {
            f: FlagRegister::default(),
            a: 0,
            c: 0,
            b: 0,
            e: 0,
            d: 0,
            l: 0,
            h: 0,
            sp: 0,
            pc: 0,
            index: 0,
            t_multiplier: 2,
            cycles: 0,
            next_event: 0,
            execution_state: SM83_CORE_FETCH,
            halted: false,
            bus: 0,
            condition: false,
            instruction: isa::INSTRUCTION_TABLE[0],
            irq_pending: false,
        }
    }

    // C's cpu->af etc. as combined 16-bit views.
    #[inline]
    pub fn af(&self) -> u16 {
        (self.a as u16) << 8 | self.f.packed as u16
    }
    #[inline]
    pub fn set_af(&mut self, v: u16) {
        self.a = (v >> 8) as u8;
        self.f.packed = (v as u8) & 0xF0;
    }
    #[inline]
    pub fn bc(&self) -> u16 {
        (self.b as u16) << 8 | self.c as u16
    }
    #[inline]
    pub fn set_bc(&mut self, v: u16) {
        self.b = (v >> 8) as u8;
        self.c = v as u8;
    }
    #[inline]
    pub fn de(&self) -> u16 {
        (self.d as u16) << 8 | self.e as u16
    }
    #[inline]
    pub fn set_de(&mut self, v: u16) {
        self.d = (v >> 8) as u8;
        self.e = v as u8;
    }
    #[inline]
    pub fn hl(&self) -> u16 {
        (self.h as u16) << 8 | self.l as u16
    }
    #[inline]
    pub fn set_hl(&mut self, v: u16) {
        self.h = (v >> 8) as u8;
        self.l = v as u8;
    }
}

impl Gb {
    /// SM83Reset
    pub fn sm83_reset(&mut self) {
        self.cpu.set_af(0);
        self.cpu.set_bc(0);
        self.cpu.set_de(0);
        self.cpu.set_hl(0);
        self.cpu.sp = 0;
        self.cpu.pc = 0;
        self.cpu.instruction = isa::INSTRUCTION_TABLE[0];
        self.cpu.t_multiplier = 2;
        self.cpu.cycles = 0;
        self.cpu.next_event = 0;
        self.cpu.execution_state = SM83_CORE_FETCH;
        self.cpu.halted = false;
        self.cpu.irq_pending = false;
        self.irq_reset();
    }

    /// SM83RaiseIRQ
    pub fn sm83_raise_irq(&mut self) {
        self.cpu.irq_pending = true;
    }

    fn sm83_instruction_irq_stall(&mut self) {
        self.cpu.execution_state = SM83_CORE_STALL;
    }

    fn sm83_instruction_irq_finish(&mut self) {
        self.cpu.execution_state = SM83_CORE_OP2;
        self.cpu.instruction = Gb::sm83_instruction_irq_stall;
    }

    fn sm83_instruction_irq_delay(&mut self) {
        self.cpu.sp = self.cpu.sp.wrapping_sub(1);
        self.cpu.index = self.cpu.sp;
        self.cpu.bus = self.cpu.pc as u8;
        self.cpu.execution_state = SM83_CORE_MEMORY_STORE;
        self.cpu.instruction = Gb::sm83_instruction_irq_finish;
        self.cpu.pc = self.irq_vector();
        self.set_active_region(self.cpu.pc);
    }

    fn sm83_instruction_irq(&mut self) {
        self.cpu.sp = self.cpu.sp.wrapping_sub(1);
        self.cpu.index = self.cpu.sp;
        self.cpu.bus = (self.cpu.pc >> 8) as u8;
        self.cpu.execution_state = SM83_CORE_MEMORY_STORE;
        self.cpu.instruction = Gb::sm83_instruction_irq_delay;
    }

    /// _SM83Step: one M-cycle of bus activity.
    fn sm83_step(&mut self) {
        self.cpu.cycles += self.cpu.t_multiplier;
        let state = self.cpu.execution_state;
        self.cpu.execution_state = SM83_CORE_IDLE_0;
        match state {
            SM83_CORE_FETCH => {
                if self.cpu.irq_pending {
                    self.cpu.index = self.cpu.sp;
                    self.cpu.irq_pending = false;
                    self.cpu.instruction = Gb::sm83_instruction_irq;
                    self.gb_set_interrupts(false);
                    return;
                }
                self.cpu.bus = self.cpu_load8(self.cpu.pc);
                self.cpu.instruction = isa::INSTRUCTION_TABLE[self.cpu.bus as usize];
                self.cpu.pc = self.cpu.pc.wrapping_add(1);
            }
            SM83_CORE_MEMORY_LOAD => {
                self.cpu.bus = self.load8(self.cpu.index);
            }
            SM83_CORE_MEMORY_STORE => {
                let (index, bus) = (self.cpu.index, self.cpu.bus);
                self.store8(index, bus);
            }
            SM83_CORE_READ_PC => {
                self.cpu.bus = self.cpu_load8(self.cpu.pc);
                self.cpu.pc = self.cpu.pc.wrapping_add(1);
            }
            SM83_CORE_STALL => {
                self.cpu.instruction = isa::INSTRUCTION_TABLE[0]; // NOP
            }
            SM83_CORE_HALT_BUG => {
                if self.cpu.irq_pending {
                    self.cpu.index = self.cpu.sp;
                    self.cpu.irq_pending = false;
                    self.cpu.instruction = Gb::sm83_instruction_irq;
                    self.gb_set_interrupts(false);
                    return;
                }
                self.cpu.bus = self.cpu_load8(self.cpu.pc);
                self.cpu.instruction = isa::INSTRUCTION_TABLE[self.cpu.bus as usize];
                // NB: no ++pc (the halt bug re-executes the next opcode)
            }
            _ => {}
        }
    }

    /// _SM83TickInternal: returns false when the caller's budget has expired
    /// mid-tick (mirrors the C's early-exit behavior).
    fn sm83_tick_internal(&mut self) -> bool {
        let mut running = true;
        self.sm83_step();
        let t = self.cpu.t_multiplier;
        if self.cpu.cycles + t * 2 >= self.cpu.next_event {
            if self.cpu.cycles >= self.cpu.next_event {
                self.process_events();
            }
            self.cpu.cycles += t;
            self.cpu.execution_state += 1;
            if self.cpu.cycles >= self.cpu.next_event {
                self.process_events();
            }
            self.cpu.cycles += t;
            self.cpu.execution_state += 1;
            if self.cpu.cycles >= self.cpu.next_event {
                self.process_events();
            }
            running = false;
        } else {
            self.cpu.cycles += t * 2;
        }
        self.cpu.execution_state = SM83_CORE_FETCH;
        let instr = self.cpu.instruction;
        instr(self);
        self.cpu.cycles += t;
        running
    }

    /// SM83Tick: single instruction step (used by the debugger).
    pub fn sm83_tick(&mut self) {
        while self.cpu.cycles >= self.cpu.next_event {
            self.process_events();
        }
        self.sm83_tick_internal();
        while self.cpu.cycles >= self.cpu.next_event {
            self.process_events();
        }
    }

    /// SM83Run
    pub fn sm83_run(&mut self) {
        let mut running = true;
        while running || self.cpu.execution_state != SM83_CORE_FETCH {
            if self.cpu.cycles < self.cpu.next_event {
                running = self.sm83_tick_internal() && running;
            } else {
                self.process_events();
                running = false;
            }
        }
    }
}
