// Copyright (c) 2013-2014 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/arm/isa-arm.c and the dispatch table in
// mgba/include/mgba/internal/arm/emitter-arm.h (DECLARE_ARM_EMITTER_BLOCK).
//
// Function names are the C handler names lowercased (`_ARMInstructionADDS_LSL`
// -> `adds_lsl`). The macro families mirror the C preprocessor families:
// `define_alu_family!` is DEFINE_ALU_INSTRUCTION_ARM, `define_ls_family!` is
// DEFINE_LOAD_STORE_INSTRUCTION_ARM (addressing mode 2), `define_ls_mode3_family!`
// is DEFINE_LOAD_STORE_MODE_3_INSTRUCTION_ARM, `define_ls_t_family!` is
// DEFINE_LOAD_STORE_T_INSTRUCTION_ARM, and `define_lsm!` is
// DEFINE_LOAD_STORE_MULTIPLE_INSTRUCTION_ARM. C's `DEFINE_INSTRUCTION_ARM` frame
// (`currentCycles = ARM_PREFETCH_CYCLES` where ARM_PREFETCH_CYCLES is
// `1 + cpu->memory.activeSeqCycles32`) is spelled out in every handler.
//
// NOTE (coprocessors): in mGBA the MCR/MRC/CDP handlers consult a per-slot
// vtable and execute ARM_ILL (irqh.hitIllegal) when the coprocessor is missing.
// The GBA exposes `Gba::cp_mrc`/`cp_mcr`/`cp_cdp` stubs which reproduce the
// GBA behavior; the handlers here call them unconditionally.

use super::{
    ExecutionMode, PrivilegeMode, Psr, ARM_LR, ARM_PC, LSM_DA, LSM_DB, LSM_IA, LSM_IB,
    PSR_PRIV_MASK, PSR_STATE_MASK, PSR_USER_MASK, WORD_SIZE_ARM, WORD_SIZE_THUMB,
};
use crate::gba::Gba;

// ----- isa-inlines.h helpers -----

/// ARM_SIGN
#[inline]
fn arm_sign(i: i32) -> i32 {
    i >> 31
}

/// ARM_SXT_8
#[inline]
fn arm_sxt_8(i: i32) -> i32 {
    (i as i8) as i32
}

/// ARM_SXT_16
#[inline]
fn arm_sxt_16(i: i32) -> i32 {
    (i as i16) as i32
}

/// common.h's ROR: (((uint32_t) (I)) >> ROTATE) | ((uint32_t) (I) << ((-ROTATE) & 31))
#[inline]
fn ror32(i: i32, rotate: u32) -> i32 {
    (i as u32).rotate_right(rotate & 31) as i32
}

/// ARM_CARRY_FROM
#[inline]
fn arm_carry_from(m: i32, n: i32, d: i32) -> bool {
    ((m as u32) >> 31) + ((n as u32) >> 31) > ((d as u32) >> 31)
}

/// ARM_BORROW_FROM
#[inline]
fn arm_borrow_from(m: i32, n: i32) -> bool {
    (m as u32) >= (n as u32)
}

/// ARM_BORROW_FROM_CARRY (ARM_UXT_64(M) >= ARM_UXT_64(N) + C)
#[inline]
fn arm_borrow_from_carry(m: i32, n: i32, c: bool) -> bool {
    (m as u32 as u64) >= (n as u32 as u64) + (c as u64)
}

/// ARM_V_ADDITION
#[inline]
fn arm_v_addition(m: i32, n: i32, d: i32) -> bool {
    arm_sign(m ^ n) == 0 && arm_sign(m ^ d) != 0
}

/// ARM_V_SUBTRACTION
#[inline]
fn arm_v_subtraction(m: i32, n: i32, d: i32) -> bool {
    arm_sign(m ^ n) != 0 && arm_sign(m ^ d) != 0
}

/// (enum PrivilegeMode) ((operand & 0xF) | 0x10), as cast in the MSR handlers.
#[inline]
fn privilege_mode_from_operand(operand: i32) -> PrivilegeMode {
    Psr { packed: (operand & 0xF) | 0x10 }.priv_mode()
}

// ----- Addressing mode 1 shifters (_shiftLSL/_shiftLSR/_shiftASR/_shiftROR/_immediate) -----

#[inline]
fn shift_lsl(gba: &mut Gba, opcode: u32) {
    let rm = (opcode & 0x0000000F) as usize;
    if opcode & 0x00000010 != 0 {
        let rs = ((opcode >> 8) & 0x0000000F) as usize;
        gba.cpu.cycles += 1;
        let mut shift_val: i32 = gba.cpu.gprs[rm];
        if rm == ARM_PC {
            shift_val = shift_val.wrapping_add(4);
        }
        let shift = (gba.cpu.gprs[rs] & 0xFF) as u32;
        if shift == 0 {
            gba.cpu.shifter_operand = shift_val;
            gba.cpu.shifter_carry_out = gba.cpu.cpsr.c() as i32;
        } else if shift < 32 {
            gba.cpu.shifter_operand = shift_val.wrapping_shl(shift);
            gba.cpu.shifter_carry_out = (shift_val.wrapping_shr(32 - shift)) & 1;
        } else if shift == 32 {
            gba.cpu.shifter_operand = 0;
            gba.cpu.shifter_carry_out = shift_val & 1;
        } else {
            gba.cpu.shifter_operand = 0;
            gba.cpu.shifter_carry_out = 0;
        }
    } else {
        let immediate = ((opcode & 0x00000F80) >> 7) as u32;
        if immediate == 0 {
            gba.cpu.shifter_operand = gba.cpu.gprs[rm];
            gba.cpu.shifter_carry_out = gba.cpu.cpsr.c() as i32;
        } else {
            gba.cpu.shifter_operand = gba.cpu.gprs[rm].wrapping_shl(immediate);
            gba.cpu.shifter_carry_out = (gba.cpu.gprs[rm].wrapping_shr(32 - immediate)) & 1;
        }
    }
}

#[inline]
fn shift_lsr(gba: &mut Gba, opcode: u32) {
    let rm = (opcode & 0x0000000F) as usize;
    if opcode & 0x00000010 != 0 {
        let rs = ((opcode >> 8) & 0x0000000F) as usize;
        gba.cpu.cycles += 1;
        let mut shift_val: u32 = gba.cpu.gprs[rm] as u32;
        if rm == ARM_PC {
            shift_val = shift_val.wrapping_add(4);
        }
        let shift = (gba.cpu.gprs[rs] & 0xFF) as u32;
        if shift == 0 {
            gba.cpu.shifter_operand = shift_val as i32;
            gba.cpu.shifter_carry_out = gba.cpu.cpsr.c() as i32;
        } else if shift < 32 {
            gba.cpu.shifter_operand = (shift_val >> shift) as i32;
            gba.cpu.shifter_carry_out = ((shift_val >> (shift - 1)) & 1) as i32;
        } else if shift == 32 {
            gba.cpu.shifter_operand = 0;
            gba.cpu.shifter_carry_out = (shift_val >> 31) as i32;
        } else {
            gba.cpu.shifter_operand = 0;
            gba.cpu.shifter_carry_out = 0;
        }
    } else {
        let immediate = ((opcode & 0x00000F80) >> 7) as u32;
        if immediate != 0 {
            gba.cpu.shifter_operand = ((gba.cpu.gprs[rm] as u32) >> immediate) as i32;
            gba.cpu.shifter_carry_out = (gba.cpu.gprs[rm] >> (immediate - 1)) & 1;
        } else {
            gba.cpu.shifter_operand = 0;
            gba.cpu.shifter_carry_out = arm_sign(gba.cpu.gprs[rm]);
        }
    }
}

#[inline]
fn shift_asr(gba: &mut Gba, opcode: u32) {
    let rm = (opcode & 0x0000000F) as usize;
    if opcode & 0x00000010 != 0 {
        let rs = ((opcode >> 8) & 0x0000000F) as usize;
        gba.cpu.cycles += 1;
        let mut shift_val: i32 = gba.cpu.gprs[rm];
        if rm == ARM_PC {
            shift_val = shift_val.wrapping_add(4);
        }
        let shift = (gba.cpu.gprs[rs] & 0xFF) as u32;
        if shift == 0 {
            gba.cpu.shifter_operand = shift_val;
            gba.cpu.shifter_carry_out = gba.cpu.cpsr.c() as i32;
        } else if shift < 32 {
            gba.cpu.shifter_operand = shift_val >> shift;
            gba.cpu.shifter_carry_out = (shift_val >> (shift - 1)) & 1;
        } else if gba.cpu.gprs[rm] >> 31 != 0 {
            // NB: tests gprs[rm], not shiftVal (as in the C)
            gba.cpu.shifter_operand = 0xFFFFFFFFu32 as i32;
            gba.cpu.shifter_carry_out = 1;
        } else {
            gba.cpu.shifter_operand = 0;
            gba.cpu.shifter_carry_out = 0;
        }
    } else {
        let immediate = ((opcode & 0x00000F80) >> 7) as u32;
        if immediate != 0 {
            gba.cpu.shifter_operand = gba.cpu.gprs[rm] >> immediate;
            gba.cpu.shifter_carry_out = (gba.cpu.gprs[rm] >> (immediate - 1)) & 1;
        } else {
            gba.cpu.shifter_carry_out = arm_sign(gba.cpu.gprs[rm]);
            gba.cpu.shifter_operand = gba.cpu.shifter_carry_out;
        }
    }
}

#[inline]
fn shift_ror(gba: &mut Gba, opcode: u32) {
    let rm = (opcode & 0x0000000F) as usize;
    if opcode & 0x00000010 != 0 {
        let rs = ((opcode >> 8) & 0x0000000F) as usize;
        gba.cpu.cycles += 1;
        let mut shift_val: i32 = gba.cpu.gprs[rm];
        if rm == ARM_PC {
            shift_val = shift_val.wrapping_add(4);
        }
        let shift = (gba.cpu.gprs[rs] & 0xFF) as u32;
        let rotate = shift & 0x1F;
        if shift == 0 {
            gba.cpu.shifter_operand = shift_val;
            gba.cpu.shifter_carry_out = gba.cpu.cpsr.c() as i32;
        } else if rotate != 0 {
            gba.cpu.shifter_operand = ror32(shift_val, rotate);
            gba.cpu.shifter_carry_out = (shift_val >> (rotate - 1)) & 1;
        } else {
            gba.cpu.shifter_operand = shift_val;
            gba.cpu.shifter_carry_out = arm_sign(shift_val);
        }
    } else {
        let immediate = ((opcode & 0x00000F80) >> 7) as u32;
        if immediate != 0 {
            gba.cpu.shifter_operand = ror32(gba.cpu.gprs[rm], immediate);
            gba.cpu.shifter_carry_out = (gba.cpu.gprs[rm] >> (immediate - 1)) & 1;
        } else {
            // RRX
            gba.cpu.shifter_operand = ((gba.cpu.cpsr.c() as i32) << 31) | (((gba.cpu.gprs[rm] as u32) >> 1) as i32);
            gba.cpu.shifter_carry_out = gba.cpu.gprs[rm] & 0x00000001;
        }
    }
}

#[inline]
fn immediate(gba: &mut Gba, opcode: u32) {
    let rotate = ((opcode & 0x00000F00) >> 7) as u32;
    let immediate = (opcode & 0x000000FF) as i32;
    if rotate == 0 {
        gba.cpu.shifter_operand = immediate;
        gba.cpu.shifter_carry_out = gba.cpu.cpsr.c() as i32;
    } else {
        gba.cpu.shifter_operand = ror32(immediate, rotate);
        gba.cpu.shifter_carry_out = arm_sign(gba.cpu.shifter_operand);
    }
}

// ----- Flag-setting helpers (_additionS/_subtractionS/_neutralS + ARM_*_S macros) -----

/// _additionS: clears the whole flags byte first (cpu->cpsr.flags = 0).
#[inline]
fn apply_addition_s(gba: &mut Gba, m: i32, n: i32, d: i32) {
    gba.cpu.cpsr.packed &= 0x00FFFFFF;
    gba.cpu.cpsr.set_n(d < 0);
    gba.cpu.cpsr.set_z(d == 0);
    gba.cpu.cpsr.set_c(arm_carry_from(m, n, d));
    gba.cpu.cpsr.set_v(arm_v_addition(m, n, d));
}

/// _subtractionS: clears the whole flags byte first (cpu->cpsr.flags = 0).
#[inline]
fn apply_subtraction_s(gba: &mut Gba, m: i32, n: i32, d: i32) {
    gba.cpu.cpsr.packed &= 0x00FFFFFF;
    gba.cpu.cpsr.set_n(d < 0);
    gba.cpu.cpsr.set_z(d == 0);
    gba.cpu.cpsr.set_c(arm_borrow_from(m, n));
    gba.cpu.cpsr.set_v(arm_v_subtraction(m, n, d));
}

/// ARM_SUBTRACTION_CARRY_S flags-only path (does NOT clear the flags byte, as in the C).
#[inline]
fn apply_subtraction_carry_s(gba: &mut Gba, m: i32, n: i32, d: i32) {
    let c = gba.cpu.cpsr.c();
    gba.cpu.cpsr.set_n(d < 0);
    gba.cpu.cpsr.set_z(d == 0);
    gba.cpu.cpsr.set_c(arm_borrow_from_carry(m, n, !c));
    gba.cpu.cpsr.set_v(arm_v_subtraction(m, n, d));
}

/// _neutralS: n/z from d, c from the (possibly stale) shifter carry out.
#[inline]
fn apply_neutral_s(gba: &mut Gba, d: i32) {
    gba.cpu.cpsr.set_n(d < 0);
    gba.cpu.cpsr.set_z(d == 0);
    gba.cpu.cpsr.set_c(gba.cpu.shifter_carry_out != 0);
}

/// ARM_NEUTRAL_HI_S
#[inline]
fn apply_neutral_hi_s(gba: &mut Gba, dlo: i32, dhi: i32) {
    gba.cpu.cpsr.set_n(dhi < 0);
    gba.cpu.cpsr.set_z((dhi | dlo) == 0);
}

/// cpsr = spsr; _ARMReadCPSR (the rd == ARM_PC && has_spsr path).
#[inline]
fn restore_spsr(gba: &mut Gba) {
    gba.cpu.cpsr = gba.cpu.spsr;
    gba.arm_read_cpsr();
}

// ALU S-bodies: fn(gba, rd, n, d). The shifter operand is read from the cpu,
// exactly like the C S_BODY macros refer to cpu->shifterOperand.

#[allow(unused_variables)]
fn s_none(gba: &mut Gba, rd: usize, n: i32, d: i32) {}

/// ARM_ADDITION_S(n, shifterOperand, d) (ADD/ADC/CMN)
fn s_addition(gba: &mut Gba, rd: usize, n: i32, d: i32) {
    if rd == ARM_PC && gba.cpu.cpsr.priv_mode().has_spsr() {
        restore_spsr(gba);
    } else {
        let op = gba.cpu.shifter_operand;
        apply_addition_s(gba, n, op, d);
    }
}

/// ARM_SUBTRACTION_S(n, shifterOperand, d) (SUB/CMP)
fn s_subtraction(gba: &mut Gba, rd: usize, n: i32, d: i32) {
    if rd == ARM_PC && gba.cpu.cpsr.priv_mode().has_spsr() {
        restore_spsr(gba);
    } else {
        let op = gba.cpu.shifter_operand;
        apply_subtraction_s(gba, n, op, d);
    }
}

/// ARM_SUBTRACTION_S(shifterOperand, n, d) (RSB)
fn s_subtraction_swap(gba: &mut Gba, rd: usize, n: i32, d: i32) {
    if rd == ARM_PC && gba.cpu.cpsr.priv_mode().has_spsr() {
        restore_spsr(gba);
    } else {
        let op = gba.cpu.shifter_operand;
        apply_subtraction_s(gba, op, n, d);
    }
}

/// ARM_SUBTRACTION_CARRY_S(n, shifterOperand, d, !cpsr.c) (SBC)
fn s_subtraction_carry(gba: &mut Gba, rd: usize, n: i32, d: i32) {
    if rd == ARM_PC && gba.cpu.cpsr.priv_mode().has_spsr() {
        restore_spsr(gba);
    } else {
        let op = gba.cpu.shifter_operand;
        apply_subtraction_carry_s(gba, n, op, d);
    }
}

/// ARM_SUBTRACTION_CARRY_S(shifterOperand, n, d, !cpsr.c) (RSC)
fn s_subtraction_carry_swap(gba: &mut Gba, rd: usize, n: i32, d: i32) {
    if rd == ARM_PC && gba.cpu.cpsr.priv_mode().has_spsr() {
        restore_spsr(gba);
    } else {
        let op = gba.cpu.shifter_operand;
        apply_subtraction_carry_s(gba, op, n, d);
    }
}

/// ARM_NEUTRAL_S(n, shifterOperand, d) (logical ops, TST/TEQ)
fn s_neutral(gba: &mut Gba, rd: usize, _n: i32, d: i32) {
    if rd == ARM_PC && gba.cpu.cpsr.priv_mode().has_spsr() {
        restore_spsr(gba);
    } else {
        apply_neutral_s(gba, d);
    }
}

// ----- ALU bodies: fn(gba, n) -> result -----

fn and_body(gba: &mut Gba, n: i32) -> i32 {
    n & gba.cpu.shifter_operand
}
fn eor_body(gba: &mut Gba, n: i32) -> i32 {
    n ^ gba.cpu.shifter_operand
}
fn sub_body(gba: &mut Gba, n: i32) -> i32 {
    n.wrapping_sub(gba.cpu.shifter_operand)
}
fn rsb_body(gba: &mut Gba, n: i32) -> i32 {
    gba.cpu.shifter_operand.wrapping_sub(n)
}
fn add_body(gba: &mut Gba, n: i32) -> i32 {
    n.wrapping_add(gba.cpu.shifter_operand)
}
fn adc_body(gba: &mut Gba, n: i32) -> i32 {
    n.wrapping_add(gba.cpu.shifter_operand).wrapping_add(gba.cpu.cpsr.c() as i32)
}
fn sbc_body(gba: &mut Gba, n: i32) -> i32 {
    n.wrapping_sub(gba.cpu.shifter_operand).wrapping_sub(!gba.cpu.cpsr.c() as i32)
}
fn rsc_body(gba: &mut Gba, n: i32) -> i32 {
    gba.cpu.shifter_operand.wrapping_sub(n).wrapping_sub(!gba.cpu.cpsr.c() as i32)
}
fn tst_body(gba: &mut Gba, n: i32) -> i32 {
    n & gba.cpu.shifter_operand
}
fn teq_body(gba: &mut Gba, n: i32) -> i32 {
    n ^ gba.cpu.shifter_operand
}
fn cmp_body(gba: &mut Gba, n: i32) -> i32 {
    n.wrapping_sub(gba.cpu.shifter_operand)
}
fn cmn_body(gba: &mut Gba, n: i32) -> i32 {
    n.wrapping_add(gba.cpu.shifter_operand)
}
fn orr_body(gba: &mut Gba, n: i32) -> i32 {
    n | gba.cpu.shifter_operand
}
fn mov_body(gba: &mut Gba, _n: i32) -> i32 {
    gba.cpu.shifter_operand
}
fn bic_body(gba: &mut Gba, n: i32) -> i32 {
    n & !gba.cpu.shifter_operand
}
fn mvn_body(gba: &mut Gba, _n: i32) -> i32 {
    !gba.cpu.shifter_operand
}

// DEFINE_ALU_INSTRUCTION_EX_ARM
macro_rules! define_alu_ex {
    ($name:ident, $shifter:ident, $s:expr, $body:expr, $do_write:expr) => {
        fn $name(gba: &mut Gba, opcode: u32) {
            let mut current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
            $shifter(gba, opcode);
            let rd = ((opcode >> 12) & 0xF) as usize;
            let rn = ((opcode >> 16) & 0xF) as usize;
            #[allow(unused_mut)]
            let mut n: i32 = gba.cpu.gprs[rn];
            if rn == ARM_PC && (opcode & 0x02000010) == 0x00000010 {
                n = n.wrapping_add(WORD_SIZE_ARM);
            }
            let body: fn(&mut Gba, i32) -> i32 = $body;
            let alu_out: i32 = body(gba, n);
            if $do_write {
                gba.cpu.gprs[rd] = alu_out;
            }
            let s_body: fn(&mut Gba, usize, i32, i32) = $s;
            s_body(gba, rd, n, alu_out);
            if $do_write && rd == ARM_PC {
                if gba.cpu.execution_mode == ExecutionMode::Arm {
                    current_cycles += gba.arm_write_pc();
                } else {
                    current_cycles += gba.thumb_write_pc();
                }
            }
            gba.cpu.cycles += current_cycles;
        }
    };
}

// DEFINE_ALU_INSTRUCTION_ARM
macro_rules! define_alu_family {
    ($lsl:ident, $lsl_s:ident, $lsr:ident, $lsr_s:ident, $asr:ident, $asr_s:ident,
     $ror:ident, $ror_s:ident, $i:ident, $si:ident; $s:expr; $body:expr) => {
        define_alu_ex!($lsl, shift_lsl, s_none, $body, true);
        define_alu_ex!($lsl_s, shift_lsl, $s, $body, true);
        define_alu_ex!($lsr, shift_lsr, s_none, $body, true);
        define_alu_ex!($lsr_s, shift_lsr, $s, $body, true);
        define_alu_ex!($asr, shift_asr, s_none, $body, true);
        define_alu_ex!($asr_s, shift_asr, $s, $body, true);
        define_alu_ex!($ror, shift_ror, s_none, $body, true);
        define_alu_ex!($ror_s, shift_ror, $s, $body, true);
        define_alu_ex!($i, immediate, s_none, $body, true);
        define_alu_ex!($si, immediate, $s, $body, true);
    };
}

// DEFINE_ALU_INSTRUCTION_S_ONLY_ARM
macro_rules! define_alu_family_s_only {
    ($lsl:ident, $lsr:ident, $asr:ident, $ror:ident, $i:ident; $s:expr; $body:expr) => {
        define_alu_ex!($lsl, shift_lsl, $s, $body, false);
        define_alu_ex!($lsr, shift_lsr, $s, $body, false);
        define_alu_ex!($asr, shift_asr, $s, $body, false);
        define_alu_ex!($ror, shift_ror, $s, $body, false);
        define_alu_ex!($i, immediate, $s, $body, false);
    };
}

// Begin ALU definitions

define_alu_family!(add_lsl, adds_lsl, add_lsr, adds_lsr, add_asr, adds_asr,
    add_ror, adds_ror, addi, addsi; s_addition; add_body);
define_alu_family!(adc_lsl, adcs_lsl, adc_lsr, adcs_lsr, adc_asr, adcs_asr,
    adc_ror, adcs_ror, adci, adcsi; s_addition; adc_body);
define_alu_family!(and_lsl, ands_lsl, and_lsr, ands_lsr, and_asr, ands_asr,
    and_ror, ands_ror, andi, andsi; s_neutral; and_body);
define_alu_family!(bic_lsl, bics_lsl, bic_lsr, bics_lsr, bic_asr, bics_asr,
    bic_ror, bics_ror, bici, bicsi; s_neutral; bic_body);
define_alu_family_s_only!(cmn_lsl, cmn_lsr, cmn_asr, cmn_ror, cmni; s_addition; cmn_body);
define_alu_family_s_only!(cmp_lsl, cmp_lsr, cmp_asr, cmp_ror, cmpi; s_subtraction; cmp_body);
define_alu_family!(eor_lsl, eors_lsl, eor_lsr, eors_lsr, eor_asr, eors_asr,
    eor_ror, eors_ror, eori, eorsi; s_neutral; eor_body);
define_alu_family!(mov_lsl, movs_lsl, mov_lsr, movs_lsr, mov_asr, movs_asr,
    mov_ror, movs_ror, movi, movsi; s_neutral; mov_body);
define_alu_family!(mvn_lsl, mvns_lsl, mvn_lsr, mvns_lsr, mvn_asr, mvns_asr,
    mvn_ror, mvns_ror, mvni, mvnsi; s_neutral; mvn_body);
define_alu_family!(orr_lsl, orrs_lsl, orr_lsr, orrs_lsr, orr_asr, orrs_asr,
    orr_ror, orrs_ror, orri, orrsi; s_neutral; orr_body);
define_alu_family!(rsb_lsl, rsbs_lsl, rsb_lsr, rsbs_lsr, rsb_asr, rsbs_asr,
    rsb_ror, rsbs_ror, rsbi, rsbsi; s_subtraction_swap; rsb_body);
define_alu_family!(rsc_lsl, rscs_lsl, rsc_lsr, rscs_lsr, rsc_asr, rscs_asr,
    rsc_ror, rscs_ror, rsci, rscsi; s_subtraction_carry_swap; rsc_body);
define_alu_family!(sbc_lsl, sbcs_lsl, sbc_lsr, sbcs_lsr, sbc_asr, sbcs_asr,
    sbc_ror, sbcs_ror, sbci, sbcsi; s_subtraction_carry; sbc_body);
define_alu_family!(sub_lsl, subs_lsl, sub_lsr, subs_lsr, sub_asr, subs_asr,
    sub_ror, subs_ror, subi, subsi; s_subtraction; sub_body);
define_alu_family_s_only!(teq_lsl, teq_lsr, teq_asr, teq_ror, teqi; s_neutral; teq_body);
define_alu_family_s_only!(tst_lsl, tst_lsr, tst_asr, tst_ror, tsti; s_neutral; tst_body);

// End ALU definitions

// ----- Multiply definitions -----

/// ARM_WAIT_SMUL
#[inline]
fn arm_wait_smul(gba: &mut Gba, r: i32, wait_base: i32, current_cycles: &mut i32) {
    let mut wait: i32 = wait_base;
    let ru = r as u32;
    if (ru & 0xFFFFFF00) == 0xFFFFFF00 || (ru & 0xFFFFFF00) == 0 {
        wait += 1;
    } else if (ru & 0xFFFF0000) == 0xFFFF0000 || (ru & 0xFFFF0000) == 0 {
        wait += 2;
    } else if (ru & 0xFF000000) == 0xFF000000 || (ru & 0xFF000000) == 0 {
        wait += 3;
    } else {
        wait += 4;
    }
    *current_cycles += gba.cpu_stall(wait);
}

/// ARM_WAIT_UMUL
#[inline]
fn arm_wait_umul(gba: &mut Gba, r: i32, wait_base: i32, current_cycles: &mut i32) {
    let mut wait: i32 = wait_base;
    let ru = r as u32;
    if (ru & 0xFFFFFF00) == 0 {
        wait += 1;
    } else if (ru & 0xFFFF0000) == 0 {
        wait += 2;
    } else if (ru & 0xFF000000) == 0 {
        wait += 3;
    } else {
        wait += 4;
    }
    *current_cycles += gba.cpu_stall(wait);
}

// Multiply S-bodies: fn(gba, rd, rd_hi). rd/rd_hi are register indices; the
// multiply guards already ensure neither is ARM_PC, so there is no SPSR path.
#[allow(unused_variables)]
fn s_mul_none(gba: &mut Gba, rd: usize, rd_hi: usize) {}

/// ARM_NEUTRAL_S(gprs[rm], gprs[rs], gprs[rd]) (MULS)
fn s_mul_neutral(gba: &mut Gba, rd: usize, _rd_hi: usize) {
    let d = gba.cpu.gprs[rd];
    apply_neutral_s(gba, d);
}

/// ARM_NEUTRAL_S(, , gprs[rdHi]) (MLAS)
fn s_mla_neutral(gba: &mut Gba, _rd: usize, rd_hi: usize) {
    let d = gba.cpu.gprs[rd_hi];
    apply_neutral_s(gba, d);
}

/// ARM_NEUTRAL_HI_S(gprs[rd], gprs[rdHi]) (long multiplies)
fn s_mul_neutral_hi(gba: &mut Gba, rd: usize, rd_hi: usize) {
    let dlo = gba.cpu.gprs[rd];
    let dhi = gba.cpu.gprs[rd_hi];
    apply_neutral_hi_s(gba, dlo, dhi);
}

// Multiply bodies: fn(gba, rd, rd_hi, rs, rm) with the register indices as
// decoded by the generating macro (MUL: rd=bits19:16; others: rd=bits15:12,
// rd_hi=bits19:16, mirroring DEFINE_MULTIPLY_INSTRUCTION_2_EX_ARM locals).
fn mul_body(gba: &mut Gba, rd: usize, _rd_hi: usize, rs: usize, rm: usize) {
    gba.cpu.gprs[rd] = gba.cpu.gprs[rm].wrapping_mul(gba.cpu.gprs[rs]);
}

fn mla_body(gba: &mut Gba, rd: usize, rd_hi: usize, rs: usize, rm: usize) {
    gba.cpu.gprs[rd_hi] = gba.cpu.gprs[rm].wrapping_mul(gba.cpu.gprs[rs]).wrapping_add(gba.cpu.gprs[rd]);
}

fn smlal_body(gba: &mut Gba, rd: usize, rd_hi: usize, rs: usize, rm: usize) {
    let d = (gba.cpu.gprs[rm] as i64)
        .wrapping_mul(gba.cpu.gprs[rs] as i64)
        .wrapping_add(gba.cpu.gprs[rd] as u32 as i64);
    let d_hi = (gba.cpu.gprs[rd_hi] as i64).wrapping_add(d >> 32) as i32;
    gba.cpu.gprs[rd] = d as i32;
    gba.cpu.gprs[rd_hi] = d_hi;
}

fn smull_body(gba: &mut Gba, rd: usize, rd_hi: usize, rs: usize, rm: usize) {
    let d = (gba.cpu.gprs[rm] as i64).wrapping_mul(gba.cpu.gprs[rs] as i64);
    gba.cpu.gprs[rd] = d as i32;
    gba.cpu.gprs[rd_hi] = (d >> 32) as i32;
}

fn umlal_body(gba: &mut Gba, rd: usize, rd_hi: usize, rs: usize, rm: usize) {
    let d = (gba.cpu.gprs[rm] as u32 as u64)
        .wrapping_mul(gba.cpu.gprs[rs] as u32 as u64)
        .wrapping_add(gba.cpu.gprs[rd] as u32 as u64);
    let d_hi = (gba.cpu.gprs[rd_hi] as u32).wrapping_add((d >> 32) as u32);
    gba.cpu.gprs[rd] = d as u32 as i32;
    gba.cpu.gprs[rd_hi] = d_hi as i32;
}

fn umull_body(gba: &mut Gba, rd: usize, rd_hi: usize, rs: usize, rm: usize) {
    let d = (gba.cpu.gprs[rm] as u32 as u64).wrapping_mul(gba.cpu.gprs[rs] as u32 as u64);
    gba.cpu.gprs[rd] = d as u32 as i32;
    gba.cpu.gprs[rd_hi] = (d >> 32) as u32 as i32;
}

// DEFINE_MULTIPLY_INSTRUCTION_EX_ARM
macro_rules! define_multiply_ex {
    ($name:ident, $body:expr, $s:expr, $wait:ident, $wait_base:expr) => {
        fn $name(gba: &mut Gba, opcode: u32) {
            let mut current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
            let rd = ((opcode >> 16) & 0xF) as usize;
            let rs = ((opcode >> 8) & 0xF) as usize;
            let rm = (opcode & 0xF) as usize;
            if rd != ARM_PC {
                $wait(gba, gba.cpu.gprs[rs], $wait_base, &mut current_cycles);
                let body: fn(&mut Gba, usize, usize, usize, usize) = $body;
                body(gba, rd, rd, rs, rm);
                let s_body: fn(&mut Gba, usize, usize) = $s;
                s_body(gba, rd, rd);
            }
            current_cycles += gba.cpu.active_nonseq_cycles32 - gba.cpu.active_seq_cycles32;
            gba.cpu.cycles += current_cycles;
        }
    };
}

// DEFINE_MULTIPLY_INSTRUCTION_2_EX_ARM
macro_rules! define_multiply2_ex {
    ($name:ident, $body:expr, $s:expr, $wait:ident, $wait_base:expr) => {
        fn $name(gba: &mut Gba, opcode: u32) {
            let mut current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
            let rd = ((opcode >> 12) & 0xF) as usize;
            let rd_hi = ((opcode >> 16) & 0xF) as usize;
            let rs = ((opcode >> 8) & 0xF) as usize;
            let rm = (opcode & 0xF) as usize;
            if rd_hi != ARM_PC && rd != ARM_PC {
                $wait(gba, gba.cpu.gprs[rs], $wait_base, &mut current_cycles);
                let body: fn(&mut Gba, usize, usize, usize, usize) = $body;
                body(gba, rd, rd_hi, rs, rm);
                let s_body: fn(&mut Gba, usize, usize) = $s;
                s_body(gba, rd, rd_hi);
            }
            current_cycles += gba.cpu.active_nonseq_cycles32 - gba.cpu.active_seq_cycles32;
            gba.cpu.cycles += current_cycles;
        }
    };
}

// DEFINE_MULTIPLY_INSTRUCTION_2_ARM(MLA, ..., S, 1)
define_multiply2_ex!(mla, mla_body, s_mul_none, arm_wait_smul, 1);
define_multiply2_ex!(mlas, mla_body, s_mla_neutral, arm_wait_smul, 1);

// DEFINE_MULTIPLY_INSTRUCTION_ARM(MUL, ..., S)
define_multiply_ex!(mul, mul_body, s_mul_none, arm_wait_smul, 0);
define_multiply_ex!(muls, mul_body, s_mul_neutral, arm_wait_smul, 0);

// SMLAL/SMULL (S), UMLAL/UMULL (U)
define_multiply2_ex!(smlal, smlal_body, s_mul_none, arm_wait_smul, 2);
define_multiply2_ex!(smlals, smlal_body, s_mul_neutral_hi, arm_wait_smul, 2);
define_multiply2_ex!(smull, smull_body, s_mul_none, arm_wait_smul, 1);
define_multiply2_ex!(smulls, smull_body, s_mul_neutral_hi, arm_wait_smul, 1);
define_multiply2_ex!(umlal, umlal_body, s_mul_none, arm_wait_umul, 2);
define_multiply2_ex!(umlals, umlal_body, s_mul_neutral_hi, arm_wait_umul, 2);
define_multiply2_ex!(umull, umull_body, s_mul_none, arm_wait_umul, 1);
define_multiply2_ex!(umulls, umull_body, s_mul_neutral_hi, arm_wait_umul, 1);

// End multiply definitions

// ----- Load/store definitions -----

// Addressing mode 2 offset computations.
fn addr_mode_2_lsl(gba: &mut Gba, rm: usize, opcode: u32) -> i32 {
    let i = ((opcode & 0x00000F80) >> 7) as u32;
    gba.cpu.gprs[rm].wrapping_shl(i)
}
fn addr_mode_2_lsr(gba: &mut Gba, rm: usize, opcode: u32) -> i32 {
    let _ = rm;
    if opcode & 0x00000F80 != 0 {
        let i = ((opcode & 0x00000F80) >> 7) as u32;
        ((gba.cpu.gprs[rm] as u32) >> i) as i32
    } else {
        0
    }
}
fn addr_mode_2_asr(gba: &mut Gba, rm: usize, opcode: u32) -> i32 {
    if opcode & 0x00000F80 != 0 {
        let i = ((opcode & 0x00000F80) >> 7) as u32;
        gba.cpu.gprs[rm] >> i
    } else {
        gba.cpu.gprs[rm] >> 31
    }
}
fn addr_mode_2_ror(gba: &mut Gba, rm: usize, opcode: u32) -> i32 {
    if opcode & 0x00000F80 != 0 {
        let i = ((opcode & 0x00000F80) >> 7) as u32;
        ror32(gba.cpu.gprs[rm], i)
    } else {
        ((gba.cpu.cpsr.c() as i32) << 31) | (((gba.cpu.gprs[rm] as u32) >> 1) as i32)
    }
}
fn addr_mode_2_immediate(_gba: &mut Gba, _rm: usize, opcode: u32) -> i32 {
    (opcode & 0x00000FFF) as i32
}

// Addressing mode 3 offset computations.
fn addr_mode_3_rm(gba: &mut Gba, rm: usize, _opcode: u32) -> i32 {
    gba.cpu.gprs[rm]
}
fn addr_mode_3_immediate(_gba: &mut Gba, _rm: usize, opcode: u32) -> i32 {
    (((opcode & 0x00000F00) >> 4) | (opcode & 0x0000000F)) as i32
}

// Address selection (ADDR_MODE_2_ADDRESS): fn(base, offset) -> address
fn addr_base(base: i32, _offset: i32) -> i32 {
    base
}
fn addr_down(base: i32, offset: i32) -> i32 {
    base.wrapping_sub(offset)
}
fn addr_up(base: i32, offset: i32) -> i32 {
    base.wrapping_add(offset)
}

// Writeback value selection: fn(base, offset, address) -> value to write to rn
fn wb_down(base: i32, offset: i32, _address: u32) -> i32 {
    base.wrapping_sub(offset)
}
fn wb_up(base: i32, offset: i32, _address: u32) -> i32 {
    base.wrapping_add(offset)
}
fn wb_addr(_base: i32, _offset: i32, address: u32) -> i32 {
    address as i32
}

/// ADDR_MODE_2_WRITEBACK/ADDR_MODE_3_WRITEBACK: gprs[rn] = value plus the
/// ARMWritePC refill when rn == ARM_PC.
#[inline]
fn ls_writeback(gba: &mut Gba, rn: usize, value: i32, current_cycles: &mut i32) {
    gba.cpu.gprs[rn] = value;
    if rn == ARM_PC {
        *current_cycles += gba.arm_write_pc();
    }
}

/// ARM_LOAD_POST_BODY
#[inline]
fn arm_load_post_body(gba: &mut Gba, rd: usize, current_cycles: &mut i32) {
    *current_cycles += gba.cpu.active_nonseq_cycles32 - gba.cpu.active_seq_cycles32;
    if rd == ARM_PC {
        *current_cycles += gba.arm_write_pc();
    }
}

/// ARM_STORE_POST_BODY
#[inline]
fn arm_store_post_body(gba: &mut Gba, current_cycles: &mut i32) {
    *current_cycles += gba.cpu.active_nonseq_cycles32 - gba.cpu.active_seq_cycles32;
}

// Load/store bodies: fn(gba, rd, d, address, current_cycles) where d is the
// (PC-adjusted) value of gprs[rd].

fn ldr_body(gba: &mut Gba, rd: usize, _d: i32, address: u32, current_cycles: &mut i32) {
    gba.cpu.gprs[rd] = gba.load32(address, current_cycles) as i32;
    arm_load_post_body(gba, rd, current_cycles);
}
fn ldrb_body(gba: &mut Gba, rd: usize, _d: i32, address: u32, current_cycles: &mut i32) {
    gba.cpu.gprs[rd] = gba.load8(address, current_cycles) as i32;
    arm_load_post_body(gba, rd, current_cycles);
}
fn ldrh_body(gba: &mut Gba, rd: usize, _d: i32, address: u32, current_cycles: &mut i32) {
    gba.cpu.gprs[rd] = gba.load16(address, current_cycles) as i32;
    arm_load_post_body(gba, rd, current_cycles);
}
fn ldrsb_body(gba: &mut Gba, rd: usize, _d: i32, address: u32, current_cycles: &mut i32) {
    gba.cpu.gprs[rd] = arm_sxt_8(gba.load8(address, current_cycles) as i32);
    arm_load_post_body(gba, rd, current_cycles);
}
fn ldrsh_body(gba: &mut Gba, rd: usize, _d: i32, address: u32, current_cycles: &mut i32) {
    if address & 1 != 0 {
        gba.cpu.gprs[rd] = arm_sxt_8(gba.load16(address, current_cycles) as i32);
    } else {
        gba.cpu.gprs[rd] = arm_sxt_16(gba.load16(address, current_cycles) as i32);
    }
    arm_load_post_body(gba, rd, current_cycles);
}
fn str_body(gba: &mut Gba, _rd: usize, d: i32, address: u32, current_cycles: &mut i32) {
    gba.store32(address, d, current_cycles);
    arm_store_post_body(gba, current_cycles);
}
fn strb_body(gba: &mut Gba, _rd: usize, d: i32, address: u32, current_cycles: &mut i32) {
    gba.store8(address, d, current_cycles);
    arm_store_post_body(gba, current_cycles);
}
fn strh_body(gba: &mut Gba, _rd: usize, d: i32, address: u32, current_cycles: &mut i32) {
    gba.store16(address, d, current_cycles);
    arm_store_post_body(gba, current_cycles);
}

// USER-mode (T) load/store bodies. GBA has no MPU, so these call the same
// accessors as the C (which only differs in the surrounding privilege switch).
fn ldrbt_body(gba: &mut Gba, rd: usize, _d: i32, address: u32, current_cycles: &mut i32) {
    let priv_mode = gba.cpu.privilege_mode;
    gba.arm_set_privilege_mode(PrivilegeMode::User);
    let r = gba.load8(address, current_cycles) as i32;
    gba.arm_set_privilege_mode(priv_mode);
    gba.cpu.gprs[rd] = r;
    arm_load_post_body(gba, rd, current_cycles);
}
fn ldrt_body(gba: &mut Gba, rd: usize, _d: i32, address: u32, current_cycles: &mut i32) {
    let priv_mode = gba.cpu.privilege_mode;
    gba.arm_set_privilege_mode(PrivilegeMode::User);
    let r = gba.load32(address, current_cycles) as i32;
    gba.arm_set_privilege_mode(priv_mode);
    gba.cpu.gprs[rd] = r;
    arm_load_post_body(gba, rd, current_cycles);
}
fn strbt_body(gba: &mut Gba, rd: usize, _d: i32, address: u32, current_cycles: &mut i32) {
    let priv_mode = gba.cpu.privilege_mode;
    let r = gba.cpu.gprs[rd]; // NB: raw gprs[rd], not the PC-adjusted d (as in the C)
    gba.arm_set_privilege_mode(PrivilegeMode::User);
    gba.store8(address, r, current_cycles);
    gba.arm_set_privilege_mode(priv_mode);
    arm_store_post_body(gba, current_cycles);
}
fn strt_body(gba: &mut Gba, rd: usize, _d: i32, address: u32, current_cycles: &mut i32) {
    let priv_mode = gba.cpu.privilege_mode;
    let r = gba.cpu.gprs[rd]; // NB: raw gprs[rd], not the PC-adjusted d (as in the C)
    gba.arm_set_privilege_mode(PrivilegeMode::User);
    gba.store32(address, r, current_cycles);
    gba.arm_set_privilege_mode(priv_mode);
    arm_store_post_body(gba, current_cycles);
}

// DEFINE_LOAD_STORE_INSTRUCTION_EX_ARM. $ls is `load` or `store` and selects
// writeback placement: ADDR_MODE_2_WRITEBACK_PRE_LOAD / _POST_STORE perform the
// writeback; the other two are empty.
macro_rules! define_ls_ex {
    ($name:ident, $ls:ident, $offset:expr, $addr:expr, $wb:expr, $body:expr) => {
        fn $name(gba: &mut Gba, opcode: u32) {
            let mut current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
            let rn = ((opcode >> 16) & 0xF) as usize;
            let rd = ((opcode >> 12) & 0xF) as usize;
            #[allow(unused_mut)]
            let mut d: i32 = gba.cpu.gprs[rd];
            if rd == ARM_PC {
                d = d.wrapping_add(WORD_SIZE_ARM);
            }
            let rm = (opcode & 0xF) as usize;
            let offset_fn: fn(&mut Gba, usize, u32) -> i32 = $offset;
            let offset: i32 = offset_fn(gba, rm, opcode);
            let base: i32 = gba.cpu.gprs[rn];
            let addr_fn: fn(i32, i32) -> i32 = $addr;
            let address: u32 = addr_fn(base, offset) as u32;
            let wb_fn: Option<fn(i32, i32, u32) -> i32> = $wb;
            let body_fn: fn(&mut Gba, usize, i32, u32, &mut i32) = $body;
            define_ls_ex!(@$ls gba, current_cycles, wb_fn, rn, base, offset, address, body_fn, rd, d);
            gba.cpu.cycles += current_cycles;
        }
    };
    (@load $gba:ident, $cc:ident, $wb_fn:ident, $rn:ident, $base:ident, $offset:ident,
     $address:ident, $body_fn:ident, $rd:ident, $d:ident) => {
        if let Some(wb) = $wb_fn {
            let wb_value = wb($base, $offset, $address);
            ls_writeback($gba, $rn, wb_value, &mut $cc);
        }
        $body_fn($gba, $rd, $d, $address, &mut $cc);
    };
    (@store $gba:ident, $cc:ident, $wb_fn:ident, $rn:ident, $base:ident, $offset:ident,
     $address:ident, $body_fn:ident, $rd:ident, $d:ident) => {
        $body_fn($gba, $rd, $d, $address, &mut $cc);
        if let Some(wb) = $wb_fn {
            let wb_value = wb($base, $offset, $address);
            ls_writeback($gba, $rn, wb_value, &mut $cc);
        }
    };
}

// DEFINE_LOAD_STORE_INSTRUCTION_SHIFTER_ARM (register offset, mode 2 or 3): the
// six P/U/W variants for one offset source.
macro_rules! define_ls_variants {
    ($n:ident, $u:ident, $p:ident, $pw:ident, $pu:ident, $puw:ident;
     $ls:ident, $offset:expr, $body:expr) => {
        define_ls_ex!($n, $ls, $offset, addr_base, Some(wb_down), $body);
        define_ls_ex!($u, $ls, $offset, addr_base, Some(wb_up), $body);
        define_ls_ex!($p, $ls, $offset, addr_down, None, $body);
        define_ls_ex!($pw, $ls, $offset, addr_down, Some(wb_addr), $body);
        define_ls_ex!($pu, $ls, $offset, addr_up, None, $body);
        define_ls_ex!($puw, $ls, $offset, addr_up, Some(wb_addr), $body);
    };
}

// DEFINE_LOAD_STORE_INSTRUCTION_ARM (addressing mode 2: 4 shifters + immediate).
macro_rules! define_ls_family {
    ($ls:ident, $body:expr;
     $lsl_n:ident, $lsl_u:ident, $lsl_p:ident, $lsl_pw:ident, $lsl_pu:ident, $lsl_puw:ident;
     $lsr_n:ident, $lsr_u:ident, $lsr_p:ident, $lsr_pw:ident, $lsr_pu:ident, $lsr_puw:ident;
     $asr_n:ident, $asr_u:ident, $asr_p:ident, $asr_pw:ident, $asr_pu:ident, $asr_puw:ident;
     $ror_n:ident, $ror_u:ident, $ror_p:ident, $ror_pw:ident, $ror_pu:ident, $ror_puw:ident;
     $i:ident, $iu:ident, $ip:ident, $ipw:ident, $ipu:ident, $ipuw:ident) => {
        define_ls_variants!($lsl_n, $lsl_u, $lsl_p, $lsl_pw, $lsl_pu, $lsl_puw; $ls, addr_mode_2_lsl, $body);
        define_ls_variants!($lsr_n, $lsr_u, $lsr_p, $lsr_pw, $lsr_pu, $lsr_puw; $ls, addr_mode_2_lsr, $body);
        define_ls_variants!($asr_n, $asr_u, $asr_p, $asr_pw, $asr_pu, $asr_puw; $ls, addr_mode_2_asr, $body);
        define_ls_variants!($ror_n, $ror_u, $ror_p, $ror_pw, $ror_pu, $ror_puw; $ls, addr_mode_2_ror, $body);
        define_ls_variants!($i, $iu, $ip, $ipw, $ipu, $ipuw; $ls, addr_mode_2_immediate, $body);
    };
}

// DEFINE_LOAD_STORE_MODE_3_INSTRUCTION_ARM (register offset + immediate).
macro_rules! define_ls_mode3_family {
    ($ls:ident, $body:expr;
     $n:ident, $u:ident, $p:ident, $pw:ident, $pu:ident, $puw:ident;
     $i:ident, $iu:ident, $ip:ident, $ipw:ident, $ipu:ident, $ipuw:ident) => {
        define_ls_variants!($n, $u, $p, $pw, $pu, $puw; $ls, addr_mode_3_rm, $body);
        define_ls_variants!($i, $iu, $ip, $ipw, $ipu, $ipuw; $ls, addr_mode_3_immediate, $body);
    };
}

// DEFINE_LOAD_STORE_T_INSTRUCTION_ARM (post-indexed, user mode; 4 shifters + immediate,
// no pre-indexed variants).
macro_rules! define_ls_t_family {
    ($ls:ident, $body:expr;
     $lsl_n:ident, $lsl_u:ident; $lsr_n:ident, $lsr_u:ident;
     $asr_n:ident, $asr_u:ident; $ror_n:ident, $ror_u:ident; $i:ident, $iu:ident) => {
        define_ls_ex!($lsl_n, $ls, addr_mode_2_lsl, addr_base, Some(wb_down), $body);
        define_ls_ex!($lsl_u, $ls, addr_mode_2_lsl, addr_base, Some(wb_up), $body);
        define_ls_ex!($lsr_n, $ls, addr_mode_2_lsr, addr_base, Some(wb_down), $body);
        define_ls_ex!($lsr_u, $ls, addr_mode_2_lsr, addr_base, Some(wb_up), $body);
        define_ls_ex!($asr_n, $ls, addr_mode_2_asr, addr_base, Some(wb_down), $body);
        define_ls_ex!($asr_u, $ls, addr_mode_2_asr, addr_base, Some(wb_up), $body);
        define_ls_ex!($ror_n, $ls, addr_mode_2_ror, addr_base, Some(wb_down), $body);
        define_ls_ex!($ror_u, $ls, addr_mode_2_ror, addr_base, Some(wb_up), $body);
        define_ls_ex!($i, $ls, addr_mode_2_immediate, addr_base, Some(wb_down), $body);
        define_ls_ex!($iu, $ls, addr_mode_2_immediate, addr_base, Some(wb_up), $body);
    };
}

define_ls_family!(load, ldr_body;
    ldr_lsl_, ldr_lsl_u, ldr_lsl_p, ldr_lsl_pw, ldr_lsl_pu, ldr_lsl_puw;
    ldr_lsr_, ldr_lsr_u, ldr_lsr_p, ldr_lsr_pw, ldr_lsr_pu, ldr_lsr_puw;
    ldr_asr_, ldr_asr_u, ldr_asr_p, ldr_asr_pw, ldr_asr_pu, ldr_asr_puw;
    ldr_ror_, ldr_ror_u, ldr_ror_p, ldr_ror_pw, ldr_ror_pu, ldr_ror_puw;
    ldri, ldriu, ldrip, ldripw, ldripu, ldripuw);
define_ls_family!(load, ldrb_body;
    ldrb_lsl_, ldrb_lsl_u, ldrb_lsl_p, ldrb_lsl_pw, ldrb_lsl_pu, ldrb_lsl_puw;
    ldrb_lsr_, ldrb_lsr_u, ldrb_lsr_p, ldrb_lsr_pw, ldrb_lsr_pu, ldrb_lsr_puw;
    ldrb_asr_, ldrb_asr_u, ldrb_asr_p, ldrb_asr_pw, ldrb_asr_pu, ldrb_asr_puw;
    ldrb_ror_, ldrb_ror_u, ldrb_ror_p, ldrb_ror_pw, ldrb_ror_pu, ldrb_ror_puw;
    ldrbi, ldrbiu, ldrbip, ldrbipw, ldrbipu, ldrbipuw);
define_ls_family!(store, str_body;
    str_lsl_, str_lsl_u, str_lsl_p, str_lsl_pw, str_lsl_pu, str_lsl_puw;
    str_lsr_, str_lsr_u, str_lsr_p, str_lsr_pw, str_lsr_pu, str_lsr_puw;
    str_asr_, str_asr_u, str_asr_p, str_asr_pw, str_asr_pu, str_asr_puw;
    str_ror_, str_ror_u, str_ror_p, str_ror_pw, str_ror_pu, str_ror_puw;
    stri, striu, strip, stripw, stripu, stripuw);
define_ls_family!(store, strb_body;
    strb_lsl_, strb_lsl_u, strb_lsl_p, strb_lsl_pw, strb_lsl_pu, strb_lsl_puw;
    strb_lsr_, strb_lsr_u, strb_lsr_p, strb_lsr_pw, strb_lsr_pu, strb_lsr_puw;
    strb_asr_, strb_asr_u, strb_asr_p, strb_asr_pw, strb_asr_pu, strb_asr_puw;
    strb_ror_, strb_ror_u, strb_ror_p, strb_ror_pw, strb_ror_pu, strb_ror_puw;
    strbi, strbiu, strbip, strbipw, strbipu, strbipuw);

define_ls_mode3_family!(load, ldrh_body;
    ldrh, ldrhu, ldrhp, ldrhpw, ldrhpu, ldrhpuw;
    ldrhi, ldrhiu, ldrhip, ldrhipw, ldrhipu, ldrhipuw);
define_ls_mode3_family!(load, ldrsb_body;
    ldrsb, ldrsbu, ldrsbp, ldrsbpw, ldrsbpu, ldrsbpuw;
    ldrsbi, ldrsbiu, ldrsbip, ldrsbipw, ldrsbipu, ldrsbipuw);
define_ls_mode3_family!(load, ldrsh_body;
    ldrsh, ldrshu, ldrshp, ldrshpw, ldrshpu, ldrshpuw;
    ldrshi, ldrshiu, ldrship, ldrshipw, ldrshipu, ldrshipuw);
define_ls_mode3_family!(store, strh_body;
    strh, strhu, strhp, strhpw, strhpu, strhpuw;
    strhi, strhiu, strhip, strhipw, strhipu, strhipuw);

define_ls_t_family!(load, ldrbt_body;
    ldrbt_lsl_, ldrbt_lsl_u; ldrbt_lsr_, ldrbt_lsr_u;
    ldrbt_asr_, ldrbt_asr_u; ldrbt_ror_, ldrbt_ror_u; ldrbti, ldrbtiu);
define_ls_t_family!(load, ldrt_body;
    ldrt_lsl_, ldrt_lsl_u; ldrt_lsr_, ldrt_lsr_u;
    ldrt_asr_, ldrt_asr_u; ldrt_ror_, ldrt_ror_u; ldrti, ldrtiu);
define_ls_t_family!(store, strbt_body;
    strbt_lsl_, strbt_lsl_u; strbt_lsr_, strbt_lsr_u;
    strbt_asr_, strbt_asr_u; strbt_ror_, strbt_ror_u; strbti, strbtiu);
define_ls_t_family!(store, strt_body;
    strt_lsl_, strt_lsl_u; strt_lsr_, strt_lsr_u;
    strt_asr_, strt_asr_u; strt_ror_, strt_ror_u; strti, strtiu);

// DEFINE_LOAD_STORE_MULTIPLE_INSTRUCTION_ARM. $s is `with_s` (S bit set) or
// `no_s`; $wb is `with_wb` (W bit) or `no_wb`; $dir is the LSM_* direction.
macro_rules! define_lsm {
    (@wb with_wb, load, $gba:ident, $rn:ident, $rs:ident, $address:ident) => {
        // ADDR_MODE_4_WRITEBACK_LDM
        if ((1i32 << $rn) & $rs) == 0 {
            $gba.cpu.gprs[$rn] = $address as i32;
        }
    };
    (@wb with_wb, store, $gba:ident, $rn:ident, $rs:ident, $address:ident) => {
        // ADDR_MODE_4_WRITEBACK_STM
        let _ = $rs;
        $gba.cpu.gprs[$rn] = $address as i32;
    };
    (@wb no_wb, $ls:ident, $gba:ident, $rn:ident, $rs:ident, $address:ident) => {
        let _ = ($rn, $rs, $address);
    };
    ($name:ident, load, with_s, $wb:ident, $dir:expr) => {
        fn $name(gba: &mut Gba, opcode: u32) {
            let mut current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
            let rn = ((opcode >> 16) & 0xF) as usize;
            let rs: i32 = (opcode & 0x0000FFFF) as i32;
            let mut address: u32 = gba.cpu.gprs[rn] as u32;
            // ARM_MS_PRE_load
            let privilege_mode = gba.cpu.privilege_mode;
            let user_bank: bool = (rs & 0x8000) == 0 && rs != 0;
            if user_bank {
                gba.arm_set_privilege_mode(PrivilegeMode::System);
            }
            address = gba.load_multiple(address, rs, $dir, &mut current_cycles);
            define_lsm!(@wb $wb, load, gba, rn, rs, address);
            // ARM_MS_POST_load
            if user_bank {
                gba.arm_set_privilege_mode(privilege_mode);
            } else if gba.cpu.cpsr.priv_mode().has_spsr() {
                gba.cpu.cpsr = gba.cpu.spsr;
                gba.arm_read_cpsr();
            }
            current_cycles += gba.cpu.active_nonseq_cycles32 - gba.cpu.active_seq_cycles32;
            if (rs & 0x8000) != 0 || rs == 0 {
                if gba.cpu.execution_mode == ExecutionMode::Thumb {
                    current_cycles += gba.thumb_write_pc();
                } else {
                    current_cycles += gba.arm_write_pc();
                }
            }
            gba.cpu.cycles += current_cycles;
        }
    };
    ($name:ident, load, no_s, $wb:ident, $dir:expr) => {
        fn $name(gba: &mut Gba, opcode: u32) {
            let mut current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
            let rn = ((opcode >> 16) & 0xF) as usize;
            let rs: i32 = (opcode & 0x0000FFFF) as i32;
            let mut address: u32 = gba.cpu.gprs[rn] as u32;
            address = gba.load_multiple(address, rs, $dir, &mut current_cycles);
            define_lsm!(@wb $wb, load, gba, rn, rs, address);
            current_cycles += gba.cpu.active_nonseq_cycles32 - gba.cpu.active_seq_cycles32;
            if (rs & 0x8000) != 0 || rs == 0 {
                if gba.cpu.execution_mode == ExecutionMode::Thumb {
                    current_cycles += gba.thumb_write_pc();
                } else {
                    current_cycles += gba.arm_write_pc();
                }
            }
            gba.cpu.cycles += current_cycles;
        }
    };
    ($name:ident, store, with_s, $wb:ident, $dir:expr) => {
        fn $name(gba: &mut Gba, opcode: u32) {
            let mut current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
            let rn = ((opcode >> 16) & 0xF) as usize;
            let rs: i32 = (opcode & 0x0000FFFF) as i32;
            let mut address: u32 = gba.cpu.gprs[rn] as u32;
            // ARM_MS_PRE_store
            let privilege_mode = gba.cpu.privilege_mode;
            gba.arm_set_privilege_mode(PrivilegeMode::System);
            address = gba.store_multiple(address, rs, $dir, &mut current_cycles);
            define_lsm!(@wb $wb, store, gba, rn, rs, address);
            // ARM_MS_POST_store
            gba.arm_set_privilege_mode(privilege_mode);
            current_cycles += gba.cpu.active_nonseq_cycles32 - gba.cpu.active_seq_cycles32;
            gba.cpu.cycles += current_cycles;
        }
    };
    ($name:ident, store, no_s, $wb:ident, $dir:expr) => {
        fn $name(gba: &mut Gba, opcode: u32) {
            let mut current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
            let rn = ((opcode >> 16) & 0xF) as usize;
            let rs: i32 = (opcode & 0x0000FFFF) as i32;
            let mut address: u32 = gba.cpu.gprs[rn] as u32;
            address = gba.store_multiple(address, rs, $dir, &mut current_cycles);
            define_lsm!(@wb $wb, store, gba, rn, rs, address);
            current_cycles += gba.cpu.active_nonseq_cycles32 - gba.cpu.active_seq_cycles32;
            gba.cpu.cycles += current_cycles;
        }
    };
}

define_lsm!(ldmda, load, no_s, no_wb, LSM_DA);
define_lsm!(ldmdaw, load, no_s, with_wb, LSM_DA);
define_lsm!(ldmdb, load, no_s, no_wb, LSM_DB);
define_lsm!(ldmdbw, load, no_s, with_wb, LSM_DB);
define_lsm!(ldmia, load, no_s, no_wb, LSM_IA);
define_lsm!(ldmiaw, load, no_s, with_wb, LSM_IA);
define_lsm!(ldmib, load, no_s, no_wb, LSM_IB);
define_lsm!(ldmibw, load, no_s, with_wb, LSM_IB);
define_lsm!(ldmsda, load, with_s, no_wb, LSM_DA);
define_lsm!(ldmsdaw, load, with_s, with_wb, LSM_DA);
define_lsm!(ldmsdb, load, with_s, no_wb, LSM_DB);
define_lsm!(ldmsdbw, load, with_s, with_wb, LSM_DB);
define_lsm!(ldmsia, load, with_s, no_wb, LSM_IA);
define_lsm!(ldmsiaw, load, with_s, with_wb, LSM_IA);
define_lsm!(ldmsib, load, with_s, no_wb, LSM_IB);
define_lsm!(ldmsibw, load, with_s, with_wb, LSM_IB);
define_lsm!(stmda, store, no_s, no_wb, LSM_DA);
define_lsm!(stmdaw, store, no_s, with_wb, LSM_DA);
define_lsm!(stmdb, store, no_s, no_wb, LSM_DB);
define_lsm!(stmdbw, store, no_s, with_wb, LSM_DB);
define_lsm!(stmia, store, no_s, no_wb, LSM_IA);
define_lsm!(stmiaw, store, no_s, with_wb, LSM_IA);
define_lsm!(stmib, store, no_s, no_wb, LSM_IB);
define_lsm!(stmibw, store, no_s, with_wb, LSM_IB);
define_lsm!(stmsda, store, with_s, no_wb, LSM_DA);
define_lsm!(stmsdaw, store, with_s, with_wb, LSM_DA);
define_lsm!(stmsdb, store, with_s, no_wb, LSM_DB);
define_lsm!(stmsdbw, store, with_s, with_wb, LSM_DB);
define_lsm!(stmsia, store, with_s, no_wb, LSM_IA);
define_lsm!(stmsiaw, store, with_s, with_wb, LSM_IA);
define_lsm!(stmsib, store, with_s, no_wb, LSM_IB);
define_lsm!(stmsibw, store, with_s, with_wb, LSM_IB);

fn swp(gba: &mut Gba, opcode: u32) {
    let mut current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
    let rm = (opcode & 0xF) as usize;
    let rd = ((opcode >> 12) & 0xF) as usize;
    let rn = ((opcode >> 16) & 0xF) as usize;
    let d = gba.load32(gba.cpu.gprs[rn] as u32, &mut current_cycles) as i32;
    let value = gba.cpu.gprs[rm];
    gba.store32(gba.cpu.gprs[rn] as u32, value, &mut current_cycles);
    gba.cpu.gprs[rd] = d;
    gba.cpu.cycles += current_cycles;
}

fn swpb(gba: &mut Gba, opcode: u32) {
    let mut current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
    let rm = (opcode & 0xF) as usize;
    let rd = ((opcode >> 12) & 0xF) as usize;
    let rn = ((opcode >> 16) & 0xF) as usize;
    let d = gba.load8(gba.cpu.gprs[rn] as u32, &mut current_cycles) as i32;
    let value = gba.cpu.gprs[rm];
    gba.store8(gba.cpu.gprs[rn] as u32, value, &mut current_cycles);
    gba.cpu.gprs[rd] = d;
    gba.cpu.cycles += current_cycles;
}

// End load/store definitions

// ----- Branch definitions -----

fn b(gba: &mut Gba, opcode: u32) {
    let mut current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
    let offset: i32 = ((opcode << 8) as i32) >> 6;
    gba.cpu.gprs[ARM_PC] = gba.cpu.gprs[ARM_PC].wrapping_add(offset);
    current_cycles += gba.arm_write_pc();
    gba.cpu.cycles += current_cycles;
}

fn bl(gba: &mut Gba, opcode: u32) {
    let mut current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
    let immediate: i32 = ((opcode & 0x00FFFFFF) << 8) as i32;
    gba.cpu.gprs[ARM_LR] = gba.cpu.gprs[ARM_PC].wrapping_sub(WORD_SIZE_ARM);
    gba.cpu.gprs[ARM_PC] = gba.cpu.gprs[ARM_PC].wrapping_add(immediate >> 6);
    current_cycles += gba.arm_write_pc();
    gba.cpu.cycles += current_cycles;
}

fn bx(gba: &mut Gba, opcode: u32) {
    let mut current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
    let rm = (opcode & 0x0000000F) as usize;
    let mode = if gba.cpu.gprs[rm] & 0x00000001 != 0 {
        ExecutionMode::Thumb
    } else {
        ExecutionMode::Arm
    };
    gba.arm_set_mode(mode);
    gba.cpu.gprs[ARM_PC] = gba.cpu.gprs[rm] & 0xFFFFFFFEu32 as i32;
    if gba.cpu.execution_mode == ExecutionMode::Thumb {
        current_cycles += gba.thumb_write_pc();
    } else {
        current_cycles += gba.arm_write_pc();
    }
    gba.cpu.cycles += current_cycles;
}

// End branch definitions

// ----- Coprocessor definitions -----

fn cdp(gba: &mut Gba, opcode: u32) {
    let current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
    let op1 = ((opcode >> 21) & 7) as i32;
    let op2 = ((opcode >> 5) & 7) as i32;
    let rd = ((opcode >> 12) & 0xF) as i32;
    let cp = ((opcode >> 8) & 0xF) as i32;
    let crn = ((opcode >> 16) & 0xF) as i32;
    let crm = (opcode & 0xF) as i32;
    gba.cp_cdp(crn, crm, rd, op1, op2);
    gba.cpu.cycles += current_cycles;
}

fn mcr(gba: &mut Gba, opcode: u32) {
    let current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
    let op1 = ((opcode >> 21) & 7) as i32;
    let op2 = ((opcode >> 5) & 7) as i32;
    let rd = ((opcode >> 12) & 0xF) as usize;
    let cp = ((opcode >> 8) & 0xF) as i32;
    let crn = ((opcode >> 16) & 0xF) as i32;
    let crm = (opcode & 0xF) as i32;
    let value = gba.cpu.gprs[rd];
    gba.cp_mcr(crn, crm, op1, op2, value);
    gba.cpu.cycles += current_cycles;
}

fn mrc(gba: &mut Gba, opcode: u32) {
    let current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
    let op1 = ((opcode >> 21) & 7) as i32;
    let op2 = ((opcode >> 5) & 7) as i32;
    let rd = ((opcode >> 12) & 0xF) as usize;
    let cp = ((opcode >> 8) & 0xF) as i32;
    let crn = ((opcode >> 16) & 0xF) as i32;
    let crm = (opcode & 0xF) as i32;
    gba.cpu.gprs[rd] = gba.cp_mrc(crn, crm, op1, op2);
    gba.cpu.cycles += current_cycles;
}

fn ldc(gba: &mut Gba, opcode: u32) {
    let current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
    gba.hit_stub(opcode);
    gba.cpu.cycles += current_cycles;
}

fn stc(gba: &mut Gba, opcode: u32) {
    let current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
    gba.hit_stub(opcode);
    gba.cpu.cycles += current_cycles;
}

// ----- Miscellaneous definitions -----

fn bkpt(gba: &mut Gba, opcode: u32) {
    // Not strictly in ARMv4T, but here for convenience. currentCycles = 0 in
    // the C (ARM_PREFETCH_CYCLES discarded).
    gba.bkpt32((((opcode >> 4) & 0xFFF0) | (opcode & 0xF)) as i32);
    gba.cpu.cycles += 0;
}

/// Illegal opcode (ARM_ILL)
fn ill(gba: &mut Gba, opcode: u32) {
    let current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
    gba.hit_illegal(opcode);
    gba.cpu.cycles += current_cycles;
}

fn mrs(gba: &mut Gba, opcode: u32) {
    let current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
    let rd = ((opcode >> 12) & 0xF) as usize;
    gba.cpu.gprs[rd] = gba.cpu.cpsr.packed;
    gba.cpu.cycles += current_cycles;
}

fn mrsr(gba: &mut Gba, opcode: u32) {
    let current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
    let rd = ((opcode >> 12) & 0xF) as usize;
    gba.cpu.gprs[rd] = gba.cpu.spsr.packed;
    gba.cpu.cycles += current_cycles;
}

/// The tail of the MSR (cpsr) handlers: _ARMReadCPSR plus the prefetch refill
/// (with the Thumb pipeline hack).
fn msr_read_cpsr_and_refill(gba: &mut Gba) {
    gba.arm_read_cpsr();
    if gba.cpu.execution_mode == ExecutionMode::Thumb {
        gba.cpu.prefetch[0] = 0x46C0; // nop
        gba.cpu.prefetch[1] &= 0xFFFF;
        gba.cpu.gprs[ARM_PC] += WORD_SIZE_THUMB;
    } else {
        let pc = gba.cpu.gprs[ARM_PC] as u32;
        let mask = gba.cpu.active_mask;
        gba.cpu.prefetch[0] = gba.active_region_load32(pc.wrapping_sub(WORD_SIZE_ARM as u32) & mask);
        gba.cpu.prefetch[1] = gba.active_region_load32(pc & mask);
    }
}

fn msr(gba: &mut Gba, opcode: u32) {
    let current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
    let c = opcode & 0x00010000 != 0;
    let f = opcode & 0x00080000 != 0;
    let operand: i32 = gba.cpu.gprs[(opcode & 0x0000000F) as usize];
    let mask: u32 = (if c { 0x000000FF } else { 0 }) | (if f { 0xFF000000 } else { 0 });
    if mask & PSR_USER_MASK != 0 {
        gba.cpu.cpsr.packed = (gba.cpu.cpsr.packed & !(PSR_USER_MASK as i32)) | (operand & PSR_USER_MASK as i32);
    }
    if mask & PSR_STATE_MASK != 0 {
        gba.cpu.cpsr.packed = (gba.cpu.cpsr.packed & !(PSR_STATE_MASK as i32)) | (operand & PSR_STATE_MASK as i32);
    }
    if gba.cpu.privilege_mode != PrivilegeMode::User && (mask & PSR_PRIV_MASK) != 0 {
        gba.arm_set_privilege_mode(privilege_mode_from_operand(operand));
        gba.cpu.cpsr.packed = (gba.cpu.cpsr.packed & !(PSR_PRIV_MASK as i32)) | (operand & PSR_PRIV_MASK as i32);
    }
    msr_read_cpsr_and_refill(gba);
    gba.cpu.cycles += current_cycles;
}

fn msrr(gba: &mut Gba, opcode: u32) {
    let current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
    let c = opcode & 0x00010000 != 0;
    let f = opcode & 0x00080000 != 0;
    let operand: i32 = gba.cpu.gprs[(opcode & 0x0000000F) as usize];
    let mut mask: u32 = (if c { 0x000000FF } else { 0 }) | (if f { 0xFF000000 } else { 0 });
    mask &= PSR_USER_MASK | PSR_PRIV_MASK | PSR_STATE_MASK;
    gba.cpu.spsr.packed = (gba.cpu.spsr.packed & !(mask as i32)) | (operand & mask as i32) | 0x00000010;
    gba.cpu.cycles += current_cycles;
}

fn msri(gba: &mut Gba, opcode: u32) {
    let current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
    let c = opcode & 0x00010000 != 0;
    let f = opcode & 0x00080000 != 0;
    let rotate = ((opcode & 0x00000F00) >> 7) as u32;
    let operand: i32 = ror32((opcode & 0x000000FF) as i32, rotate);
    let mask: u32 = (if c { 0x000000FF } else { 0 }) | (if f { 0xFF000000 } else { 0 });
    if mask & PSR_USER_MASK != 0 {
        gba.cpu.cpsr.packed = (gba.cpu.cpsr.packed & !(PSR_USER_MASK as i32)) | (operand & PSR_USER_MASK as i32);
    }
    if mask & PSR_STATE_MASK != 0 {
        gba.cpu.cpsr.packed = (gba.cpu.cpsr.packed & !(PSR_STATE_MASK as i32)) | (operand & PSR_STATE_MASK as i32);
    }
    if gba.cpu.privilege_mode != PrivilegeMode::User && (mask & PSR_PRIV_MASK) != 0 {
        gba.arm_set_privilege_mode(privilege_mode_from_operand(operand));
        gba.cpu.cpsr.packed = (gba.cpu.cpsr.packed & !(PSR_PRIV_MASK as i32)) | (operand & PSR_PRIV_MASK as i32);
    }
    msr_read_cpsr_and_refill(gba);
    gba.cpu.cycles += current_cycles;
}

fn msrri(gba: &mut Gba, opcode: u32) {
    let current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
    let c = opcode & 0x00010000 != 0;
    let f = opcode & 0x00080000 != 0;
    let rotate = ((opcode & 0x00000F00) >> 7) as u32;
    let operand: i32 = ror32((opcode & 0x000000FF) as i32, rotate);
    let mut mask: u32 = (if c { 0x000000FF } else { 0 }) | (if f { 0xFF000000 } else { 0 });
    mask &= PSR_USER_MASK | PSR_PRIV_MASK | PSR_STATE_MASK;
    gba.cpu.spsr.packed = (gba.cpu.spsr.packed & !(mask as i32)) | (operand & mask as i32) | 0x00000010;
    gba.cpu.cycles += current_cycles;
}

fn swi(gba: &mut Gba, opcode: u32) {
    let current_cycles: i32 = 1 + gba.cpu.active_seq_cycles32;
    gba.swi32((opcode & 0xFFFFFF) as i32);
    gba.cpu.cycles += current_cycles;
}

// Begin miscellaneous definitions (dispatch table)
// _armTable[0x1000] = { DECLARE_ARM_EMITTER_BLOCK(_ARMInstruction) }
#[rustfmt::skip]
pub static ARM_INSTRUCTION_TABLE: [fn(&mut Gba, u32); 0x1000] = [
    // 0x000
    and_lsl, and_lsl, and_lsr, and_lsr,
    and_asr, and_asr, and_ror, and_ror,
    and_lsl, mul, and_lsr, strh,
    and_asr, ill, and_ror, ill,
    // 0x010
    ands_lsl, ands_lsl, ands_lsr, ands_lsr,
    ands_asr, ands_asr, ands_ror, ands_ror,
    ands_lsl, muls, ands_lsr, ldrh,
    ands_asr, ldrsb, ands_ror, ldrsh,
    // 0x020
    eor_lsl, eor_lsl, eor_lsr, eor_lsr,
    eor_asr, eor_asr, eor_ror, eor_ror,
    eor_lsl, mla, eor_lsr, strh,
    eor_asr, ill, eor_ror, ill,
    // 0x030
    eors_lsl, eors_lsl, eors_lsr, eors_lsr,
    eors_asr, eors_asr, eors_ror, eors_ror,
    eors_lsl, mlas, eors_lsr, ldrh,
    eors_asr, ldrsb, eors_ror, ldrsh,
    // 0x040
    sub_lsl, sub_lsl, sub_lsr, sub_lsr,
    sub_asr, sub_asr, sub_ror, sub_ror,
    sub_lsl, ill, sub_lsr, strhi,
    sub_asr, ill, sub_ror, ill,
    // 0x050
    subs_lsl, subs_lsl, subs_lsr, subs_lsr,
    subs_asr, subs_asr, subs_ror, subs_ror,
    subs_lsl, ill, subs_lsr, ldrhi,
    subs_asr, ldrsbi, subs_ror, ldrshi,
    // 0x060
    rsb_lsl, rsb_lsl, rsb_lsr, rsb_lsr,
    rsb_asr, rsb_asr, rsb_ror, rsb_ror,
    rsb_lsl, ill, rsb_lsr, strhi,
    rsb_asr, ill, rsb_ror, ill,
    // 0x070
    rsbs_lsl, rsbs_lsl, rsbs_lsr, rsbs_lsr,
    rsbs_asr, rsbs_asr, rsbs_ror, rsbs_ror,
    rsbs_lsl, ill, rsbs_lsr, ldrhi,
    rsbs_asr, ldrsbi, rsbs_ror, ldrshi,
    // 0x080
    add_lsl, add_lsl, add_lsr, add_lsr,
    add_asr, add_asr, add_ror, add_ror,
    add_lsl, umull, add_lsr, strhu,
    add_asr, ill, add_ror, ill,
    // 0x090
    adds_lsl, adds_lsl, adds_lsr, adds_lsr,
    adds_asr, adds_asr, adds_ror, adds_ror,
    adds_lsl, umulls, adds_lsr, ldrhu,
    adds_asr, ldrsbu, adds_ror, ldrshu,
    // 0x0a0
    adc_lsl, adc_lsl, adc_lsr, adc_lsr,
    adc_asr, adc_asr, adc_ror, adc_ror,
    adc_lsl, umlal, adc_lsr, strhu,
    adc_asr, ill, adc_ror, ill,
    // 0x0b0
    adcs_lsl, adcs_lsl, adcs_lsr, adcs_lsr,
    adcs_asr, adcs_asr, adcs_ror, adcs_ror,
    adcs_lsl, umlals, adcs_lsr, ldrhu,
    adcs_asr, ldrsbu, adcs_ror, ldrshu,
    // 0x0c0
    sbc_lsl, sbc_lsl, sbc_lsr, sbc_lsr,
    sbc_asr, sbc_asr, sbc_ror, sbc_ror,
    sbc_lsl, smull, sbc_lsr, strhiu,
    sbc_asr, ill, sbc_ror, ill,
    // 0x0d0
    sbcs_lsl, sbcs_lsl, sbcs_lsr, sbcs_lsr,
    sbcs_asr, sbcs_asr, sbcs_ror, sbcs_ror,
    sbcs_lsl, smulls, sbcs_lsr, ldrhiu,
    sbcs_asr, ldrsbiu, sbcs_ror, ldrshiu,
    // 0x0e0
    rsc_lsl, rsc_lsl, rsc_lsr, rsc_lsr,
    rsc_asr, rsc_asr, rsc_ror, rsc_ror,
    rsc_lsl, smlal, rsc_lsr, strhiu,
    rsc_asr, ill, rsc_ror, ill,
    // 0x0f0
    rscs_lsl, rscs_lsl, rscs_lsr, rscs_lsr,
    rscs_asr, rscs_asr, rscs_ror, rscs_ror,
    rscs_lsl, smlals, rscs_lsr, ldrhiu,
    rscs_asr, ldrsbiu, rscs_ror, ldrshiu,
    // 0x100
    mrs, ill, ill, ill,
    ill, ill, ill, ill,
    ill, swp, ill, strhp,
    ill, ill, ill, ill,
    // 0x110
    tst_lsl, tst_lsl, tst_lsr, tst_lsr,
    tst_asr, tst_asr, tst_ror, tst_ror,
    tst_lsl, ill, tst_lsr, ldrhp,
    tst_asr, ldrsbp, tst_ror, ldrshp,
    // 0x120
    msr, bx, ill, ill,
    ill, ill, ill, bkpt,
    ill, ill, ill, strhpw,
    ill, ill, ill, ill,
    // 0x130
    teq_lsl, teq_lsl, teq_lsr, teq_lsr,
    teq_asr, teq_asr, teq_ror, teq_ror,
    teq_lsl, ill, teq_lsr, ldrhpw,
    teq_asr, ldrsbpw, teq_ror, ldrshpw,
    // 0x140
    mrsr, ill, ill, ill,
    ill, ill, ill, ill,
    ill, swpb, ill, strhip,
    ill, ill, ill, ill,
    // 0x150
    cmp_lsl, cmp_lsl, cmp_lsr, cmp_lsr,
    cmp_asr, cmp_asr, cmp_ror, cmp_ror,
    cmp_lsl, ill, cmp_lsr, ldrhip,
    cmp_asr, ldrsbip, cmp_ror, ldrship,
    // 0x160
    msrr, ill, ill, ill,
    ill, ill, ill, ill,
    ill, ill, ill, strhipw,
    ill, ill, ill, ill,
    // 0x170
    cmn_lsl, cmn_lsl, cmn_lsr, cmn_lsr,
    cmn_asr, cmn_asr, cmn_ror, cmn_ror,
    cmn_lsl, ill, cmn_lsr, ldrhipw,
    cmn_asr, ldrsbipw, cmn_ror, ldrshipw,
    // 0x180
    orr_lsl, orr_lsl, orr_lsr, orr_lsr,
    orr_asr, orr_asr, orr_ror, orr_ror,
    orr_lsl, ill, orr_lsr, strhpu,
    orr_asr, ill, orr_ror, ill,
    // 0x190
    orrs_lsl, orrs_lsl, orrs_lsr, orrs_lsr,
    orrs_asr, orrs_asr, orrs_ror, orrs_ror,
    orrs_lsl, ill, orrs_lsr, ldrhpu,
    orrs_asr, ldrsbpu, orrs_ror, ldrshpu,
    // 0x1a0
    mov_lsl, mov_lsl, mov_lsr, mov_lsr,
    mov_asr, mov_asr, mov_ror, mov_ror,
    mov_lsl, ill, mov_lsr, strhpuw,
    mov_asr, ill, mov_ror, ill,
    // 0x1b0
    movs_lsl, movs_lsl, movs_lsr, movs_lsr,
    movs_asr, movs_asr, movs_ror, movs_ror,
    movs_lsl, ill, movs_lsr, ldrhpuw,
    movs_asr, ldrsbpuw, movs_ror, ldrshpuw,
    // 0x1c0
    bic_lsl, bic_lsl, bic_lsr, bic_lsr,
    bic_asr, bic_asr, bic_ror, bic_ror,
    bic_lsl, ill, bic_lsr, strhipu,
    bic_asr, ill, bic_ror, ill,
    // 0x1d0
    bics_lsl, bics_lsl, bics_lsr, bics_lsr,
    bics_asr, bics_asr, bics_ror, bics_ror,
    bics_lsl, ill, bics_lsr, ldrhipu,
    bics_asr, ldrsbipu, bics_ror, ldrshipu,
    // 0x1e0
    mvn_lsl, mvn_lsl, mvn_lsr, mvn_lsr,
    mvn_asr, mvn_asr, mvn_ror, mvn_ror,
    mvn_lsl, ill, mvn_lsr, strhipuw,
    mvn_asr, ill, mvn_ror, ill,
    // 0x1f0
    mvns_lsl, mvns_lsl, mvns_lsr, mvns_lsr,
    mvns_asr, mvns_asr, mvns_ror, mvns_ror,
    mvns_lsl, ill, mvns_lsr, ldrhipuw,
    mvns_asr, ldrsbipuw, mvns_ror, ldrshipuw,
    // 0x200
    andi, andi, andi, andi,
    andi, andi, andi, andi,
    andi, andi, andi, andi,
    andi, andi, andi, andi,
    // 0x210
    andsi, andsi, andsi, andsi,
    andsi, andsi, andsi, andsi,
    andsi, andsi, andsi, andsi,
    andsi, andsi, andsi, andsi,
    // 0x220
    eori, eori, eori, eori,
    eori, eori, eori, eori,
    eori, eori, eori, eori,
    eori, eori, eori, eori,
    // 0x230
    eorsi, eorsi, eorsi, eorsi,
    eorsi, eorsi, eorsi, eorsi,
    eorsi, eorsi, eorsi, eorsi,
    eorsi, eorsi, eorsi, eorsi,
    // 0x240
    subi, subi, subi, subi,
    subi, subi, subi, subi,
    subi, subi, subi, subi,
    subi, subi, subi, subi,
    // 0x250
    subsi, subsi, subsi, subsi,
    subsi, subsi, subsi, subsi,
    subsi, subsi, subsi, subsi,
    subsi, subsi, subsi, subsi,
    // 0x260
    rsbi, rsbi, rsbi, rsbi,
    rsbi, rsbi, rsbi, rsbi,
    rsbi, rsbi, rsbi, rsbi,
    rsbi, rsbi, rsbi, rsbi,
    // 0x270
    rsbsi, rsbsi, rsbsi, rsbsi,
    rsbsi, rsbsi, rsbsi, rsbsi,
    rsbsi, rsbsi, rsbsi, rsbsi,
    rsbsi, rsbsi, rsbsi, rsbsi,
    // 0x280
    addi, addi, addi, addi,
    addi, addi, addi, addi,
    addi, addi, addi, addi,
    addi, addi, addi, addi,
    // 0x290
    addsi, addsi, addsi, addsi,
    addsi, addsi, addsi, addsi,
    addsi, addsi, addsi, addsi,
    addsi, addsi, addsi, addsi,
    // 0x2a0
    adci, adci, adci, adci,
    adci, adci, adci, adci,
    adci, adci, adci, adci,
    adci, adci, adci, adci,
    // 0x2b0
    adcsi, adcsi, adcsi, adcsi,
    adcsi, adcsi, adcsi, adcsi,
    adcsi, adcsi, adcsi, adcsi,
    adcsi, adcsi, adcsi, adcsi,
    // 0x2c0
    sbci, sbci, sbci, sbci,
    sbci, sbci, sbci, sbci,
    sbci, sbci, sbci, sbci,
    sbci, sbci, sbci, sbci,
    // 0x2d0
    sbcsi, sbcsi, sbcsi, sbcsi,
    sbcsi, sbcsi, sbcsi, sbcsi,
    sbcsi, sbcsi, sbcsi, sbcsi,
    sbcsi, sbcsi, sbcsi, sbcsi,
    // 0x2e0
    rsci, rsci, rsci, rsci,
    rsci, rsci, rsci, rsci,
    rsci, rsci, rsci, rsci,
    rsci, rsci, rsci, rsci,
    // 0x2f0
    rscsi, rscsi, rscsi, rscsi,
    rscsi, rscsi, rscsi, rscsi,
    rscsi, rscsi, rscsi, rscsi,
    rscsi, rscsi, rscsi, rscsi,
    // 0x300
    tsti, tsti, tsti, tsti,
    tsti, tsti, tsti, tsti,
    tsti, tsti, tsti, tsti,
    tsti, tsti, tsti, tsti,
    // 0x310
    tsti, tsti, tsti, tsti,
    tsti, tsti, tsti, tsti,
    tsti, tsti, tsti, tsti,
    tsti, tsti, tsti, tsti,
    // 0x320
    msri, msri, msri, msri,
    msri, msri, msri, msri,
    msri, msri, msri, msri,
    msri, msri, msri, msri,
    // 0x330
    teqi, teqi, teqi, teqi,
    teqi, teqi, teqi, teqi,
    teqi, teqi, teqi, teqi,
    teqi, teqi, teqi, teqi,
    // 0x340
    cmpi, cmpi, cmpi, cmpi,
    cmpi, cmpi, cmpi, cmpi,
    cmpi, cmpi, cmpi, cmpi,
    cmpi, cmpi, cmpi, cmpi,
    // 0x350
    cmpi, cmpi, cmpi, cmpi,
    cmpi, cmpi, cmpi, cmpi,
    cmpi, cmpi, cmpi, cmpi,
    cmpi, cmpi, cmpi, cmpi,
    // 0x360
    msrri, msrri, msrri, msrri,
    msrri, msrri, msrri, msrri,
    msrri, msrri, msrri, msrri,
    msrri, msrri, msrri, msrri,
    // 0x370
    cmni, cmni, cmni, cmni,
    cmni, cmni, cmni, cmni,
    cmni, cmni, cmni, cmni,
    cmni, cmni, cmni, cmni,
    // 0x380
    orri, orri, orri, orri,
    orri, orri, orri, orri,
    orri, orri, orri, orri,
    orri, orri, orri, orri,
    // 0x390
    orrsi, orrsi, orrsi, orrsi,
    orrsi, orrsi, orrsi, orrsi,
    orrsi, orrsi, orrsi, orrsi,
    orrsi, orrsi, orrsi, orrsi,
    // 0x3a0
    movi, movi, movi, movi,
    movi, movi, movi, movi,
    movi, movi, movi, movi,
    movi, movi, movi, movi,
    // 0x3b0
    movsi, movsi, movsi, movsi,
    movsi, movsi, movsi, movsi,
    movsi, movsi, movsi, movsi,
    movsi, movsi, movsi, movsi,
    // 0x3c0
    bici, bici, bici, bici,
    bici, bici, bici, bici,
    bici, bici, bici, bici,
    bici, bici, bici, bici,
    // 0x3d0
    bicsi, bicsi, bicsi, bicsi,
    bicsi, bicsi, bicsi, bicsi,
    bicsi, bicsi, bicsi, bicsi,
    bicsi, bicsi, bicsi, bicsi,
    // 0x3e0
    mvni, mvni, mvni, mvni,
    mvni, mvni, mvni, mvni,
    mvni, mvni, mvni, mvni,
    mvni, mvni, mvni, mvni,
    // 0x3f0
    mvnsi, mvnsi, mvnsi, mvnsi,
    mvnsi, mvnsi, mvnsi, mvnsi,
    mvnsi, mvnsi, mvnsi, mvnsi,
    mvnsi, mvnsi, mvnsi, mvnsi,
    // 0x400
    stri, stri, stri, stri,
    stri, stri, stri, stri,
    stri, stri, stri, stri,
    stri, stri, stri, stri,
    // 0x410
    ldri, ldri, ldri, ldri,
    ldri, ldri, ldri, ldri,
    ldri, ldri, ldri, ldri,
    ldri, ldri, ldri, ldri,
    // 0x420
    strti, strti, strti, strti,
    strti, strti, strti, strti,
    strti, strti, strti, strti,
    strti, strti, strti, strti,
    // 0x430
    ldrti, ldrti, ldrti, ldrti,
    ldrti, ldrti, ldrti, ldrti,
    ldrti, ldrti, ldrti, ldrti,
    ldrti, ldrti, ldrti, ldrti,
    // 0x440
    strbi, strbi, strbi, strbi,
    strbi, strbi, strbi, strbi,
    strbi, strbi, strbi, strbi,
    strbi, strbi, strbi, strbi,
    // 0x450
    ldrbi, ldrbi, ldrbi, ldrbi,
    ldrbi, ldrbi, ldrbi, ldrbi,
    ldrbi, ldrbi, ldrbi, ldrbi,
    ldrbi, ldrbi, ldrbi, ldrbi,
    // 0x460
    strbti, strbti, strbti, strbti,
    strbti, strbti, strbti, strbti,
    strbti, strbti, strbti, strbti,
    strbti, strbti, strbti, strbti,
    // 0x470
    ldrbti, ldrbti, ldrbti, ldrbti,
    ldrbti, ldrbti, ldrbti, ldrbti,
    ldrbti, ldrbti, ldrbti, ldrbti,
    ldrbti, ldrbti, ldrbti, ldrbti,
    // 0x480
    striu, striu, striu, striu,
    striu, striu, striu, striu,
    striu, striu, striu, striu,
    striu, striu, striu, striu,
    // 0x490
    ldriu, ldriu, ldriu, ldriu,
    ldriu, ldriu, ldriu, ldriu,
    ldriu, ldriu, ldriu, ldriu,
    ldriu, ldriu, ldriu, ldriu,
    // 0x4a0
    strtiu, strtiu, strtiu, strtiu,
    strtiu, strtiu, strtiu, strtiu,
    strtiu, strtiu, strtiu, strtiu,
    strtiu, strtiu, strtiu, strtiu,
    // 0x4b0
    ldrtiu, ldrtiu, ldrtiu, ldrtiu,
    ldrtiu, ldrtiu, ldrtiu, ldrtiu,
    ldrtiu, ldrtiu, ldrtiu, ldrtiu,
    ldrtiu, ldrtiu, ldrtiu, ldrtiu,
    // 0x4c0
    strbiu, strbiu, strbiu, strbiu,
    strbiu, strbiu, strbiu, strbiu,
    strbiu, strbiu, strbiu, strbiu,
    strbiu, strbiu, strbiu, strbiu,
    // 0x4d0
    ldrbiu, ldrbiu, ldrbiu, ldrbiu,
    ldrbiu, ldrbiu, ldrbiu, ldrbiu,
    ldrbiu, ldrbiu, ldrbiu, ldrbiu,
    ldrbiu, ldrbiu, ldrbiu, ldrbiu,
    // 0x4e0
    strbtiu, strbtiu, strbtiu, strbtiu,
    strbtiu, strbtiu, strbtiu, strbtiu,
    strbtiu, strbtiu, strbtiu, strbtiu,
    strbtiu, strbtiu, strbtiu, strbtiu,
    // 0x4f0
    ldrbtiu, ldrbtiu, ldrbtiu, ldrbtiu,
    ldrbtiu, ldrbtiu, ldrbtiu, ldrbtiu,
    ldrbtiu, ldrbtiu, ldrbtiu, ldrbtiu,
    ldrbtiu, ldrbtiu, ldrbtiu, ldrbtiu,
    // 0x500
    strip, strip, strip, strip,
    strip, strip, strip, strip,
    strip, strip, strip, strip,
    strip, strip, strip, strip,
    // 0x510
    ldrip, ldrip, ldrip, ldrip,
    ldrip, ldrip, ldrip, ldrip,
    ldrip, ldrip, ldrip, ldrip,
    ldrip, ldrip, ldrip, ldrip,
    // 0x520
    stripw, stripw, stripw, stripw,
    stripw, stripw, stripw, stripw,
    stripw, stripw, stripw, stripw,
    stripw, stripw, stripw, stripw,
    // 0x530
    ldripw, ldripw, ldripw, ldripw,
    ldripw, ldripw, ldripw, ldripw,
    ldripw, ldripw, ldripw, ldripw,
    ldripw, ldripw, ldripw, ldripw,
    // 0x540
    strbip, strbip, strbip, strbip,
    strbip, strbip, strbip, strbip,
    strbip, strbip, strbip, strbip,
    strbip, strbip, strbip, strbip,
    // 0x550
    ldrbip, ldrbip, ldrbip, ldrbip,
    ldrbip, ldrbip, ldrbip, ldrbip,
    ldrbip, ldrbip, ldrbip, ldrbip,
    ldrbip, ldrbip, ldrbip, ldrbip,
    // 0x560
    strbipw, strbipw, strbipw, strbipw,
    strbipw, strbipw, strbipw, strbipw,
    strbipw, strbipw, strbipw, strbipw,
    strbipw, strbipw, strbipw, strbipw,
    // 0x570
    ldrbipw, ldrbipw, ldrbipw, ldrbipw,
    ldrbipw, ldrbipw, ldrbipw, ldrbipw,
    ldrbipw, ldrbipw, ldrbipw, ldrbipw,
    ldrbipw, ldrbipw, ldrbipw, ldrbipw,
    // 0x580
    stripu, stripu, stripu, stripu,
    stripu, stripu, stripu, stripu,
    stripu, stripu, stripu, stripu,
    stripu, stripu, stripu, stripu,
    // 0x590
    ldripu, ldripu, ldripu, ldripu,
    ldripu, ldripu, ldripu, ldripu,
    ldripu, ldripu, ldripu, ldripu,
    ldripu, ldripu, ldripu, ldripu,
    // 0x5a0
    stripuw, stripuw, stripuw, stripuw,
    stripuw, stripuw, stripuw, stripuw,
    stripuw, stripuw, stripuw, stripuw,
    stripuw, stripuw, stripuw, stripuw,
    // 0x5b0
    ldripuw, ldripuw, ldripuw, ldripuw,
    ldripuw, ldripuw, ldripuw, ldripuw,
    ldripuw, ldripuw, ldripuw, ldripuw,
    ldripuw, ldripuw, ldripuw, ldripuw,
    // 0x5c0
    strbipu, strbipu, strbipu, strbipu,
    strbipu, strbipu, strbipu, strbipu,
    strbipu, strbipu, strbipu, strbipu,
    strbipu, strbipu, strbipu, strbipu,
    // 0x5d0
    ldrbipu, ldrbipu, ldrbipu, ldrbipu,
    ldrbipu, ldrbipu, ldrbipu, ldrbipu,
    ldrbipu, ldrbipu, ldrbipu, ldrbipu,
    ldrbipu, ldrbipu, ldrbipu, ldrbipu,
    // 0x5e0
    strbipuw, strbipuw, strbipuw, strbipuw,
    strbipuw, strbipuw, strbipuw, strbipuw,
    strbipuw, strbipuw, strbipuw, strbipuw,
    strbipuw, strbipuw, strbipuw, strbipuw,
    // 0x5f0
    ldrbipuw, ldrbipuw, ldrbipuw, ldrbipuw,
    ldrbipuw, ldrbipuw, ldrbipuw, ldrbipuw,
    ldrbipuw, ldrbipuw, ldrbipuw, ldrbipuw,
    ldrbipuw, ldrbipuw, ldrbipuw, ldrbipuw,
    // 0x600
    str_lsl_, ill, str_lsr_, ill,
    str_asr_, ill, str_ror_, ill,
    str_lsl_, ill, str_lsr_, ill,
    str_asr_, ill, str_ror_, ill,
    // 0x610
    ldr_lsl_, ill, ldr_lsr_, ill,
    ldr_asr_, ill, ldr_ror_, ill,
    ldr_lsl_, ill, ldr_lsr_, ill,
    ldr_asr_, ill, ldr_ror_, ill,
    // 0x620
    strt_lsl_, ill, strt_lsr_, ill,
    strt_asr_, ill, strt_ror_, ill,
    strt_lsl_, ill, strt_lsr_, ill,
    strt_asr_, ill, strt_ror_, ill,
    // 0x630
    ldrt_lsl_, ill, ldrt_lsr_, ill,
    ldrt_asr_, ill, ldrt_ror_, ill,
    ldrt_lsl_, ill, ldrt_lsr_, ill,
    ldrt_asr_, ill, ldrt_ror_, ill,
    // 0x640
    strb_lsl_, ill, strb_lsr_, ill,
    strb_asr_, ill, strb_ror_, ill,
    strb_lsl_, ill, strb_lsr_, ill,
    strb_asr_, ill, strb_ror_, ill,
    // 0x650
    ldrb_lsl_, ill, ldrb_lsr_, ill,
    ldrb_asr_, ill, ldrb_ror_, ill,
    ldrb_lsl_, ill, ldrb_lsr_, ill,
    ldrb_asr_, ill, ldrb_ror_, ill,
    // 0x660
    strbt_lsl_, ill, strbt_lsr_, ill,
    strbt_asr_, ill, strbt_ror_, ill,
    strbt_lsl_, ill, strbt_lsr_, ill,
    strbt_asr_, ill, strbt_ror_, ill,
    // 0x670
    ldrbt_lsl_, ill, ldrbt_lsr_, ill,
    ldrbt_asr_, ill, ldrbt_ror_, ill,
    ldrbt_lsl_, ill, ldrbt_lsr_, ill,
    ldrbt_asr_, ill, ldrbt_ror_, ill,
    // 0x680
    str_lsl_u, ill, str_lsr_u, ill,
    str_asr_u, ill, str_ror_u, ill,
    str_lsl_u, ill, str_lsr_u, ill,
    str_asr_u, ill, str_ror_u, ill,
    // 0x690
    ldr_lsl_u, ill, ldr_lsr_u, ill,
    ldr_asr_u, ill, ldr_ror_u, ill,
    ldr_lsl_u, ill, ldr_lsr_u, ill,
    ldr_asr_u, ill, ldr_ror_u, ill,
    // 0x6a0
    strt_lsl_u, ill, strt_lsr_u, ill,
    strt_asr_u, ill, strt_ror_u, ill,
    strt_lsl_u, ill, strt_lsr_u, ill,
    strt_asr_u, ill, strt_ror_u, ill,
    // 0x6b0
    ldrt_lsl_u, ill, ldrt_lsr_u, ill,
    ldrt_asr_u, ill, ldrt_ror_u, ill,
    ldrt_lsl_u, ill, ldrt_lsr_u, ill,
    ldrt_asr_u, ill, ldrt_ror_u, ill,
    // 0x6c0
    strb_lsl_u, ill, strb_lsr_u, ill,
    strb_asr_u, ill, strb_ror_u, ill,
    strb_lsl_u, ill, strb_lsr_u, ill,
    strb_asr_u, ill, strb_ror_u, ill,
    // 0x6d0
    ldrb_lsl_u, ill, ldrb_lsr_u, ill,
    ldrb_asr_u, ill, ldrb_ror_u, ill,
    ldrb_lsl_u, ill, ldrb_lsr_u, ill,
    ldrb_asr_u, ill, ldrb_ror_u, ill,
    // 0x6e0
    strbt_lsl_u, ill, strbt_lsr_u, ill,
    strbt_asr_u, ill, strbt_ror_u, ill,
    strbt_lsl_u, ill, strbt_lsr_u, ill,
    strbt_asr_u, ill, strbt_ror_u, ill,
    // 0x6f0
    ldrbt_lsl_u, ill, ldrbt_lsr_u, ill,
    ldrbt_asr_u, ill, ldrbt_ror_u, ill,
    ldrbt_lsl_u, ill, ldrbt_lsr_u, ill,
    ldrbt_asr_u, ill, ldrbt_ror_u, ill,
    // 0x700
    str_lsl_p, ill, str_lsr_p, ill,
    str_asr_p, ill, str_ror_p, ill,
    str_lsl_p, ill, str_lsr_p, ill,
    str_asr_p, ill, str_ror_p, ill,
    // 0x710
    ldr_lsl_p, ill, ldr_lsr_p, ill,
    ldr_asr_p, ill, ldr_ror_p, ill,
    ldr_lsl_p, ill, ldr_lsr_p, ill,
    ldr_asr_p, ill, ldr_ror_p, ill,
    // 0x720
    str_lsl_pw, ill, str_lsr_pw, ill,
    str_asr_pw, ill, str_ror_pw, ill,
    str_lsl_pw, ill, str_lsr_pw, ill,
    str_asr_pw, ill, str_ror_pw, ill,
    // 0x730
    ldr_lsl_pw, ill, ldr_lsr_pw, ill,
    ldr_asr_pw, ill, ldr_ror_pw, ill,
    ldr_lsl_pw, ill, ldr_lsr_pw, ill,
    ldr_asr_pw, ill, ldr_ror_pw, ill,
    // 0x740
    strb_lsl_p, ill, strb_lsr_p, ill,
    strb_asr_p, ill, strb_ror_p, ill,
    strb_lsl_p, ill, strb_lsr_p, ill,
    strb_asr_p, ill, strb_ror_p, ill,
    // 0x750
    ldrb_lsl_p, ill, ldrb_lsr_p, ill,
    ldrb_asr_p, ill, ldrb_ror_p, ill,
    ldrb_lsl_p, ill, ldrb_lsr_p, ill,
    ldrb_asr_p, ill, ldrb_ror_p, ill,
    // 0x760
    strb_lsl_pw, ill, strb_lsr_pw, ill,
    strb_asr_pw, ill, strb_ror_pw, ill,
    strb_lsl_pw, ill, strb_lsr_pw, ill,
    strb_asr_pw, ill, strb_ror_pw, ill,
    // 0x770
    ldrb_lsl_pw, ill, ldrb_lsr_pw, ill,
    ldrb_asr_pw, ill, ldrb_ror_pw, ill,
    ldrb_lsl_pw, ill, ldrb_lsr_pw, ill,
    ldrb_asr_pw, ill, ldrb_ror_pw, ill,
    // 0x780
    str_lsl_pu, ill, str_lsr_pu, ill,
    str_asr_pu, ill, str_ror_pu, ill,
    str_lsl_pu, ill, str_lsr_pu, ill,
    str_asr_pu, ill, str_ror_pu, ill,
    // 0x790
    ldr_lsl_pu, ill, ldr_lsr_pu, ill,
    ldr_asr_pu, ill, ldr_ror_pu, ill,
    ldr_lsl_pu, ill, ldr_lsr_pu, ill,
    ldr_asr_pu, ill, ldr_ror_pu, ill,
    // 0x7a0
    str_lsl_puw, ill, str_lsr_puw, ill,
    str_asr_puw, ill, str_ror_puw, ill,
    str_lsl_puw, ill, str_lsr_puw, ill,
    str_asr_puw, ill, str_ror_puw, ill,
    // 0x7b0
    ldr_lsl_puw, ill, ldr_lsr_puw, ill,
    ldr_asr_puw, ill, ldr_ror_puw, ill,
    ldr_lsl_puw, ill, ldr_lsr_puw, ill,
    ldr_asr_puw, ill, ldr_ror_puw, ill,
    // 0x7c0
    strb_lsl_pu, ill, strb_lsr_pu, ill,
    strb_asr_pu, ill, strb_ror_pu, ill,
    strb_lsl_pu, ill, strb_lsr_pu, ill,
    strb_asr_pu, ill, strb_ror_pu, ill,
    // 0x7d0
    ldrb_lsl_pu, ill, ldrb_lsr_pu, ill,
    ldrb_asr_pu, ill, ldrb_ror_pu, ill,
    ldrb_lsl_pu, ill, ldrb_lsr_pu, ill,
    ldrb_asr_pu, ill, ldrb_ror_pu, ill,
    // 0x7e0
    strb_lsl_puw, ill, strb_lsr_puw, ill,
    strb_asr_puw, ill, strb_ror_puw, ill,
    strb_lsl_puw, ill, strb_lsr_puw, ill,
    strb_asr_puw, ill, strb_ror_puw, ill,
    // 0x7f0
    ldrb_lsl_puw, ill, ldrb_lsr_puw, ill,
    ldrb_asr_puw, ill, ldrb_ror_puw, ill,
    ldrb_lsl_puw, ill, ldrb_lsr_puw, ill,
    ldrb_asr_puw, ill, ldrb_ror_puw, ill,
    // 0x800
    stmda, stmda, stmda, stmda,
    stmda, stmda, stmda, stmda,
    stmda, stmda, stmda, stmda,
    stmda, stmda, stmda, stmda,
    // 0x810
    ldmda, ldmda, ldmda, ldmda,
    ldmda, ldmda, ldmda, ldmda,
    ldmda, ldmda, ldmda, ldmda,
    ldmda, ldmda, ldmda, ldmda,
    // 0x820
    stmdaw, stmdaw, stmdaw, stmdaw,
    stmdaw, stmdaw, stmdaw, stmdaw,
    stmdaw, stmdaw, stmdaw, stmdaw,
    stmdaw, stmdaw, stmdaw, stmdaw,
    // 0x830
    ldmdaw, ldmdaw, ldmdaw, ldmdaw,
    ldmdaw, ldmdaw, ldmdaw, ldmdaw,
    ldmdaw, ldmdaw, ldmdaw, ldmdaw,
    ldmdaw, ldmdaw, ldmdaw, ldmdaw,
    // 0x840
    stmsda, stmsda, stmsda, stmsda,
    stmsda, stmsda, stmsda, stmsda,
    stmsda, stmsda, stmsda, stmsda,
    stmsda, stmsda, stmsda, stmsda,
    // 0x850
    ldmsda, ldmsda, ldmsda, ldmsda,
    ldmsda, ldmsda, ldmsda, ldmsda,
    ldmsda, ldmsda, ldmsda, ldmsda,
    ldmsda, ldmsda, ldmsda, ldmsda,
    // 0x860
    stmsdaw, stmsdaw, stmsdaw, stmsdaw,
    stmsdaw, stmsdaw, stmsdaw, stmsdaw,
    stmsdaw, stmsdaw, stmsdaw, stmsdaw,
    stmsdaw, stmsdaw, stmsdaw, stmsdaw,
    // 0x870
    ldmsdaw, ldmsdaw, ldmsdaw, ldmsdaw,
    ldmsdaw, ldmsdaw, ldmsdaw, ldmsdaw,
    ldmsdaw, ldmsdaw, ldmsdaw, ldmsdaw,
    ldmsdaw, ldmsdaw, ldmsdaw, ldmsdaw,
    // 0x880
    stmia, stmia, stmia, stmia,
    stmia, stmia, stmia, stmia,
    stmia, stmia, stmia, stmia,
    stmia, stmia, stmia, stmia,
    // 0x890
    ldmia, ldmia, ldmia, ldmia,
    ldmia, ldmia, ldmia, ldmia,
    ldmia, ldmia, ldmia, ldmia,
    ldmia, ldmia, ldmia, ldmia,
    // 0x8a0
    stmiaw, stmiaw, stmiaw, stmiaw,
    stmiaw, stmiaw, stmiaw, stmiaw,
    stmiaw, stmiaw, stmiaw, stmiaw,
    stmiaw, stmiaw, stmiaw, stmiaw,
    // 0x8b0
    ldmiaw, ldmiaw, ldmiaw, ldmiaw,
    ldmiaw, ldmiaw, ldmiaw, ldmiaw,
    ldmiaw, ldmiaw, ldmiaw, ldmiaw,
    ldmiaw, ldmiaw, ldmiaw, ldmiaw,
    // 0x8c0
    stmsia, stmsia, stmsia, stmsia,
    stmsia, stmsia, stmsia, stmsia,
    stmsia, stmsia, stmsia, stmsia,
    stmsia, stmsia, stmsia, stmsia,
    // 0x8d0
    ldmsia, ldmsia, ldmsia, ldmsia,
    ldmsia, ldmsia, ldmsia, ldmsia,
    ldmsia, ldmsia, ldmsia, ldmsia,
    ldmsia, ldmsia, ldmsia, ldmsia,
    // 0x8e0
    stmsiaw, stmsiaw, stmsiaw, stmsiaw,
    stmsiaw, stmsiaw, stmsiaw, stmsiaw,
    stmsiaw, stmsiaw, stmsiaw, stmsiaw,
    stmsiaw, stmsiaw, stmsiaw, stmsiaw,
    // 0x8f0
    ldmsiaw, ldmsiaw, ldmsiaw, ldmsiaw,
    ldmsiaw, ldmsiaw, ldmsiaw, ldmsiaw,
    ldmsiaw, ldmsiaw, ldmsiaw, ldmsiaw,
    ldmsiaw, ldmsiaw, ldmsiaw, ldmsiaw,
    // 0x900
    stmdb, stmdb, stmdb, stmdb,
    stmdb, stmdb, stmdb, stmdb,
    stmdb, stmdb, stmdb, stmdb,
    stmdb, stmdb, stmdb, stmdb,
    // 0x910
    ldmdb, ldmdb, ldmdb, ldmdb,
    ldmdb, ldmdb, ldmdb, ldmdb,
    ldmdb, ldmdb, ldmdb, ldmdb,
    ldmdb, ldmdb, ldmdb, ldmdb,
    // 0x920
    stmdbw, stmdbw, stmdbw, stmdbw,
    stmdbw, stmdbw, stmdbw, stmdbw,
    stmdbw, stmdbw, stmdbw, stmdbw,
    stmdbw, stmdbw, stmdbw, stmdbw,
    // 0x930
    ldmdbw, ldmdbw, ldmdbw, ldmdbw,
    ldmdbw, ldmdbw, ldmdbw, ldmdbw,
    ldmdbw, ldmdbw, ldmdbw, ldmdbw,
    ldmdbw, ldmdbw, ldmdbw, ldmdbw,
    // 0x940
    stmsdb, stmsdb, stmsdb, stmsdb,
    stmsdb, stmsdb, stmsdb, stmsdb,
    stmsdb, stmsdb, stmsdb, stmsdb,
    stmsdb, stmsdb, stmsdb, stmsdb,
    // 0x950
    ldmsdb, ldmsdb, ldmsdb, ldmsdb,
    ldmsdb, ldmsdb, ldmsdb, ldmsdb,
    ldmsdb, ldmsdb, ldmsdb, ldmsdb,
    ldmsdb, ldmsdb, ldmsdb, ldmsdb,
    // 0x960
    stmsdbw, stmsdbw, stmsdbw, stmsdbw,
    stmsdbw, stmsdbw, stmsdbw, stmsdbw,
    stmsdbw, stmsdbw, stmsdbw, stmsdbw,
    stmsdbw, stmsdbw, stmsdbw, stmsdbw,
    // 0x970
    ldmsdbw, ldmsdbw, ldmsdbw, ldmsdbw,
    ldmsdbw, ldmsdbw, ldmsdbw, ldmsdbw,
    ldmsdbw, ldmsdbw, ldmsdbw, ldmsdbw,
    ldmsdbw, ldmsdbw, ldmsdbw, ldmsdbw,
    // 0x980
    stmib, stmib, stmib, stmib,
    stmib, stmib, stmib, stmib,
    stmib, stmib, stmib, stmib,
    stmib, stmib, stmib, stmib,
    // 0x990
    ldmib, ldmib, ldmib, ldmib,
    ldmib, ldmib, ldmib, ldmib,
    ldmib, ldmib, ldmib, ldmib,
    ldmib, ldmib, ldmib, ldmib,
    // 0x9a0
    stmibw, stmibw, stmibw, stmibw,
    stmibw, stmibw, stmibw, stmibw,
    stmibw, stmibw, stmibw, stmibw,
    stmibw, stmibw, stmibw, stmibw,
    // 0x9b0
    ldmibw, ldmibw, ldmibw, ldmibw,
    ldmibw, ldmibw, ldmibw, ldmibw,
    ldmibw, ldmibw, ldmibw, ldmibw,
    ldmibw, ldmibw, ldmibw, ldmibw,
    // 0x9c0
    stmsib, stmsib, stmsib, stmsib,
    stmsib, stmsib, stmsib, stmsib,
    stmsib, stmsib, stmsib, stmsib,
    stmsib, stmsib, stmsib, stmsib,
    // 0x9d0
    ldmsib, ldmsib, ldmsib, ldmsib,
    ldmsib, ldmsib, ldmsib, ldmsib,
    ldmsib, ldmsib, ldmsib, ldmsib,
    ldmsib, ldmsib, ldmsib, ldmsib,
    // 0x9e0
    stmsibw, stmsibw, stmsibw, stmsibw,
    stmsibw, stmsibw, stmsibw, stmsibw,
    stmsibw, stmsibw, stmsibw, stmsibw,
    stmsibw, stmsibw, stmsibw, stmsibw,
    // 0x9f0
    ldmsibw, ldmsibw, ldmsibw, ldmsibw,
    ldmsibw, ldmsibw, ldmsibw, ldmsibw,
    ldmsibw, ldmsibw, ldmsibw, ldmsibw,
    ldmsibw, ldmsibw, ldmsibw, ldmsibw,
    // 0xa00
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    // 0xa10
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    // 0xa20
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    // 0xa30
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    // 0xa40
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    // 0xa50
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    // 0xa60
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    // 0xa70
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    // 0xa80
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    // 0xa90
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    // 0xaa0
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    // 0xab0
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    // 0xac0
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    // 0xad0
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    // 0xae0
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    // 0xaf0
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    b, b, b, b,
    // 0xb00
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    // 0xb10
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    // 0xb20
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    // 0xb30
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    // 0xb40
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    // 0xb50
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    // 0xb60
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    // 0xb70
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    // 0xb80
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    // 0xb90
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    // 0xba0
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    // 0xbb0
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    // 0xbc0
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    // 0xbd0
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    // 0xbe0
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    // 0xbf0
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    bl, bl, bl, bl,
    // 0xc00
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    // 0xc10
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    // 0xc20
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    // 0xc30
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    // 0xc40
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    // 0xc50
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    // 0xc60
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    // 0xc70
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    // 0xc80
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    // 0xc90
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    // 0xca0
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    // 0xcb0
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    // 0xcc0
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    // 0xcd0
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    // 0xce0
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    // 0xcf0
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    // 0xd00
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    // 0xd10
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    // 0xd20
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    // 0xd30
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    // 0xd40
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    // 0xd50
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    // 0xd60
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    // 0xd70
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    // 0xd80
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    // 0xd90
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    // 0xda0
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    // 0xdb0
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    // 0xdc0
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    // 0xdd0
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    // 0xde0
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    stc, stc, stc, stc,
    // 0xdf0
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    ldc, ldc, ldc, ldc,
    // 0xe00
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    // 0xe10
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    // 0xe20
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    // 0xe30
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    // 0xe40
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    // 0xe50
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    // 0xe60
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    // 0xe70
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    // 0xe80
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    // 0xe90
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    // 0xea0
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    // 0xeb0
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    // 0xec0
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    // 0xed0
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    // 0xee0
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    cdp, mcr, cdp, mcr,
    // 0xef0
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    cdp, mrc, cdp, mrc,
    // 0xf00
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    // 0xf10
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    // 0xf20
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    // 0xf30
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    // 0xf40
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    // 0xf50
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    // 0xf60
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    // 0xf70
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    // 0xf80
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    // 0xf90
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    // 0xfa0
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    // 0xfb0
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    // 0xfc0
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    // 0xfd0
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    // 0xfe0
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    // 0xff0
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
    swi, swi, swi, swi,
];
