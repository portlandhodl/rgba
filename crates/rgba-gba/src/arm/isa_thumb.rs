// Copyright (c) 2013-2014 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/arm/isa-thumb.c. Flag/carry math is from
// mgba/include/mgba/internal/arm/isa-inlines.h (ARM_CARRY_FROM etc.); the decode
// table at the bottom follows the order of DECLARE_THUMB_EMITTER_BLOCK in
// mgba/include/mgba/internal/arm/emitter-thumb.h.
//
// Function names are the C handler names lowercased (`_ThumbInstructionLSL1` →
// `lsl1`, `_ThumbInstructionADD400` → `add400`, ...). Each handler mirrors the
// DEFINE_INSTRUCTION_THUMB frame: current_cycles starts at THUMB_PREFETCH_CYCLES
// (1 + active_seq_cycles16) and is added into cpu.cycles at the end.

use super::{ExecutionMode, ARM_LR, ARM_PC, ARM_SP, LSM_DB, LSM_IA};
use crate::gba::Gba;

// ----- isa-inlines.h flag math -----

/// ARM_SIGN(I)
#[inline]
fn arm_sign(i: i32) -> bool {
    i < 0
}

/// ARM_CARRY_FROM(M, N, D)
#[inline]
fn arm_carry_from(m: i32, n: i32, d: i32) -> bool {
    ((m as u32) >> 31) + ((n as u32) >> 31) > ((d as u32) >> 31)
}

/// ARM_BORROW_FROM(M, N, D) (D unused in the C macro)
#[inline]
fn arm_borrow_from(m: i32, n: i32) -> bool {
    (m as u32) >= (n as u32)
}

/// ARM_BORROW_FROM_CARRY(M, N, D, C) (D unused in the C macro)
#[inline]
fn arm_borrow_from_carry(m: i32, n: i32, c: bool) -> bool {
    (m as u32 as u64) >= (n as u32 as u64) + (c as u64)
}

/// ARM_V_ADDITION(M, N, D)
#[inline]
fn arm_v_addition(m: i32, n: i32, d: i32) -> bool {
    !arm_sign(m ^ n) && arm_sign(m ^ d)
}

/// ARM_V_SUBTRACTION(M, N, D)
#[inline]
fn arm_v_subtraction(m: i32, n: i32, d: i32) -> bool {
    arm_sign(m ^ n) && arm_sign(m ^ d)
}

// ----- isa-thumb.c THUMB_*_S flag macros -----

/// THUMB_ADDITION_S(M, N, D)
#[inline]
fn thumb_addition_s(gba: &mut Gba, m: i32, n: i32, d: i32) {
    gba.cpu.cpsr.set_n(arm_sign(d));
    gba.cpu.cpsr.set_z(d == 0);
    gba.cpu.cpsr.set_c(arm_carry_from(m, n, d));
    gba.cpu.cpsr.set_v(arm_v_addition(m, n, d));
}

/// THUMB_SUBTRACTION_S(M, N, D)
#[inline]
fn thumb_subtraction_s(gba: &mut Gba, m: i32, n: i32, d: i32) {
    gba.cpu.cpsr.set_n(arm_sign(d));
    gba.cpu.cpsr.set_z(d == 0);
    gba.cpu.cpsr.set_c(arm_borrow_from(m, n));
    gba.cpu.cpsr.set_v(arm_v_subtraction(m, n, d));
}

/// THUMB_SUBTRACTION_CARRY_S(M, N, D, C)
#[inline]
fn thumb_subtraction_carry_s(gba: &mut Gba, m: i32, n: i32, d: i32, c: bool) {
    gba.cpu.cpsr.set_n(arm_sign(d));
    gba.cpu.cpsr.set_z(d == 0);
    gba.cpu.cpsr.set_c(arm_borrow_from_carry(m, n, c));
    gba.cpu.cpsr.set_v(arm_v_subtraction(m, n, d));
}

/// THUMB_NEUTRAL_S(M, N, D) (only D is used in the C macro)
#[inline]
fn thumb_neutral_s(gba: &mut Gba, d: i32) {
    gba.cpu.cpsr.set_n(arm_sign(d));
    gba.cpu.cpsr.set_z(d == 0);
}

/// THUMB_LOAD_POST_BODY / THUMB_STORE_POST_BODY
#[inline]
fn thumb_ls_post_body(gba: &Gba) -> i32 {
    gba.cpu.active_nonseq_cycles16 - gba.cpu.active_seq_cycles16
}

// ----- Shifts by immediate (DEFINE_IMMEDIATE_5_INSTRUCTION_THUMB) -----

fn lsl1(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16; // THUMB_PREFETCH_CYCLES
    let immediate = ((opcode >> 6) & 0x001F) as i32;
    let rd = (opcode & 0x0007) as usize;
    let rm = ((opcode >> 3) & 0x0007) as usize;
    if immediate == 0 {
        gba.cpu.gprs[rd] = gba.cpu.gprs[rm];
    } else {
        gba.cpu.cpsr.set_c(((gba.cpu.gprs[rm] >> (32 - immediate)) & 1) != 0);
        gba.cpu.gprs[rd] = gba.cpu.gprs[rm] << immediate;
    }
    thumb_neutral_s(gba, gba.cpu.gprs[rd]);
    gba.cpu.cycles += current_cycles;
}

fn lsr1(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let immediate = ((opcode >> 6) & 0x001F) as i32;
    let rd = (opcode & 0x0007) as usize;
    let rm = ((opcode >> 3) & 0x0007) as usize;
    if immediate == 0 {
        gba.cpu.cpsr.set_c(arm_sign(gba.cpu.gprs[rm]));
        gba.cpu.gprs[rd] = 0;
    } else {
        gba.cpu.cpsr.set_c(((gba.cpu.gprs[rm] >> (immediate - 1)) & 1) != 0);
        gba.cpu.gprs[rd] = ((gba.cpu.gprs[rm] as u32) >> immediate) as i32;
    }
    thumb_neutral_s(gba, gba.cpu.gprs[rd]);
    gba.cpu.cycles += current_cycles;
}

fn asr1(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let immediate = ((opcode >> 6) & 0x001F) as i32;
    let rd = (opcode & 0x0007) as usize;
    let rm = ((opcode >> 3) & 0x0007) as usize;
    if immediate == 0 {
        gba.cpu.cpsr.set_c(arm_sign(gba.cpu.gprs[rm]));
        if gba.cpu.cpsr.c() {
            gba.cpu.gprs[rd] = -1; // 0xFFFFFFFF
        } else {
            gba.cpu.gprs[rd] = 0;
        }
    } else {
        gba.cpu.cpsr.set_c(((gba.cpu.gprs[rm] >> (immediate - 1)) & 1) != 0);
        gba.cpu.gprs[rd] = gba.cpu.gprs[rm] >> immediate;
    }
    thumb_neutral_s(gba, gba.cpu.gprs[rd]);
    gba.cpu.cycles += current_cycles;
}

// ----- Load/store with immediate offset (DEFINE_IMMEDIATE_5_INSTRUCTION_THUMB) -----

fn ldr1(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let immediate = ((opcode >> 6) & 0x001F) as i32;
    let rd = (opcode & 0x0007) as usize;
    let rm = ((opcode >> 3) & 0x0007) as usize;
    let address = gba.cpu.gprs[rm].wrapping_add(immediate * 4) as u32;
    gba.cpu.gprs[rd] = gba.load32(address, &mut current_cycles) as i32;
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.cycles += current_cycles;
}

fn ldrb1(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let immediate = ((opcode >> 6) & 0x001F) as i32;
    let rd = (opcode & 0x0007) as usize;
    let rm = ((opcode >> 3) & 0x0007) as usize;
    let address = gba.cpu.gprs[rm].wrapping_add(immediate) as u32;
    gba.cpu.gprs[rd] = gba.load8(address, &mut current_cycles) as i32;
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.cycles += current_cycles;
}

fn ldrh1(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let immediate = ((opcode >> 6) & 0x001F) as i32;
    let rd = (opcode & 0x0007) as usize;
    let rm = ((opcode >> 3) & 0x0007) as usize;
    let address = gba.cpu.gprs[rm].wrapping_add(immediate * 2) as u32;
    gba.cpu.gprs[rd] = gba.load16(address, &mut current_cycles) as i32;
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.cycles += current_cycles;
}

fn str1(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let immediate = ((opcode >> 6) & 0x001F) as i32;
    let rd = (opcode & 0x0007) as usize;
    let rm = ((opcode >> 3) & 0x0007) as usize;
    let address = gba.cpu.gprs[rm].wrapping_add(immediate * 4) as u32;
    gba.store32(address, gba.cpu.gprs[rd], &mut current_cycles);
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.cycles += current_cycles;
}

fn strb1(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let immediate = ((opcode >> 6) & 0x001F) as i32;
    let rd = (opcode & 0x0007) as usize;
    let rm = ((opcode >> 3) & 0x0007) as usize;
    let address = gba.cpu.gprs[rm].wrapping_add(immediate) as u32;
    gba.store8(address, gba.cpu.gprs[rd], &mut current_cycles);
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.cycles += current_cycles;
}

fn strh1(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let immediate = ((opcode >> 6) & 0x001F) as i32;
    let rd = (opcode & 0x0007) as usize;
    let rm = ((opcode >> 3) & 0x0007) as usize;
    let address = gba.cpu.gprs[rm].wrapping_add(immediate * 2) as u32;
    gba.store16(address, gba.cpu.gprs[rd], &mut current_cycles);
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.cycles += current_cycles;
}

// ----- 3-register ADD/SUB (DEFINE_DATA_FORM_1_INSTRUCTION_THUMB) -----

fn add3(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rm = ((opcode >> 6) & 0x0007) as usize;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    // THUMB_ADDITION
    let n = gba.cpu.gprs[rm];
    let m = gba.cpu.gprs[rn];
    let d = m.wrapping_add(n);
    gba.cpu.gprs[rd] = d;
    thumb_addition_s(gba, m, n, d);
    gba.cpu.cycles += current_cycles;
}

fn sub3(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rm = ((opcode >> 6) & 0x0007) as usize;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    // THUMB_SUBTRACTION
    let n = gba.cpu.gprs[rm];
    let m = gba.cpu.gprs[rn];
    let d = m.wrapping_sub(n);
    gba.cpu.gprs[rd] = d;
    thumb_subtraction_s(gba, m, n, d);
    gba.cpu.cycles += current_cycles;
}

// ----- ADD/SUB with 3-bit immediate (DEFINE_DATA_FORM_2_INSTRUCTION_THUMB) -----

fn add1(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let immediate = ((opcode >> 6) & 0x0007) as i32;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    // THUMB_ADDITION
    let n = immediate;
    let m = gba.cpu.gprs[rn];
    let d = m.wrapping_add(n);
    gba.cpu.gprs[rd] = d;
    thumb_addition_s(gba, m, n, d);
    gba.cpu.cycles += current_cycles;
}

fn sub1(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let immediate = ((opcode >> 6) & 0x0007) as i32;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    // THUMB_SUBTRACTION
    let n = immediate;
    let m = gba.cpu.gprs[rn];
    let d = m.wrapping_sub(n);
    gba.cpu.gprs[rd] = d;
    thumb_subtraction_s(gba, m, n, d);
    gba.cpu.cycles += current_cycles;
}

// ----- MOV/CMP/ADD/SUB with 8-bit immediate (DEFINE_DATA_FORM_3_INSTRUCTION_THUMB) -----

fn add2(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = ((opcode >> 8) & 0x0007) as usize;
    let immediate = (opcode & 0x00FF) as i32;
    // THUMB_ADDITION
    let n = immediate;
    let m = gba.cpu.gprs[rd];
    let d = m.wrapping_add(n);
    gba.cpu.gprs[rd] = d;
    thumb_addition_s(gba, m, n, d);
    gba.cpu.cycles += current_cycles;
}

fn cmp1(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = ((opcode >> 8) & 0x0007) as usize;
    let immediate = (opcode & 0x00FF) as i32;
    let m = gba.cpu.gprs[rd];
    let alu_out = m.wrapping_sub(immediate);
    thumb_subtraction_s(gba, m, immediate, alu_out);
    gba.cpu.cycles += current_cycles;
}

fn mov1(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = ((opcode >> 8) & 0x0007) as usize;
    let immediate = (opcode & 0x00FF) as i32;
    gba.cpu.gprs[rd] = immediate;
    thumb_neutral_s(gba, gba.cpu.gprs[rd]);
    gba.cpu.cycles += current_cycles;
}

fn sub2(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = ((opcode >> 8) & 0x0007) as usize;
    let immediate = (opcode & 0x00FF) as i32;
    // THUMB_SUBTRACTION
    let n = immediate;
    let m = gba.cpu.gprs[rd];
    let d = m.wrapping_sub(n);
    gba.cpu.gprs[rd] = d;
    thumb_subtraction_s(gba, m, n, d);
    gba.cpu.cycles += current_cycles;
}

// ----- ALU ops (DEFINE_DATA_FORM_5_INSTRUCTION_THUMB) -----

fn and(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    gba.cpu.gprs[rd] = gba.cpu.gprs[rd] & gba.cpu.gprs[rn];
    thumb_neutral_s(gba, gba.cpu.gprs[rd]);
    gba.cpu.cycles += current_cycles;
}

fn eor(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    gba.cpu.gprs[rd] = gba.cpu.gprs[rd] ^ gba.cpu.gprs[rn];
    thumb_neutral_s(gba, gba.cpu.gprs[rd]);
    gba.cpu.cycles += current_cycles;
}

fn lsl2(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    let rs = gba.cpu.gprs[rn] & 0xFF;
    if rs != 0 {
        if rs < 32 {
            gba.cpu.cpsr.set_c(((gba.cpu.gprs[rd] >> (32 - rs)) & 1) != 0);
            gba.cpu.gprs[rd] <<= rs;
        } else {
            if rs > 32 {
                gba.cpu.cpsr.set_c(false);
            } else {
                gba.cpu.cpsr.set_c((gba.cpu.gprs[rd] & 0x00000001) != 0);
            }
            gba.cpu.gprs[rd] = 0;
        }
    }
    current_cycles += 1;
    thumb_neutral_s(gba, gba.cpu.gprs[rd]);
    gba.cpu.cycles += current_cycles;
}

fn lsr2(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    let rs = gba.cpu.gprs[rn] & 0xFF;
    if rs != 0 {
        if rs < 32 {
            gba.cpu.cpsr.set_c(((gba.cpu.gprs[rd] >> (rs - 1)) & 1) != 0);
            gba.cpu.gprs[rd] = ((gba.cpu.gprs[rd] as u32) >> rs) as i32;
        } else {
            if rs > 32 {
                gba.cpu.cpsr.set_c(false);
            } else {
                gba.cpu.cpsr.set_c(arm_sign(gba.cpu.gprs[rd]));
            }
            gba.cpu.gprs[rd] = 0;
        }
    }
    current_cycles += 1;
    thumb_neutral_s(gba, gba.cpu.gprs[rd]);
    gba.cpu.cycles += current_cycles;
}

fn asr2(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    let rs = gba.cpu.gprs[rn] & 0xFF;
    if rs != 0 {
        if rs < 32 {
            gba.cpu.cpsr.set_c(((gba.cpu.gprs[rd] >> (rs - 1)) & 1) != 0);
            gba.cpu.gprs[rd] >>= rs;
        } else {
            gba.cpu.cpsr.set_c(arm_sign(gba.cpu.gprs[rd]));
            if gba.cpu.cpsr.c() {
                gba.cpu.gprs[rd] = -1; // 0xFFFFFFFF
            } else {
                gba.cpu.gprs[rd] = 0;
            }
        }
    }
    current_cycles += 1;
    thumb_neutral_s(gba, gba.cpu.gprs[rd]);
    gba.cpu.cycles += current_cycles;
}

fn adc(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    let n = gba.cpu.gprs[rn];
    let d = gba.cpu.gprs[rd];
    let c = gba.cpu.cpsr.c() as i32;
    gba.cpu.gprs[rd] = d.wrapping_add(n).wrapping_add(c);
    thumb_addition_s(gba, d, n, gba.cpu.gprs[rd]);
    gba.cpu.cycles += current_cycles;
}

fn sbc(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    let n = gba.cpu.gprs[rn];
    let d = gba.cpu.gprs[rd];
    let nc = !gba.cpu.cpsr.c();
    gba.cpu.gprs[rd] = d.wrapping_sub(n).wrapping_sub(nc as i32);
    thumb_subtraction_carry_s(gba, d, n, gba.cpu.gprs[rd], nc);
    gba.cpu.cycles += current_cycles;
}

fn ror(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    let rs = gba.cpu.gprs[rn] & 0xFF;
    if rs != 0 {
        let r4 = rs & 0x1F;
        if r4 > 0 {
            gba.cpu.cpsr.set_c(((gba.cpu.gprs[rd] >> (r4 - 1)) & 1) != 0);
            // ROR(I, ROTATE) from mgba/include/mgba-util/common.h
            gba.cpu.gprs[rd] = (gba.cpu.gprs[rd] as u32).rotate_right(r4 as u32) as i32;
        } else {
            gba.cpu.cpsr.set_c(arm_sign(gba.cpu.gprs[rd]));
        }
    }
    current_cycles += 1;
    thumb_neutral_s(gba, gba.cpu.gprs[rd]);
    gba.cpu.cycles += current_cycles;
}

fn tst(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    let alu_out = gba.cpu.gprs[rd] & gba.cpu.gprs[rn];
    thumb_neutral_s(gba, alu_out);
    gba.cpu.cycles += current_cycles;
}

fn neg(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    // THUMB_SUBTRACTION(gprs[rd], 0, gprs[rn])
    let n = gba.cpu.gprs[rn];
    let m: i32 = 0;
    let d = m.wrapping_sub(n);
    gba.cpu.gprs[rd] = d;
    thumb_subtraction_s(gba, m, n, d);
    gba.cpu.cycles += current_cycles;
}

fn cmp2(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    let m = gba.cpu.gprs[rd];
    let n = gba.cpu.gprs[rn];
    let alu_out = m.wrapping_sub(n);
    thumb_subtraction_s(gba, m, n, alu_out);
    gba.cpu.cycles += current_cycles;
}

fn cmn(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    let m = gba.cpu.gprs[rd];
    let n = gba.cpu.gprs[rn];
    let alu_out = m.wrapping_add(n);
    thumb_addition_s(gba, m, n, alu_out);
    gba.cpu.cycles += current_cycles;
}

fn orr(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    gba.cpu.gprs[rd] = gba.cpu.gprs[rd] | gba.cpu.gprs[rn];
    thumb_neutral_s(gba, gba.cpu.gprs[rd]);
    gba.cpu.cycles += current_cycles;
}

fn mul(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    // ARM_WAIT_SMUL(gprs[rd], 0)
    let r = gba.cpu.gprs[rd] as u32;
    let mut wait = 0;
    if (r & 0xFFFFFF00) == 0xFFFFFF00 || (r & 0xFFFFFF00) == 0 {
        wait += 1;
    } else if (r & 0xFFFF0000) == 0xFFFF0000 || (r & 0xFFFF0000) == 0 {
        wait += 2;
    } else if (r & 0xFF000000) == 0xFF000000 || (r & 0xFF000000) == 0 {
        wait += 3;
    } else {
        wait += 4;
    }
    current_cycles += gba.cpu_stall(wait);
    gba.cpu.gprs[rd] = gba.cpu.gprs[rd].wrapping_mul(gba.cpu.gprs[rn]);
    thumb_neutral_s(gba, gba.cpu.gprs[rd]);
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.cycles += current_cycles;
}

fn bic(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    gba.cpu.gprs[rd] = gba.cpu.gprs[rd] & !gba.cpu.gprs[rn];
    thumb_neutral_s(gba, gba.cpu.gprs[rd]);
    gba.cpu.cycles += current_cycles;
}

fn mvn(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    gba.cpu.gprs[rd] = !gba.cpu.gprs[rn];
    thumb_neutral_s(gba, gba.cpu.gprs[rd]);
    gba.cpu.cycles += current_cycles;
}

// ----- HI-register ops (DEFINE_INSTRUCTION_WITH_HIGH_EX_THUMB) -----

macro_rules! define_high_ex {
    ($name:ident, $h1:expr, $h2:expr, $body:expr) => {
        fn $name(gba: &mut Gba, opcode: u16) {
            let mut current_cycles = 1 + gba.cpu.active_seq_cycles16; // THUMB_PREFETCH_CYCLES
            let rd = ((opcode & 0x0007) as usize) | $h1;
            let rm = (((opcode >> 3) & 0x0007) as usize) | $h2;
            let body: fn(&mut Gba, usize, usize, &mut i32) = $body;
            body(gba, rd, rm, &mut current_cycles);
            gba.cpu.cycles += current_cycles;
        }
    };
}

fn add4_body(gba: &mut Gba, rd: usize, rm: usize, current_cycles: &mut i32) {
    gba.cpu.gprs[rd] = gba.cpu.gprs[rd].wrapping_add(gba.cpu.gprs[rm]);
    if rd == ARM_PC {
        *current_cycles += gba.thumb_write_pc();
    }
}

define_high_ex!(add400, 0, 0, add4_body);
define_high_ex!(add401, 0, 8, add4_body);
define_high_ex!(add410, 8, 0, add4_body);
define_high_ex!(add411, 8, 8, add4_body);

fn cmp3_body(gba: &mut Gba, rd: usize, rm: usize, _current_cycles: &mut i32) {
    let m = gba.cpu.gprs[rd];
    let n = gba.cpu.gprs[rm];
    let alu_out = m.wrapping_sub(n);
    thumb_subtraction_s(gba, m, n, alu_out);
}

define_high_ex!(cmp300, 0, 0, cmp3_body);
define_high_ex!(cmp301, 0, 8, cmp3_body);
define_high_ex!(cmp310, 8, 0, cmp3_body);
define_high_ex!(cmp311, 8, 8, cmp3_body);

fn mov3_body(gba: &mut Gba, rd: usize, rm: usize, current_cycles: &mut i32) {
    gba.cpu.gprs[rd] = gba.cpu.gprs[rm];
    if rd == ARM_PC {
        *current_cycles += gba.thumb_write_pc();
    }
}

define_high_ex!(mov300, 0, 0, mov3_body);
define_high_ex!(mov301, 0, 8, mov3_body);
define_high_ex!(mov310, 8, 0, mov3_body);
define_high_ex!(mov311, 8, 8, mov3_body);

// ----- PC/SP-relative loads and address math (DEFINE_IMMEDIATE_WITH_REGISTER_THUMB) -----

fn ldr3(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = ((opcode >> 8) & 0x0007) as usize;
    let immediate = ((opcode & 0x00FF) << 2) as i32;
    let address = (gba.cpu.gprs[ARM_PC] & (0xFFFFFFFC as u32 as i32)).wrapping_add(immediate) as u32;
    gba.cpu.gprs[rd] = gba.load32(address, &mut current_cycles) as i32;
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.cycles += current_cycles;
}

fn ldr4(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = ((opcode >> 8) & 0x0007) as usize;
    let immediate = ((opcode & 0x00FF) << 2) as i32;
    let address = gba.cpu.gprs[ARM_SP].wrapping_add(immediate) as u32;
    gba.cpu.gprs[rd] = gba.load32(address, &mut current_cycles) as i32;
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.cycles += current_cycles;
}

fn str3(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = ((opcode >> 8) & 0x0007) as usize;
    let immediate = ((opcode & 0x00FF) << 2) as i32;
    let address = gba.cpu.gprs[ARM_SP].wrapping_add(immediate) as u32;
    gba.store32(address, gba.cpu.gprs[rd], &mut current_cycles);
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.cycles += current_cycles;
}

fn add5(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = ((opcode >> 8) & 0x0007) as usize;
    let immediate = ((opcode & 0x00FF) << 2) as i32;
    gba.cpu.gprs[rd] = (gba.cpu.gprs[ARM_PC] & (0xFFFFFFFC as u32 as i32)).wrapping_add(immediate);
    gba.cpu.cycles += current_cycles;
}

fn add6(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rd = ((opcode >> 8) & 0x0007) as usize;
    let immediate = ((opcode & 0x00FF) << 2) as i32;
    gba.cpu.gprs[rd] = gba.cpu.gprs[ARM_SP].wrapping_add(immediate);
    gba.cpu.cycles += current_cycles;
}

// ----- Load/store with register offset (DEFINE_LOAD_STORE_WITH_REGISTER_THUMB) -----

fn ldr2(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rm = ((opcode >> 6) & 0x0007) as usize;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    let address = gba.cpu.gprs[rn].wrapping_add(gba.cpu.gprs[rm]) as u32;
    gba.cpu.gprs[rd] = gba.load32(address, &mut current_cycles) as i32;
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.cycles += current_cycles;
}

fn ldrb2(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rm = ((opcode >> 6) & 0x0007) as usize;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    let address = gba.cpu.gprs[rn].wrapping_add(gba.cpu.gprs[rm]) as u32;
    gba.cpu.gprs[rd] = gba.load8(address, &mut current_cycles) as i32;
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.cycles += current_cycles;
}

fn ldrh2(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rm = ((opcode >> 6) & 0x0007) as usize;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    let address = gba.cpu.gprs[rn].wrapping_add(gba.cpu.gprs[rm]) as u32;
    gba.cpu.gprs[rd] = gba.load16(address, &mut current_cycles) as i32;
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.cycles += current_cycles;
}

fn ldrsb(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rm = ((opcode >> 6) & 0x0007) as usize;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    let address = gba.cpu.gprs[rn].wrapping_add(gba.cpu.gprs[rm]) as u32;
    // ARM_SXT_8
    gba.cpu.gprs[rd] = gba.load8(address, &mut current_cycles) as i8 as i32;
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.cycles += current_cycles;
}

fn ldrsh(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rm = ((opcode >> 6) & 0x0007) as usize;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    // C reuses `rm` as the address here; keep that readable.
    let rm_addr = gba.cpu.gprs[rn].wrapping_add(gba.cpu.gprs[rm]);
    let loaded = gba.load16(rm_addr as u32, &mut current_cycles) as i32;
    gba.cpu.gprs[rd] = if rm_addr & 1 != 0 {
        loaded as i8 as i32 // ARM_SXT_8
    } else {
        loaded as i16 as i32 // ARM_SXT_16
    };
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.cycles += current_cycles;
}

fn str2(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rm = ((opcode >> 6) & 0x0007) as usize;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    let address = gba.cpu.gprs[rn].wrapping_add(gba.cpu.gprs[rm]) as u32;
    gba.store32(address, gba.cpu.gprs[rd], &mut current_cycles);
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.cycles += current_cycles;
}

fn strb2(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rm = ((opcode >> 6) & 0x0007) as usize;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    let address = gba.cpu.gprs[rn].wrapping_add(gba.cpu.gprs[rm]) as u32;
    gba.store8(address, gba.cpu.gprs[rd], &mut current_cycles);
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.cycles += current_cycles;
}

fn strh2(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rm = ((opcode >> 6) & 0x0007) as usize;
    let rd = (opcode & 0x0007) as usize;
    let rn = ((opcode >> 3) & 0x0007) as usize;
    let address = gba.cpu.gprs[rn].wrapping_add(gba.cpu.gprs[rm]) as u32;
    gba.store16(address, gba.cpu.gprs[rd], &mut current_cycles);
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.cycles += current_cycles;
}

// ----- Load/store multiple (DEFINE_LOAD_STORE_MULTIPLE_THUMB) -----

fn ldmia(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rn = ((opcode >> 8) & 0x0007) as usize;
    let rs = (opcode & 0xFF) as i32;
    let mut address = gba.cpu.gprs[rn] as u32;
    address = gba.load_multiple(address, rs, LSM_IA, &mut current_cycles);
    current_cycles += thumb_ls_post_body(gba);
    if rs == 0 {
        current_cycles += gba.thumb_write_pc();
    }
    if ((1 << rn) & rs) == 0 {
        gba.cpu.gprs[rn] = address as i32;
    }
    gba.cpu.cycles += current_cycles;
}

fn stmia(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rn = ((opcode >> 8) & 0x0007) as usize;
    let rs = (opcode & 0xFF) as i32;
    let mut address = gba.cpu.gprs[rn] as u32;
    address = gba.store_multiple(address, rs, LSM_IA, &mut current_cycles);
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.gprs[rn] = address as i32;
    gba.cpu.cycles += current_cycles;
}

// ----- Conditional branches (DEFINE_CONDITIONAL_BRANCH_THUMB) -----

macro_rules! define_cond_branch {
    ($name:ident, $cond:expr) => {
        fn $name(gba: &mut Gba, opcode: u16) {
            let mut current_cycles = 1 + gba.cpu.active_seq_cycles16; // THUMB_PREFETCH_CYCLES
            if $cond(gba) {
                // int8_t immediate = opcode;
                let immediate = opcode as i8;
                gba.cpu.gprs[ARM_PC] = gba.cpu.gprs[ARM_PC].wrapping_add((immediate as i32) << 1);
                current_cycles += gba.thumb_write_pc();
            }
            gba.cpu.cycles += current_cycles;
        }
    };
}

// ARM_COND_*
define_cond_branch!(beq, |gba: &mut Gba| gba.cpu.cpsr.z());
define_cond_branch!(bne, |gba: &mut Gba| !gba.cpu.cpsr.z());
define_cond_branch!(bcs, |gba: &mut Gba| gba.cpu.cpsr.c());
define_cond_branch!(bcc, |gba: &mut Gba| !gba.cpu.cpsr.c());
define_cond_branch!(bmi, |gba: &mut Gba| gba.cpu.cpsr.n());
define_cond_branch!(bpl, |gba: &mut Gba| !gba.cpu.cpsr.n());
define_cond_branch!(bvs, |gba: &mut Gba| gba.cpu.cpsr.v());
define_cond_branch!(bvc, |gba: &mut Gba| !gba.cpu.cpsr.v());
define_cond_branch!(bls, |gba: &mut Gba| !gba.cpu.cpsr.c() || gba.cpu.cpsr.z());
define_cond_branch!(bhi, |gba: &mut Gba| gba.cpu.cpsr.c() && !gba.cpu.cpsr.z());
define_cond_branch!(bge, |gba: &mut Gba| gba.cpu.cpsr.n() == gba.cpu.cpsr.v());
define_cond_branch!(blt, |gba: &mut Gba| gba.cpu.cpsr.n() != gba.cpu.cpsr.v());
define_cond_branch!(bgt, |gba: &mut Gba| !gba.cpu.cpsr.z() && (gba.cpu.cpsr.n() == gba.cpu.cpsr.v()));
define_cond_branch!(ble, |gba: &mut Gba| gba.cpu.cpsr.z() || (gba.cpu.cpsr.n() != gba.cpu.cpsr.v()));

// ----- SP adjust -----

fn add7(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    gba.cpu.gprs[ARM_SP] = gba.cpu.gprs[ARM_SP].wrapping_add(((opcode & 0x7F) << 2) as i32);
    gba.cpu.cycles += current_cycles;
}

fn sub4(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    gba.cpu.gprs[ARM_SP] = gba.cpu.gprs[ARM_SP].wrapping_sub(((opcode & 0x7F) << 2) as i32);
    gba.cpu.cycles += current_cycles;
}

// ----- PUSH/POP (DEFINE_LOAD_STORE_MULTIPLE_THUMB with rn = ARM_SP) -----

fn pop(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rs = (opcode & 0xFF) as i32;
    let mut address = gba.cpu.gprs[ARM_SP] as u32;
    address = gba.load_multiple(address, rs, LSM_IA, &mut current_cycles);
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.gprs[ARM_SP] = address as i32;
    gba.cpu.cycles += current_cycles;
}

fn popr(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let mut rs = (opcode & 0xFF) as i32;
    let mut address = gba.cpu.gprs[ARM_SP] as u32;
    rs |= 1 << ARM_PC;
    address = gba.load_multiple(address, rs, LSM_IA, &mut current_cycles);
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.gprs[ARM_SP] = address as i32;
    current_cycles += gba.thumb_write_pc();
    gba.cpu.cycles += current_cycles;
}

fn push(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rs = (opcode & 0xFF) as i32;
    let mut address = gba.cpu.gprs[ARM_SP] as u32;
    address = gba.store_multiple(address, rs, LSM_DB, &mut current_cycles);
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.gprs[ARM_SP] = address as i32;
    gba.cpu.cycles += current_cycles;
}

fn pushr(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let mut rs = (opcode & 0xFF) as i32;
    let mut address = gba.cpu.gprs[ARM_SP] as u32;
    rs |= 1 << ARM_LR;
    address = gba.store_multiple(address, rs, LSM_DB, &mut current_cycles);
    current_cycles += thumb_ls_post_body(gba);
    gba.cpu.gprs[ARM_SP] = address as i32;
    gba.cpu.cycles += current_cycles;
}

// ----- Branches, SWI, illegal -----

fn ill(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    // ARM_ILL
    gba.hit_illegal(opcode as u32);
    gba.cpu.cycles += current_cycles;
}

fn bkpt(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    gba.bkpt16((opcode & 0xFF) as i32);
    current_cycles = 0; // Not strictly in ARMv4T, but here for convenience
    gba.cpu.cycles += current_cycles;
}

fn b(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    // int16_t immediate = (opcode & 0x07FF) << 5;
    let immediate = (((opcode & 0x07FF) << 5) as i16) as i32;
    gba.cpu.gprs[ARM_PC] = gba.cpu.gprs[ARM_PC].wrapping_add(immediate >> 4);
    current_cycles += gba.thumb_write_pc();
    gba.cpu.cycles += current_cycles;
}

fn bl1(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    // int16_t immediate = (opcode & 0x07FF) << 5;
    let immediate = (((opcode & 0x07FF) << 5) as i16) as i32;
    gba.cpu.gprs[ARM_LR] = gba.cpu.gprs[ARM_PC].wrapping_add(immediate << 7);
    gba.cpu.cycles += current_cycles;
}

fn bl2(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    // uint16_t immediate = (opcode & 0x07FF) << 1;
    let immediate = ((opcode & 0x07FF) << 1) as u32;
    let pc = gba.cpu.gprs[ARM_PC];
    gba.cpu.gprs[ARM_PC] = gba.cpu.gprs[ARM_LR].wrapping_add(immediate as i32);
    gba.cpu.gprs[ARM_LR] = pc.wrapping_sub(1);
    current_cycles += gba.thumb_write_pc();
    gba.cpu.cycles += current_cycles;
}

fn bx(gba: &mut Gba, opcode: u16) {
    let mut current_cycles = 1 + gba.cpu.active_seq_cycles16;
    let rm = ((opcode >> 3) & 0xF) as usize;
    // _ARMSetMode(cpu, cpu->gprs[rm] & 0x00000001)
    let mode = if gba.cpu.gprs[rm] & 0x00000001 != 0 {
        ExecutionMode::Thumb
    } else {
        ExecutionMode::Arm
    };
    gba.arm_set_mode(mode);
    gba.cpu.gprs[ARM_PC] = gba.cpu.gprs[rm] & (0xFFFFFFFE as u32 as i32);
    if gba.cpu.execution_mode == ExecutionMode::Thumb {
        current_cycles += gba.thumb_write_pc();
    } else {
        current_cycles += gba.arm_write_pc();
    }
    gba.cpu.cycles += current_cycles;
}

fn swi(gba: &mut Gba, opcode: u16) {
    let current_cycles = 1 + gba.cpu.active_seq_cycles16;
    gba.swi16((opcode & 0xFF) as i32);
    gba.cpu.cycles += current_cycles;
}

// ----- Decode table -----
// Order mirrors DECLARE_THUMB_EMITTER_BLOCK(EMITTER) with EMITTER = _ThumbInstruction.

pub static THUMB_INSTRUCTION_TABLE: [fn(&mut Gba, u16); 0x400] = [
    // lsl1 x32 (DO_8(DO_4(LSL1)))
    lsl1, lsl1, lsl1, lsl1, lsl1, lsl1, lsl1, lsl1, lsl1, lsl1, lsl1, lsl1, lsl1, lsl1, lsl1,
    lsl1, lsl1, lsl1, lsl1, lsl1, lsl1, lsl1, lsl1, lsl1, lsl1, lsl1, lsl1, lsl1, lsl1, lsl1,
    lsl1, lsl1,
    // lsr1 x32
    lsr1, lsr1, lsr1, lsr1, lsr1, lsr1, lsr1, lsr1, lsr1, lsr1, lsr1, lsr1, lsr1, lsr1, lsr1,
    lsr1, lsr1, lsr1, lsr1, lsr1, lsr1, lsr1, lsr1, lsr1, lsr1, lsr1, lsr1, lsr1, lsr1, lsr1,
    lsr1, lsr1,
    // asr1 x32
    asr1, asr1, asr1, asr1, asr1, asr1, asr1, asr1, asr1, asr1, asr1, asr1, asr1, asr1, asr1,
    asr1, asr1, asr1, asr1, asr1, asr1, asr1, asr1, asr1, asr1, asr1, asr1, asr1, asr1, asr1,
    asr1, asr1,
    // add3 x8
    add3, add3, add3, add3, add3, add3, add3, add3,
    // sub3 x8
    sub3, sub3, sub3, sub3, sub3, sub3, sub3, sub3,
    // add1 x8
    add1, add1, add1, add1, add1, add1, add1, add1,
    // sub1 x8
    sub1, sub1, sub1, sub1, sub1, sub1, sub1, sub1,
    // mov1 x32
    mov1, mov1, mov1, mov1, mov1, mov1, mov1, mov1, mov1, mov1, mov1, mov1, mov1, mov1, mov1,
    mov1, mov1, mov1, mov1, mov1, mov1, mov1, mov1, mov1, mov1, mov1, mov1, mov1, mov1, mov1,
    mov1, mov1,
    // cmp1 x32
    cmp1, cmp1, cmp1, cmp1, cmp1, cmp1, cmp1, cmp1, cmp1, cmp1, cmp1, cmp1, cmp1, cmp1, cmp1,
    cmp1, cmp1, cmp1, cmp1, cmp1, cmp1, cmp1, cmp1, cmp1, cmp1, cmp1, cmp1, cmp1, cmp1, cmp1,
    cmp1, cmp1,
    // add2 x32
    add2, add2, add2, add2, add2, add2, add2, add2, add2, add2, add2, add2, add2, add2, add2,
    add2, add2, add2, add2, add2, add2, add2, add2, add2, add2, add2, add2, add2, add2, add2,
    add2, add2,
    // sub2 x32
    sub2, sub2, sub2, sub2, sub2, sub2, sub2, sub2, sub2, sub2, sub2, sub2, sub2, sub2, sub2,
    sub2, sub2, sub2, sub2, sub2, sub2, sub2, sub2, sub2, sub2, sub2, sub2, sub2, sub2, sub2,
    sub2, sub2,
    // ALU ops, one each
    and, eor, lsl2, lsr2, asr2, adc, sbc, ror, tst, neg, cmp2, cmn, orr, mul, bic, mvn,
    // DECLARE_INSTRUCTION_WITH_HIGH_THUMB(ADD4/CMP3/MOV3)
    add400, add401, add410, add411,
    cmp300, cmp301, cmp310, cmp311,
    mov300, mov301, mov310, mov311,
    // bx, bx, ill, ill
    bx, bx, ill, ill,
    // ldr3 x32
    ldr3, ldr3, ldr3, ldr3, ldr3, ldr3, ldr3, ldr3, ldr3, ldr3, ldr3, ldr3, ldr3, ldr3, ldr3,
    ldr3, ldr3, ldr3, ldr3, ldr3, ldr3, ldr3, ldr3, ldr3, ldr3, ldr3, ldr3, ldr3, ldr3, ldr3,
    ldr3, ldr3,
    // str2/strh2/strb2/ldrsb/ldr2/ldrh2/ldrb2/ldrsh x8
    str2, str2, str2, str2, str2, str2, str2, str2,
    strh2, strh2, strh2, strh2, strh2, strh2, strh2, strh2,
    strb2, strb2, strb2, strb2, strb2, strb2, strb2, strb2,
    ldrsb, ldrsb, ldrsb, ldrsb, ldrsb, ldrsb, ldrsb, ldrsb,
    ldr2, ldr2, ldr2, ldr2, ldr2, ldr2, ldr2, ldr2,
    ldrh2, ldrh2, ldrh2, ldrh2, ldrh2, ldrh2, ldrh2, ldrh2,
    ldrb2, ldrb2, ldrb2, ldrb2, ldrb2, ldrb2, ldrb2, ldrb2,
    ldrsh, ldrsh, ldrsh, ldrsh, ldrsh, ldrsh, ldrsh, ldrsh,
    // str1 x32
    str1, str1, str1, str1, str1, str1, str1, str1, str1, str1, str1, str1, str1, str1, str1,
    str1, str1, str1, str1, str1, str1, str1, str1, str1, str1, str1, str1, str1, str1, str1,
    str1, str1,
    // ldr1 x32
    ldr1, ldr1, ldr1, ldr1, ldr1, ldr1, ldr1, ldr1, ldr1, ldr1, ldr1, ldr1, ldr1, ldr1, ldr1,
    ldr1, ldr1, ldr1, ldr1, ldr1, ldr1, ldr1, ldr1, ldr1, ldr1, ldr1, ldr1, ldr1, ldr1, ldr1,
    ldr1, ldr1,
    // strb1 x32
    strb1, strb1, strb1, strb1, strb1, strb1, strb1, strb1, strb1, strb1, strb1, strb1, strb1,
    strb1, strb1, strb1, strb1, strb1, strb1, strb1, strb1, strb1, strb1, strb1, strb1, strb1,
    strb1, strb1, strb1, strb1, strb1, strb1,
    // ldrb1 x32
    ldrb1, ldrb1, ldrb1, ldrb1, ldrb1, ldrb1, ldrb1, ldrb1, ldrb1, ldrb1, ldrb1, ldrb1, ldrb1,
    ldrb1, ldrb1, ldrb1, ldrb1, ldrb1, ldrb1, ldrb1, ldrb1, ldrb1, ldrb1, ldrb1, ldrb1, ldrb1,
    ldrb1, ldrb1, ldrb1, ldrb1, ldrb1, ldrb1,
    // strh1 x32
    strh1, strh1, strh1, strh1, strh1, strh1, strh1, strh1, strh1, strh1, strh1, strh1, strh1,
    strh1, strh1, strh1, strh1, strh1, strh1, strh1, strh1, strh1, strh1, strh1, strh1, strh1,
    strh1, strh1, strh1, strh1, strh1, strh1,
    // ldrh1 x32
    ldrh1, ldrh1, ldrh1, ldrh1, ldrh1, ldrh1, ldrh1, ldrh1, ldrh1, ldrh1, ldrh1, ldrh1, ldrh1,
    ldrh1, ldrh1, ldrh1, ldrh1, ldrh1, ldrh1, ldrh1, ldrh1, ldrh1, ldrh1, ldrh1, ldrh1, ldrh1,
    ldrh1, ldrh1, ldrh1, ldrh1, ldrh1, ldrh1,
    // str3 x32
    str3, str3, str3, str3, str3, str3, str3, str3, str3, str3, str3, str3, str3, str3, str3,
    str3, str3, str3, str3, str3, str3, str3, str3, str3, str3, str3, str3, str3, str3, str3,
    str3, str3,
    // ldr4 x32
    ldr4, ldr4, ldr4, ldr4, ldr4, ldr4, ldr4, ldr4, ldr4, ldr4, ldr4, ldr4, ldr4, ldr4, ldr4,
    ldr4, ldr4, ldr4, ldr4, ldr4, ldr4, ldr4, ldr4, ldr4, ldr4, ldr4, ldr4, ldr4, ldr4, ldr4,
    ldr4, ldr4,
    // add5 x32
    add5, add5, add5, add5, add5, add5, add5, add5, add5, add5, add5, add5, add5, add5, add5,
    add5, add5, add5, add5, add5, add5, add5, add5, add5, add5, add5, add5, add5, add5, add5,
    add5, add5,
    // add6 x32
    add6, add6, add6, add6, add6, add6, add6, add6, add6, add6, add6, add6, add6, add6, add6,
    add6, add6, add6, add6, add6, add6, add6, add6, add6, add6, add6, add6, add6, add6, add6,
    add6, add6,
    // add7 x2, sub4 x2
    add7, add7,
    sub4, sub4,
    // ill x12 (DO_4 x3)
    ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill,
    // push x4, pushr x4
    push, push, push, push,
    pushr, pushr, pushr, pushr,
    // ill x24 (DO_8 x3)
    ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill,
    ill, ill, ill, ill, ill, ill,
    // pop x4, popr x4
    pop, pop, pop, pop,
    popr, popr, popr, popr,
    // bkpt x4, ill x4
    bkpt, bkpt, bkpt, bkpt,
    ill, ill, ill, ill,
    // stmia x32
    stmia, stmia, stmia, stmia, stmia, stmia, stmia, stmia, stmia, stmia, stmia, stmia, stmia,
    stmia, stmia, stmia, stmia, stmia, stmia, stmia, stmia, stmia, stmia, stmia, stmia, stmia,
    stmia, stmia, stmia, stmia, stmia, stmia,
    // ldmia x32
    ldmia, ldmia, ldmia, ldmia, ldmia, ldmia, ldmia, ldmia, ldmia, ldmia, ldmia, ldmia, ldmia,
    ldmia, ldmia, ldmia, ldmia, ldmia, ldmia, ldmia, ldmia, ldmia, ldmia, ldmia, ldmia, ldmia,
    ldmia, ldmia, ldmia, ldmia, ldmia, ldmia,
    // conditional branches x4 each, in DECLARE_THUMB_EMITTER_BLOCK order (BHI before BLS)
    beq, beq, beq, beq,
    bne, bne, bne, bne,
    bcs, bcs, bcs, bcs,
    bcc, bcc, bcc, bcc,
    bmi, bmi, bmi, bmi,
    bpl, bpl, bpl, bpl,
    bvs, bvs, bvs, bvs,
    bvc, bvc, bvc, bvc,
    bhi, bhi, bhi, bhi,
    bls, bls, bls, bls,
    bge, bge, bge, bge,
    blt, blt, blt, blt,
    bgt, bgt, bgt, bgt,
    ble, ble, ble, ble,
    // ill x4, swi x4
    ill, ill, ill, ill,
    swi, swi, swi, swi,
    // b x32
    b, b, b, b, b, b, b, b, b, b, b, b, b, b, b, b, b, b, b, b, b, b, b, b, b, b, b, b, b,
    b, b, b,
    // ill x32
    ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill,
    ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill, ill,
    // bl1 x32
    bl1, bl1, bl1, bl1, bl1, bl1, bl1, bl1, bl1, bl1, bl1, bl1, bl1, bl1, bl1, bl1, bl1, bl1,
    bl1, bl1, bl1, bl1, bl1, bl1, bl1, bl1, bl1, bl1, bl1, bl1, bl1, bl1,
    // bl2 x32
    bl2, bl2, bl2, bl2, bl2, bl2, bl2, bl2, bl2, bl2, bl2, bl2, bl2, bl2, bl2, bl2, bl2, bl2,
    bl2, bl2, bl2, bl2, bl2, bl2, bl2, bl2, bl2, bl2, bl2, bl2, bl2, bl2,
];
