// Copyright (c) 2013-2014 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/arm/decoder.c, mgba/src/arm/decoder-arm.c,
// mgba/src/arm/decoder-thumb.c, mgba/include/mgba/internal/arm/decoder.h and
// mgba/include/mgba/internal/arm/decoder-inlines.h.
//
// The decoder table indices are the same as the interpreter's
// (arm_decode_arm: ((opcode >> 16) & 0xFF0) | ((opcode >> 4) & 0x00F);
// arm_decode_thumb: opcode >> 6) and the per-slot decode behavior is the code
// in the C DEFINE_*_DECODER_* macros. In C the decode fn for each slot is a
// distinct preprocessor-generated function and emitter-arm.h/emitter-thumb.h
// build a static fn-pointer table; here the decode fns are parameterized
// helpers (one helper per C macro family) and the table expansion is a
// row-by-row `match` mirroring DECLARE_ARM_EMITTER_BLOCK /
// DECLARE_THUMB_EMITTER_BLOCK (same rows as isa_arm.rs / isa_thumb.rs).
//
// Differences from C, all intentional:
// - union ARMOperand becomes a plain struct with all members (the C never
//   reads a member through a different union arm than it was written with).
// - ARMDisassemble in C takes (cpu, symbols); symbols are used only to label
//   branch/pc-relative targets and cpu only to peek memory for "ldr rd,
//   [pc, #imm]" value comments. This port takes a symbol-reverse-lookup
//   callback instead and behaves like the C cpu == NULL case (no "=<value>"
//   annotation; the effective address is printed inside the brackets).
// - ARMResolveMemoryAccess in C reads regs->cpsr.c for the RRX shift; this
//   port's regs argument is only [u32; 16] so RRX shifts resolve with C=0
//   (only reachable for pathological `ldr pc, [rn, rm, rrx]`).

use super::ExecutionMode;

// ----- operand format bits (mgba decoder.h) -----

pub const ARM_OPERAND_NONE: u32 = 0x00000000;
pub const ARM_OPERAND_REGISTER_1: u32 = 0x00000001;
pub const ARM_OPERAND_IMMEDIATE_1: u32 = 0x00000002;
pub const ARM_OPERAND_MEMORY_1: u32 = 0x00000004;
pub const ARM_OPERAND_AFFECTED_1: u32 = 0x00000008;
pub const ARM_OPERAND_SHIFT_REGISTER_1: u32 = 0x00000010;
pub const ARM_OPERAND_SHIFT_IMMEDIATE_1: u32 = 0x00000020;
pub const ARM_OPERAND_1: u32 = 0x000000FF;

pub const ARM_OPERAND_REGISTER_2: u32 = 0x00000100;
pub const ARM_OPERAND_IMMEDIATE_2: u32 = 0x00000200;
pub const ARM_OPERAND_MEMORY_2: u32 = 0x00000400;
pub const ARM_OPERAND_AFFECTED_2: u32 = 0x00000800;
pub const ARM_OPERAND_SHIFT_REGISTER_2: u32 = 0x00001000;
pub const ARM_OPERAND_SHIFT_IMMEDIATE_2: u32 = 0x00002000;
pub const ARM_OPERAND_2: u32 = 0x0000FF00;

pub const ARM_OPERAND_REGISTER_3: u32 = 0x00010000;
pub const ARM_OPERAND_IMMEDIATE_3: u32 = 0x00020000;
pub const ARM_OPERAND_MEMORY_3: u32 = 0x00040000;
pub const ARM_OPERAND_AFFECTED_3: u32 = 0x00080000;
pub const ARM_OPERAND_SHIFT_REGISTER_3: u32 = 0x00100000;
pub const ARM_OPERAND_SHIFT_IMMEDIATE_3: u32 = 0x00200000;
pub const ARM_OPERAND_3: u32 = 0x00FF0000;

pub const ARM_OPERAND_REGISTER_4: u32 = 0x01000000;
pub const ARM_OPERAND_IMMEDIATE_4: u32 = 0x02000000;
pub const ARM_OPERAND_MEMORY_4: u32 = 0x04000000;
pub const ARM_OPERAND_AFFECTED_4: u32 = 0x08000000;
pub const ARM_OPERAND_SHIFT_REGISTER_4: u32 = 0x10000000;
pub const ARM_OPERAND_SHIFT_IMMEDIATE_4: u32 = 0x20000000;
pub const ARM_OPERAND_4: u32 = 0xFF000000;

pub const ARM_OPERAND_MEMORY: u32 = ARM_OPERAND_MEMORY_1
    | ARM_OPERAND_MEMORY_2
    | ARM_OPERAND_MEMORY_3
    | ARM_OPERAND_MEMORY_4;

// ----- memory access format bits (mgba decoder.h) -----

pub const ARM_MEMORY_REGISTER_BASE: u16 = 0x0001;
pub const ARM_MEMORY_IMMEDIATE_OFFSET: u16 = 0x0002;
pub const ARM_MEMORY_REGISTER_OFFSET: u16 = 0x0004;
pub const ARM_MEMORY_SHIFTED_OFFSET: u16 = 0x0008;
pub const ARM_MEMORY_PRE_INCREMENT: u16 = 0x0010;
pub const ARM_MEMORY_POST_INCREMENT: u16 = 0x0020;
pub const ARM_MEMORY_OFFSET_SUBTRACT: u16 = 0x0040;
pub const ARM_MEMORY_WRITEBACK: u16 = 0x0080;
pub const ARM_MEMORY_DECREMENT_AFTER: u16 = 0x0000;
pub const ARM_MEMORY_INCREMENT_AFTER: u16 = 0x0100;
pub const ARM_MEMORY_DECREMENT_BEFORE: u16 = 0x0200;
pub const ARM_MEMORY_INCREMENT_BEFORE: u16 = 0x0300;
pub const ARM_MEMORY_SPSR_SWAP: u16 = 0x0400;
pub const ARM_MEMORY_STORE: u16 = 0x1000;
pub const ARM_MEMORY_LOAD: u16 = 0x2000;
pub const ARM_MEMORY_SWAP: u16 = 0x3000;

pub const ARM_PSR_C: u8 = 1;
pub const ARM_PSR_X: u8 = 2;
pub const ARM_PSR_S: u8 = 4;
pub const ARM_PSR_F: u8 = 8;
pub const ARM_PSR_MASK: u8 = 0xF;

/// C: enum { ARM_CPSR = 16, ARM_SPSR = 17 } — pseudo-register ids used in operands.
pub const ARM_CPSR: u8 = 16;
pub const ARM_SPSR: u8 = 17;

/// C enum ARMMemoryAccessType (values double as indices into the disassembler's
/// access-type string table).
pub const ARM_ACCESS_WORD: u8 = 4;
pub const ARM_ACCESS_HALFWORD: u8 = 2;
pub const ARM_ACCESS_SIGNED_HALFWORD: u8 = 10;
pub const ARM_ACCESS_BYTE: u8 = 1;
pub const ARM_ACCESS_SIGNED_BYTE: u8 = 9;
pub const ARM_ACCESS_TRANSLATED_WORD: u8 = 20;
pub const ARM_ACCESS_TRANSLATED_BYTE: u8 = 17;

/// C enum ARMBranchType (info.branch_type).
pub const ARM_BRANCH_NONE: u32 = 0;
pub const ARM_BRANCH: u32 = 1;
pub const ARM_BRANCH_INDIRECT: u32 = 2;
pub const ARM_BRANCH_LINKED: u32 = 4;

// u8 register ids mirroring arm.h's ARM_SP/ARM_LR/ARM_PC (which are usize there).
const REG_SP: u8 = 13;
const REG_LR: u8 = 14;
const REG_PC: u8 = 15;

/// C enum ARMCondition; discriminants are the architectural condition codes.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Debug)]
#[repr(u8)]
pub enum ARMCondition {
    Eq = 0x0,
    Ne = 0x1,
    Cs = 0x2,
    Cc = 0x3,
    Mi = 0x4,
    Pl = 0x5,
    Vs = 0x6,
    Vc = 0x7,
    Hi = 0x8,
    Ls = 0x9,
    Ge = 0xA,
    Lt = 0xB,
    Gt = 0xC,
    Le = 0xD,
    Al = 0xE,
    Nv = 0xF,
}

impl ARMCondition {
    pub fn from_u8(v: u8) -> ARMCondition {
        match v & 0xF {
            0x0 => ARMCondition::Eq,
            0x1 => ARMCondition::Ne,
            0x2 => ARMCondition::Cs,
            0x3 => ARMCondition::Cc,
            0x4 => ARMCondition::Mi,
            0x5 => ARMCondition::Pl,
            0x6 => ARMCondition::Vs,
            0x7 => ARMCondition::Vc,
            0x8 => ARMCondition::Hi,
            0x9 => ARMCondition::Ls,
            0xA => ARMCondition::Ge,
            0xB => ARMCondition::Lt,
            0xC => ARMCondition::Gt,
            0xD => ARMCondition::Le,
            0xE => ARMCondition::Al,
            _ => ARMCondition::Nv,
        }
    }
}

/// C enum ARMShifterOperation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ARMShifterOperation {
    None = 0,
    Lsl = 1,
    Lsr = 2,
    Asr = 3,
    Ror = 4,
    Rrx = 5,
}

/// C enum ARMMnemonic; same order and discriminants as mgba (ARM_MN_ILL = 0 ..
/// ARM_MN_UMULL). Debugger code compares these (e.g. `mnemonic == ARM_MN_BL`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ARMMnemonic {
    Ill = 0,
    Adc,
    Add,
    And,
    Asr,
    B,
    Bic,
    Bkpt,
    Bl,
    Bx,
    Cmn,
    Cmp,
    Eor,
    Ldm,
    Ldr,
    Lsl,
    Lsr,
    Mla,
    Mov,
    Mrs,
    Msr,
    Mul,
    Mvn,
    Neg,
    Orr,
    Ror,
    Rsb,
    Rsc,
    Sbc,
    Smlal,
    Smull,
    Stm,
    Str,
    Sub,
    Swi,
    Swp,
    Teq,
    Tst,
    Umlal,
    Umull,
}

/// C union ARMOperand. In C the immediate overlaps the register/shifter view;
/// decoded consumers only read the form the decoder wrote, so a struct is exact.
#[derive(Clone, Copy, Default, Debug)]
pub struct ARMOperand {
    pub reg: u8,
    pub shifter_op: ARMShifterOperation,
    pub shifter_reg: u8,
    pub shifter_imm: u8,
    pub psr_bits: u8,
    pub immediate: i32,
}

/// C struct ARMMemoryAccess.
#[derive(Clone, Copy, Default, Debug)]
pub struct ARMMemoryAccess {
    pub base_reg: u8,
    pub width: u8,
    pub format: u16,
    pub offset: ARMOperand,
}

/// C struct ARMInstructionInfo. The C bitfield block is spelled out as plain
/// fields; cycle-count field widths from the C bitfields are observably
/// irrelevant to decode/disassembly and stay u32 here.
#[derive(Clone, Copy, Debug)]
pub struct ARMInstructionInfo {
    pub opcode: u32,
    pub op1: ARMOperand,
    pub op2: ARMOperand,
    pub op3: ARMOperand,
    pub op4: ARMOperand,
    pub memory: ARMMemoryAccess,
    pub operand_format: u32,
    pub exec_mode: ExecutionMode,
    pub traps: bool,
    pub affects_cpsr: bool,
    pub branch_type: u32,
    pub condition: ARMCondition,
    pub mnemonic: ARMMnemonic,
    pub i_cycles: u32,
    pub c_cycles: u32,
    pub s_instruction_cycles: u32,
    pub n_instruction_cycles: u32,
    pub s_data_cycles: u32,
    pub n_data_cycles: u32,
}

impl Default for ARMInstructionInfo {
    /// Equivalent of the memset(info, 0, ...) in ARMDecodeARM/ARMDecodeThumb
    /// (execMode 0 = ARM, condition 0 = EQ, mnemonic 0 = ILL, ...).
    fn default() -> Self {
        ARMInstructionInfo {
            opcode: 0,
            op1: ARMOperand::default(),
            op2: ARMOperand::default(),
            op3: ARMOperand::default(),
            op4: ARMOperand::default(),
            memory: ARMMemoryAccess::default(),
            operand_format: 0,
            exec_mode: ExecutionMode::Arm,
            traps: false,
            affects_cpsr: false,
            branch_type: ARM_BRANCH_NONE,
            condition: ARMCondition::Eq,
            mnemonic: ARMMnemonic::Ill,
            i_cycles: 0,
            c_cycles: 0,
            s_instruction_cycles: 0,
            n_instruction_cycles: 0,
            s_data_cycles: 0,
            n_data_cycles: 0,
        }
    }
}

impl Default for ARMShifterOperation {
    fn default() -> Self {
        ARMShifterOperation::None
    }
}

// ----- decoder-inlines.h cycle macros -----

/// LOAD_CYCLES
#[inline]
fn load_cycles(info: &mut ARMInstructionInfo) {
    info.i_cycles = 1;
    info.n_data_cycles = 1;
}

/// STORE_CYCLES
#[inline]
fn store_cycles(info: &mut ARMInstructionInfo) {
    info.s_instruction_cycles = 0;
    info.n_instruction_cycles = 1;
    info.n_data_cycles = 1;
}

/// ADDR_MODE_1_* operand-3 addressing flavors for the ALU decoders.
#[derive(Clone, Copy)]
enum AddrMode1 {
    Lsl,
    Lsr,
    Asr,
    Ror,
    Imm,
}

/// ADDR_MODE_2_* (Lsl..Ror, Imm) and ADDR_MODE_3_* (Reg3, Imm3) for load/store.
#[derive(Clone, Copy)]
enum LsAddr {
    Lsl,
    Lsr,
    Asr,
    Ror,
    Imm,
    Reg3,
    Imm3,
}

// ADDRESSING_MODE composites as they appear in DEFINE_LOAD_STORE_DECODER_SET_ARM's
// P/U/W variants (matching the C names ""/U/P/PW/PU/PUW).
const FMT_POST_WB_SUB: u16 = ARM_MEMORY_POST_INCREMENT | ARM_MEMORY_WRITEBACK | ARM_MEMORY_OFFSET_SUBTRACT;
const FMT_POST_WB: u16 = ARM_MEMORY_POST_INCREMENT | ARM_MEMORY_WRITEBACK;
const FMT_SUB: u16 = ARM_MEMORY_OFFSET_SUBTRACT;
const FMT_PRE_WB_SUB: u16 = ARM_MEMORY_PRE_INCREMENT | ARM_MEMORY_WRITEBACK | ARM_MEMORY_OFFSET_SUBTRACT;
const FMT_PRE_WB: u16 = ARM_MEMORY_PRE_INCREMENT | ARM_MEMORY_WRITEBACK;
const FMT_NONE: u16 = 0;

// Short names for the LSM address-direction/extra format constants used in the
// ARM dispatch (memory block rows -80..-9F).
const DA: u16 = ARM_MEMORY_DECREMENT_AFTER;
const IA: u16 = ARM_MEMORY_INCREMENT_AFTER;
const DB: u16 = ARM_MEMORY_DECREMENT_BEFORE;
const IB: u16 = ARM_MEMORY_INCREMENT_BEFORE;
const WB: u16 = ARM_MEMORY_WRITEBACK;
const SP: u16 = ARM_MEMORY_SPSR_SWAP;

// ----- ARM decoder families (decoder-arm.c DEFINE_* macros) -----

fn arm_decode_ill(_opcode: u32, info: &mut ARMInstructionInfo) {
    // DEFINE_DECODER_ARM(ILL, ...)
    info.mnemonic = ARMMnemonic::Ill;
    info.operand_format = ARM_OPERAND_NONE;
    info.traps = true; // Illegal opcode
}

fn arm_decode_bkpt(_opcode: u32, info: &mut ARMInstructionInfo) {
    info.mnemonic = ARMMnemonic::Bkpt;
    info.operand_format = ARM_OPERAND_NONE;
    info.traps = true;
}

fn arm_decode_coprocessor(_opcode: u32, info: &mut ARMInstructionInfo) {
    // DEFINE_DECODER_ARM(CDP/LDC/STC/MCR/MRC, ILL, ...) — the GBA has no
    // coprocessor; every coprocessor opcode decodes as ILL with no trap flag.
    info.mnemonic = ARMMnemonic::Ill;
    info.operand_format = ARM_OPERAND_NONE;
}

/// DEFINE_ALU_DECODER_EX_ARM: (S, SHIFTER, OTHER_AFFECTED, SKIPPED) become
/// (s, addressing, other_affected, skipped).
fn arm_decode_alu(
    opcode: u32,
    info: &mut ARMInstructionInfo,
    mnemonic: ARMMnemonic,
    s: bool,
    addressing: AddrMode1,
    other_affected: bool,
    skipped: u8,
) {
    info.mnemonic = mnemonic;
    info.op1.reg = ((opcode >> 12) & 0xF) as u8;
    info.op2.reg = ((opcode >> 16) & 0xF) as u8;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | if other_affected { ARM_OPERAND_AFFECTED_1 } else { 0 }
        | ARM_OPERAND_REGISTER_2;
    info.affects_cpsr = s;
    match addressing {
        AddrMode1::Lsl | AddrMode1::Lsr | AddrMode1::Asr | AddrMode1::Ror => {
            // ADDR_MODE_1_SHIFT(OP)
            info.op3.reg = (opcode & 0x0000000F) as u8;
            info.op3.shifter_op = match addressing {
                AddrMode1::Lsl => ARMShifterOperation::Lsl,
                AddrMode1::Lsr => ARMShifterOperation::Lsr,
                AddrMode1::Asr => ARMShifterOperation::Asr,
                AddrMode1::Ror => ARMShifterOperation::Ror,
                AddrMode1::Imm => unreachable!(),
            };
            info.operand_format |= ARM_OPERAND_REGISTER_3;
            if opcode & 0x00000010 != 0 {
                info.op3.shifter_reg = ((opcode >> 8) & 0xF) as u8;
                info.i_cycles += 1;
                info.operand_format |= ARM_OPERAND_SHIFT_REGISTER_3;
            } else {
                info.op3.shifter_imm = ((opcode >> 7) & 0x1F) as u8;
                if info.op3.shifter_imm == 0
                    && matches!(addressing, AddrMode1::Lsr | AddrMode1::Asr)
                {
                    info.op3.shifter_imm = 32;
                }
                info.operand_format |= ARM_OPERAND_SHIFT_IMMEDIATE_3;
            }
            // ADDR_MODE_1_LSL fixup: lsl #0 means "no shift".
            if matches!(addressing, AddrMode1::Lsl)
                && (info.operand_format & ARM_OPERAND_SHIFT_IMMEDIATE_3) != 0
                && info.op3.shifter_imm == 0
            {
                info.operand_format &= !ARM_OPERAND_SHIFT_IMMEDIATE_3;
                info.op3.shifter_op = ARMShifterOperation::None;
            }
            // ADDR_MODE_1_ROR fixup: ror #0 means rrx.
            if matches!(addressing, AddrMode1::Ror)
                && (info.operand_format & ARM_OPERAND_SHIFT_IMMEDIATE_3) != 0
                && info.op3.shifter_imm == 0
            {
                info.op3.shifter_op = ARMShifterOperation::Rrx;
            }
        }
        AddrMode1::Imm => {
            // ADDR_MODE_1_IMM
            let rotate = (opcode & 0x00000F00) >> 7;
            let immediate = opcode & 0x000000FF;
            info.op3.immediate = immediate.rotate_right(rotate) as i32;
            info.operand_format |= ARM_OPERAND_IMMEDIATE_3;
        }
    }
    if skipped == 1 {
        info.op1 = info.op2;
        info.op2 = info.op3;
        info.operand_format >>= 8;
    } else if skipped == 2 {
        info.op2 = info.op3;
        info.operand_format |= info.operand_format >> 8;
        info.operand_format &= !ARM_OPERAND_3;
    }
    if info.op1.reg == REG_PC && other_affected {
        info.branch_type = ARM_BRANCH_INDIRECT;
    }
}

/// DEFINE_MULTIPLY_DECODER_EX_ARM (MLA has accumulate == true).
fn arm_decode_mul(
    opcode: u32,
    info: &mut ARMInstructionInfo,
    mnemonic: ARMMnemonic,
    s: bool,
    accumulate: bool,
) {
    info.mnemonic = mnemonic;
    info.op1.reg = ((opcode >> 16) & 0xF) as u8;
    info.op2.reg = (opcode & 0xF) as u8;
    info.op3.reg = ((opcode >> 8) & 0xF) as u8;
    info.op4.reg = ((opcode >> 12) & 0xF) as u8;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | ARM_OPERAND_AFFECTED_1
        | ARM_OPERAND_REGISTER_2
        | ARM_OPERAND_REGISTER_3
        | if accumulate { ARM_OPERAND_REGISTER_4 } else { 0 };
    info.affects_cpsr = s;
    if info.op1.reg == REG_PC {
        info.branch_type = ARM_BRANCH_INDIRECT;
    }
}

/// DEFINE_LONG_MULTIPLY_DECODER_EX_ARM.
fn arm_decode_mull(opcode: u32, info: &mut ARMInstructionInfo, mnemonic: ARMMnemonic, s: bool) {
    info.mnemonic = mnemonic;
    info.op1.reg = ((opcode >> 12) & 0xF) as u8;
    info.op2.reg = ((opcode >> 16) & 0xF) as u8;
    info.op3.reg = (opcode & 0xF) as u8;
    info.op4.reg = ((opcode >> 8) & 0xF) as u8;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | ARM_OPERAND_AFFECTED_1
        | ARM_OPERAND_REGISTER_2
        | ARM_OPERAND_AFFECTED_2
        | ARM_OPERAND_REGISTER_3
        | ARM_OPERAND_REGISTER_4;
    info.affects_cpsr = s;
    if info.op1.reg == REG_PC {
        info.branch_type = ARM_BRANCH_INDIRECT;
    }
}

/// ADDRESSING_DECODING for load/store decoders (ADDR_MODE_2_*, ADDR_MODE_3_*).
fn arm_decode_ls_addr(opcode: u32, info: &mut ARMInstructionInfo, addr: LsAddr) {
    match addr {
        LsAddr::Lsl | LsAddr::Lsr | LsAddr::Asr | LsAddr::Ror => {
            // ADDR_MODE_2_SHIFT(OP)
            info.memory.format |= ARM_MEMORY_REGISTER_OFFSET | ARM_MEMORY_SHIFTED_OFFSET;
            info.memory.offset.shifter_op = match addr {
                LsAddr::Lsl => ARMShifterOperation::Lsl,
                LsAddr::Lsr => ARMShifterOperation::Lsr,
                LsAddr::Asr => ARMShifterOperation::Asr,
                LsAddr::Ror => ARMShifterOperation::Ror,
                _ => unreachable!(),
            };
            info.memory.offset.shifter_imm = ((opcode >> 7) & 0x1F) as u8;
            info.memory.offset.reg = (opcode & 0x0000000F) as u8;
            match addr {
                LsAddr::Lsl => {
                    if info.memory.offset.shifter_imm == 0 {
                        info.memory.format &= !ARM_MEMORY_SHIFTED_OFFSET;
                        info.memory.offset.shifter_op = ARMShifterOperation::None;
                    }
                }
                LsAddr::Lsr | LsAddr::Asr => {
                    if info.memory.offset.shifter_imm == 0 {
                        info.memory.offset.shifter_imm = 32;
                    }
                }
                LsAddr::Ror => {
                    if info.memory.offset.shifter_imm == 0 {
                        info.memory.offset.shifter_op = ARMShifterOperation::Rrx;
                    }
                }
                _ => unreachable!(),
            }
        }
        LsAddr::Imm => {
            // ADDR_MODE_2_IMM
            info.memory.format |= ARM_MEMORY_IMMEDIATE_OFFSET;
            info.memory.offset.immediate = (opcode & 0x00000FFF) as i32;
        }
        LsAddr::Reg3 => {
            // ADDR_MODE_3_REG
            info.memory.format |= ARM_MEMORY_REGISTER_OFFSET;
            info.memory.offset.reg = (opcode & 0x0000000F) as u8;
        }
        LsAddr::Imm3 => {
            // ADDR_MODE_3_IMM
            info.memory.format |= ARM_MEMORY_IMMEDIATE_OFFSET;
            info.memory.offset.immediate = ((opcode & 0x0000000F) | ((opcode & 0x00000F00) >> 4)) as i32;
        }
    }
}

/// DEFINE_LOAD_STORE_DECODER_EX_ARM. `fmt` is the ADDRESSING_MODE composite
/// (FMT_* consts), `load` selects LDR vs STR semantics (ARM_OPERAND_AFFECTED_1
/// vs _2, LOAD vs STORE format bit, LOAD_CYCLES vs STORE_CYCLES).
fn arm_decode_ls(
    opcode: u32,
    info: &mut ARMInstructionInfo,
    mnemonic: ARMMnemonic,
    fmt: u16,
    addr: LsAddr,
    load: bool,
    width: u8,
) {
    info.mnemonic = mnemonic;
    info.op1.reg = ((opcode >> 12) & 0xF) as u8;
    info.memory.base_reg = ((opcode >> 16) & 0xF) as u8;
    info.memory.width = width;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | if load { ARM_OPERAND_AFFECTED_1 } else { ARM_OPERAND_AFFECTED_2 }
        | ARM_OPERAND_MEMORY_2;
    info.memory.format = ARM_MEMORY_REGISTER_BASE
        | fmt
        | if load { ARM_MEMORY_LOAD } else { ARM_MEMORY_STORE };
    arm_decode_ls_addr(opcode, info, addr);
    if info.op1.reg == REG_PC && load {
        info.branch_type = ARM_BRANCH_INDIRECT;
    }
    if (info.memory.format & (ARM_MEMORY_WRITEBACK | ARM_MEMORY_REGISTER_OFFSET))
        == (ARM_MEMORY_WRITEBACK | ARM_MEMORY_REGISTER_OFFSET)
        && info.memory.offset.reg == REG_PC
    {
        info.branch_type = ARM_BRANCH_INDIRECT;
    }
    if load {
        load_cycles(info);
    } else {
        store_cycles(info);
    }
}

/// DEFINE_LOAD_STORE_MULTIPLE_DECODER_EX_ARM. `direction` is one of
/// ARM_MEMORY_*_{AFTER,BEFORE} and `fmt` is {LOAD,STORE} [| WRITEBACK | SPSR_SWAP].
fn arm_decode_lsm(
    opcode: u32,
    info: &mut ARMInstructionInfo,
    mnemonic: ARMMnemonic,
    direction: u16,
    fmt: u16,
) {
    info.mnemonic = mnemonic;
    info.memory.base_reg = ((opcode >> 16) & 0xF) as u8;
    info.op1.immediate = (opcode & 0x0000FFFF) as i32;
    if info.op1.immediate & (1 << 15) != 0 {
        info.branch_type = ARM_BRANCH_INDIRECT;
    }
    info.operand_format = ARM_OPERAND_MEMORY_1;
    info.memory.format = ARM_MEMORY_REGISTER_BASE | fmt | direction;
}

/// DEFINE_SWP_DECODER_ARM.
fn arm_decode_swp(opcode: u32, info: &mut ARMInstructionInfo, width: u8) {
    info.mnemonic = ARMMnemonic::Swp;
    info.memory.base_reg = ((opcode >> 16) & 0xF) as u8;
    info.op1.reg = ((opcode >> 12) & 0xF) as u8;
    info.op2.reg = (opcode & 0xF) as u8;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | ARM_OPERAND_AFFECTED_1
        | ARM_OPERAND_REGISTER_2
        | ARM_OPERAND_MEMORY_3
        | ARM_OPERAND_AFFECTED_3;
    info.memory.format = ARM_MEMORY_REGISTER_BASE | ARM_MEMORY_SWAP;
    info.memory.width = width;
}

// ----- branch / miscellaneous ARM decoders -----

/// DEFINE_DECODER_ARM(B, B, ...)
fn arm_decode_b(opcode: u32, info: &mut ARMInstructionInfo) {
    info.mnemonic = ARMMnemonic::B;
    let offset = (opcode << 8) as i32;
    info.op1.immediate = offset >> 6;
    info.operand_format = ARM_OPERAND_IMMEDIATE_1;
    info.branch_type = ARM_BRANCH;
}

/// DEFINE_DECODER_ARM(BL, BL, ...)
fn arm_decode_bl(opcode: u32, info: &mut ARMInstructionInfo) {
    info.mnemonic = ARMMnemonic::Bl;
    let offset = (opcode << 8) as i32;
    info.op1.immediate = offset >> 6;
    info.operand_format = ARM_OPERAND_IMMEDIATE_1;
    info.branch_type = ARM_BRANCH_LINKED;
}

/// DEFINE_DECODER_ARM(BX, BX, ...)
fn arm_decode_bx(opcode: u32, info: &mut ARMInstructionInfo) {
    info.mnemonic = ARMMnemonic::Bx;
    info.op1.reg = (opcode & 0x0000000F) as u8;
    info.operand_format = ARM_OPERAND_REGISTER_1;
    info.branch_type = ARM_BRANCH_INDIRECT;
}

/// DEFINE_DECODER_ARM(MSR/MSRR, MSR, ...): register source. to_spsr == false is
/// MSR (targets CPSR, sets affectsCPSR); true is MSRR (targets SPSR).
fn arm_decode_msr_reg(opcode: u32, info: &mut ARMInstructionInfo, to_spsr: bool) {
    info.mnemonic = ARMMnemonic::Msr;
    if !to_spsr {
        info.affects_cpsr = true;
    }
    info.op1.reg = if to_spsr { ARM_SPSR } else { ARM_CPSR };
    info.op1.psr_bits = ((opcode >> 16) & ARM_PSR_MASK as u32) as u8;
    info.op2.reg = (opcode & 0x0000000F) as u8;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | ARM_OPERAND_AFFECTED_1
        | ARM_OPERAND_REGISTER_2;
}

/// DEFINE_DECODER_ARM(MSRI/MSRRI, MSR, ...): immediate source.
fn arm_decode_msr_imm(opcode: u32, info: &mut ARMInstructionInfo, to_spsr: bool) {
    let rotate = (opcode & 0x00000F00) >> 7;
    let operand = (opcode & 0x000000FF).rotate_right(rotate) as i32;
    info.mnemonic = ARMMnemonic::Msr;
    if !to_spsr {
        info.affects_cpsr = true;
    }
    info.op1.reg = if to_spsr { ARM_SPSR } else { ARM_CPSR };
    info.op1.psr_bits = ((opcode >> 16) & ARM_PSR_MASK as u32) as u8;
    info.op2.immediate = operand;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | ARM_OPERAND_AFFECTED_1
        | ARM_OPERAND_IMMEDIATE_2;
}

/// DEFINE_DECODER_ARM(MRS/MRSR, MRS, ...).
fn arm_decode_mrs(opcode: u32, info: &mut ARMInstructionInfo, from_spsr: bool) {
    info.mnemonic = ARMMnemonic::Mrs;
    if !from_spsr {
        info.affects_cpsr = true;
    }
    info.op1.reg = ((opcode >> 12) & 0xF) as u8;
    info.op2.reg = if from_spsr { ARM_SPSR } else { ARM_CPSR };
    info.op2.psr_bits = 0;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | ARM_OPERAND_AFFECTED_1
        | ARM_OPERAND_REGISTER_2;
}

/// DEFINE_DECODER_ARM(SWI, SWI, ...)
fn arm_decode_swi(opcode: u32, info: &mut ARMInstructionInfo) {
    info.mnemonic = ARMMnemonic::Swi;
    info.op1.immediate = (opcode & 0xFFFFFF) as i32;
    info.operand_format = ARM_OPERAND_IMMEDIATE_1;
    info.traps = true;
}

// ----- ARM dispatch (DECLARE_ARM_EMITTER_BLOCK with EMITTER = _ARMDecode) -----

/// Extra slots of DECLARE_ARM_ALU_BLOCK (index bits 9, 11, 13, 15).
#[derive(Clone, Copy)]
enum ExSlot {
    /// _ARMDecodeILL
    Ill,
    /// _ARMDecodeMUL/MLA/S variants (DEFINE_MULTIPLY_DECODER_*).
    Mul {
        mnemonic: ARMMnemonic,
        s: bool,
        accumulate: bool,
    },
    /// _ARMDecodeUMULL/UMLAL/SMULL/SMLAL/S variants.
    Mull { mnemonic: ARMMnemonic, s: bool },
    /// _ARMDecodeSTRH/LDRH/LDRSB/LDRSH family variants.
    Ls {
        mnemonic: ARMMnemonic,
        fmt: u16,
        addr: LsAddr,
        load: bool,
        width: u8,
    },
}

fn arm_decode_ex(ex: ExSlot, opcode: u32, info: &mut ARMInstructionInfo) {
    match ex {
        ExSlot::Ill => arm_decode_ill(opcode, info),
        ExSlot::Mul { mnemonic, s, accumulate } => arm_decode_mul(opcode, info, mnemonic, s, accumulate),
        ExSlot::Mull { mnemonic, s } => arm_decode_mull(opcode, info, mnemonic, s),
        ExSlot::Ls { mnemonic, fmt, addr, load, width } => {
            arm_decode_ls(opcode, info, mnemonic, fmt, addr, load, width)
        }
    }
}

/// DECLARE_ARM_ALU_BLOCK(EMITTER, ALU, EX1, EX2, EX3, EX4).
#[allow(clippy::too_many_arguments)]
fn arm_alu_row(
    opcode: u32,
    info: &mut ARMInstructionInfo,
    mnemonic: ARMMnemonic,
    s: bool,
    other_affected: bool,
    skipped: u8,
    ex1: ExSlot,
    ex2: ExSlot,
    ex3: ExSlot,
    ex4: ExSlot,
    low: usize,
) {
    match low {
        0x0 | 0x1 | 0x8 => arm_decode_alu(opcode, info, mnemonic, s, AddrMode1::Lsl, other_affected, skipped),
        0x2 | 0x3 | 0xA => arm_decode_alu(opcode, info, mnemonic, s, AddrMode1::Lsr, other_affected, skipped),
        0x4 | 0x5 | 0xC => arm_decode_alu(opcode, info, mnemonic, s, AddrMode1::Asr, other_affected, skipped),
        0x6 | 0x7 | 0xE => arm_decode_alu(opcode, info, mnemonic, s, AddrMode1::Ror, other_affected, skipped),
        0x9 => arm_decode_ex(ex1, opcode, info),
        0xB => arm_decode_ex(ex2, opcode, info),
        0xD => arm_decode_ex(ex3, opcode, info),
        _ => arm_decode_ex(ex4, opcode, info),
    }
}

/// DECLARE_ARM_LOAD_STORE_BLOCK(EMITTER, NAME, P, U, W) — register-offset rows,
/// ILL in every odd slot.
fn arm_ls_reg_row(
    opcode: u32,
    info: &mut ARMInstructionInfo,
    mnemonic: ARMMnemonic,
    load: bool,
    width: u8,
    fmt: u16,
    low: usize,
) {
    if low & 1 != 0 {
        arm_decode_ill(opcode, info);
        return;
    }
    let addr = match (low >> 1) & 3 {
        0 => LsAddr::Lsl,
        1 => LsAddr::Lsr,
        2 => LsAddr::Asr,
        _ => LsAddr::Ror,
    };
    arm_decode_ls(opcode, info, mnemonic, fmt, addr, load, width);
}
/// Big `match` mirroring DECLARE_ARM_EMITTER_BLOCK(_ARMDecode) from
/// emitter-arm.h (rows annotated). `index` is as C: bits [11:4] = opcode[27:20],
/// bits [3:0] = opcode[7:4].
fn arm_decode_dispatch(index: usize, opcode: u32, info: &mut ARMInstructionInfo) {
    use ARMMnemonic::*;
    let low = index & 0xF;
    match index >> 4 {
        // -00---X-: DECLARE_ARM_ALU_BLOCK(AND, MUL, STRH, ILL, ILL)
        0x00 => arm_alu_row(opcode, info, And, false, true, 0,
            ExSlot::Mul { mnemonic: Mul, s: false, accumulate: false },
            ExSlot::Ls { mnemonic: Str, fmt: FMT_POST_WB_SUB, addr: LsAddr::Reg3, load: false, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ill, ExSlot::Ill, low),
        // -01---X-: (ANDS, MULS, LDRH, LDRSB, LDRSH)
        0x01 => arm_alu_row(opcode, info, And, true, true, 0,
            ExSlot::Mul { mnemonic: Mul, s: true, accumulate: false },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB_SUB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB_SUB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_SIGNED_BYTE },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB_SUB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_SIGNED_HALFWORD }, low),
        // -02---X-: (EOR, MLA, STRH, ILL, ILL)
        0x02 => arm_alu_row(opcode, info, Eor, false, true, 0,
            ExSlot::Mul { mnemonic: Mla, s: false, accumulate: true },
            ExSlot::Ls { mnemonic: Str, fmt: FMT_POST_WB_SUB, addr: LsAddr::Reg3, load: false, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ill, ExSlot::Ill, low),
        // -03---X-: (EORS, MLAS, LDRH, LDRSB, LDRSH)
        0x03 => arm_alu_row(opcode, info, Eor, true, true, 0,
            ExSlot::Mul { mnemonic: Mla, s: true, accumulate: true },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB_SUB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB_SUB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_SIGNED_BYTE },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB_SUB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_SIGNED_HALFWORD }, low),
        // -04---X-: (SUB, ILL, STRHI, ILL, ILL)
        0x04 => arm_alu_row(opcode, info, Sub, false, true, 0,
            ExSlot::Ill,
            ExSlot::Ls { mnemonic: Str, fmt: FMT_POST_WB_SUB, addr: LsAddr::Imm3, load: false, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ill, ExSlot::Ill, low),
        // -05---X-: (SUBS, ILL, LDRHI, LDRSBI, LDRSHI)
        0x05 => arm_alu_row(opcode, info, Sub, true, true, 0,
            ExSlot::Ill,
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB_SUB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB_SUB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_SIGNED_BYTE },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB_SUB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_SIGNED_HALFWORD }, low),
        // -06---X-: (RSB, ILL, STRHI, ILL, ILL)
        0x06 => arm_alu_row(opcode, info, Rsb, false, true, 0,
            ExSlot::Ill,
            ExSlot::Ls { mnemonic: Str, fmt: FMT_POST_WB_SUB, addr: LsAddr::Imm3, load: false, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ill, ExSlot::Ill, low),
        // -07---X-: (RSBS, ILL, LDRHI, LDRSBI, LDRSHI)
        0x07 => arm_alu_row(opcode, info, Rsb, true, true, 0,
            ExSlot::Ill,
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB_SUB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB_SUB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_SIGNED_BYTE },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB_SUB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_SIGNED_HALFWORD }, low),
        // -08---X-: (ADD, UMULL, STRHU, ILL, ILL)
        0x08 => arm_alu_row(opcode, info, Add, false, true, 0,
            ExSlot::Mull { mnemonic: Umull, s: false },
            ExSlot::Ls { mnemonic: Str, fmt: FMT_POST_WB, addr: LsAddr::Reg3, load: false, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ill, ExSlot::Ill, low),
        // -09---X-: (ADDS, UMULLS, LDRHU, LDRSBU, LDRSHU)
        0x09 => arm_alu_row(opcode, info, Add, true, true, 0,
            ExSlot::Mull { mnemonic: Umull, s: true },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_SIGNED_BYTE },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_SIGNED_HALFWORD }, low),
        // -0A---X-: (ADC, UMLAL, STRHU, ILL, ILL)
        0x0A => arm_alu_row(opcode, info, Adc, false, true, 0,
            ExSlot::Mull { mnemonic: Umlal, s: false },
            ExSlot::Ls { mnemonic: Str, fmt: FMT_POST_WB, addr: LsAddr::Reg3, load: false, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ill, ExSlot::Ill, low),
        // -0B---X-: (ADCS, UMLALS, LDRHU, LDRSBU, LDRSHU)
        0x0B => arm_alu_row(opcode, info, Adc, true, true, 0,
            ExSlot::Mull { mnemonic: Umlal, s: true },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_SIGNED_BYTE },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_SIGNED_HALFWORD }, low),
        // -0C---X-: (SBC, SMULL, STRHIU, ILL, ILL)
        0x0C => arm_alu_row(opcode, info, Sbc, false, true, 0,
            ExSlot::Mull { mnemonic: Smull, s: false },
            ExSlot::Ls { mnemonic: Str, fmt: FMT_POST_WB, addr: LsAddr::Imm3, load: false, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ill, ExSlot::Ill, low),
        // -0D---X-: (SBCS, SMULLS, LDRHIU, LDRSBIU, LDRSHIU)
        0x0D => arm_alu_row(opcode, info, Sbc, true, true, 0,
            ExSlot::Mull { mnemonic: Smull, s: true },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_SIGNED_BYTE },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_SIGNED_HALFWORD }, low),
        // -0E---X-: (RSC, SMLAL, STRHIU, ILL, ILL)
        0x0E => arm_alu_row(opcode, info, Rsc, false, true, 0,
            ExSlot::Mull { mnemonic: Smlal, s: false },
            ExSlot::Ls { mnemonic: Str, fmt: FMT_POST_WB, addr: LsAddr::Imm3, load: false, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ill, ExSlot::Ill, low),
        // -0F---X-: (RSCS, SMLALS, LDRHIU, LDRSBIU, LDRSHIU)
        0x0F => arm_alu_row(opcode, info, Rsc, true, true, 0,
            ExSlot::Mull { mnemonic: Smlal, s: true },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_SIGNED_BYTE },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_POST_WB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_SIGNED_HALFWORD }, low),
        // -10---0-: MRS; -10---9-: SWP; -10---B-: STRHP; rest ILL
        0x10 => match low {
            0x0 => arm_decode_mrs(opcode, info, false),
            0x9 => arm_decode_swp(opcode, info, ARM_ACCESS_WORD),
            0xB => arm_decode_ls(opcode, info, Str, FMT_SUB, LsAddr::Reg3, false, ARM_ACCESS_HALFWORD),
            _ => arm_decode_ill(opcode, info),
        },
        // -11---X-: (TST, ILL, LDRHP, LDRSBP, LDRSHP)
        0x11 => arm_alu_row(opcode, info, Tst, true, false, 1,
            ExSlot::Ill,
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_SUB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_SUB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_SIGNED_BYTE },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_SUB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_SIGNED_HALFWORD }, low),
        // -12---0-: MSR; -12---1-: BX; -12---7-: BKPT; -12---B-: STRHPW; rest ILL
        0x12 => match low {
            0x0 => arm_decode_msr_reg(opcode, info, false),
            0x1 => arm_decode_bx(opcode, info),
            0x7 => arm_decode_bkpt(opcode, info),
            0xB => arm_decode_ls(opcode, info, Str, FMT_PRE_WB_SUB, LsAddr::Reg3, false, ARM_ACCESS_HALFWORD),
            _ => arm_decode_ill(opcode, info),
        },
        // -13---X-: (TEQ, ILL, LDRHPW, LDRSBPW, LDRSHPW)
        0x13 => arm_alu_row(opcode, info, Teq, true, false, 1,
            ExSlot::Ill,
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_PRE_WB_SUB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_PRE_WB_SUB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_SIGNED_BYTE },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_PRE_WB_SUB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_SIGNED_HALFWORD }, low),
        // -14---0-: MRSR; -14---9-: SWPB; -14---B-: STRHIP; rest ILL
        0x14 => match low {
            0x0 => arm_decode_mrs(opcode, info, true),
            0x9 => arm_decode_swp(opcode, info, ARM_ACCESS_BYTE),
            0xB => arm_decode_ls(opcode, info, Str, FMT_SUB, LsAddr::Imm3, false, ARM_ACCESS_HALFWORD),
            _ => arm_decode_ill(opcode, info),
        },
        // -15---X-: (CMP, ILL, LDRHIP, LDRSBIP, LDRSHIP)
        0x15 => arm_alu_row(opcode, info, Cmp, true, false, 1,
            ExSlot::Ill,
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_SUB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_SUB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_SIGNED_BYTE },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_SUB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_SIGNED_HALFWORD }, low),
        // -16---0-: MSRR; -16---B-: STRHIPW; rest ILL
        0x16 => match low {
            0x0 => arm_decode_msr_reg(opcode, info, true),
            0xB => arm_decode_ls(opcode, info, Str, FMT_PRE_WB_SUB, LsAddr::Imm3, false, ARM_ACCESS_HALFWORD),
            _ => arm_decode_ill(opcode, info),
        },
        // -17---X-: (CMN, ILL, LDRHIPW, LDRSBIPW, LDRSHIPW)
        0x17 => arm_alu_row(opcode, info, Cmn, true, false, 1,
            ExSlot::Ill,
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_PRE_WB_SUB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_PRE_WB_SUB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_SIGNED_BYTE },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_PRE_WB_SUB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_SIGNED_HALFWORD }, low),
        // -18---X-: (ORR, ILL, STRHPU, ILL, ILL)
        0x18 => arm_alu_row(opcode, info, Orr, false, true, 0,
            ExSlot::Ill,
            ExSlot::Ls { mnemonic: Str, fmt: FMT_NONE, addr: LsAddr::Reg3, load: false, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ill, ExSlot::Ill, low),
        // -19---X-: (ORRS, ILL, LDRHPU, LDRSBPU, LDRSHPU)
        0x19 => arm_alu_row(opcode, info, Orr, true, true, 0,
            ExSlot::Ill,
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_NONE, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_NONE, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_SIGNED_BYTE },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_NONE, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_SIGNED_HALFWORD }, low),
        // -1A---X-: (MOV, ILL, STRHPUW, ILL, ILL)
        0x1A => arm_alu_row(opcode, info, Mov, false, true, 2,
            ExSlot::Ill,
            ExSlot::Ls { mnemonic: Str, fmt: FMT_PRE_WB, addr: LsAddr::Reg3, load: false, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ill, ExSlot::Ill, low),
        // -1B---X-: (MOVS, ILL, LDRHPUW, LDRSBPUW, LDRSHPUW)
        0x1B => arm_alu_row(opcode, info, Mov, true, true, 2,
            ExSlot::Ill,
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_PRE_WB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_PRE_WB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_SIGNED_BYTE },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_PRE_WB, addr: LsAddr::Reg3, load: true, width: ARM_ACCESS_SIGNED_HALFWORD }, low),
        // -1C---X-: (BIC, ILL, STRHIPU, ILL, ILL)
        0x1C => arm_alu_row(opcode, info, Bic, false, true, 0,
            ExSlot::Ill,
            ExSlot::Ls { mnemonic: Str, fmt: FMT_NONE, addr: LsAddr::Imm3, load: false, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ill, ExSlot::Ill, low),
        // -1D---X-: (BICS, ILL, LDRHIPU, LDRSBIPU, LDRSHIPU)
        0x1D => arm_alu_row(opcode, info, Bic, true, true, 0,
            ExSlot::Ill,
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_NONE, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_NONE, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_SIGNED_BYTE },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_NONE, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_SIGNED_HALFWORD }, low),
        // -1E---X-: (MVN, ILL, STRHIPUW, ILL, ILL)
        0x1E => arm_alu_row(opcode, info, Mvn, false, true, 2,
            ExSlot::Ill,
            ExSlot::Ls { mnemonic: Str, fmt: FMT_PRE_WB, addr: LsAddr::Imm3, load: false, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ill, ExSlot::Ill, low),
        // -1F---X-: (MVNS, ILL, LDRHIPUW, LDRSBIPUW, LDRSHIPUW)
        0x1F => arm_alu_row(opcode, info, Mvn, true, true, 2,
            ExSlot::Ill,
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_PRE_WB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_HALFWORD },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_PRE_WB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_SIGNED_BYTE },
            ExSlot::Ls { mnemonic: Ldr, fmt: FMT_PRE_WB, addr: LsAddr::Imm3, load: true, width: ARM_ACCESS_SIGNED_HALFWORD }, low),
        // DECLARE_ARM_ALU_IMMEDIATE_BLOCK rows (-20..-3F)
        0x20 => arm_decode_alu(opcode, info, And, false, AddrMode1::Imm, true, 0),
        0x21 => arm_decode_alu(opcode, info, And, true, AddrMode1::Imm, true, 0),
        0x22 => arm_decode_alu(opcode, info, Eor, false, AddrMode1::Imm, true, 0),
        0x23 => arm_decode_alu(opcode, info, Eor, true, AddrMode1::Imm, true, 0),
        0x24 => arm_decode_alu(opcode, info, Sub, false, AddrMode1::Imm, true, 0),
        0x25 => arm_decode_alu(opcode, info, Sub, true, AddrMode1::Imm, true, 0),
        0x26 => arm_decode_alu(opcode, info, Rsb, false, AddrMode1::Imm, true, 0),
        0x27 => arm_decode_alu(opcode, info, Rsb, true, AddrMode1::Imm, true, 0),
        0x28 => arm_decode_alu(opcode, info, Add, false, AddrMode1::Imm, true, 0),
        0x29 => arm_decode_alu(opcode, info, Add, true, AddrMode1::Imm, true, 0),
        0x2A => arm_decode_alu(opcode, info, Adc, false, AddrMode1::Imm, true, 0),
        0x2B => arm_decode_alu(opcode, info, Adc, true, AddrMode1::Imm, true, 0),
        0x2C => arm_decode_alu(opcode, info, Sbc, false, AddrMode1::Imm, true, 0),
        0x2D => arm_decode_alu(opcode, info, Sbc, true, AddrMode1::Imm, true, 0),
        0x2E => arm_decode_alu(opcode, info, Rsc, false, AddrMode1::Imm, true, 0),
        0x2F => arm_decode_alu(opcode, info, Rsc, true, AddrMode1::Imm, true, 0),
        // -30/-31: TST (immediate); -32: MSRI; -33: TEQ; -34/-35: CMP;
        // -36: MSRRI; -37: CMN; -38..-3F: ORR..MVN (immediate)
        0x30 | 0x31 => arm_decode_alu(opcode, info, Tst, true, AddrMode1::Imm, false, 1),
        0x32 => arm_decode_msr_imm(opcode, info, false),
        0x33 => arm_decode_alu(opcode, info, Teq, true, AddrMode1::Imm, false, 1),
        0x34 | 0x35 => arm_decode_alu(opcode, info, Cmp, true, AddrMode1::Imm, false, 1),
        0x36 => arm_decode_msr_imm(opcode, info, true),
        0x37 => arm_decode_alu(opcode, info, Cmn, true, AddrMode1::Imm, false, 1),
        0x38 => arm_decode_alu(opcode, info, Orr, false, AddrMode1::Imm, true, 0),
        0x39 => arm_decode_alu(opcode, info, Orr, true, AddrMode1::Imm, true, 0),
        0x3A => arm_decode_alu(opcode, info, Mov, false, AddrMode1::Imm, true, 2),
        0x3B => arm_decode_alu(opcode, info, Mov, true, AddrMode1::Imm, true, 2),
        0x3C => arm_decode_alu(opcode, info, Bic, false, AddrMode1::Imm, true, 0),
        0x3D => arm_decode_alu(opcode, info, Bic, true, AddrMode1::Imm, true, 0),
        0x3E => arm_decode_alu(opcode, info, Mvn, false, AddrMode1::Imm, true, 2),
        0x3F => arm_decode_alu(opcode, info, Mvn, true, AddrMode1::Imm, true, 2),
        // DECLARE_ARM_LOAD_STORE_IMMEDIATE_BLOCK rows (-40..-5F)
        0x40 => arm_decode_ls(opcode, info, Str, FMT_POST_WB_SUB, LsAddr::Imm, false, ARM_ACCESS_WORD),
        0x41 => arm_decode_ls(opcode, info, Ldr, FMT_POST_WB_SUB, LsAddr::Imm, true, ARM_ACCESS_WORD),
        0x42 => arm_decode_ls(opcode, info, Str, FMT_POST_WB_SUB, LsAddr::Imm, false, ARM_ACCESS_TRANSLATED_WORD),
        0x43 => arm_decode_ls(opcode, info, Ldr, FMT_POST_WB_SUB, LsAddr::Imm, true, ARM_ACCESS_TRANSLATED_WORD),
        0x44 => arm_decode_ls(opcode, info, Str, FMT_POST_WB_SUB, LsAddr::Imm, false, ARM_ACCESS_BYTE),
        0x45 => arm_decode_ls(opcode, info, Ldr, FMT_POST_WB_SUB, LsAddr::Imm, true, ARM_ACCESS_BYTE),
        0x46 => arm_decode_ls(opcode, info, Str, FMT_POST_WB_SUB, LsAddr::Imm, false, ARM_ACCESS_TRANSLATED_BYTE),
        0x47 => arm_decode_ls(opcode, info, Ldr, FMT_POST_WB_SUB, LsAddr::Imm, true, ARM_ACCESS_TRANSLATED_BYTE),
        0x48 => arm_decode_ls(opcode, info, Str, FMT_POST_WB, LsAddr::Imm, false, ARM_ACCESS_WORD),
        0x49 => arm_decode_ls(opcode, info, Ldr, FMT_POST_WB, LsAddr::Imm, true, ARM_ACCESS_WORD),
        0x4A => arm_decode_ls(opcode, info, Str, FMT_POST_WB, LsAddr::Imm, false, ARM_ACCESS_TRANSLATED_WORD),
        0x4B => arm_decode_ls(opcode, info, Ldr, FMT_POST_WB, LsAddr::Imm, true, ARM_ACCESS_TRANSLATED_WORD),
        0x4C => arm_decode_ls(opcode, info, Str, FMT_POST_WB, LsAddr::Imm, false, ARM_ACCESS_BYTE),
        0x4D => arm_decode_ls(opcode, info, Ldr, FMT_POST_WB, LsAddr::Imm, true, ARM_ACCESS_BYTE),
        0x4E => arm_decode_ls(opcode, info, Str, FMT_POST_WB, LsAddr::Imm, false, ARM_ACCESS_TRANSLATED_BYTE),
        0x4F => arm_decode_ls(opcode, info, Ldr, FMT_POST_WB, LsAddr::Imm, true, ARM_ACCESS_TRANSLATED_BYTE),
        0x50 => arm_decode_ls(opcode, info, Str, FMT_SUB, LsAddr::Imm, false, ARM_ACCESS_WORD),
        0x51 => arm_decode_ls(opcode, info, Ldr, FMT_SUB, LsAddr::Imm, true, ARM_ACCESS_WORD),
        0x52 => arm_decode_ls(opcode, info, Str, FMT_PRE_WB_SUB, LsAddr::Imm, false, ARM_ACCESS_WORD),
        0x53 => arm_decode_ls(opcode, info, Ldr, FMT_PRE_WB_SUB, LsAddr::Imm, true, ARM_ACCESS_WORD),
        0x54 => arm_decode_ls(opcode, info, Str, FMT_SUB, LsAddr::Imm, false, ARM_ACCESS_BYTE),
        0x55 => arm_decode_ls(opcode, info, Ldr, FMT_SUB, LsAddr::Imm, true, ARM_ACCESS_BYTE),
        0x56 => arm_decode_ls(opcode, info, Str, FMT_PRE_WB_SUB, LsAddr::Imm, false, ARM_ACCESS_BYTE),
        0x57 => arm_decode_ls(opcode, info, Ldr, FMT_PRE_WB_SUB, LsAddr::Imm, true, ARM_ACCESS_BYTE),
        0x58 => arm_decode_ls(opcode, info, Str, FMT_NONE, LsAddr::Imm, false, ARM_ACCESS_WORD),
        0x59 => arm_decode_ls(opcode, info, Ldr, FMT_NONE, LsAddr::Imm, true, ARM_ACCESS_WORD),
        0x5A => arm_decode_ls(opcode, info, Str, FMT_PRE_WB, LsAddr::Imm, false, ARM_ACCESS_WORD),
        0x5B => arm_decode_ls(opcode, info, Ldr, FMT_PRE_WB, LsAddr::Imm, true, ARM_ACCESS_WORD),
        0x5C => arm_decode_ls(opcode, info, Str, FMT_NONE, LsAddr::Imm, false, ARM_ACCESS_BYTE),
        0x5D => arm_decode_ls(opcode, info, Ldr, FMT_NONE, LsAddr::Imm, true, ARM_ACCESS_BYTE),
        0x5E => arm_decode_ls(opcode, info, Str, FMT_PRE_WB, LsAddr::Imm, false, ARM_ACCESS_BYTE),
        0x5F => arm_decode_ls(opcode, info, Ldr, FMT_PRE_WB, LsAddr::Imm, true, ARM_ACCESS_BYTE),
        // DECLARE_ARM_LOAD_STORE_BLOCK rows (-60..-7F): register offset, ILL at odd slots
        0x60 => arm_ls_reg_row(opcode, info, Str, false, ARM_ACCESS_WORD, FMT_POST_WB_SUB, low),
        0x61 => arm_ls_reg_row(opcode, info, Ldr, true, ARM_ACCESS_WORD, FMT_POST_WB_SUB, low),
        0x62 => arm_ls_reg_row(opcode, info, Str, false, ARM_ACCESS_TRANSLATED_WORD, FMT_POST_WB_SUB, low),
        0x63 => arm_ls_reg_row(opcode, info, Ldr, true, ARM_ACCESS_TRANSLATED_WORD, FMT_POST_WB_SUB, low),
        0x64 => arm_ls_reg_row(opcode, info, Str, false, ARM_ACCESS_BYTE, FMT_POST_WB_SUB, low),
        0x65 => arm_ls_reg_row(opcode, info, Ldr, true, ARM_ACCESS_BYTE, FMT_POST_WB_SUB, low),
        0x66 => arm_ls_reg_row(opcode, info, Str, false, ARM_ACCESS_TRANSLATED_BYTE, FMT_POST_WB_SUB, low),
        0x67 => arm_ls_reg_row(opcode, info, Ldr, true, ARM_ACCESS_TRANSLATED_BYTE, FMT_POST_WB_SUB, low),
        0x68 => arm_ls_reg_row(opcode, info, Str, false, ARM_ACCESS_WORD, FMT_POST_WB, low),
        0x69 => arm_ls_reg_row(opcode, info, Ldr, true, ARM_ACCESS_WORD, FMT_POST_WB, low),
        0x6A => arm_ls_reg_row(opcode, info, Str, false, ARM_ACCESS_TRANSLATED_WORD, FMT_POST_WB, low),
        0x6B => arm_ls_reg_row(opcode, info, Ldr, true, ARM_ACCESS_TRANSLATED_WORD, FMT_POST_WB, low),
        0x6C => arm_ls_reg_row(opcode, info, Str, false, ARM_ACCESS_BYTE, FMT_POST_WB, low),
        0x6D => arm_ls_reg_row(opcode, info, Ldr, true, ARM_ACCESS_BYTE, FMT_POST_WB, low),
        0x6E => arm_ls_reg_row(opcode, info, Str, false, ARM_ACCESS_TRANSLATED_BYTE, FMT_POST_WB, low),
        0x6F => arm_ls_reg_row(opcode, info, Ldr, true, ARM_ACCESS_TRANSLATED_BYTE, FMT_POST_WB, low),
        0x70 => arm_ls_reg_row(opcode, info, Str, false, ARM_ACCESS_WORD, FMT_SUB, low),
        0x71 => arm_ls_reg_row(opcode, info, Ldr, true, ARM_ACCESS_WORD, FMT_SUB, low),
        0x72 => arm_ls_reg_row(opcode, info, Str, false, ARM_ACCESS_WORD, FMT_PRE_WB_SUB, low),
        0x73 => arm_ls_reg_row(opcode, info, Ldr, true, ARM_ACCESS_WORD, FMT_PRE_WB_SUB, low),
        0x74 => arm_ls_reg_row(opcode, info, Str, false, ARM_ACCESS_BYTE, FMT_SUB, low),
        0x75 => arm_ls_reg_row(opcode, info, Ldr, true, ARM_ACCESS_BYTE, FMT_SUB, low),
        0x76 => arm_ls_reg_row(opcode, info, Str, false, ARM_ACCESS_BYTE, FMT_PRE_WB_SUB, low),
        0x77 => arm_ls_reg_row(opcode, info, Ldr, true, ARM_ACCESS_BYTE, FMT_PRE_WB_SUB, low),
        0x78 => arm_ls_reg_row(opcode, info, Str, false, ARM_ACCESS_WORD, FMT_NONE, low),
        0x79 => arm_ls_reg_row(opcode, info, Ldr, true, ARM_ACCESS_WORD, FMT_NONE, low),
        0x7A => arm_ls_reg_row(opcode, info, Str, false, ARM_ACCESS_WORD, FMT_PRE_WB, low),
        0x7B => arm_ls_reg_row(opcode, info, Ldr, true, ARM_ACCESS_WORD, FMT_PRE_WB, low),
        0x7C => arm_ls_reg_row(opcode, info, Str, false, ARM_ACCESS_BYTE, FMT_NONE, low),
        0x7D => arm_ls_reg_row(opcode, info, Ldr, true, ARM_ACCESS_BYTE, FMT_NONE, low),
        0x7E => arm_ls_reg_row(opcode, info, Str, false, ARM_ACCESS_BYTE, FMT_PRE_WB, low),
        0x7F => arm_ls_reg_row(opcode, info, Ldr, true, ARM_ACCESS_BYTE, FMT_PRE_WB, low),
        // DECLARE_ARM_LOAD_STORE_MULTIPLE_BLOCK rows (-80..-9F)
        0x80 => arm_decode_lsm(opcode, info, Stm, DA, ARM_MEMORY_STORE),
        0x81 => arm_decode_lsm(opcode, info, Ldm, DA, ARM_MEMORY_LOAD),
        0x82 => arm_decode_lsm(opcode, info, Stm, DA, ARM_MEMORY_STORE | WB),
        0x83 => arm_decode_lsm(opcode, info, Ldm, DA, ARM_MEMORY_LOAD | WB),
        0x84 => arm_decode_lsm(opcode, info, Stm, DA, ARM_MEMORY_STORE | SP),
        0x85 => arm_decode_lsm(opcode, info, Ldm, DA, ARM_MEMORY_LOAD | SP),
        0x86 => arm_decode_lsm(opcode, info, Stm, DA, ARM_MEMORY_STORE | WB | SP),
        0x87 => arm_decode_lsm(opcode, info, Ldm, DA, ARM_MEMORY_LOAD | WB | SP),
        0x88 => arm_decode_lsm(opcode, info, Stm, IA, ARM_MEMORY_STORE),
        0x89 => arm_decode_lsm(opcode, info, Ldm, IA, ARM_MEMORY_LOAD),
        0x8A => arm_decode_lsm(opcode, info, Stm, IA, ARM_MEMORY_STORE | WB),
        0x8B => arm_decode_lsm(opcode, info, Ldm, IA, ARM_MEMORY_LOAD | WB),
        0x8C => arm_decode_lsm(opcode, info, Stm, IA, ARM_MEMORY_STORE | SP),
        0x8D => arm_decode_lsm(opcode, info, Ldm, IA, ARM_MEMORY_LOAD | SP),
        0x8E => arm_decode_lsm(opcode, info, Stm, IA, ARM_MEMORY_STORE | WB | SP),
        0x8F => arm_decode_lsm(opcode, info, Ldm, IA, ARM_MEMORY_LOAD | WB | SP),
        0x90 => arm_decode_lsm(opcode, info, Stm, DB, ARM_MEMORY_STORE),
        0x91 => arm_decode_lsm(opcode, info, Ldm, DB, ARM_MEMORY_LOAD),
        0x92 => arm_decode_lsm(opcode, info, Stm, DB, ARM_MEMORY_STORE | WB),
        0x93 => arm_decode_lsm(opcode, info, Ldm, DB, ARM_MEMORY_LOAD | WB),
        0x94 => arm_decode_lsm(opcode, info, Stm, DB, ARM_MEMORY_STORE | SP),
        0x95 => arm_decode_lsm(opcode, info, Ldm, DB, ARM_MEMORY_LOAD | SP),
        0x96 => arm_decode_lsm(opcode, info, Stm, DB, ARM_MEMORY_STORE | WB | SP),
        0x97 => arm_decode_lsm(opcode, info, Ldm, DB, ARM_MEMORY_LOAD | WB | SP),
        0x98 => arm_decode_lsm(opcode, info, Stm, IB, ARM_MEMORY_STORE),
        0x99 => arm_decode_lsm(opcode, info, Ldm, IB, ARM_MEMORY_LOAD),
        0x9A => arm_decode_lsm(opcode, info, Stm, IB, ARM_MEMORY_STORE | WB),
        0x9B => arm_decode_lsm(opcode, info, Ldm, IB, ARM_MEMORY_LOAD | WB),
        0x9C => arm_decode_lsm(opcode, info, Stm, IB, ARM_MEMORY_STORE | SP),
        0x9D => arm_decode_lsm(opcode, info, Ldm, IB, ARM_MEMORY_LOAD | SP),
        0x9E => arm_decode_lsm(opcode, info, Stm, IB, ARM_MEMORY_STORE | WB | SP),
        0x9F => arm_decode_lsm(opcode, info, Ldm, IB, ARM_MEMORY_LOAD | WB | SP),
        // DECLARE_ARM_BRANCH_BLOCK rows (-A/-B)
        0xA0..=0xAF => arm_decode_b(opcode, info),
        0xB0..=0xBF => arm_decode_bl(opcode, info),
        // Coprocessor rows (-C/-D load/store, -E CDP/MCR/MRC): all decode as ILL
        // without the trap bit on the GBA (mGBA has no coprocessor here).
        0xC0..=0xEF => arm_decode_coprocessor(opcode, info),
        // DECLARE_ARM_SWI_BLOCK (-F)
        0xF0..=0xFF => arm_decode_swi(opcode, info),
        _ => unreachable!(),
    }
}

/// C: void ARMDecodeARM(uint32_t opcode, struct ARMInstructionInfo* info).
pub fn arm_decode_arm(opcode: u32) -> ARMInstructionInfo {
    let mut info = ARMInstructionInfo::default();
    info.exec_mode = ExecutionMode::Arm;
    info.opcode = opcode;
    info.branch_type = ARM_BRANCH_NONE;
    info.condition = ARMCondition::from_u8((opcode >> 28) as u8);
    info.s_instruction_cycles = 1;
    arm_decode_dispatch((((opcode >> 16) & 0xFF0) | ((opcode >> 4) & 0x00F)) as usize, opcode, &mut info);
    info
}

// ----- Thumb decoder families (decoder-thumb.c DEFINE_* macros) -----

fn thumb_decode_ill(_opcode: u16, info: &mut ARMInstructionInfo) {
    info.mnemonic = ARMMnemonic::Ill;
    info.operand_format = ARM_OPERAND_NONE;
    info.traps = true;
}

fn thumb_decode_bkpt(_opcode: u16, info: &mut ARMInstructionInfo) {
    info.mnemonic = ARMMnemonic::Bkpt;
    info.operand_format = ARM_OPERAND_NONE;
    info.traps = true;
}

/// DEFINE_IMMEDIATE_5_DECODER_DATA_THUMB (LSL1/LSR1/ASR1).
fn thumb_decode_shift_imm5(opcode: u16, info: &mut ARMInstructionInfo, mnemonic: ARMMnemonic) {
    info.mnemonic = mnemonic;
    info.op3.immediate = ((opcode >> 6) & 0x001F) as i32;
    info.op1.reg = (opcode & 0x0007) as u8;
    info.op2.reg = ((opcode >> 3) & 0x0007) as u8;
    info.affects_cpsr = true;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | ARM_OPERAND_AFFECTED_1
        | ARM_OPERAND_REGISTER_2
        | ARM_OPERAND_IMMEDIATE_3;
}

/// DEFINE_IMMEDIATE_5_DECODER_MEM_THUMB (LDR1/LDRB1/LDRH1/STR1/STRB1/STRH1).
fn thumb_decode_mem_imm5(
    opcode: u16,
    info: &mut ARMInstructionInfo,
    mnemonic: ARMMnemonic,
    load: bool,
    width: u8,
) {
    info.mnemonic = mnemonic;
    info.op1.reg = (opcode & 0x0007) as u8;
    info.memory.base_reg = ((opcode >> 3) & 0x0007) as u8;
    info.memory.offset.immediate = (((opcode >> 6) & 0x001F) as u32 * width as u32) as i32;
    info.memory.width = width;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | if load { ARM_OPERAND_AFFECTED_1 } else { ARM_OPERAND_AFFECTED_2 }
        | ARM_OPERAND_MEMORY_2;
    info.memory.format = ARM_MEMORY_REGISTER_BASE
        | ARM_MEMORY_IMMEDIATE_OFFSET
        | if load { ARM_MEMORY_LOAD } else { ARM_MEMORY_STORE };
    if load {
        load_cycles(info);
    } else {
        store_cycles(info);
    }
}

/// DEFINE_DATA_FORM_1_DECODER_THUMB (ADD3/SUB3).
fn thumb_decode_data_form1(opcode: u16, info: &mut ARMInstructionInfo, mnemonic: ARMMnemonic) {
    info.mnemonic = mnemonic;
    info.op1.reg = (opcode & 0x0007) as u8;
    info.op2.reg = ((opcode >> 3) & 0x0007) as u8;
    info.op3.reg = ((opcode >> 6) & 0x0007) as u8;
    info.affects_cpsr = true;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | ARM_OPERAND_AFFECTED_1
        | ARM_OPERAND_REGISTER_2
        | ARM_OPERAND_REGISTER_3;
}

/// DEFINE_DATA_FORM_2_DECODER_THUMB (ADD1/SUB1).
fn thumb_decode_data_form2(opcode: u16, info: &mut ARMInstructionInfo, mnemonic: ARMMnemonic) {
    info.mnemonic = mnemonic;
    info.op1.reg = (opcode & 0x0007) as u8;
    info.op2.reg = ((opcode >> 3) & 0x0007) as u8;
    info.op3.immediate = ((opcode >> 6) & 0x0007) as i32;
    info.affects_cpsr = true;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | ARM_OPERAND_AFFECTED_1
        | ARM_OPERAND_REGISTER_2
        | ARM_OPERAND_IMMEDIATE_3;
}

/// DEFINE_DATA_FORM_3_DECODER_THUMB (ADD2/CMP1/MOV1/SUB2).
fn thumb_decode_data_form3(
    opcode: u16,
    info: &mut ARMInstructionInfo,
    mnemonic: ARMMnemonic,
    affected: bool,
) {
    info.mnemonic = mnemonic;
    info.op1.reg = ((opcode >> 8) & 0x0007) as u8;
    info.op2.immediate = (opcode & 0x00FF) as i32;
    info.affects_cpsr = true;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | if affected { ARM_OPERAND_AFFECTED_1 } else { 0 }
        | ARM_OPERAND_IMMEDIATE_2;
}

/// DEFINE_DATA_FORM_5_DECODER_THUMB (AND/EOR/LSL2/LSR2/ASR2/ADC/SBC/ROR/TST/NEG/
/// CMP2/CMN/ORR/MUL/BIC/MVN).
fn thumb_decode_data_form5(
    opcode: u16,
    info: &mut ARMInstructionInfo,
    mnemonic: ARMMnemonic,
    affected: bool,
) {
    info.mnemonic = mnemonic;
    info.op1.reg = (opcode & 0x0007) as u8;
    info.op2.reg = ((opcode >> 3) & 0x0007) as u8;
    info.affects_cpsr = true;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | if affected { ARM_OPERAND_AFFECTED_1 } else { 0 }
        | ARM_OPERAND_REGISTER_2;
}

/// DEFINE_DECODER_WITH_HIGH_EX_THUMB (ADD4/CMP3/MOV3, h1/h2 = bit 3 added in).
fn thumb_decode_high(
    opcode: u16,
    info: &mut ARMInstructionInfo,
    mnemonic: ARMMnemonic,
    h1: bool,
    h2: bool,
    affected: bool,
    cpsr: bool,
) {
    info.mnemonic = mnemonic;
    info.op1.reg = ((opcode & 0x0007) | if h1 { 8 } else { 0 }) as u8;
    info.op2.reg = (((opcode >> 3) & 0x0007) | if h2 { 8 } else { 0 }) as u8;
    if info.op1.reg == REG_PC {
        info.branch_type = ARM_BRANCH_INDIRECT;
    }
    info.affects_cpsr = cpsr;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | if affected { ARM_OPERAND_AFFECTED_1 } else { 0 }
        | ARM_OPERAND_REGISTER_2;
}

/// DEFINE_IMMEDIATE_WITH_REGISTER_DATA_THUMB (ADD5: ARM_PC, ADD6: ARM_SP).
fn thumb_decode_add_reg(
    opcode: u16,
    info: &mut ARMInstructionInfo,
    reg: u8,
) {
    info.mnemonic = ARMMnemonic::Add;
    info.op1.reg = ((opcode >> 8) & 0x0007) as u8;
    info.op2.reg = reg;
    info.op3.immediate = ((opcode & 0x00FF) << 2) as i32;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | ARM_OPERAND_AFFECTED_1
        | ARM_OPERAND_REGISTER_2
        | ARM_OPERAND_IMMEDIATE_3;
}

/// DEFINE_IMMEDIATE_WITH_REGISTER_MEM_THUMB (LDR3: PC, LDR4: SP, STR3: SP).
fn thumb_decode_mem_imm_reg(
    opcode: u16,
    info: &mut ARMInstructionInfo,
    load: bool,
    reg: u8,
) {
    info.mnemonic = if load { ARMMnemonic::Ldr } else { ARMMnemonic::Str };
    info.op1.reg = ((opcode >> 8) & 0x0007) as u8;
    info.memory.base_reg = reg;
    info.memory.offset.immediate = ((opcode & 0x00FF) << 2) as i32;
    info.memory.width = ARM_ACCESS_WORD;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | if load { ARM_OPERAND_AFFECTED_1 } else { ARM_OPERAND_AFFECTED_2 }
        | ARM_OPERAND_MEMORY_2;
    info.memory.format = ARM_MEMORY_REGISTER_BASE
        | ARM_MEMORY_IMMEDIATE_OFFSET
        | if load { ARM_MEMORY_LOAD } else { ARM_MEMORY_STORE };
    if load {
        load_cycles(info);
    } else {
        store_cycles(info);
    }
}

/// DEFINE_LOAD_STORE_WITH_REGISTER_THUMB (LDR2/LDRB2/LDRH2/LDRSB/LDRSH/STR2/
/// STRB2/STRH2).
fn thumb_decode_ls_reg(
    opcode: u16,
    info: &mut ARMInstructionInfo,
    mnemonic: ARMMnemonic,
    load: bool,
    width: u8,
) {
    info.mnemonic = mnemonic;
    info.memory.offset.reg = ((opcode >> 6) & 0x0007) as u8;
    info.op1.reg = (opcode & 0x0007) as u8;
    info.memory.base_reg = ((opcode >> 3) & 0x0007) as u8;
    info.memory.width = width;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | if load { ARM_OPERAND_AFFECTED_1 } else { ARM_OPERAND_AFFECTED_2 }
        | ARM_OPERAND_MEMORY_2;
    info.memory.format = ARM_MEMORY_REGISTER_BASE
        | ARM_MEMORY_REGISTER_OFFSET
        | if load { ARM_MEMORY_LOAD } else { ARM_MEMORY_STORE };
    if load {
        load_cycles(info);
    } else {
        store_cycles(info);
    }
}

/// DEFINE_LOAD_STORE_MULTIPLE_EX_THUMB (LDMIA/STMIA via DEFINE_LOAD_STORE_MULTIPLE_THUMB,
/// and POP/POPR/PUSH/PUSHR). `additional` is the ADDITIONAL_REG mask (1<<ARM_PC
/// for POPR, 1<<ARM_LR for PUSHR).
fn thumb_decode_lsm(
    opcode: u16,
    info: &mut ARMInstructionInfo,
    mnemonic: ARMMnemonic,
    base_reg: u8,
    direction: u16,
    load: bool,
    additional: i32,
) {
    info.mnemonic = mnemonic;
    info.memory.base_reg = base_reg;
    info.op1.immediate = ((opcode & 0xFF) as i32) | additional;
    if info.op1.immediate & (1 << 15) != 0 {
        info.branch_type = ARM_BRANCH_INDIRECT;
    }
    info.operand_format = ARM_OPERAND_MEMORY_1 | ARM_OPERAND_AFFECTED_1;
    info.memory.format = ARM_MEMORY_REGISTER_BASE
        | ARM_MEMORY_WRITEBACK
        | if load { ARM_MEMORY_LOAD } else { ARM_MEMORY_STORE }
        | direction;
}

/// DEFINE_CONDITIONAL_BRANCH_THUMB.
fn thumb_decode_bcond(opcode: u16, info: &mut ARMInstructionInfo, cond: ARMCondition) {
    info.mnemonic = ARMMnemonic::B;
    let immediate = (opcode & 0xFF) as u8 as i8;
    info.op1.immediate = (immediate as i32) << 1;
    info.branch_type = ARM_BRANCH;
    info.condition = cond;
    info.operand_format = ARM_OPERAND_IMMEDIATE_1;
}

/// DEFINE_SP_MODIFY_THUMB (ADD7/SUB4).
fn thumb_decode_sp_modify(opcode: u16, info: &mut ARMInstructionInfo, mnemonic: ARMMnemonic) {
    info.mnemonic = mnemonic;
    info.op1.reg = REG_SP;
    info.op2.immediate = ((opcode & 0x7F) << 2) as i32;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | ARM_OPERAND_AFFECTED_1
        | ARM_OPERAND_IMMEDIATE_2;
}

/// DEFINE_THUMB_DECODER(B, B, ...)
fn thumb_decode_b(opcode: u16, info: &mut ARMInstructionInfo) {
    info.mnemonic = ARMMnemonic::B;
    let immediate = ((opcode & 0x07FF) << 5) as i16;
    info.op1.immediate = (immediate as i32) >> 4;
    info.operand_format = ARM_OPERAND_IMMEDIATE_1;
    info.branch_type = ARM_BRANCH;
}

/// DEFINE_THUMB_DECODER(BL1, BL, ...): first half of a BL pair.
fn thumb_decode_bl1(opcode: u16, info: &mut ARMInstructionInfo) {
    info.mnemonic = ARMMnemonic::Bl;
    let immediate = ((opcode & 0x07FF) << 5) as i16;
    info.op1.reg = REG_LR;
    info.op2.reg = REG_PC;
    info.op3.immediate = (immediate as i32) << 7;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | ARM_OPERAND_AFFECTED_1
        | ARM_OPERAND_REGISTER_2
        | ARM_OPERAND_AFFECTED_2
        | ARM_OPERAND_IMMEDIATE_3;
}

/// DEFINE_THUMB_DECODER(BL2, BL, ...): second half of a BL pair.
fn thumb_decode_bl2(opcode: u16, info: &mut ARMInstructionInfo) {
    info.mnemonic = ARMMnemonic::Bl;
    info.op1.reg = REG_PC;
    info.op2.reg = REG_LR;
    info.op3.immediate = ((opcode & 0x07FF) << 1) as i32;
    info.operand_format = ARM_OPERAND_REGISTER_1
        | ARM_OPERAND_AFFECTED_1
        | ARM_OPERAND_REGISTER_2
        | ARM_OPERAND_IMMEDIATE_3;
    info.branch_type = ARM_BRANCH_LINKED;
}

/// DEFINE_THUMB_DECODER(BX, BX, ...)
fn thumb_decode_bx(opcode: u16, info: &mut ARMInstructionInfo) {
    info.mnemonic = ARMMnemonic::Bx;
    info.op1.reg = ((opcode >> 3) & 0xF) as u8;
    info.operand_format = ARM_OPERAND_REGISTER_1;
    info.branch_type = ARM_BRANCH_INDIRECT;
}

/// DEFINE_THUMB_DECODER(SWI, SWI, ...)
fn thumb_decode_swi(opcode: u16, info: &mut ARMInstructionInfo) {
    info.mnemonic = ARMMnemonic::Swi;
    info.op1.immediate = (opcode & 0xFF) as i32;
    info.operand_format = ARM_OPERAND_IMMEDIATE_1;
    info.traps = true;
}

/// (mnemonic, affected) pairs for the thumb ALU block at table indices
/// 0x100..=0x10F (DECLARE_INSTRUCTION_THUMB(EMITTER, AND) .. MVN).
const THUMB_ALU_BLOCK: [(ARMMnemonic, bool); 16] = [
    (ARMMnemonic::And, true),
    (ARMMnemonic::Eor, true),
    (ARMMnemonic::Lsl, true),
    (ARMMnemonic::Lsr, true),
    (ARMMnemonic::Asr, true),
    (ARMMnemonic::Adc, true),
    (ARMMnemonic::Sbc, true),
    (ARMMnemonic::Ror, true),
    (ARMMnemonic::Tst, false),
    (ARMMnemonic::Neg, true),
    (ARMMnemonic::Cmp, false),
    (ARMMnemonic::Cmn, false),
    (ARMMnemonic::Orr, true),
    (ARMMnemonic::Mul, true),
    (ARMMnemonic::Bic, true),
    (ARMMnemonic::Mvn, true),
];

/// DECLARE_THUMB_EMITTER_BLOCK(_ThumbDecode): `index` is opcode >> 6 (0x400
/// entries). Range layout mirrors emitter-thumb.h exactly.
fn thumb_decode_dispatch(index: usize, opcode: u16, info: &mut ARMInstructionInfo) {
    use ARMMnemonic::*;
    match index {
        // DO_8(DO_4(LSL1/LSR1/ASR1))
        0x000..=0x01F => thumb_decode_shift_imm5(opcode, info, Lsl),
        0x020..=0x03F => thumb_decode_shift_imm5(opcode, info, Lsr),
        0x040..=0x05F => thumb_decode_shift_imm5(opcode, info, Asr),
        // DO_8(ADD3/SUB3/ADD1/SUB1)
        0x060..=0x067 => thumb_decode_data_form1(opcode, info, Add),
        0x068..=0x06F => thumb_decode_data_form1(opcode, info, Sub),
        0x070..=0x077 => thumb_decode_data_form2(opcode, info, Add),
        0x078..=0x07F => thumb_decode_data_form2(opcode, info, Sub),
        // DO_8(DO_4(MOV1/CMP1/ADD2/SUB2))
        0x080..=0x09F => thumb_decode_data_form3(opcode, info, Mov, true),
        0x0A0..=0x0BF => thumb_decode_data_form3(opcode, info, Cmp, false),
        0x0C0..=0x0DF => thumb_decode_data_form3(opcode, info, Add, true),
        0x0E0..=0x0FF => thumb_decode_data_form3(opcode, info, Sub, true),
        // 16 thumb ALU ops
        0x100..=0x10F => {
            let (mnemonic, affected) = THUMB_ALU_BLOCK[index - 0x100];
            thumb_decode_data_form5(opcode, info, mnemonic, affected);
        }
        // DECLARE_INSTRUCTION_WITH_HIGH_THUMB(ADD4/CMP3/MOV3): lowest index bits
        // give h1 (bit 2) / h2 (bit 1) high-register flags: 00, 01, 10, 11.
        0x110..=0x113 => thumb_decode_high(opcode, info, Add, index & 2 != 0, index & 1 != 0, true, false),
        0x114..=0x117 => thumb_decode_high(opcode, info, Cmp, index & 2 != 0, index & 1 != 0, false, true),
        0x118..=0x11B => thumb_decode_high(opcode, info, Mov, index & 2 != 0, index & 1 != 0, true, false),
        0x11C..=0x11D => thumb_decode_bx(opcode, info),
        0x11E..=0x11F => thumb_decode_ill(opcode, info),
        // DO_8(DO_4(LDR3)) — LDR rd, [pc, #imm]
        0x120..=0x13F => thumb_decode_mem_imm_reg(opcode, info, true, REG_PC),
        // DO_8(STR2/STRH2/STRB2/LDRSB/LDR2/LDRH2/LDRB2/LDRSH)
        0x140..=0x147 => thumb_decode_ls_reg(opcode, info, Str, false, ARM_ACCESS_WORD),
        0x148..=0x14F => thumb_decode_ls_reg(opcode, info, Str, false, ARM_ACCESS_HALFWORD),
        0x150..=0x157 => thumb_decode_ls_reg(opcode, info, Str, false, ARM_ACCESS_BYTE),
        0x158..=0x15F => thumb_decode_ls_reg(opcode, info, Ldr, true, ARM_ACCESS_SIGNED_BYTE),
        0x160..=0x167 => thumb_decode_ls_reg(opcode, info, Ldr, true, ARM_ACCESS_WORD),
        0x168..=0x16F => thumb_decode_ls_reg(opcode, info, Ldr, true, ARM_ACCESS_HALFWORD),
        0x170..=0x177 => thumb_decode_ls_reg(opcode, info, Ldr, true, ARM_ACCESS_BYTE),
        0x178..=0x17F => thumb_decode_ls_reg(opcode, info, Ldr, true, ARM_ACCESS_SIGNED_HALFWORD),
        // DO_8(DO_4(STR1/LDR1/STRB1/LDRB1/STRH1/LDRH1/STR3/LDR4))
        0x180..=0x19F => thumb_decode_mem_imm5(opcode, info, Str, false, ARM_ACCESS_WORD as u8),
        0x1A0..=0x1BF => thumb_decode_mem_imm5(opcode, info, Ldr, true, ARM_ACCESS_WORD as u8),
        0x1C0..=0x1DF => thumb_decode_mem_imm5(opcode, info, Str, false, ARM_ACCESS_BYTE as u8),
        0x1E0..=0x1FF => thumb_decode_mem_imm5(opcode, info, Ldr, true, ARM_ACCESS_BYTE as u8),
        0x200..=0x21F => thumb_decode_mem_imm5(opcode, info, Str, false, ARM_ACCESS_HALFWORD as u8),
        0x220..=0x23F => thumb_decode_mem_imm5(opcode, info, Ldr, true, ARM_ACCESS_HALFWORD as u8),
        0x240..=0x25F => thumb_decode_mem_imm_reg(opcode, info, false, REG_SP), // STR3
        0x260..=0x27F => thumb_decode_mem_imm_reg(opcode, info, true, REG_SP),  // LDR4
        // DO_8(DO_4(ADD5/ADD6))
        0x280..=0x29F => thumb_decode_add_reg(opcode, info, REG_PC),
        0x2A0..=0x2BF => thumb_decode_add_reg(opcode, info, REG_SP),
        // ADD7 x2, SUB4 x2
        0x2C0..=0x2C1 => thumb_decode_sp_modify(opcode, info, Add),
        0x2C2..=0x2C3 => thumb_decode_sp_modify(opcode, info, Sub),
        // ILL x12
        0x2C4..=0x2CF => thumb_decode_ill(opcode, info),
        // DO_4(PUSH), DO_4(PUSHR)
        0x2D0..=0x2D3 => thumb_decode_lsm(opcode, info, Stm, REG_SP, DB, false, 0),
        0x2D4..=0x2D7 => thumb_decode_lsm(opcode, info, Stm, REG_SP, DB, false, 1 << REG_LR),
        // ILL x24
        0x2D8..=0x2EF => thumb_decode_ill(opcode, info),
        // DO_4(POP), DO_4(POPR), DO_4(BKPT), DO_4(ILL)
        0x2F0..=0x2F3 => thumb_decode_lsm(opcode, info, Ldm, REG_SP, IA, true, 0),
        0x2F4..=0x2F7 => thumb_decode_lsm(opcode, info, Ldm, REG_SP, IA, true, 1 << REG_PC),
        0x2F8..=0x2FB => thumb_decode_bkpt(opcode, info),
        0x2FC..=0x2FF => thumb_decode_ill(opcode, info),
        // DO_8(DO_4(STMIA/LDMIA))
        0x300..=0x31F => thumb_decode_lsm(opcode, info, Stm, ((opcode >> 8) & 0x7) as u8, IA, false, 0),
        0x320..=0x33F => thumb_decode_lsm(opcode, info, Ldm, ((opcode >> 8) & 0x7) as u8, IA, true, 0),
        // DO_4(B<cond>) x 14 (HI before LS per emitter-thumb.h)
        0x340..=0x377 => {
            let cond = ARMCondition::from_u8(((index - 0x340) >> 2) as u8);
            thumb_decode_bcond(opcode, info, cond);
        }
        0x378..=0x37B => thumb_decode_ill(opcode, info),
        0x37C..=0x37F => thumb_decode_swi(opcode, info),
        // DO_8(DO_4(B)), DO_8(DO_4(ILL)), DO_8(DO_4(BL1)), DO_8(DO_4(BL2))
        0x380..=0x39F => thumb_decode_b(opcode, info),
        0x3A0..=0x3BF => thumb_decode_ill(opcode, info),
        0x3C0..=0x3DF => thumb_decode_bl1(opcode, info),
        0x3E0..=0x3FF => thumb_decode_bl2(opcode, info),
        _ => unreachable!(),
    }
}

/// C: void ARMDecodeThumb(uint16_t opcode, struct ARMInstructionInfo* info).
pub fn arm_decode_thumb(opcode: u16) -> ARMInstructionInfo {
    let mut info = ARMInstructionInfo::default();
    info.exec_mode = ExecutionMode::Thumb;
    info.opcode = opcode as u32;
    info.branch_type = ARM_BRANCH_NONE;
    info.condition = ARMCondition::Al;
    info.s_instruction_cycles = 1;
    thumb_decode_dispatch((opcode >> 6) as usize, opcode, &mut info);
    info
}

/// C: bool ARMDecodeThumbCombine(info1, info2, out). Combines the two halves of
/// a Thumb BL pair into a single BL. The C writes through `out`, which the
/// stack-trace debugger aliases with info1; this port returns a new info whose
/// unset fields are copied from info1 (C's aliased case), except op1 which C's
/// union write would fully overwrite — here it is cleared before storing the
/// combined immediate.
pub fn arm_decode_thumb_combine(
    info1: &ARMInstructionInfo,
    info2: &ARMInstructionInfo,
) -> Option<ARMInstructionInfo> {
    if info1.exec_mode != ExecutionMode::Thumb || info1.mnemonic != ARMMnemonic::Bl {
        return None;
    }
    if info2.exec_mode != ExecutionMode::Thumb || info2.mnemonic != ARMMnemonic::Bl {
        return None;
    }
    if info1.op1.reg != REG_LR || info1.op2.reg != REG_PC {
        return None;
    }
    if info2.op1.reg != REG_PC || info2.op2.reg != REG_LR {
        return None;
    }
    let mut out = *info1;
    out.op1 = ARMOperand::default();
    out.op1.immediate = info1.op3.immediate | info2.op3.immediate;
    out.operand_format = ARM_OPERAND_IMMEDIATE_1;
    out.exec_mode = ExecutionMode::Thumb;
    out.mnemonic = ARMMnemonic::Bl;
    out.branch_type = ARM_BRANCH_LINKED;
    out.traps = false;
    out.affects_cpsr = false;
    out.condition = ARMCondition::Al;
    out.s_data_cycles = 0;
    out.n_data_cycles = 0;
    out.s_instruction_cycles = 2;
    out.n_instruction_cycles = 0;
    out.i_cycles = 0;
    out.c_cycles = 0;
    Some(out)
}

/// decoder-inlines.h: static inline bool ARMInstructionIsBranch(enum ARMMnemonic).
pub fn arm_instruction_is_branch(mnemonic: ARMMnemonic) -> bool {
    // TODO: ARM_MN_BLX if ever added upstream
    matches!(mnemonic, ARMMnemonic::B | ARMMnemonic::Bl | ARMMnemonic::Bx)
}

/// C: uint32_t ARMResolveMemoryAccess(info, regs, pc). `regs` corresponds to
/// regs->gprs; the RRX shift reads cpsr.c in C, which this port treats as 0
/// (no PSR is passed in).
pub fn arm_resolve_memory_access(info: &ARMInstructionInfo, regs: &[u32; 16], pc: u32) -> u32 {
    let mut address: u32 = 0;
    let mut offset: i32 = 0;
    if info.memory.format & ARM_MEMORY_REGISTER_BASE != 0 {
        if info.memory.base_reg == REG_PC && info.memory.format & ARM_MEMORY_IMMEDIATE_OFFSET != 0 {
            address = pc;
        } else {
            address = regs[info.memory.base_reg as usize];
        }
    }
    if info.memory.format & ARM_MEMORY_POST_INCREMENT != 0 {
        return address;
    }
    if info.memory.format & ARM_MEMORY_IMMEDIATE_OFFSET != 0 {
        offset = info.memory.offset.immediate;
    } else if info.memory.format & ARM_MEMORY_REGISTER_OFFSET != 0 {
        offset = if info.memory.offset.reg == REG_PC {
            pc as i32
        } else {
            regs[info.memory.offset.reg as usize] as i32
        };
    }
    if info.memory.format & ARM_MEMORY_SHIFTED_OFFSET != 0 {
        let shift_size = info.memory.offset.shifter_imm as u32;
        // Note: for memory shifted offsets LSL is only encodable with shift
        // 1..=31 (lsl #0 clears SHIFTED_OFFSET) and ROR 1..=31 (ror #0 becomes
        // RRX), so only LSR/ASR can reach shift 32 here.
        match info.memory.offset.shifter_op {
            ARMShifterOperation::Lsl => {
                offset = if shift_size >= 32 { 0 } else { ((offset as u32) << shift_size) as i32 };
            }
            ARMShifterOperation::Lsr => {
                offset = if shift_size >= 32 { 0 } else { ((offset as u32) >> shift_size) as i32 };
            }
            ARMShifterOperation::Asr => {
                offset = if shift_size >= 32 { offset >> 31 } else { offset >> shift_size };
            }
            ARMShifterOperation::Ror => {
                offset = (offset as u32).rotate_right(shift_size) as i32;
            }
            ARMShifterOperation::Rrx => {
                offset = (((/* cpsr.c */ false as u32) << 31) | ((offset as u32) >> 1)) as i32;
            }
            ARMShifterOperation::None => {}
        }
    }
    address.wrapping_add(if info.memory.format & ARM_MEMORY_OFFSET_SUBTRACT != 0 {
        offset.wrapping_neg() as u32
    } else {
        offset as u32
    })
}

// ----- Disassembly (decoder.c, ENABLE_DEBUGGERS section) -----
//
// Symbol annotation: C looks up branch/pc-relative target addresses with
// mDebuggerSymbolReverseLookup; here `resolve_symbol(addr)` plays that role.

/// C _armConditions.
const ARM_CONDITIONS: [&str; 16] = [
    "eq", "ne", "cs", "cc", "mi", "pl", "vs", "vc", "hi", "ls", "ge", "lt", "gt", "le", "al", "nv",
];

/// C _armMnemonicStrings (index = mnemonic discriminant; last entry is for
/// ARM_MN_MAX which is never stored in an info).
const ARM_MNEMONIC_STRINGS: [&str; 41] = [
    "ill", "adc", "add", "and", "asr", "b", "bic", "bkpt", "bl", "bx", "cmn", "cmp", "eor", "ldm",
    "ldr", "lsl", "lsr", "mla", "mov", "mrs", "msr", "mul", "mvn", "neg", "orr", "ror", "rsb",
    "rsc", "sbc", "smlal", "smull", "stm", "str", "sub", "swi", "swp", "teq", "tst", "umlal",
    "umull", "ill",
];

/// C _armDirectionStrings (index = MEMORY_FORMAT_TO_DIRECTION(format)).
const ARM_DIRECTION_STRINGS: [&str; 4] = ["da", "ia", "db", "ib"];

/// C _armAccessTypeStrings (index = memory.width).
const ARM_ACCESS_TYPE_STRINGS: [&str; 24] = [
    "", "b", "h", "", "", "", "", "", "", "sb", "sh", "", "", "", "", "", "", "bt", "", "", "t",
    "", "", "",
];

fn decode_register(out: &mut String, reg: u8) {
    match reg {
        REG_SP => out.push_str("sp"),
        REG_LR => out.push_str("lr"),
        REG_PC => out.push_str("pc"),
        ARM_CPSR => out.push_str("cpsr"),
        ARM_SPSR => out.push_str("spsr"),
        _ => out.push_str(&format!("r{}", reg)),
    }
}

fn decode_register_list(out: &mut String, list: u32) {
    out.push('{');
    let mut list = list;
    let mut start: i32 = -1;
    let mut end: i32 = -1;
    for i in 0..=15i32 {
        if list & 1 != 0 {
            if start < 0 {
                start = i;
                end = i;
            } else if end + 1 == i {
                end = i;
            } else {
                if end > start {
                    decode_register(out, start as u8);
                    out.push('-');
                }
                decode_register(out, end as u8);
                out.push(',');
                start = i;
                end = i;
            }
        }
        list >>= 1;
    }
    if start >= 0 {
        if end > start {
            decode_register(out, start as u8);
            out.push('-');
        }
        decode_register(out, end as u8);
    }
    out.push('}');
}

fn decode_psr(out: &mut String, psr_bits: u8) {
    if psr_bits == 0 {
        return;
    }
    out.push('_');
    if psr_bits & ARM_PSR_C != 0 {
        out.push('c');
    }
    if psr_bits & ARM_PSR_X != 0 {
        out.push('x');
    }
    if psr_bits & ARM_PSR_S != 0 {
        out.push('s');
    }
    if psr_bits & ARM_PSR_F != 0 {
        out.push('f');
    }
}

type SymbolLookup<'a> = Option<&'a dyn Fn(u32) -> Option<String>>;

/// C _decodePCRelative: `address` is the decoded (relative) immediate.
fn decode_pc_relative(
    out: &mut String,
    address: i32,
    pc: u32,
    thumb_branch: bool,
    resolve_symbol: SymbolLookup,
) {
    let address = pc.wrapping_add(address as u32);
    let mut label = resolve_symbol.and_then(|f| f(address));
    if label.is_none() && thumb_branch {
        label = resolve_symbol.and_then(|f| f(address | 1));
    }
    match label {
        Some(label) => out.push_str(&label),
        None => out.push_str(&format!("0x{:08X}", address)),
    }
}

/// C _decodeMemory with cpu == NULL (no "=0x..." literal-pool annotation; the
/// effective address is shown in brackets).
fn decode_memory(out: &mut String, memory: &ARMMemoryAccess, pc: u32, resolve_symbol: SymbolLookup) {
    let mut elide_close = false;
    if memory.format & ARM_MEMORY_REGISTER_BASE != 0 {
        if memory.base_reg == REG_PC && memory.format & ARM_MEMORY_IMMEDIATE_OFFSET != 0 {
            let addr_base = if memory.format & ARM_MEMORY_OFFSET_SUBTRACT != 0 {
                memory.offset.immediate.wrapping_neg()
            } else {
                memory.offset.immediate
            };
            out.push('[');
            decode_pc_relative(out, addr_base, pc & 0xFFFFFFFC, false, resolve_symbol);
        } else {
            out.push('[');
            decode_register(out, memory.base_reg);
            if memory.format & (ARM_MEMORY_REGISTER_OFFSET | ARM_MEMORY_IMMEDIATE_OFFSET) != 0
                && memory.format & ARM_MEMORY_POST_INCREMENT == 0
            {
                out.push_str(", ");
            }
        }
    } else {
        out.push('[');
    }
    if memory.format & ARM_MEMORY_POST_INCREMENT != 0 {
        out.push_str("], ");
        elide_close = true;
    }
    if memory.format & ARM_MEMORY_IMMEDIATE_OFFSET != 0 && memory.base_reg != REG_PC {
        if memory.format & ARM_MEMORY_OFFSET_SUBTRACT != 0 {
            out.push_str(&format!("#-{}", memory.offset.immediate));
        } else {
            out.push_str(&format!("#{}", memory.offset.immediate));
        }
    } else if memory.format & ARM_MEMORY_REGISTER_OFFSET != 0 {
        if memory.format & ARM_MEMORY_OFFSET_SUBTRACT != 0 {
            out.push('-');
        }
        decode_register(out, memory.offset.reg);
    }
    if memory.format & ARM_MEMORY_SHIFTED_OFFSET != 0 {
        decode_shift(out, &memory.offset, false);
    }
    if !elide_close {
        out.push(']');
    }
    if (memory.format & (ARM_MEMORY_PRE_INCREMENT | ARM_MEMORY_WRITEBACK))
        == (ARM_MEMORY_PRE_INCREMENT | ARM_MEMORY_WRITEBACK)
    {
        out.push('!');
    }
}

/// C _decodeShift. `reg` selects the shifter amount form
/// (register vs immediate); callers come from the operandFormat shift bits.
fn decode_shift(out: &mut String, op: &ARMOperand, reg: bool) {
    out.push_str(", ");
    match op.shifter_op {
        ARMShifterOperation::Lsl => out.push_str("lsl "),
        ARMShifterOperation::Lsr => out.push_str("lsr "),
        ARMShifterOperation::Asr => out.push_str("asr "),
        ARMShifterOperation::Ror => out.push_str("ror "),
        ARMShifterOperation::Rrx => {
            out.push_str("rrx");
            return;
        }
        ARMShifterOperation::None => {} // C: no case, falls through to print amount
    }
    if !reg {
        out.push_str(&format!("#{}", op.shifter_imm));
    } else {
        decode_register(out, op.shifter_reg);
    }
}

/// C: int ARMDisassemble(const ARMInstructionInfo* info, ARMCore* core,
/// symbols, pc, buffer, blen). `pc` is the architecturally-visible pc (what the
/// caller's r15 reads) exactly as the C callers pass it
/// (address + WORD_SIZE_ARM * 2 / address + WORD_SIZE_THUMB * 2).
pub fn arm_disassemble(info: &ARMInstructionInfo, pc: u32, resolve_symbol: SymbolLookup) -> String {
    let mut mnemonic = ARM_MNEMONIC_STRINGS[info.mnemonic as usize];
    let mut skip3 = false;
    let cond = if info.condition != ARMCondition::Al && info.condition < ARMCondition::Nv {
        ARM_CONDITIONS[info.condition as usize]
    } else {
        ""
    };
    let mut flags = "";
    match info.mnemonic {
        ARMMnemonic::Ldm | ARMMnemonic::Stm => {
            flags = ARM_DIRECTION_STRINGS[((info.memory.format >> 8) & 0x3) as usize];
        }
        ARMMnemonic::Ldr | ARMMnemonic::Str | ARMMnemonic::Swp => {
            flags = ARM_ACCESS_TYPE_STRINGS[info.memory.width as usize];
        }
        ARMMnemonic::Add => {
            if (info.operand_format & (ARM_OPERAND_3 | ARM_OPERAND_4)) == ARM_OPERAND_IMMEDIATE_3
                && info.op3.immediate == 0
                && info.exec_mode == ExecutionMode::Thumb
            {
                skip3 = true;
                mnemonic = "mov";
            }
            // Fall through (matches C case fallthrough)
            if info.affects_cpsr && info.exec_mode == ExecutionMode::Arm {
                flags = "s";
            }
        }
        ARMMnemonic::Adc
        | ARMMnemonic::And
        | ARMMnemonic::Asr
        | ARMMnemonic::Bic
        | ARMMnemonic::Eor
        | ARMMnemonic::Lsl
        | ARMMnemonic::Lsr
        | ARMMnemonic::Mla
        | ARMMnemonic::Mul
        | ARMMnemonic::Mov
        | ARMMnemonic::Mvn
        | ARMMnemonic::Orr
        | ARMMnemonic::Ror
        | ARMMnemonic::Rsb
        | ARMMnemonic::Rsc
        | ARMMnemonic::Sbc
        | ARMMnemonic::Smlal
        | ARMMnemonic::Smull
        | ARMMnemonic::Sub
        | ARMMnemonic::Umlal
        | ARMMnemonic::Umull => {
            if info.affects_cpsr && info.exec_mode == ExecutionMode::Arm {
                flags = "s";
            }
        }
        _ => {}
    }
    let mut out = format!("{}{}{} ", mnemonic, cond, flags);

    match info.mnemonic {
        ARMMnemonic::Ldm | ARMMnemonic::Stm => {
            decode_register(&mut out, info.memory.base_reg);
            if info.memory.format & ARM_MEMORY_WRITEBACK != 0 {
                out.push('!');
            }
            out.push_str(", ");
            decode_register_list(&mut out, info.op1.immediate as u32);
            if info.memory.format & ARM_MEMORY_SPSR_SWAP != 0 {
                out.push('^');
            }
        }
        ARMMnemonic::B | ARMMnemonic::Bl => {
            if info.operand_format & ARM_OPERAND_IMMEDIATE_1 != 0 {
                decode_pc_relative(&mut out, info.op1.immediate, pc, true, resolve_symbol);
            }
        }
        _ => {
            let f = info.operand_format;
            if f & ARM_OPERAND_IMMEDIATE_1 != 0 {
                out.push_str(&format!("#{}", info.op1.immediate));
            } else if f & ARM_OPERAND_MEMORY_1 != 0 {
                decode_memory(&mut out, &info.memory, pc, resolve_symbol);
            } else if f & ARM_OPERAND_REGISTER_1 != 0 {
                decode_register(&mut out, info.op1.reg);
                if info.op1.reg > REG_PC {
                    decode_psr(&mut out, info.op1.psr_bits);
                }
            }
            if f & ARM_OPERAND_SHIFT_REGISTER_1 != 0 {
                decode_shift(&mut out, &info.op1, true);
            } else if f & ARM_OPERAND_SHIFT_IMMEDIATE_1 != 0 {
                decode_shift(&mut out, &info.op1, false);
            }
            if f & ARM_OPERAND_2 != 0 {
                out.push_str(", ");
            }
            if f & ARM_OPERAND_IMMEDIATE_2 != 0 {
                out.push_str(&format!("#{}", info.op2.immediate));
            } else if f & ARM_OPERAND_MEMORY_2 != 0 {
                decode_memory(&mut out, &info.memory, pc, resolve_symbol);
            } else if f & ARM_OPERAND_REGISTER_2 != 0 {
                decode_register(&mut out, info.op2.reg);
            }
            if f & ARM_OPERAND_SHIFT_REGISTER_2 != 0 {
                decode_shift(&mut out, &info.op2, true);
            } else if f & ARM_OPERAND_SHIFT_IMMEDIATE_2 != 0 {
                decode_shift(&mut out, &info.op2, false);
            }
            if !skip3 {
                if f & ARM_OPERAND_3 != 0 {
                    out.push_str(", ");
                }
                if f & ARM_OPERAND_IMMEDIATE_3 != 0 {
                    out.push_str(&format!("#{}", info.op3.immediate));
                } else if f & ARM_OPERAND_MEMORY_3 != 0 {
                    decode_memory(&mut out, &info.memory, pc, resolve_symbol);
                } else if f & ARM_OPERAND_REGISTER_3 != 0 {
                    decode_register(&mut out, info.op3.reg);
                }
                if f & ARM_OPERAND_SHIFT_REGISTER_3 != 0 {
                    decode_shift(&mut out, &info.op3, true);
                } else if f & ARM_OPERAND_SHIFT_IMMEDIATE_3 != 0 {
                    decode_shift(&mut out, &info.op3, false);
                }
            }
            if f & ARM_OPERAND_4 != 0 {
                out.push_str(", ");
            }
            if f & ARM_OPERAND_IMMEDIATE_4 != 0 {
                out.push_str(&format!("#{}", info.op4.immediate));
            } else if f & ARM_OPERAND_MEMORY_4 != 0 {
                decode_memory(&mut out, &info.memory, pc, resolve_symbol);
            } else if f & ARM_OPERAND_REGISTER_4 != 0 {
                decode_register(&mut out, info.op4.reg);
            }
            if f & ARM_OPERAND_SHIFT_REGISTER_4 != 0 {
                decode_shift(&mut out, &info.op4, true);
            } else if f & ARM_OPERAND_SHIFT_IMMEDIATE_4 != 0 {
                decode_shift(&mut out, &info.op4, false);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PC: u32 = 0x08000004;

    fn dis_arm(opcode: u32, pc: u32) -> String {
        arm_disassemble(&arm_decode_arm(opcode), pc, None)
    }

    fn dis_thumb(opcode: u16, pc: u32) -> String {
        arm_disassemble(&arm_decode_thumb(opcode), pc, None)
    }

    // ----- ARM: data processing / moves -----

    #[test]
    fn arm_mov_nop() {
        let info = arm_decode_arm(0xE1A00000);
        assert_eq!(info.mnemonic, ARMMnemonic::Mov);
        assert_eq!(info.condition, ARMCondition::Al);
        assert_eq!(info.operand_format, ARM_OPERAND_REGISTER_1 | ARM_OPERAND_AFFECTED_1 | ARM_OPERAND_REGISTER_2);
        assert_eq!(info.op1.reg, 0);
        assert_eq!(info.op2.reg, 0);
        assert!(!info.affects_cpsr);
        assert!(!arm_instruction_is_branch(info.mnemonic));
        assert_eq!(dis_arm(0xE1A00000, PC), "mov r0, r0");
    }

    #[test]
    fn arm_movs_and_conditions() {
        let info = arm_decode_arm(0xE1B00000);
        assert_eq!(info.mnemonic, ARMMnemonic::Mov);
        assert!(info.affects_cpsr);
        assert_eq!(dis_arm(0xE1B00000, PC), "movs r0, r0");
        let info = arm_decode_arm(0x11A00000);
        assert_eq!(info.condition, ARMCondition::Ne);
        assert_eq!(dis_arm(0x11A00000, PC), "movne r0, r0");
    }

    #[test]
    fn arm_moves_with_shifts() {
        let info = arm_decode_arm(0xE1A00230); // mov r0, r0, lsr r2
        assert_eq!(info.mnemonic, ARMMnemonic::Mov);
        assert_eq!(info.i_cycles, 1); // register shift penalty
        assert_ne!(info.operand_format & ARM_OPERAND_SHIFT_REGISTER_2, 0);
        assert_eq!(dis_arm(0xE1A00230, PC), "mov r0, r0, lsr r2");
        assert_eq!(dis_arm(0xE1A00060, PC), "mov r0, r0, rrx"); // ror #0
        assert_eq!(dis_arm(0xE1A000E0, PC), "mov r0, r0, ror #1");
        assert_eq!(dis_arm(0xE1A01100, PC), "mov r1, r0, lsl #2");
        assert_eq!(dis_arm(0xE3A00C01, PC), "mov r0, #256"); // imm 8 ror 24
    }

    #[test]
    fn arm_alu_imm() {
        let info = arm_decode_arm(0xE29100FF); // adds r0, r1, #0xFF
        assert_eq!(info.mnemonic, ARMMnemonic::Add);
        assert!(info.affects_cpsr);
        assert_eq!(info.op2.reg, 1);
        assert_eq!(dis_arm(0xE29100FF, PC), "adds r0, r1, #255");
        // tst/cmp/teq/cmn re-map operands (SKIPPED == 1)
        let info = arm_decode_arm(0xE1300001); // teq r0, r1
        assert_eq!(info.mnemonic, ARMMnemonic::Teq);
        assert!(info.affects_cpsr);
        assert_eq!(info.operand_format, ARM_OPERAND_REGISTER_1 | ARM_OPERAND_REGISTER_2);
        assert_eq!(info.op1.reg, 0);
        assert_eq!(info.op2.reg, 1);
        assert_eq!(dis_arm(0xE1300001, PC), "teq r0, r1");
        assert_eq!(dis_arm(0xE3500001, PC), "cmp r0, #1");
    }

    // ----- ARM: branches -----

    #[test]
    fn arm_bx() {
        let info = arm_decode_arm(0xE12FFF1E); // bx lr
        assert_eq!(info.mnemonic, ARMMnemonic::Bx);
        assert_eq!(info.branch_type, ARM_BRANCH_INDIRECT);
        assert_eq!(info.op1.reg, REG_LR);
        assert!(arm_instruction_is_branch(info.mnemonic));
        assert_eq!(dis_arm(0xE12FFF1E, PC), "bx lr");
        assert_eq!(dis_arm(0x012FFF1E, PC), "bxeq lr");
    }

    #[test]
    fn arm_b_bl() {
        let info = arm_decode_arm(0xEB000000); // bl +0
        assert_eq!(info.mnemonic, ARMMnemonic::Bl);
        assert_eq!(info.branch_type, ARM_BRANCH_LINKED);
        assert_eq!(info.op1.immediate, 0);
        assert!(arm_instruction_is_branch(info.mnemonic));
        assert_eq!(dis_arm(0xEB000000, 0x08000008), "bl 0x08000008");
        let info = arm_decode_arm(0xEAFFFFFE); // b -8
        assert_eq!(info.mnemonic, ARMMnemonic::B);
        assert_eq!(info.branch_type, ARM_BRANCH);
        assert_eq!(info.op1.immediate, -8);
        assert_eq!(dis_arm(0xEAFFFFFE, 0x08000008), "b 0x08000000");
    }

    #[test]
    fn disasm_branch_symbol_lookup() {
        let info = arm_decode_arm(0xEB000002); // bl +8
        let resolve = |addr: u32| if addr == 0x08000010 { Some("func".to_string()) } else { None };
        assert_eq!(arm_disassemble(&info, 0x08000008, Some(&resolve)), "bl func");
        // Thumb-side label fallback (addr | 1) applies to B/BL targets
        let info = arm_decode_thumb(0xE7FE); // b -4
        let resolve = |addr: u32| if addr == 0x08000001 { Some("thumb_target".to_string()) } else { None };
        assert_eq!(arm_disassemble(&info, 0x08000004, Some(&resolve)), "b thumb_target");
        // ...but not to pc-relative LDR targets
        let info = arm_decode_arm(0xE59F0000);
        let resolve = |addr: u32| if addr == 0x08000009 { Some("nope".to_string()) } else { None };
        assert_eq!(arm_disassemble(&info, 0x08000009, Some(&resolve)), "ldr r0, [0x08000008]");
    }

    // ----- ARM: load/store -----

    #[test]
    fn arm_ldr_pc_relative() {
        let info = arm_decode_arm(0xE59F0000); // ldr r0, [pc]
        assert_eq!(info.mnemonic, ARMMnemonic::Ldr);
        assert_eq!(info.memory.base_reg, REG_PC);
        assert_eq!(info.memory.width, ARM_ACCESS_WORD);
        assert_eq!(info.memory.format, ARM_MEMORY_REGISTER_BASE | ARM_MEMORY_IMMEDIATE_OFFSET | ARM_MEMORY_LOAD);
        assert_eq!(info.memory.offset.immediate, 0);
        // pc gets masked to a word boundary for the printed address
        assert_eq!(dis_arm(0xE59F0000, 0x0800000A), "ldr r0, [0x08000008]");
    }

    #[test]
    fn arm_str_pre_index_writeback() {
        let info = arm_decode_arm(0xE52D2004); // str r2, [sp, #-4]!
        assert_eq!(info.mnemonic, ARMMnemonic::Str);
        assert_eq!(info.memory.base_reg, REG_SP);
        assert_eq!(info.memory.offset.immediate, 4);
        assert_eq!(
            info.memory.format,
            ARM_MEMORY_REGISTER_BASE
                | ARM_MEMORY_PRE_INCREMENT
                | ARM_MEMORY_WRITEBACK
                | ARM_MEMORY_OFFSET_SUBTRACT
                | ARM_MEMORY_IMMEDIATE_OFFSET
                | ARM_MEMORY_STORE
        );
        assert_eq!(dis_arm(0xE52D2004, PC), "str r2, [sp, #-4]!");
    }

    #[test]
    fn arm_ldr_post_index() {
        assert_eq!(dis_arm(0xE4910004, PC), "ldr r0, [r1], #4");
        assert_eq!(dis_arm(0xE4110004, PC), "ldr r0, [r1], #-4");
        assert_eq!(dis_arm(0xE5910000, PC), "ldr r0, [r1, #0]");
        assert_eq!(dis_arm(0xE5D10003, PC), "ldrb r0, [r1, #3]");
    }

    #[test]
    fn arm_ldr_register_offset() {
        // lsl #0 as offset gets folded away
        assert_eq!(dis_arm(0xE7910002, PC), "ldr r0, [r1, r2]");
        let info = arm_decode_arm(0xE7910102); // ldr r0, [r1, r2, lsl #2]
        assert_eq!(info.memory.offset.shifter_op, ARMShifterOperation::Lsl);
        assert_eq!(info.memory.offset.shifter_imm, 2);
        assert_ne!(info.memory.format & ARM_MEMORY_SHIFTED_OFFSET, 0);
        assert_eq!(dis_arm(0xE7910102, PC), "ldr r0, [r1, r2, lsl #2]");
        // ldr pc, [...] is an indirect branch
        let info = arm_decode_arm(0xE791F102);
        assert_eq!(info.branch_type, ARM_BRANCH_INDIRECT);
        // register-offset with writeback and offset reg pc is also indirect
        let info = arm_decode_arm(0xE7A0000F);
        assert_eq!(info.branch_type, ARM_BRANCH_INDIRECT);
    }

    #[test]
    fn arm_halfword_signed() {
        assert_eq!(dis_arm(0xE1D100B0, PC), "ldrh r0, [r1, #0]");
        assert_eq!(dis_arm(0xE1D100D0, PC), "ldrsb r0, [r1, #0]");
        assert_eq!(dis_arm(0xE1D100F0, PC), "ldrsh r0, [r1, #0]");
        assert_eq!(dis_arm(0xE1C100B0, PC), "strh r0, [r1, #0]");
        assert_eq!(dis_arm(0xE15101B2, PC), "ldrh r0, [r1, #-18]");
        assert_eq!(dis_arm(0xE0D101D2, PC), "ldrsb r0, [r1], #18");
    }

    // ----- ARM: block data -----

    #[test]
    fn arm_ldm_stm() {
        let info = arm_decode_arm(0xE92D4010); // stmdb sp!, {r4,lr}
        assert_eq!(info.mnemonic, ARMMnemonic::Stm);
        assert_eq!(dis_arm(0xE92D4010, PC), "stmdb sp!, {r4,lr}");
        let info = arm_decode_arm(0xE8BD8010); // ldmia sp!, {r4,pc}
        assert_eq!(info.mnemonic, ARMMnemonic::Ldm);
        assert_eq!(info.branch_type, ARM_BRANCH_INDIRECT);
        assert_eq!(dis_arm(0xE8BD8010, PC), "ldmia sp!, {r4,pc}");
        // register-range compaction
        assert_eq!(dis_arm(0xE8BD00FF, PC), "ldmia sp!, {r0-r7}");
        assert_eq!(dis_arm(0xE8BD8FFF, PC), "ldmia sp!, {r0-r11,pc}");
        // SPSR swap marker
        assert_eq!(dis_arm(0xE8FD8010, PC), "ldmia sp!, {r4,pc}^");
        // stmfd/stmed etc. naming comes from the direction strings
        assert_eq!(dis_arm(0xE80D0010, PC), "stmda sp, {r4}");
    }

    // ----- ARM: multiply -----

    #[test]
    fn arm_multiply() {
        let info = arm_decode_arm(0xE0000090); // mul r0, r0, r0
        assert_eq!(info.mnemonic, ARMMnemonic::Mul);
        assert_eq!(info.operand_format & ARM_OPERAND_REGISTER_4, 0); // no accumulate operand
        assert_eq!(dis_arm(0xE0000090, PC), "mul r0, r0, r0");
        let info = arm_decode_arm(0xE0203291); // mla r0, r1, r2, r3
        assert_eq!(info.mnemonic, ARMMnemonic::Mla);
        assert_ne!(info.operand_format & ARM_OPERAND_REGISTER_4, 0);
        assert_eq!(info.op4.reg, 3);
        assert_eq!(dis_arm(0xE0203291, PC), "mla r0, r1, r2, r3");
        let info = arm_decode_arm(0xE0C10392); // smull r0, r1, r2, r3
        assert_eq!(info.mnemonic, ARMMnemonic::Smull);
        assert_eq!(dis_arm(0xE0C10392, PC), "smull r0, r1, r2, r3");
        assert_eq!(dis_arm(0xE0E21193, PC), "smlal r1, r2, r3, r1");
    }

    // ----- ARM: psr transfer / swp / swi / ill -----

    #[test]
    fn arm_swaps_and_psr() {
        assert_eq!(dis_arm(0xE1001092, PC), "swp r1, r2, [r0]");
        assert_eq!(dis_arm(0xE1401092, PC), "swpb r1, r2, [r0]");
        let info = arm_decode_arm(0xE10F0000); // mrs r0, cpsr
        assert_eq!(info.mnemonic, ARMMnemonic::Mrs);
        assert_eq!(info.op2.reg, ARM_CPSR);
        assert_eq!(dis_arm(0xE10F0000, PC), "mrs r0, cpsr");
        let info = arm_decode_arm(0xE128F001); // msr cpsr_f, r1
        assert_eq!(info.mnemonic, ARMMnemonic::Msr);
        assert!(info.affects_cpsr);
        assert_eq!(info.op1.reg, ARM_CPSR);
        assert_eq!(info.op1.psr_bits, ARM_PSR_F);
        assert_eq!(dis_arm(0xE128F001, PC), "msr cpsr_f, r1");
        assert_eq!(dis_arm(0xE120F001, PC), "msr cpsr, r1"); // no mask: no _suffix
        assert_eq!(dis_arm(0xE328F0FF, PC), "msr cpsr_f, #255"); // immediate form
        assert_eq!(dis_arm(0xE168F001, PC), "msr spsr_f, r1"); // MSRR does not set affectsCPSR
        assert!(!arm_decode_arm(0xE168F001).affects_cpsr);
    }

    #[test]
    fn arm_swi_bkpt_ill() {
        let info = arm_decode_arm(0xEF000042);
        assert_eq!(info.mnemonic, ARMMnemonic::Swi);
        assert!(info.traps);
        assert_eq!(info.op1.immediate, 0x42);
        assert_eq!(dis_arm(0xEF000042, PC), "swi #66");
        let info = arm_decode_arm(0xE1200070);
        assert_eq!(info.mnemonic, ARMMnemonic::Bkpt);
        assert!(info.traps);
        assert_eq!(dis_arm(0xE1200070, PC), "bkpt ");
        // Architecturally undefined
        let info = arm_decode_arm(0xE7F000F0);
        assert_eq!(info.mnemonic, ARMMnemonic::Ill);
        assert!(info.traps);
        assert_eq!(dis_arm(0xE7F000F0, PC), "ill ");
        // Coprocessor opcodes decode as ill without trapping (GBA has none)
        let info = arm_decode_arm(0xEE000010);
        assert_eq!(info.mnemonic, ARMMnemonic::Ill);
        assert!(!info.traps);
        let info = arm_decode_arm(0xEC100000);
        assert_eq!(info.mnemonic, ARMMnemonic::Ill);
        assert!(!info.traps);
    }

    // ----- ARMResolveMemoryAccess -----

    #[test]
    fn resolve_memory_access() {
        let mut regs = [0u32; 16];
        // ldr r0, [pc, #4]
        let info = arm_decode_arm(0xE59F0004);
        assert_eq!(arm_resolve_memory_access(&info, &regs, 8), 12);
        // ldr r0, [r1, r2, lsl #2]
        regs[1] = 0x100;
        regs[2] = 3;
        let info = arm_decode_arm(0xE7910102);
        assert_eq!(arm_resolve_memory_access(&info, &regs, 0), 0x100 + 3 * 4);
        // post-increment returns the base register value
        regs[1] = 0x1000;
        let info = arm_decode_arm(0xE4910004);
        assert_eq!(arm_resolve_memory_access(&info, &regs, 0), 0x1000);
        // pre-index subtract
        regs[13] = 0x2000;
        let info = arm_decode_arm(0xE52D2004);
        assert_eq!(arm_resolve_memory_access(&info, &regs, 0), 0x1FFC);
    }

    // ----- Thumb -----

    #[test]
    fn thumb_shifts() {
        let info = arm_decode_thumb(0x0008); // lsls r0, r1, #0
        assert_eq!(info.mnemonic, ARMMnemonic::Lsl);
        assert!(info.affects_cpsr);
        assert_eq!(info.condition, ARMCondition::Al);
        assert_eq!(dis_thumb(0x0008, PC), "lsl r0, r1, #0");
        assert_eq!(dis_thumb(0x09C9, PC), "lsr r1, r1, #7");
    }

    #[test]
    fn thumb_add_sub_mov_cmp() {
        assert_eq!(dis_thumb(0x1800, PC), "add r0, r0, r0"); // add3 form
        assert_eq!(dis_thumb(0x1A40, PC), "sub r0, r0, r1"); // sub3 form
        let info = arm_decode_thumb(0x1C01); // adds r1, r0, #0 (imm3 form)
        assert_eq!(info.mnemonic, ARMMnemonic::Add);
        assert_eq!(info.op3.immediate, 0);
        // ...but like the C skip3 special case, it prints as mov
        assert_eq!(dis_thumb(0x1C01, PC), "mov r1, r0");
        assert_eq!(dis_thumb(0x3801, PC), "sub r0, #1");
        assert_eq!(dis_thumb(0x2101, PC), "mov r1, #1");
        assert_eq!(dis_thumb(0x2801, PC), "cmp r0, #1");
    }

    #[test]
    fn thumb_alu_ops() {
        assert_eq!(dis_thumb(0x4008, PC), "and r0, r1");
        assert_eq!(dis_thumb(0x4240, PC), "neg r0, r0");
        let info = arm_decode_thumb(0x434D); // muls r5, r1
        assert_eq!(info.mnemonic, ARMMnemonic::Mul);
        assert_eq!(dis_thumb(0x434D, PC), "mul r5, r1");
        assert_eq!(dis_thumb(0x4108, PC), "asr r0, r1");
    }

    #[test]
    fn thumb_hi_registers() {
        let info = arm_decode_thumb(0x46C0); // mov r8, r8
        assert_eq!(info.mnemonic, ARMMnemonic::Mov);
        assert!(!info.affects_cpsr);
        assert_eq!(info.op1.reg, 8);
        assert_eq!(info.op2.reg, 8);
        assert_eq!(dis_thumb(0x46C0, PC), "mov r8, r8");
        // h1/h2 flag decoding
        assert_eq!(dis_thumb(0x4408, PC), "add r0, r1");
        assert_eq!(dis_thumb(0x4448, PC), "add r0, r9");
        assert_eq!(dis_thumb(0x4480, PC), "add r8, r0");
        assert_eq!(dis_thumb(0x45C8, PC), "cmp r8, r9");
        // mov pc is an indirect branch, mov lr is not
        let info = arm_decode_thumb(0x46EF); // mov pc, sp
        assert_eq!(info.branch_type, ARM_BRANCH_INDIRECT);
        let info = arm_decode_thumb(0x4670);
        assert_eq!(info.branch_type, ARM_BRANCH_NONE);
    }

    #[test]
    fn thumb_bx() {
        let info = arm_decode_thumb(0x4770); // bx lr
        assert_eq!(info.mnemonic, ARMMnemonic::Bx);
        assert_eq!(info.branch_type, ARM_BRANCH_INDIRECT);
        assert!(arm_instruction_is_branch(info.mnemonic));
        assert_eq!(dis_thumb(0x4770, PC), "bx lr");
    }

    #[test]
    fn thumb_load_store() {
        let info = arm_decode_thumb(0x4800); // ldr r0, [pc, #0]
        assert_eq!(info.mnemonic, ARMMnemonic::Ldr);
        assert_eq!(info.memory.base_reg, REG_PC);
        assert_eq!(info.memory.width, ARM_ACCESS_WORD);
        assert_eq!(dis_thumb(0x4800, 0x08000006), "ldr r0, [0x08000004]");
        assert_eq!(dis_thumb(0x6848, PC), "ldr r0, [r1, #4]"); // imm5 = 1, *4
        assert_eq!(dis_thumb(0x6008, PC), "str r0, [r1, #0]");
        assert_eq!(dis_thumb(0x7A08, PC), "ldrb r0, [r1, #8]"); // imm5 = 8, *1
        assert_eq!(dis_thumb(0x5348, PC), "strh r0, [r1, r5]");
        assert_eq!(dis_thumb(0x5001, PC), "str r1, [r0, r0]");
        assert_eq!(dis_thumb(0x5E08, PC), "ldrsh r0, [r1, r0]");
        assert_eq!(dis_thumb(0x5E81, PC), "ldrsh r1, [r0, r2]");
        assert_eq!(dis_thumb(0x9008, PC), "str r0, [sp, #32]");
        assert_eq!(dis_thumb(0x9808, PC), "ldr r0, [sp, #32]");
    }

    #[test]
    fn thumb_pc_sp_add() {
        assert_eq!(dis_thumb(0xA008, PC), "add r0, pc, #32");
        // add rd, pc, #0 disassembles as mov (the C skip3 special case)
        assert_eq!(dis_thumb(0xA000, PC), "mov r0, pc");
        assert_eq!(dis_thumb(0xA808, PC), "add r0, sp, #32");
        assert_eq!(dis_thumb(0xAF00, PC), "mov r7, sp"); // add r7, sp, #0 also rewrites
        assert_eq!(dis_thumb(0xB008, PC), "add sp, #32");
        assert_eq!(dis_thumb(0xB088, PC), "sub sp, #32");
    }

    #[test]
    fn thumb_push_pop_ldm_stm() {
        let info = arm_decode_thumb(0xB510); // push {r4,lr}
        assert_eq!(info.mnemonic, ARMMnemonic::Stm);
        assert_eq!(info.memory.base_reg, REG_SP);
        assert_eq!(dis_thumb(0xB510, PC), "stmdb sp!, {r4,lr}");
        let info = arm_decode_thumb(0xBD10); // pop {r4,pc}
        assert_eq!(info.mnemonic, ARMMnemonic::Ldm);
        assert_eq!(info.branch_type, ARM_BRANCH_INDIRECT);
        assert_eq!(dis_thumb(0xBD10, PC), "ldmia sp!, {r4,pc}");
        assert_eq!(dis_thumb(0xC010, PC), "stmia r0!, {r4}");
        assert_eq!(dis_thumb(0xC810, PC), "ldmia r0!, {r4}");
    }

    #[test]
    fn thumb_branches() {
        let info = arm_decode_thumb(0xD001); // beq +2
        assert_eq!(info.mnemonic, ARMMnemonic::B);
        assert_eq!(info.condition, ARMCondition::Eq);
        assert_eq!(info.branch_type, ARM_BRANCH);
        assert_eq!(info.op1.immediate, 2);
        assert_eq!(dis_thumb(0xD001, 0x08000004), "beq 0x08000006");
        assert_eq!(dis_thumb(0xD1FE, 0x08000004), "bne 0x08000000");
        // unconditional
        let info = arm_decode_thumb(0xE7FE); // b -4
        assert_eq!(info.condition, ARMCondition::Al);
        assert_eq!(info.op1.immediate, -4);
        assert_eq!(dis_thumb(0xE7FE, 0x08000004), "b 0x08000000");
    }

    #[test]
    fn thumb_swi_bkpt_ill() {
        let info = arm_decode_thumb(0xDF42);
        assert_eq!(info.mnemonic, ARMMnemonic::Swi);
        assert!(info.traps);
        assert_eq!(dis_thumb(0xDF42, PC), "swi #66");
        let info = arm_decode_thumb(0xBE00);
        assert_eq!(info.mnemonic, ARMMnemonic::Bkpt);
        assert!(info.traps);
        let info = arm_decode_thumb(0xDE00); // unconditional-gap is ILL
        assert_eq!(info.mnemonic, ARMMnemonic::Ill);
        assert!(info.traps);
    }

    #[test]
    fn thumb_bl_pair() {
        let info1 = arm_decode_thumb(0xF000);
        assert_eq!(info1.mnemonic, ARMMnemonic::Bl);
        assert_eq!(info1.op1.reg, REG_LR);
        assert_eq!(info1.op2.reg, REG_PC);
        assert_eq!(info1.op3.immediate, 0);
        assert_eq!(info1.branch_type, ARM_BRANCH_NONE);

        let info2 = arm_decode_thumb(0xF800);
        assert_eq!(info2.mnemonic, ARMMnemonic::Bl);
        assert_eq!(info2.op1.reg, REG_PC);
        assert_eq!(info2.op2.reg, REG_LR);
        assert_eq!(info2.branch_type, ARM_BRANCH_LINKED);

        let combined = arm_decode_thumb_combine(&info1, &info2).unwrap();
        assert_eq!(combined.mnemonic, ARMMnemonic::Bl);
        assert_eq!(combined.branch_type, ARM_BRANCH_LINKED);
        assert_eq!(combined.condition, ARMCondition::Al);
        assert_eq!(combined.operand_format, ARM_OPERAND_IMMEDIATE_1);
        assert_eq!(combined.op1.immediate, 0);
        assert_eq!(arm_disassemble(&combined, 0x08000004, None), "bl 0x08000004");

        // Offset spread across the two halves: 0xF7FF/0xFFFF
        let negative1 = arm_decode_thumb(0xF7FF);
        let negative2 = arm_decode_thumb(0xFFFF);
        let combined = arm_decode_thumb_combine(&negative1, &negative2).unwrap();
        assert_eq!(combined.op1.immediate, -2);
        assert_eq!(arm_disassemble(&combined, 0x08000004, None), "bl 0x08000002");

        // Non-pairs are rejected
        assert!(arm_decode_thumb_combine(&info2, &info1).is_none());
        assert!(arm_decode_thumb_combine(&arm_decode_thumb(0x46C0), &info2).is_none());
        assert!(arm_decode_thumb_combine(&info1, &arm_decode_thumb(0xE7FE)).is_none());
    }

    // ----- smoke: decode+disassemble never panics -----

    #[test]
    fn thumb_exhaustive_smoke() {
        for opcode in 0..=0xFFFFu16 {
            let info = arm_decode_thumb(opcode);
            let _ = arm_disassemble(&info, 0x08000004, None);
            let _ = arm_instruction_is_branch(info.mnemonic);
        }
    }

    #[test]
    fn arm_sampled_smoke() {
        // Cover all 4096 table indices with varied filler bits in the fields the
        // index does not cover, plus the leading condition codes.
        let mut seed: u32 = 0x12345678;
        let mut bit = |w: u32| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 16) & ((1 << w) - 1)
        };
        for cond in [0x0u32, 0x1, 0xE, 0xF] {
            for idx in 0..0x1000u32 {
                let rest = bit(20); // rn/rd/shifter/imm fields
                let opcode = (cond << 28)
                    | (((idx >> 4) & 0xFF) << 20)
                    | (rest & 0xFFF0F)
                    | ((idx & 0xF) << 4);
                let info = arm_decode_arm(opcode);
                let _ = arm_disassemble(&info, 0x08000008, None);
                let _ = arm_instruction_is_branch(info.mnemonic);
            }
        }
    }
}

