// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/sm83/isa-sm83.c and include/mgba/internal/sm83/emitter-sm83.h.
//
// Function names are the C handler names lowercased. Each handler is one
// M-cycle of the instruction microcode; handlers chain via `cpu.instruction`
// and schedule bus activity via `cpu.execution_state`.

use super::{
    Instruction, Sm83, SM83_CORE_FETCH, SM83_CORE_MEMORY_LOAD, SM83_CORE_MEMORY_STORE,
    SM83_CORE_OP2, SM83_CORE_READ_PC, SM83_CORE_STALL,
};
use crate::gb::Gb;

#[inline]
fn cpu(gb: &mut Gb) -> &mut Sm83 {
    &mut gb.cpu
}

pub fn nop(_gb: &mut Gb) {}

// ----- Jumps -----

fn jp_finish(gb: &mut Gb) {
    if cpu(gb).condition {
        let c = cpu(gb);
        c.pc = (c.bus as u16) << 8 | c.index;
        gb.set_active_region(gb.cpu.pc);
        cpu(gb).execution_state = SM83_CORE_STALL;
    }
}

fn jp_delay(gb: &mut Gb) {
    cpu(gb).execution_state = SM83_CORE_READ_PC;
    cpu(gb).instruction = jp_finish;
    cpu(gb).index = cpu(gb).bus as u16;
}

macro_rules! define_jp {
    ($name:ident, $cond:expr) => {
        pub fn $name(gb: &mut Gb) {
            cpu(gb).execution_state = SM83_CORE_READ_PC;
            cpu(gb).instruction = jp_delay;
            let c = cpu(gb);
            let cond: bool = $cond(&mut *c);
            c.condition = cond;
        }
    };
}

define_jp!(jp, |_c| true);
define_jp!(jpc, |c: &mut Sm83| c.f.c());
define_jp!(jpz, |c: &mut Sm83| c.f.z());
define_jp!(jpnc, |c: &mut Sm83| !c.f.c());
define_jp!(jpnz, |c: &mut Sm83| !c.f.z());

pub fn jphl(gb: &mut Gb) {
    let hl = cpu(gb).hl();
    cpu(gb).pc = hl;
    gb.set_active_region(gb.cpu.pc);
}

fn jr_finish(gb: &mut Gb) {
    if cpu(gb).condition {
        let offset = cpu(gb).bus as i8;
        cpu(gb).pc = cpu(gb).pc.wrapping_add(offset as u16);
        gb.set_active_region(gb.cpu.pc);
        cpu(gb).execution_state = SM83_CORE_STALL;
    }
}

macro_rules! define_jr {
    ($name:ident, $cond:expr) => {
        pub fn $name(gb: &mut Gb) {
            cpu(gb).execution_state = SM83_CORE_READ_PC;
            cpu(gb).instruction = jr_finish;
            let c = cpu(gb);
            let cond: bool = $cond(&mut *c);
            c.condition = cond;
        }
    };
}

define_jr!(jr, |_c| true);
define_jr!(jrc, |c: &mut Sm83| c.f.c());
define_jr!(jrz, |c: &mut Sm83| c.f.z());
define_jr!(jrnc, |c: &mut Sm83| !c.f.c());
define_jr!(jrnz, |c: &mut Sm83| !c.f.z());

// ----- Calls/returns -----

fn call_update_spl(gb: &mut Gb) {
    cpu(gb).index = cpu(gb).index.wrapping_sub(1);
    cpu(gb).bus = cpu(gb).sp as u8;
    cpu(gb).sp = cpu(gb).index;
    cpu(gb).execution_state = SM83_CORE_MEMORY_STORE;
    cpu(gb).instruction = nop;
}

fn call_update_sph(gb: &mut Gb) {
    cpu(gb).execution_state = SM83_CORE_MEMORY_STORE;
    cpu(gb).instruction = call_update_spl;
}

fn call_update_pch(gb: &mut Gb) {
    if cpu(gb).condition {
        let new_pc = (cpu(gb).bus as u16) << 8 | cpu(gb).index;
        cpu(gb).bus = (cpu(gb).pc >> 8) as u8;
        cpu(gb).index = cpu(gb).sp.wrapping_sub(1);
        cpu(gb).sp = cpu(gb).pc; // GROSS (as in the C)
        cpu(gb).pc = new_pc;
        gb.set_active_region(gb.cpu.pc);
        cpu(gb).execution_state = SM83_CORE_OP2;
        cpu(gb).instruction = call_update_sph;
    }
}

fn call_update_pcl(gb: &mut Gb) {
    cpu(gb).execution_state = SM83_CORE_READ_PC;
    cpu(gb).index = cpu(gb).bus as u16;
    cpu(gb).instruction = call_update_pch;
}

macro_rules! define_call {
    ($name:ident, $cond:expr) => {
        pub fn $name(gb: &mut Gb) {
            let c = cpu(gb);
            let cond: bool = $cond(&mut *c);
            c.condition = cond;
            c.execution_state = SM83_CORE_READ_PC;
            c.instruction = call_update_pcl;
        }
    };
}

define_call!(callnz, |c: &mut Sm83| !c.f.z());
define_call!(callz, |c: &mut Sm83| c.f.z());
define_call!(callnc, |c: &mut Sm83| !c.f.c());
define_call!(callc, |c: &mut Sm83| c.f.c());
define_call!(call, |_c| true);

fn ret_finish(gb: &mut Gb) {
    let c = cpu(gb);
    c.sp = c.sp.wrapping_add(2); /* TODO: Atomic incrementing? */
    c.pc |= (c.bus as u16) << 8;
    gb.set_active_region(gb.cpu.pc);
    cpu(gb).execution_state = SM83_CORE_STALL;
}

fn ret_update_spl(gb: &mut Gb) {
    let c = cpu(gb);
    c.index = c.sp.wrapping_add(1);
    c.pc = c.bus as u16;
    c.execution_state = SM83_CORE_MEMORY_LOAD;
    c.instruction = ret_finish;
}

fn ret_update_sph(gb: &mut Gb) {
    if cpu(gb).condition {
        let c = cpu(gb);
        c.index = c.sp;
        c.execution_state = SM83_CORE_MEMORY_LOAD;
        c.instruction = ret_update_spl;
    }
}

macro_rules! define_ret {
    ($name:ident, $cond:expr) => {
        pub fn $name(gb: &mut Gb) {
            let c = cpu(gb);
            let cond: bool = $cond(&mut *c);
            c.condition = cond;
            c.execution_state = SM83_CORE_OP2;
            c.instruction = ret_update_sph;
        }
    };
}

pub fn ret(gb: &mut Gb) {
    cpu(gb).condition = true;
    ret_update_sph(gb);
}

pub fn reti(gb: &mut Gb) {
    cpu(gb).condition = true;
    gb.gb_set_interrupts(true);
    ret_update_sph(gb);
}

define_ret!(retnz, |c: &mut Sm83| !c.f.z());
define_ret!(retz, |c: &mut Sm83| c.f.z());
define_ret!(retnc, |c: &mut Sm83| !c.f.c());
define_ret!(retc, |c: &mut Sm83| c.f.c());

// ----- 8-bit ALU: logic -----

macro_rules! define_logic {
    ($name:ident, $bus_name:ident, $hl_name:ident, $op:tt, $h_after:expr) => {
        pub fn $bus_name(gb: &mut Gb) {
            let bus = cpu(gb).bus;
            let c = cpu(gb);
            c.a = c.a $op bus;
            c.f.set_z(c.a == 0);
            c.f.set_n(false);
            c.f.set_c(false);
            c.f.set_h($h_after);
        }
        pub fn $hl_name(gb: &mut Gb) {
            let hl = cpu(gb).hl();
            let c = cpu(gb);
            c.execution_state = SM83_CORE_MEMORY_LOAD;
            c.index = hl;
            c.instruction = $bus_name;
        }
        pub fn $name(gb: &mut Gb) {
            cpu(gb).execution_state = SM83_CORE_READ_PC;
            cpu(gb).instruction = $bus_name;
        }
    };
}

define_logic!(and, and_bus, and_hl, &, true);
define_logic!(xor, xor_bus, xor_hl, ^, false);
define_logic!(or, or_bus, or_hl, |, false);

macro_rules! define_logic_reg {
    ($bus_name:ident, $name:ident, $reg:ident, $op:tt, $h_after:expr) => {
        pub fn $name(gb: &mut Gb) {
            let v = cpu(gb).$reg;
            let c = cpu(gb);
            c.a = c.a $op v;
            c.f.set_z(c.a == 0);
            c.f.set_n(false);
            c.f.set_c(false);
            c.f.set_h($h_after);
        }
    };
}

define_logic_reg!(and_bus, and_a, a, &, true);
define_logic_reg!(and_bus, and_b, b, &, true);
define_logic_reg!(and_bus, and_c, c, &, true);
define_logic_reg!(and_bus, and_d, d, &, true);
define_logic_reg!(and_bus, and_e, e, &, true);
define_logic_reg!(and_bus, and_h, h, &, true);
define_logic_reg!(and_bus, and_l, l, &, true);

define_logic_reg!(xor_bus, xor_a, a, ^, false);
define_logic_reg!(xor_bus, xor_b, b, ^, false);
define_logic_reg!(xor_bus, xor_c, c, ^, false);
define_logic_reg!(xor_bus, xor_d, d, ^, false);
define_logic_reg!(xor_bus, xor_e, e, ^, false);
define_logic_reg!(xor_bus, xor_h, h, ^, false);
define_logic_reg!(xor_bus, xor_l, l, ^, false);

define_logic_reg!(or_bus, or_a, a, |, false);
define_logic_reg!(or_bus, or_b, b, |, false);
define_logic_reg!(or_bus, or_c, c, |, false);
define_logic_reg!(or_bus, or_d, d, |, false);
define_logic_reg!(or_bus, or_e, e, |, false);
define_logic_reg!(or_bus, or_h, h, |, false);
define_logic_reg!(or_bus, or_l, l, |, false);

// ----- 8-bit ALU: CP -----

fn cp_impl(c: &mut Sm83, operand: u8) {
    let a = c.a;
    let diff = a as i32 - operand as i32;
    c.f.set_n(true);
    c.f.set_z((diff & 0xFF) == 0);
    c.f.set_h((a as i32 & 0xF) - (operand as i32 & 0xF) < 0);
    c.f.set_c(diff < 0);
}

pub fn cp_bus(gb: &mut Gb) {
    let bus = cpu(gb).bus;
    cp_impl(cpu(gb), bus);
}

pub fn cp_hl(gb: &mut Gb) {
    let hl = cpu(gb).hl();
    let c = cpu(gb);
    c.execution_state = SM83_CORE_MEMORY_LOAD;
    c.index = hl;
    c.instruction = cp_bus;
}

pub fn cp(gb: &mut Gb) {
    cpu(gb).execution_state = SM83_CORE_READ_PC;
    cpu(gb).instruction = cp_bus;
}

macro_rules! define_cp_reg {
    ($name:ident, $reg:ident) => {
        pub fn $name(gb: &mut Gb) {
            let v = cpu(gb).$reg;
            cp_impl(cpu(gb), v);
        }
    };
}

define_cp_reg!(cp_a, a);
define_cp_reg!(cp_b, b);
define_cp_reg!(cp_c, c);
define_cp_reg!(cp_d, d);
define_cp_reg!(cp_e, e);
define_cp_reg!(cp_h, h);
define_cp_reg!(cp_l, l);

// ----- 8-bit LDs -----

macro_rules! define_ld {
    ($bus_name:ident, $name:ident, $dst:ident, $src:ident) => {
        pub fn $name(gb: &mut Gb) {
            let v = cpu(gb).$src;
            cpu(gb).$dst = v;
        }
    };
}

define_ld!(lda_bus, lda_a, a, a);
define_ld!(lda_bus, lda_b, a, b);
define_ld!(lda_bus, lda_c, a, c);
define_ld!(lda_bus, lda_d, a, d);
define_ld!(lda_bus, lda_e, a, e);
define_ld!(lda_bus, lda_h, a, h);
define_ld!(lda_bus, lda_l, a, l);

define_ld!(ldb_bus, ldb_a, b, a);
define_ld!(ldb_bus, ldb_b, b, b);
define_ld!(ldb_bus, ldb_c, b, c);
define_ld!(ldb_bus, ldb_d, b, d);
define_ld!(ldb_bus, ldb_e, b, e);
define_ld!(ldb_bus, ldb_h, b, h);
define_ld!(ldb_bus, ldb_l, b, l);

define_ld!(ldc_bus, ldc_a, c, a);
define_ld!(ldc_bus, ldc_b, c, b);
define_ld!(ldc_bus, ldc_c, c, c);
define_ld!(ldc_bus, ldc_d, c, d);
define_ld!(ldc_bus, ldc_e, c, e);
define_ld!(ldc_bus, ldc_h, c, h);
define_ld!(ldc_bus, ldc_l, c, l);

define_ld!(ldd_bus, ldd_a, d, a);
define_ld!(ldd_bus, ldd_b, d, b);
define_ld!(ldd_bus, ldd_c, d, c);
define_ld!(ldd_bus, ldd_d, d, d);
define_ld!(ldd_bus, ldd_e, d, e);
define_ld!(ldd_bus, ldd_h, d, h);
define_ld!(ldd_bus, ldd_l, d, l);

define_ld!(lde_bus, lde_a, e, a);
define_ld!(lde_bus, lde_b, e, b);
define_ld!(lde_bus, lde_c, e, c);
define_ld!(lde_bus, lde_d, e, d);
define_ld!(lde_bus, lde_e, e, e);
define_ld!(lde_bus, lde_h, e, h);
define_ld!(lde_bus, lde_l, e, l);

define_ld!(ldh_bus, ldh_a, h, a);
define_ld!(ldh_bus, ldh_b, h, b);
define_ld!(ldh_bus, ldh_c, h, c);
define_ld!(ldh_bus, ldh_d, h, d);
define_ld!(ldh_bus, ldh_e, h, e);
define_ld!(ldh_bus, ldh_h, h, h);
define_ld!(ldh_bus, ldh_l, h, l);

define_ld!(ldl_bus, ldl_a, l, a);
define_ld!(ldl_bus, ldl_b, l, b);
define_ld!(ldl_bus, ldl_c, l, c);
define_ld!(ldl_bus, ldl_d, l, d);
define_ld!(ldl_bus, ldl_e, l, e);
define_ld!(ldl_bus, ldl_h, l, h);
define_ld!(ldl_bus, ldl_l, l, l);

// LD r,(HL) loads
macro_rules! define_ld_from_hl {
    ($bus_name:ident, $name:ident) => {
        pub fn $name(gb: &mut Gb) {
            let hl = cpu(gb).hl();
            let c = cpu(gb);
            c.execution_state = SM83_CORE_MEMORY_LOAD;
            c.index = hl;
            c.instruction = $bus_name;
        }
    };
}

fn ld_from_bus_impl(gb: &mut Gb, reg: u8) -> u8 {
    let _ = gb;
    reg
}

pub fn lda_bus(gb: &mut Gb) {
    let v = cpu(gb).bus;
    cpu(gb).a = v;
}
pub fn ldb_bus(gb: &mut Gb) {
    let v = cpu(gb).bus;
    cpu(gb).b = v;
}
pub fn ldc_bus(gb: &mut Gb) {
    let v = cpu(gb).bus;
    cpu(gb).c = v;
}
pub fn ldd_bus(gb: &mut Gb) {
    let v = cpu(gb).bus;
    cpu(gb).d = v;
}
pub fn lde_bus(gb: &mut Gb) {
    let v = cpu(gb).bus;
    cpu(gb).e = v;
}
pub fn ldh_bus(gb: &mut Gb) {
    let v = cpu(gb).bus;
    cpu(gb).h = v;
}
pub fn ldl_bus(gb: &mut Gb) {
    let v = cpu(gb).bus;
    cpu(gb).l = v;
}

define_ld_from_hl!(lda_bus, lda_hl);
define_ld_from_hl!(ldb_bus, ldb_hl);
define_ld_from_hl!(ldc_bus, ldc_hl);
define_ld_from_hl!(ldd_bus, ldd_hl);
define_ld_from_hl!(lde_bus, lde_hl);
define_ld_from_hl!(ldh_bus, ldh_hl);
define_ld_from_hl!(ldl_bus, ldl_hl);

// LD r,d8 (immediate)
macro_rules! define_ld_imm {
    ($bus_name:ident, $name:ident) => {
        pub fn $name(gb: &mut Gb) {
            cpu(gb).execution_state = SM83_CORE_READ_PC;
            cpu(gb).instruction = $bus_name;
        }
    };
}

define_ld_imm!(lda_bus, lda_i);
define_ld_imm!(ldb_bus, ldb_i);
define_ld_imm!(ldc_bus, ldc_i);
define_ld_imm!(ldd_bus, ldd_i);
define_ld_imm!(lde_bus, lde_i);
define_ld_imm!(ldh_bus, ldh_i);
define_ld_imm!(ldl_bus, ldl_i);

// LD A,(BC) / LD A,(DE) — DEFINE_ALU_INSTRUCTION_SM83_MEM(LDA_, BC)/(LDA_, DE)
pub fn lda_bc(gb: &mut Gb) {
    let addr = cpu(gb).bc();
    let c = cpu(gb);
    c.execution_state = SM83_CORE_MEMORY_LOAD;
    c.index = addr;
    c.instruction = lda_bus;
}

pub fn lda_de(gb: &mut Gb) {
    let addr = cpu(gb).de();
    let c = cpu(gb);
    c.execution_state = SM83_CORE_MEMORY_LOAD;
    c.index = addr;
    c.instruction = lda_bus;
}

// LD (HL),r
macro_rules! define_ld_hl_store {
    ($name:ident, $src:ident) => {
        pub fn $name(gb: &mut Gb) {
            let v = cpu(gb).$src;
            let hl = cpu(gb).hl();
            let c = cpu(gb);
            c.bus = v;
            c.index = hl;
            c.execution_state = SM83_CORE_MEMORY_STORE;
            c.instruction = nop;
        }
    };
}

define_ld_hl_store!(ldhl_a, a);
define_ld_hl_store!(ldhl_b, b);
define_ld_hl_store!(ldhl_c, c);
define_ld_hl_store!(ldhl_d, d);
define_ld_hl_store!(ldhl_e, e);
define_ld_hl_store!(ldhl_h, h);
define_ld_hl_store!(ldhl_l, l);

// LD (HL),d8
pub fn ldhl_bus(gb: &mut Gb) {
    let hl = cpu(gb).hl();
    let c = cpu(gb);
    c.index = hl;
    c.execution_state = SM83_CORE_MEMORY_STORE;
    c.instruction = nop;
}

pub fn ldhl_i(gb: &mut Gb) {
    cpu(gb).execution_state = SM83_CORE_READ_PC;
    cpu(gb).instruction = ldhl_bus;
}

// ----- LDHL SP+e8 / LD SP,HL -----

fn ldhl_sp_delay(gb: &mut Gb) {
    let c = cpu(gb);
    let diff = c.bus as i8;
    let sum = (c.sp as i32) + diff as i32;
    let imm = diff as i32 as u16;
    c.set_hl(sum as u16);
    c.execution_state = SM83_CORE_STALL;
    c.f.set_z(false);
    c.f.set_n(false);
    c.f.set_c((imm & 0xFF) + (c.sp & 0xFF) >= 0x100);
    c.f.set_h((imm & 0xF) + (c.sp & 0xF) >= 0x10);
}

pub fn ldhl_sp(gb: &mut Gb) {
    cpu(gb).execution_state = SM83_CORE_READ_PC;
    cpu(gb).instruction = ldhl_sp_delay;
}

pub fn ldsp_hl(gb: &mut Gb) {
    let hl = cpu(gb).hl();
    let c = cpu(gb);
    c.sp = hl;
    c.execution_state = SM83_CORE_STALL;
}

// ----- 8-bit ALU: ADD/ADC/SUB/SBC -----

macro_rules! define_arith {
    ($name:ident, $bus_name:ident, $hl_name:ident, $body:expr) => {
        pub fn $bus_name(gb: &mut Gb) {
            let bus = cpu(gb).bus;
            let c = cpu(gb);
            let f: fn(&mut Sm83, u8) = $body;
            f(c, bus);
        }
        pub fn $hl_name(gb: &mut Gb) {
            let hl = cpu(gb).hl();
            let c = cpu(gb);
            c.execution_state = SM83_CORE_MEMORY_LOAD;
            c.index = hl;
            c.instruction = $bus_name;
        }
        pub fn $name(gb: &mut Gb) {
            cpu(gb).execution_state = SM83_CORE_READ_PC;
            cpu(gb).instruction = $bus_name;
        }
    };
}

fn add_impl(c: &mut Sm83, operand: u8) {
    let a = c.a;
    let diff = a as i32 + operand as i32;
    c.f.set_n(false);
    c.f.set_h(((a as i32) & 0xF) + ((operand as i32) & 0xF) >= 0x10);
    c.f.set_c(diff >= 0x100);
    c.a = diff as u8;
    c.f.set_z(c.a == 0);
}

fn adc_impl(c: &mut Sm83, operand: u8) {
    let a = c.a;
    let carry = c.f.c() as i32;
    let diff = a as i32 + operand as i32 + carry;
    c.f.set_n(false);
    c.f.set_h(((a as i32) & 0xF) + ((operand as i32) & 0xF) + carry >= 0x10);
    c.f.set_c(diff >= 0x100);
    c.a = diff as u8;
    c.f.set_z(c.a == 0);
}

fn sub_impl(c: &mut Sm83, operand: u8) {
    let a = c.a;
    let diff = a as i32 - operand as i32;
    c.f.set_n(true);
    c.f.set_h((a as i32 & 0xF) - (operand as i32 & 0xF) < 0);
    c.f.set_c(diff < 0);
    c.a = diff as u8;
    c.f.set_z(c.a == 0);
}

fn sbc_impl(c: &mut Sm83, operand: u8) {
    let a = c.a;
    let carry = c.f.c() as i32;
    let diff = a as i32 - operand as i32 - carry;
    c.f.set_n(true);
    c.f.set_h((a as i32 & 0xF) - (operand as i32 & 0xF) - carry < 0);
    c.f.set_c(diff < 0);
    c.a = diff as u8;
    c.f.set_z(c.a == 0);
}

define_arith!(add, add_bus, add_hl, add_impl);
define_arith!(adc, adc_bus, adc_hl, adc_impl);
define_arith!(sub, sub_bus, sub_hl, sub_impl);
define_arith!(sbc, sbc_bus, sbc_hl, sbc_impl);

macro_rules! define_arith_reg {
    ($bus_name:ident, $name:ident, $reg:ident, $body:expr) => {
        pub fn $name(gb: &mut Gb) {
            let v = cpu(gb).$reg;
            let c = cpu(gb);
            let f: fn(&mut Sm83, u8) = $body;
            f(c, v);
        }
    };
}

define_arith_reg!(add_bus, add_a, a, add_impl);
define_arith_reg!(add_bus, add_b, b, add_impl);
define_arith_reg!(add_bus, add_c, c, add_impl);
define_arith_reg!(add_bus, add_d, d, add_impl);
define_arith_reg!(add_bus, add_e, e, add_impl);
define_arith_reg!(add_bus, add_h, h, add_impl);
define_arith_reg!(add_bus, add_l, l, add_impl);

define_arith_reg!(adc_bus, adc_a, a, adc_impl);
define_arith_reg!(adc_bus, adc_b, b, adc_impl);
define_arith_reg!(adc_bus, adc_c, c, adc_impl);
define_arith_reg!(adc_bus, adc_d, d, adc_impl);
define_arith_reg!(adc_bus, adc_e, e, adc_impl);
define_arith_reg!(adc_bus, adc_h, h, adc_impl);
define_arith_reg!(adc_bus, adc_l, l, adc_impl);

define_arith_reg!(sub_bus, sub_a, a, sub_impl);
define_arith_reg!(sub_bus, sub_b, b, sub_impl);
define_arith_reg!(sub_bus, sub_c, c, sub_impl);
define_arith_reg!(sub_bus, sub_d, d, sub_impl);
define_arith_reg!(sub_bus, sub_e, e, sub_impl);
define_arith_reg!(sub_bus, sub_h, h, sub_impl);
define_arith_reg!(sub_bus, sub_l, l, sub_impl);

define_arith_reg!(sbc_bus, sbc_a, a, sbc_impl);
define_arith_reg!(sbc_bus, sbc_b, b, sbc_impl);
define_arith_reg!(sbc_bus, sbc_c, c, sbc_impl);
define_arith_reg!(sbc_bus, sbc_d, d, sbc_impl);
define_arith_reg!(sbc_bus, sbc_e, e, sbc_impl);
define_arith_reg!(sbc_bus, sbc_h, h, sbc_impl);
define_arith_reg!(sbc_bus, sbc_l, l, sbc_impl);

// ----- ADD SP,e8 -----

fn addsp_finish(gb: &mut Gb) {
    let index = cpu(gb).index;
    let c = cpu(gb);
    c.sp = index;
    c.execution_state = SM83_CORE_STALL;
}

fn addsp_delay(gb: &mut Gb) {
    let c = cpu(gb);
    let diff = c.bus as i8;
    let sum = c.sp as i32 + diff as i32;
    c.index = sum as u16;
    c.execution_state = SM83_CORE_OP2;
    c.instruction = addsp_finish;
    let imm = c.bus as u16;
    c.f.set_z(false);
    c.f.set_n(false);
    c.f.set_c((imm & 0xFF) + (c.sp & 0xFF) >= 0x100);
    c.f.set_h((imm & 0xF) + (c.sp & 0xF) >= 0x10);
}

pub fn addsp(gb: &mut Gb) {
    cpu(gb).execution_state = SM83_CORE_READ_PC;
    cpu(gb).instruction = addsp_delay;
}

// ----- 16-bit LDs -----

fn ldbc_delay(gb: &mut Gb) {
    let v = cpu(gb).bus;
    let c = cpu(gb);
    c.c = v;
    c.execution_state = SM83_CORE_READ_PC;
    c.instruction = ldb_bus;
}

pub fn ldbc(gb: &mut Gb) {
    cpu(gb).execution_state = SM83_CORE_READ_PC;
    cpu(gb).instruction = ldbc_delay;
}

pub fn ldbc_a(gb: &mut Gb) {
    let bc = cpu(gb).bc();
    let a = cpu(gb).a;
    let c = cpu(gb);
    c.index = bc;
    c.bus = a;
    c.execution_state = SM83_CORE_MEMORY_STORE;
    c.instruction = nop;
}

fn ldde_delay(gb: &mut Gb) {
    let v = cpu(gb).bus;
    let c = cpu(gb);
    c.e = v;
    c.execution_state = SM83_CORE_READ_PC;
    c.instruction = ldd_bus;
}

pub fn ldde(gb: &mut Gb) {
    cpu(gb).execution_state = SM83_CORE_READ_PC;
    cpu(gb).instruction = ldde_delay;
}

pub fn ldde_a(gb: &mut Gb) {
    let de = cpu(gb).de();
    let a = cpu(gb).a;
    let c = cpu(gb);
    c.index = de;
    c.bus = a;
    c.execution_state = SM83_CORE_MEMORY_STORE;
    c.instruction = nop;
}

fn ldhl_delay(gb: &mut Gb) {
    let v = cpu(gb).bus;
    let c = cpu(gb);
    c.l = v;
    c.execution_state = SM83_CORE_READ_PC;
    c.instruction = ldh_bus;
}

pub fn ldhl(gb: &mut Gb) {
    cpu(gb).execution_state = SM83_CORE_READ_PC;
    cpu(gb).instruction = ldhl_delay;
}

fn ldsp_finish(gb: &mut Gb) {
    let bus = cpu(gb).bus;
    cpu(gb).sp |= (bus as u16) << 8;
}

fn ldsp_delay(gb: &mut Gb) {
    let bus = cpu(gb).bus;
    let c = cpu(gb);
    c.sp = bus as u16;
    c.execution_state = SM83_CORE_READ_PC;
    c.instruction = ldsp_finish;
}

pub fn ldsp(gb: &mut Gb) {
    cpu(gb).execution_state = SM83_CORE_READ_PC;
    cpu(gb).instruction = ldsp_delay;
}

pub fn ldihla(gb: &mut Gb) {
    let hl = cpu(gb).hl();
    let a = cpu(gb).a;
    let c = cpu(gb);
    c.index = hl;
    c.set_hl(hl.wrapping_add(1));
    c.bus = a;
    c.execution_state = SM83_CORE_MEMORY_STORE;
    c.instruction = nop;
}

pub fn lddhla(gb: &mut Gb) {
    let hl = cpu(gb).hl();
    let a = cpu(gb).a;
    let c = cpu(gb);
    c.index = hl;
    c.set_hl(hl.wrapping_sub(1));
    c.bus = a;
    c.execution_state = SM83_CORE_MEMORY_STORE;
    c.instruction = nop;
}

pub fn lda_ihl(gb: &mut Gb) {
    let hl = cpu(gb).hl();
    let c = cpu(gb);
    c.index = hl;
    c.set_hl(hl.wrapping_add(1));
    c.execution_state = SM83_CORE_MEMORY_LOAD;
    c.instruction = lda_bus;
}

pub fn lda_dhl(gb: &mut Gb) {
    let hl = cpu(gb).hl();
    let c = cpu(gb);
    c.index = hl;
    c.set_hl(hl.wrapping_sub(1));
    c.execution_state = SM83_CORE_MEMORY_LOAD;
    c.instruction = lda_bus;
}

// ----- LD (a16),A / LD A,(a16) -----

fn ldia_finish(gb: &mut Gb) {
    let c = cpu(gb);
    c.index |= (c.bus as u16) << 8;
    c.bus = c.a;
    c.execution_state = SM83_CORE_MEMORY_STORE;
    c.instruction = nop;
}

fn ldia_delay(gb: &mut Gb) {
    let bus = cpu(gb).bus;
    let c = cpu(gb);
    c.index = bus as u16;
    c.execution_state = SM83_CORE_READ_PC;
    c.instruction = ldia_finish;
}

pub fn ldia(gb: &mut Gb) {
    cpu(gb).execution_state = SM83_CORE_READ_PC;
    cpu(gb).instruction = ldia_delay;
}

fn ldai_finish(gb: &mut Gb) {
    let c = cpu(gb);
    c.index |= (c.bus as u16) << 8;
    c.execution_state = SM83_CORE_MEMORY_LOAD;
    c.instruction = lda_bus;
}

fn ldai_delay(gb: &mut Gb) {
    let bus = cpu(gb).bus;
    let c = cpu(gb);
    c.index = bus as u16;
    c.execution_state = SM83_CORE_READ_PC;
    c.instruction = ldai_finish;
}

pub fn ldai(gb: &mut Gb) {
    cpu(gb).execution_state = SM83_CORE_READ_PC;
    cpu(gb).instruction = ldai_delay;
}

// ----- LDH (FF00+n),A etc. -----

pub fn lda_ioc(gb: &mut Gb) {
    let reg_c = cpu(gb).c;
    let c = cpu(gb);
    c.index = 0xFF00 | reg_c as u16;
    c.execution_state = SM83_CORE_MEMORY_LOAD;
    c.instruction = lda_bus;
}

pub fn ld_ioc_a(gb: &mut Gb) {
    let reg_c = cpu(gb).c;
    let a = cpu(gb).a;
    let c = cpu(gb);
    c.index = 0xFF00 | reg_c as u16;
    c.bus = a;
    c.execution_state = SM83_CORE_MEMORY_STORE;
    c.instruction = nop;
}

fn ldaio_delay(gb: &mut Gb) {
    let bus = cpu(gb).bus;
    let c = cpu(gb);
    c.index = 0xFF00 | bus as u16;
    c.execution_state = SM83_CORE_MEMORY_LOAD;
    c.instruction = lda_bus;
}

pub fn ldaio(gb: &mut Gb) {
    cpu(gb).execution_state = SM83_CORE_READ_PC;
    cpu(gb).instruction = ldaio_delay;
}

fn ldioa_delay(gb: &mut Gb) {
    let bus = cpu(gb).bus;
    let a = cpu(gb).a;
    let c = cpu(gb);
    c.index = 0xFF00 | bus as u16;
    c.bus = a;
    c.execution_state = SM83_CORE_MEMORY_STORE;
    c.instruction = nop;
}

pub fn ldioa(gb: &mut Gb) {
    cpu(gb).execution_state = SM83_CORE_READ_PC;
    cpu(gb).instruction = ldioa_delay;
}

// ----- LD (a16),SP -----

fn ldisp_store_h(gb: &mut Gb) {
    let c = cpu(gb);
    c.index = c.index.wrapping_add(1);
    c.bus = (c.sp >> 8) as u8;
    c.execution_state = SM83_CORE_MEMORY_STORE;
    c.instruction = nop;
}

fn ldisp_store_l(gb: &mut Gb) {
    let c = cpu(gb);
    c.index |= (c.bus as u16) << 8;
    c.bus = c.sp as u8;
    c.execution_state = SM83_CORE_MEMORY_STORE;
    c.instruction = ldisp_store_h;
}

fn ldisp_read_addr(gb: &mut Gb) {
    let bus = cpu(gb).bus;
    let c = cpu(gb);
    c.index = bus as u16;
    c.execution_state = SM83_CORE_READ_PC;
    c.instruction = ldisp_store_l;
}

pub fn ldisp(gb: &mut Gb) {
    cpu(gb).execution_state = SM83_CORE_READ_PC;
    cpu(gb).instruction = ldisp_read_addr;
}

// ----- 16-bit INC/DEC -----

macro_rules! define_incdec_wide {
    ($inc:ident, $dec:ident, $getter:ident, $setter:ident) => {
        pub fn $inc(gb: &mut Gb) {
            let reg = cpu(gb).$getter();
            let c = cpu(gb);
            c.$setter(reg.wrapping_add(1));
            c.execution_state = SM83_CORE_STALL;
        }
        pub fn $dec(gb: &mut Gb) {
            let reg = cpu(gb).$getter();
            let c = cpu(gb);
            c.$setter(reg.wrapping_sub(1));
            c.execution_state = SM83_CORE_STALL;
        }
    };
}

define_incdec_wide!(incbc, decbc, bc, set_bc);
define_incdec_wide!(incde, decde, de, set_de);
define_incdec_wide!(inchl, dechl, hl, set_hl);

pub fn incsp(gb: &mut Gb) {
    let c = cpu(gb);
    c.sp = c.sp.wrapping_add(1);
    c.execution_state = SM83_CORE_STALL;
}

pub fn decsp(gb: &mut Gb) {
    let c = cpu(gb);
    c.sp = c.sp.wrapping_sub(1);
    c.execution_state = SM83_CORE_STALL;
}

// ----- ADD HL,rr -----

macro_rules! define_add_hl {
    ($name:ident, $finish:ident, $lo:expr, $hi:expr) => {
        fn $finish(gb: &mut Gb) {
            let h_in = $hi(cpu(gb));
            let c = cpu(gb);
            let hi = h_in as i32;
            let diff = hi + c.h as i32 + c.f.c() as i32;
            c.f.set_n(false);
            c.f.set_h((hi & 0xF) + (c.h as i32 & 0xF) + c.f.c() as i32 >= 0x10);
            c.f.set_c(diff >= 0x100);
            c.h = diff as u8;
        }
        pub fn $name(gb: &mut Gb) {
            let l_in = $lo(cpu(gb));
            let c = cpu(gb);
            let lo = l_in as i32;
            let diff = lo + c.l as i32;
            c.l = diff as u8;
            c.f.set_c(diff >= 0x100);
            c.execution_state = SM83_CORE_OP2;
            c.instruction = $finish;
        }
    };
}

define_add_hl!(addhl_bc, addhl_bc_finish, |c: &mut Sm83| c.c, |c: &mut Sm83| c.b);
define_add_hl!(addhl_de, addhl_de_finish, |c: &mut Sm83| c.e, |c: &mut Sm83| c.d);
define_add_hl!(addhl_hl, addhl_hl_finish, |c: &mut Sm83| c.l, |c: &mut Sm83| c.h);
define_add_hl!(addhl_sp, addhl_sp_finish, |c: &mut Sm83| (c.sp & 0xFF) as u8, |c: &mut Sm83| (c.sp >> 8) as u8);

// ----- 8-bit INC/DEC -----

macro_rules! define_inc {
    ($name:ident, $reg:ident) => {
        pub fn $name(gb: &mut Gb) {
            let v = cpu(gb).$reg;
            let c = cpu(gb);
            let diff = v.wrapping_add(1);
            c.f.set_h((v & 0xF) == 0xF);
            c.$reg = diff;
            c.f.set_n(false);
            c.f.set_z(diff == 0);
        }
    };
}

macro_rules! define_dec {
    ($name:ident, $reg:ident) => {
        pub fn $name(gb: &mut Gb) {
            let v = cpu(gb).$reg;
            let c = cpu(gb);
            let diff = v.wrapping_sub(1);
            c.f.set_h((v & 0xF) == 0x0);
            c.$reg = diff;
            c.f.set_n(true);
            c.f.set_z(diff == 0);
        }
    };
}

define_inc!(inc_a, a);
define_inc!(inc_b, b);
define_inc!(inc_c, c);
define_inc!(inc_d, d);
define_inc!(inc_e, e);
define_inc!(inc_h, h);
define_inc!(inc_l, l);

define_dec!(dec_a, a);
define_dec!(dec_b, b);
define_dec!(dec_c, c);
define_dec!(dec_d, d);
define_dec!(dec_e, e);
define_dec!(dec_h, h);
define_dec!(dec_l, l);

fn inc_hl_delay(gb: &mut Gb) {
    let c = cpu(gb);
    let diff = c.bus.wrapping_add(1);
    c.f.set_n(false);
    c.f.set_h((c.bus & 0xF) == 0xF);
    c.bus = diff;
    c.f.set_z(c.bus == 0);
    c.instruction = nop;
    c.execution_state = SM83_CORE_MEMORY_STORE;
}

pub fn inc_hl(gb: &mut Gb) {
    let hl = cpu(gb).hl();
    let c = cpu(gb);
    c.index = hl;
    c.instruction = inc_hl_delay;
    c.execution_state = SM83_CORE_MEMORY_LOAD;
}

fn dec_hl_delay(gb: &mut Gb) {
    let c = cpu(gb);
    let diff = c.bus.wrapping_sub(1);
    c.f.set_n(true);
    c.f.set_h((c.bus & 0xF) == 0);
    c.bus = diff;
    c.f.set_z(c.bus == 0);
    c.instruction = nop;
    c.execution_state = SM83_CORE_MEMORY_STORE;
}

pub fn dec_hl(gb: &mut Gb) {
    let hl = cpu(gb).hl();
    let c = cpu(gb);
    c.index = hl;
    c.instruction = dec_hl_delay;
    c.execution_state = SM83_CORE_MEMORY_LOAD;
}

// ----- Misc single-byte ops -----

pub fn scf(gb: &mut Gb) {
    let c = cpu(gb);
    c.f.set_c(true);
    c.f.set_h(false);
    c.f.set_n(false);
}

pub fn ccf(gb: &mut Gb) {
    let c = cpu(gb);
    let carry = c.f.c();
    c.f.set_c(!carry);
    c.f.set_h(false);
    c.f.set_n(false);
}

pub fn cpl_(gb: &mut Gb) {
    let c = cpu(gb);
    c.a ^= 0xFF;
    c.f.set_h(true);
    c.f.set_n(true);
}

pub fn daa(gb: &mut Gb) {
    let c = cpu(gb);
    if c.f.n() {
        if c.f.h() {
            c.a = c.a.wrapping_add(0xFA);
        }
        if c.f.c() {
            c.a = c.a.wrapping_add(0xA0);
        }
    } else {
        let mut a = c.a as i32;
        if (c.a & 0xF) > 0x9 || c.f.h() {
            a += 0x6;
        }
        if (a & 0x1F0) > 0x90 || c.f.c() {
            a += 0x60;
            c.f.set_c(true);
        } else {
            c.f.set_c(false);
        }
        c.a = a as u8;
    }
    c.f.set_h(false);
    c.f.set_z(c.a == 0);
}

// ----- POP/PUSH -----

macro_rules! define_pop {
    ($pop:ident, $pop_delay:ident, $hi_instr:ident, $lo_write:expr) => {
        fn $pop_delay(gb: &mut Gb) {
            let bus = cpu(gb).bus;
            let c = cpu(gb);
            let lw: fn(&mut Sm83, u8) = $lo_write;
            lw(c, bus);
            c.f.packed &= 0xF0; // exactly as the C macro (harmless for BC/DE/HL)
            c.index = c.sp;
            c.sp = c.sp.wrapping_add(1);
            c.instruction = $hi_instr;
            c.execution_state = SM83_CORE_MEMORY_LOAD;
        }
        pub fn $pop(gb: &mut Gb) {
            let c = cpu(gb);
            c.index = c.sp;
            c.sp = c.sp.wrapping_add(1);
            c.instruction = $pop_delay;
            c.execution_state = SM83_CORE_MEMORY_LOAD;
        }
    };
}

macro_rules! define_push {
    ($push:ident, $push_delay:ident, $push_finish:ident, $hi_read:expr, $lo_read:expr) => {
        fn $push_finish(gb: &mut Gb) {
            cpu(gb).execution_state = SM83_CORE_STALL;
        }
        fn $push_delay(gb: &mut Gb) {
            let c = cpu(gb);
            c.sp = c.sp.wrapping_sub(1);
            c.index = c.sp;
            let lr: fn(&Sm83) -> u8 = $lo_read;
            c.bus = lr(c);
            c.instruction = $push_finish;
            c.execution_state = SM83_CORE_MEMORY_STORE;
        }
        pub fn $push(gb: &mut Gb) {
            let c = cpu(gb);
            c.sp = c.sp.wrapping_sub(1);
            c.index = c.sp;
            let hr: fn(&Sm83) -> u8 = $hi_read;
            c.bus = hr(c);
            c.instruction = $push_delay;
            c.execution_state = SM83_CORE_MEMORY_STORE;
        }
    };
}

// POP uses the generic "ld r,bus" handlers for the high byte, as in the C.
define_pop!(popbc, popbc_delay, ldb_bus, |c: &mut Sm83, v: u8| c.c = v);
define_pop!(popde, popde_delay, ldd_bus, |c: &mut Sm83, v: u8| c.e = v);
define_pop!(pophl, pophl_delay, ldh_bus, |c: &mut Sm83, v: u8| c.l = v);
define_pop!(popaf, popaf_delay, lda_bus, |c: &mut Sm83, v: u8| c.f.packed = v);

define_push!(pushbc, pushbc_delay, pushbc_finish, |c: &Sm83| c.b, |c: &Sm83| c.c);
define_push!(pushde, pushde_delay, pushde_finish, |c: &Sm83| c.d, |c: &Sm83| c.e);
define_push!(pushhl, pushhl_delay, pushhl_finish, |c: &Sm83| c.h, |c: &Sm83| c.l);
define_push!(pushaf, pushaf_delay, pushaf_finish, |c: &Sm83| c.a, |c: &Sm83| c.f.packed);

// ----- CB-prefixed: BIT/RES/SET -----

// (The CB families are generated per-register by smaller macros below.)

macro_rules! define_cb_reg {
    ($name:ident, $reg:ident, $body:expr) => {
        pub fn $name(gb: &mut Gb) {
            let c = cpu(gb);
            let f: fn(&mut Sm83, u8) -> u8 = $body;
            let reg = c.$reg;
            c.$reg = f(c, reg);
        }
    };
}

macro_rules! define_cb_hl {
    ($delay:ident, $name:ident, $body:expr, $wb:expr) => {
        fn $delay(gb: &mut Gb) {
            let c = cpu(gb);
            let f: fn(&mut Sm83, u8) -> u8 = $body;
            let reg = c.bus;
            c.bus = f(c, reg);
            c.execution_state = $wb;
            c.instruction = nop;
        }
        pub fn $name(gb: &mut Gb) {
            let hl = cpu(gb).hl();
            let c = cpu(gb);
            c.index = hl;
            c.execution_state = SM83_CORE_MEMORY_LOAD;
            c.instruction = $delay;
        }
    };
}

macro_rules! bit_body {
    ($bit:expr) => {
        |c: &mut Sm83, reg: u8| -> u8 {
            c.f.set_n(false);
            c.f.set_h(true);
            c.f.set_z((reg & $bit) == 0);
            reg
        }
    };
}

macro_rules! res_body {
    ($bit:expr) => {
        |_c: &mut Sm83, reg: u8| -> u8 { reg & !$bit }
    };
}

macro_rules! set_body {
    ($bit:expr) => {
        |_c: &mut Sm83, reg: u8| -> u8 { reg | $bit }
    };
}

macro_rules! alu_body {
    ($op:expr) => {
        |c: &mut Sm83, reg: u8| -> u8 {
            let out = $op(c, reg);
            c.f.set_n(false);
            c.f.set_h(false);
            c.f.set_z(out == 0);
            out
        }
    };
}

fn cb_rl(c: &mut Sm83, reg: u8) -> u8 {
    let wide = ((reg as u16) << 1) | c.f.c() as u16;
    c.f.set_c((wide >> 8) != 0);
    wide as u8
}
fn cb_rlc(c: &mut Sm83, reg: u8) -> u8 {
    let out = (reg << 1) | (reg >> 7);
    c.f.set_c((out & 1) != 0);
    out
}
fn cb_rr(c: &mut Sm83, reg: u8) -> u8 {
    let low = reg & 1;
    let out = (reg >> 1) | ((c.f.c() as u8) << 7);
    c.f.set_c(low != 0);
    out
}
fn cb_rrc(c: &mut Sm83, reg: u8) -> u8 {
    let low = reg & 1;
    let out = (reg >> 1) | (low << 7);
    c.f.set_c(low != 0);
    out
}
fn cb_sla(c: &mut Sm83, reg: u8) -> u8 {
    c.f.set_c((reg >> 7) != 0);
    reg << 1
}
fn cb_sra(c: &mut Sm83, reg: u8) -> u8 {
    c.f.set_c((reg & 1) != 0);
    ((reg as i8) >> 1) as u8
}
fn cb_srl(c: &mut Sm83, reg: u8) -> u8 {
    c.f.set_c((reg & 1) != 0);
    reg >> 1
}
fn cb_swap(c: &mut Sm83, reg: u8) -> u8 {
    c.f.set_c(false);
    (reg << 4) | (reg >> 4)
}

macro_rules! define_cb_family {
    ($base:ident, $body:expr) => {
        paste::paste! {
            define_cb_reg!([<$base b>], b, $body);
            define_cb_reg!([<$base c>], c, $body);
            define_cb_reg!([<$base d>], d, $body);
            define_cb_reg!([<$base e>], e, $body);
            define_cb_reg!([<$base h>], h, $body);
            define_cb_reg!([<$base l>], l, $body);
            define_cb_hl!([<$base hl_delay>], [<$base hl>], $body, SM83_CORE_MEMORY_STORE);
            define_cb_reg!([<$base a>], a, $body);
        }
    };
}

macro_rules! define_cb_bit_family {
    ($bit:expr, $base:ident) => {
        paste::paste! {
            define_cb_reg!([<$base b>], b, bit_body!($bit));
            define_cb_reg!([<$base c>], c, bit_body!($bit));
            define_cb_reg!([<$base d>], d, bit_body!($bit));
            define_cb_reg!([<$base e>], e, bit_body!($bit));
            define_cb_reg!([<$base h>], h, bit_body!($bit));
            define_cb_reg!([<$base l>], l, bit_body!($bit));
            define_cb_hl!([<$base hl_delay>], [<$base hl>], bit_body!($bit), SM83_CORE_FETCH);
            define_cb_reg!([<$base a>], a, bit_body!($bit));
        }
    };
}

define_cb_family!(rlc, alu_body!(cb_rlc));
define_cb_family!(rrc, alu_body!(cb_rrc));
define_cb_family!(rl, alu_body!(cb_rl));
define_cb_family!(rr, alu_body!(cb_rr));
define_cb_family!(sla, alu_body!(cb_sla));
define_cb_family!(sra, alu_body!(cb_sra));
define_cb_family!(swap, alu_body!(cb_swap));
define_cb_family!(srl, alu_body!(cb_srl));

define_cb_bit_family!(1, bit0);
define_cb_bit_family!(2, bit1);
define_cb_bit_family!(4, bit2);
define_cb_bit_family!(8, bit3);
define_cb_bit_family!(16, bit4);
define_cb_bit_family!(32, bit5);
define_cb_bit_family!(64, bit6);
define_cb_bit_family!(128, bit7);

macro_rules! define_cb_res_set_family {
    ($bit:expr, $res_base:ident, $set_base:ident) => {
        paste::paste! {
            define_cb_reg!([<$res_base b>], b, res_body!($bit));
            define_cb_reg!([<$res_base c>], c, res_body!($bit));
            define_cb_reg!([<$res_base d>], d, res_body!($bit));
            define_cb_reg!([<$res_base e>], e, res_body!($bit));
            define_cb_reg!([<$res_base h>], h, res_body!($bit));
            define_cb_reg!([<$res_base l>], l, res_body!($bit));
            define_cb_hl!([<$res_base hl_delay>], [<$res_base hl>], res_body!($bit), SM83_CORE_MEMORY_STORE);
            define_cb_reg!([<$res_base a>], a, res_body!($bit));

            define_cb_reg!([<$set_base b>], b, set_body!($bit));
            define_cb_reg!([<$set_base c>], c, set_body!($bit));
            define_cb_reg!([<$set_base d>], d, set_body!($bit));
            define_cb_reg!([<$set_base e>], e, set_body!($bit));
            define_cb_reg!([<$set_base h>], h, set_body!($bit));
            define_cb_reg!([<$set_base l>], l, set_body!($bit));
            define_cb_hl!([<$set_base hl_delay>], [<$set_base hl>], set_body!($bit), SM83_CORE_MEMORY_STORE);
            define_cb_reg!([<$set_base a>], a, set_body!($bit));
        }
    };
}

define_cb_res_set_family!(1, res0, set0);
define_cb_res_set_family!(2, res1, set1);
define_cb_res_set_family!(4, res2, set2);
define_cb_res_set_family!(8, res3, set3);
define_cb_res_set_family!(16, res4, set4);
define_cb_res_set_family!(32, res5, set5);
define_cb_res_set_family!(64, res6, set6);
define_cb_res_set_family!(128, res7, set7);

// ----- Accumulator rotates -----

pub fn rla_(gb: &mut Gb) {
    let c = cpu(gb);
    let wide = ((c.a as u16) << 1) | c.f.c() as u16;
    c.a = wide as u8;
    c.f.set_z(false);
    c.f.set_h(false);
    c.f.set_n(false);
    c.f.set_c((wide >> 8) != 0);
}

pub fn rlca_(gb: &mut Gb) {
    let c = cpu(gb);
    c.a = (c.a << 1) | (c.a >> 7);
    c.f.set_z(false);
    c.f.set_h(false);
    c.f.set_n(false);
    c.f.set_c((c.a & 1) != 0);
}

pub fn rra_(gb: &mut Gb) {
    let c = cpu(gb);
    let low = c.a & 1;
    c.a = (c.a >> 1) | ((c.f.c() as u8) << 7);
    c.f.set_z(false);
    c.f.set_h(false);
    c.f.set_n(false);
    c.f.set_c(low != 0);
}

pub fn rrca_(gb: &mut Gb) {
    let c = cpu(gb);
    let low = c.a & 1;
    c.a = (c.a >> 1) | (low << 7);
    c.f.set_z(false);
    c.f.set_h(false);
    c.f.set_n(false);
    c.f.set_c(low != 0);
}

// ----- Interrupts / control -----

pub fn di(gb: &mut Gb) {
    gb.gb_set_interrupts(false);
}

pub fn ei(gb: &mut Gb) {
    gb.gb_set_interrupts(true);
}

pub fn halt(gb: &mut Gb) {
    gb.gb_halt();
    // XXX: Subtract the cycles that will be added later in the tick function
    let t = cpu(gb).t_multiplier;
    cpu(gb).cycles -= t;
}

macro_rules! define_rst {
    ($name:ident, $sph:ident, $spl:ident, $vec:expr) => {
        fn $spl(gb: &mut Gb) {
            let c = cpu(gb);
            c.sp = c.sp.wrapping_sub(1);
            c.index = c.sp;
            c.bus = c.pc as u8;
            c.pc = $vec;
            gb.set_active_region(gb.cpu.pc);
            let c = cpu(gb);
            c.execution_state = SM83_CORE_MEMORY_STORE;
            c.instruction = nop;
        }
        fn $sph(gb: &mut Gb) {
            let c = cpu(gb);
            c.sp = c.sp.wrapping_sub(1);
            c.index = c.sp;
            c.bus = (c.pc >> 8) as u8;
            c.execution_state = SM83_CORE_MEMORY_STORE;
            c.instruction = $spl;
        }
        pub fn $name(gb: &mut Gb) {
            let c = cpu(gb);
            c.execution_state = SM83_CORE_OP2;
            c.instruction = $sph;
        }
    };
}

define_rst!(rst00, rst00_update_sph, rst00_update_spl, 0x00);
define_rst!(rst08, rst08_update_sph, rst08_update_spl, 0x08);
define_rst!(rst10, rst10_update_sph, rst10_update_spl, 0x10);
define_rst!(rst18, rst18_update_sph, rst18_update_spl, 0x18);
define_rst!(rst20, rst20_update_sph, rst20_update_spl, 0x20);
define_rst!(rst28, rst28_update_sph, rst28_update_spl, 0x28);
define_rst!(rst30, rst30_update_sph, rst30_update_spl, 0x30);
define_rst!(rst38, rst38_update_sph, rst38_update_spl, 0x38);

pub fn ill(gb: &mut Gb) {
    gb.gb_hit_illegal();
}

fn stop2(gb: &mut Gb) {
    gb.gb_stop();
}

pub fn stop(gb: &mut Gb) {
    cpu(gb).execution_state = SM83_CORE_READ_PC;
    cpu(gb).instruction = stop2;
}

// ----- CB delegate -----

fn cb_delegate(gb: &mut Gb) {
    let sub = cpu(gb).bus;
    CB_INSTRUCTION_TABLE[sub as usize](gb);
}

pub fn cb(gb: &mut Gb) {
    cpu(gb).execution_state = SM83_CORE_READ_PC;
    cpu(gb).instruction = cb_delegate;
}

// ----- Decode tables (order mirrors DECLARE_SM83_EMITTER_BLOCK) -----

pub static INSTRUCTION_TABLE: [Instruction; 0x100] = [
    nop, ldbc, ldbc_a, incbc, inc_b, dec_b, ldb_i, rlca_, ldisp, addhl_bc, lda_bc, decbc, inc_c,
    dec_c, ldc_i, rrca_, stop, ldde, ldde_a, incde, inc_d, dec_d, ldd_i, rla_, jr, addhl_de, lda_de,
    decde, inc_e, dec_e, lde_i, rra_, jrnz, ldhl, ldihla, inchl, inc_h, dec_h, ldh_i, daa, jrz,
    addhl_hl, lda_ihl, dechl, inc_l, dec_l, ldl_i, cpl_, jrnc, ldsp, lddhla, incsp, inc_hl,
    dec_hl, ldhl_i, scf, jrc, addhl_sp, lda_dhl, decsp, inc_a, dec_a, lda_i, ccf, ldb_b, ldb_c,
    ldb_d, ldb_e, ldb_h, ldb_l, ldb_hl, ldb_a, ldc_b, ldc_c, ldc_d, ldc_e, ldc_h, ldc_l, ldc_hl,
    ldc_a, ldd_b, ldd_c, ldd_d, ldd_e, ldd_h, ldd_l, ldd_hl, ldd_a, lde_b, lde_c, lde_d, lde_e,
    lde_h, lde_l, lde_hl, lde_a, ldh_b, ldh_c, ldh_d, ldh_e, ldh_h, ldh_l, ldh_hl, ldh_a, ldl_b,
    ldl_c, ldl_d, ldl_e, ldl_h, ldl_l, ldl_hl, ldl_a, ldhl_b, ldhl_c, ldhl_d, ldhl_e, ldhl_h,
    ldhl_l, halt, ldhl_a, lda_b, lda_c, lda_d, lda_e, lda_h, lda_l, lda_hl, lda_a, add_b, add_c,
    add_d, add_e, add_h, add_l, add_hl, add_a, adc_b, adc_c, adc_d, adc_e, adc_h, adc_l, adc_hl,
    adc_a, sub_b, sub_c, sub_d, sub_e, sub_h, sub_l, sub_hl, sub_a, sbc_b, sbc_c, sbc_d, sbc_e,
    sbc_h, sbc_l, sbc_hl, sbc_a, and_b, and_c, and_d, and_e, and_h, and_l, and_hl, and_a, xor_b,
    xor_c, xor_d, xor_e, xor_h, xor_l, xor_hl, xor_a, or_b, or_c, or_d, or_e, or_h, or_l, or_hl,
    or_a, cp_b, cp_c, cp_d, cp_e, cp_h, cp_l, cp_hl, cp_a, retnz, popbc, jpnz, jp, callnz, pushbc,
    add, rst00, retz, ret, jpz, cb, callz, call, adc, rst08, retnc, popde, jpnc, ill, callnc,
    pushde, sub, rst10, retc, reti, jpc, ill, callc, ill, sbc, rst18, ldioa, pophl, ld_ioc_a, ill,
    ill, pushhl, and, rst20, addsp, jphl, ldia, ill, ill, ill, xor, rst28, ldaio, popaf, lda_ioc,
    di, ill, pushaf, or, rst30, ldhl_sp, ldsp_hl, ldai, ei, ill, ill, cp, rst38,
];

pub static CB_INSTRUCTION_TABLE: [Instruction; 0x100] = [
    rlcb, rlcc, rlcd, rlce, rlch, rlcl, rlchl, rlca, rrcb, rrcc, rrcd, rrce, rrch, rrcl, rrchl,
    rrca, rlb, rlc, rld, rle, rlh, rll, rlhl, rla, rrb, rrc, rrd, rre, rrh, rrl, rrhl, rra, slab,
    slac, slad, slae, slah, slal, slahl, slaa, srab, srac, srad, srae, srah, sral, srahl, sraa,
    swapb, swapc, swapd, swape, swaph, swapl, swaphl, swapa, srlb, srlc, srld, srle, srlh, srll,
    srlhl, srla, bit0b, bit0c, bit0d, bit0e, bit0h, bit0l, bit0hl, bit0a, bit1b, bit1c, bit1d,
    bit1e, bit1h, bit1l, bit1hl, bit1a, bit2b, bit2c, bit2d, bit2e, bit2h, bit2l, bit2hl, bit2a,
    bit3b, bit3c, bit3d, bit3e, bit3h, bit3l, bit3hl, bit3a, bit4b, bit4c, bit4d, bit4e, bit4h,
    bit4l, bit4hl, bit4a, bit5b, bit5c, bit5d, bit5e, bit5h, bit5l, bit5hl, bit5a, bit6b, bit6c,
    bit6d, bit6e, bit6h, bit6l, bit6hl, bit6a, bit7b, bit7c, bit7d, bit7e, bit7h, bit7l, bit7hl,
    bit7a, res0b, res0c, res0d, res0e, res0h, res0l, res0hl, res0a, res1b, res1c, res1d, res1e,
    res1h, res1l, res1hl, res1a, res2b, res2c, res2d, res2e, res2h, res2l, res2hl, res2a, res3b,
    res3c, res3d, res3e, res3h, res3l, res3hl, res3a, res4b, res4c, res4d, res4e, res4h, res4l,
    res4hl, res4a, res5b, res5c, res5d, res5e, res5h, res5l, res5hl, res5a, res6b, res6c, res6d,
    res6e, res6h, res6l, res6hl, res6a, res7b, res7c, res7d, res7e, res7h, res7l, res7hl, res7a,
    set0b, set0c, set0d, set0e, set0h, set0l, set0hl, set0a, set1b, set1c, set1d, set1e, set1h,
    set1l, set1hl, set1a, set2b, set2c, set2d, set2e, set2h, set2l, set2hl, set2a, set3b, set3c,
    set3d, set3e, set3h, set3l, set3hl, set3a, set4b, set4c, set4d, set4e, set4h, set4l, set4hl,
    set4a, set5b, set5c, set5d, set5e, set5h, set5l, set5hl, set5a, set6b, set6c, set6d, set6e,
    set6h, set6l, set6hl, set6a, set7b, set7c, set7d, set7e, set7h, set7l, set7hl, set7a,
];
