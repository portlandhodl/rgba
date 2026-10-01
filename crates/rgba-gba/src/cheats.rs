// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gba/cheats.c (GBACheatAddLine dispatch + parse,
// GBACheatAddVBALine, GBACheatAddressIsReal), mgba/src/gba/cheats/gameshark.c
// (GameShark Advance v1: TEA encrypt/decrypt/reseed, raw layout, autodetect
// probability), mgba/src/gba/cheats/parv3.c (Pro Action Replay v3: same TEA
// core with its own seeds/tables, raw layout, probability) and
// mgba/src/gba/cheats/codebreaker.c (CodeBreaker: custom decrypt, raw layout).
//
// Deviations from the C (documented, behavior-preserving where it matters):
// - struct GBACheatHook and its breakpoint application (GBASetBreakpoint /
//   GBAClearBreakpoint via CPU_COMPONENT_CHEAT_DEVICE) are NOT ported: the
//   ARM port has no breakpoint machinery. GbaCheatSet keeps the hook's
//   existence/address so the one-hook-per-set decode semantics stay faithful
//   and hooked sets are skipped at frame end exactly like GBAFrameEnded does;
//   hooked sets just never actually fire.
// - GBACheatDumpDirectives is not ported (only needed by mCheatSaveFile, a
//   frontend/VFile concern; GBACheatParseDirectives is).
// - mCheatParseLibretroFile / mCheatParseEZFChtFile sub-parsers of
//   mCheatParseFile are not ported; Gba::load_cheats handles the native mGBA
//   ".cheats" format only (which is what mCheatSaveFile writes).
// - The C's decorative snprintf()s in GBACheatAddGameShark /
//   GBACheatAddProActionReplay / GBACheatAddAutodetect are dead code and are
//   omitted.

use std::io::{self, BufRead};
use std::path::Path;

use rgba_core::cheats::{Cheat, CheatBus, CheatDevice, CheatPatch, CheatSet, CheatType};
use rgba_core::{mlog, Level};

use crate::gba::Gba;
use crate::io::GBA_REG_KEYINPUT;
use crate::memory::{
    BASE_OFFSET, GBA_BASE_IO, GBA_BASE_ROM0, GBA_BASE_SRAM, GBA_REGION_BIOS, GBA_REGION_EWRAM,
    GBA_REGION_IO, GBA_REGION_IWRAM, GBA_REGION_OAM, GBA_REGION_PALETTE_RAM, GBA_REGION_ROM0,
    GBA_REGION_ROM0_EX, GBA_REGION_ROM1, GBA_REGION_ROM1_EX, GBA_REGION_ROM2, GBA_REGION_ROM2_EX,
    GBA_REGION_SRAM, GBA_REGION_SRAM_MIRROR, GBA_REGION_VRAM, GBA_SIZE_EWRAM, GBA_SIZE_FLASH512,
    GBA_SIZE_IO, GBA_SIZE_IWRAM, GBA_SIZE_OAM, GBA_SIZE_PALETTE_RAM, GBA_SIZE_ROM0, GBA_SIZE_VRAM,
    OFFSET_MASK,
};

/// enum GBACheatType
pub const GBA_CHEAT_AUTODETECT: i32 = 0;
pub const GBA_CHEAT_CODEBREAKER: i32 = 1;
pub const GBA_CHEAT_GAMESHARK: i32 = 2;
pub const GBA_CHEAT_PRO_ACTION_REPLAY: i32 = 3;
pub const GBA_CHEAT_VBA: i32 = 4;

/// enum GBAGameSharkType
const GSA_ASSIGN_1: u32 = 0x0;
const GSA_ASSIGN_2: u32 = 0x1;
const GSA_ASSIGN_4: u32 = 0x2;
const GSA_ASSIGN_LIST: u32 = 0x3;
const GSA_PATCH: u32 = 0x6;
const GSA_BUTTON: u32 = 0x8;
const GSA_IF: u32 = 0xD;
const GSA_IF_RANGE: u32 = 0xE;
const GSA_HOOK: u32 = 0xF;

/// enum GBAGameSharkIfType
const GSA_IF_EQ: u32 = 0;
const GSA_IF_NE: u32 = 1;
const GSA_IF_LE: u32 = 2;
const GSA_IF_GE: u32 = 3;

/// enum GBACodeBreakerType
const CB_GAME_ID: u32 = 0x0;
const CB_HOOK: u32 = 0x1;
const CB_OR_2: u32 = 0x2;
const CB_ASSIGN_1: u32 = 0x3;
const CB_FILL: u32 = 0x4;
const CB_FILL_LIST: u32 = 0x5;
const CB_AND_2: u32 = 0x6;
const CB_IF_EQ: u32 = 0x7;
const CB_ASSIGN_2: u32 = 0x8;
const CB_ENCRYPT: u32 = 0x9;
const CB_IF_NE: u32 = 0xA;
const CB_IF_GT: u32 = 0xB;
const CB_IF_LT: u32 = 0xC;
const CB_IF_SPECIAL: u32 = 0xD;
const CB_ADD_2: u32 = 0xE;
const CB_IF_AND: u32 = 0xF;

/// enum GBAActionReplay3Condition / Width / Action / Base / Other + masks
const PAR3_COND_OTHER: u32 = 0x00000000;
const PAR3_COND_EQ: u32 = 0x08000000;
const PAR3_COND_NE: u32 = 0x10000000;
const PAR3_COND_LT: u32 = 0x18000000;
const PAR3_COND_GT: u32 = 0x20000000;
const PAR3_COND_ULT: u32 = 0x28000000;
const PAR3_COND_UGT: u32 = 0x30000000;
const PAR3_COND_AND: u32 = 0x38000000;

const PAR3_ACTION_NEXT: u32 = 0x00000000;
const PAR3_ACTION_NEXT_TWO: u32 = 0x40000000;
const PAR3_ACTION_BLOCK: u32 = 0x80000000;
const PAR3_ACTION_DISABLE: u32 = 0xC0000000;

const PAR3_BASE_ASSIGN: u32 = 0x00000000;
const PAR3_BASE_INDIRECT: u32 = 0x40000000;
const PAR3_BASE_ADD: u32 = 0x80000000;

const PAR3_OTHER_END: u32 = 0x00000000;
const PAR3_OTHER_SLOWDOWN: u32 = 0x08000000;
const PAR3_OTHER_BUTTON_1: u32 = 0x10000000;
const PAR3_OTHER_BUTTON_2: u32 = 0x12000000;
const PAR3_OTHER_BUTTON_4: u32 = 0x14000000;
const PAR3_OTHER_PATCH_1: u32 = 0x18000000;
const PAR3_OTHER_PATCH_2: u32 = 0x1A000000;
const PAR3_OTHER_PATCH_3: u32 = 0x1C000000;
const PAR3_OTHER_PATCH_4: u32 = 0x1E000000;
const PAR3_OTHER_ENDIF: u32 = 0x40000000;
const PAR3_OTHER_ELSE: u32 = 0x60000000;
const PAR3_OTHER_FILL_1: u32 = 0x80000000;
const PAR3_OTHER_FILL_2: u32 = 0x82000000;
const PAR3_OTHER_FILL_4: u32 = 0x84000000;

const PAR3_COND: u32 = 0x38000000;
const PAR3_WIDTH: u32 = 0x06000000;
const PAR3_ACTION: u32 = 0xC0000000;
const PAR3_BASE: u32 = 0xC0000000;
const PAR3_WIDTH_BASE: u32 = 25;

/// enum GBACheatGameSharkVersion (gba/cheats/gameshark.h)
pub const GBA_GS_NOT_SET: i32 = 0;
pub const GBA_GS_GSAV1: i32 = 1;
pub const GBA_GS_GSAV1_RAW: i32 = 2;
pub const GBA_GS_PARV3: i32 = 3;
pub const GBA_GS_PARV3_RAW: i32 = 4;

/// COMPLETE
const COMPLETE: usize = usize::MAX;

/// MAX_LINE_LENGTH (core/cheats.c)
#[allow(dead_code)]
const MAX_LINE_LENGTH: usize = 512;

pub const GBA_CHEAT_GAME_SHARK_SEEDS: [u32; 4] = [0x09F4FBBD, 0x9681884A, 0x352027E9, 0xF3DEE5A7];
pub const GBA_CHEAT_PRO_ACTION_REPLAY_SEEDS: [u32; 4] =
    [0x7AA9648F, 0x7FAE6994, 0xC0EFAAD5, 0x42712C57];

const GSA1_T1: [u8; 256] = [
    0x31, 0x1C, 0x23, 0xE5, 0x89, 0x8E, 0xA1, 0x37, 0x74, 0x6D, 0x67, 0xFC, 0x1F, 0xC0, 0xB1, 0x94,
    0x3B, 0x05, 0x56, 0x86, 0x00, 0x24, 0xF0, 0x17, 0x72, 0xA2, 0x3D, 0x1B, 0xE3, 0x17, 0xC5, 0x0B,
    0xB9, 0xE2, 0xBD, 0x58, 0x71, 0x1B, 0x2C, 0xFF, 0xE4, 0xC9, 0x4C, 0x5E, 0xC9, 0x55, 0x33, 0x45,
    0x7C, 0x3F, 0xB2, 0x51, 0xFE, 0x10, 0x7E, 0x75, 0x3C, 0x90, 0x8D, 0xDA, 0x94, 0x38, 0xC3, 0xE9,
    0x95, 0xEA, 0xCE, 0xA6, 0x06, 0xE0, 0x4F, 0x3F, 0x2A, 0xE3, 0x3A, 0xE4, 0x43, 0xBD, 0x7F, 0xDA,
    0x55, 0xF0, 0xEA, 0xCB, 0x2C, 0xA8, 0x47, 0x61, 0xA0, 0xEF, 0xCB, 0x13, 0x18, 0x20, 0xAF, 0x3E,
    0x4D, 0x9E, 0x1E, 0x77, 0x51, 0xC5, 0x51, 0x20, 0xCF, 0x21, 0xF9, 0x39, 0x94, 0xDE, 0xDD, 0x79,
    0x4E, 0x80, 0xC4, 0x9D, 0x94, 0xD5, 0x95, 0x01, 0x27, 0x27, 0xBD, 0x6D, 0x78, 0xB5, 0xD1, 0x31,
    0x6A, 0x65, 0x74, 0x74, 0x58, 0xB3, 0x7C, 0xC9, 0x5A, 0xED, 0x50, 0x03, 0xC4, 0xA2, 0x94, 0x4B,
    0xF0, 0x58, 0x09, 0x6F, 0x3E, 0x7D, 0xAE, 0x7D, 0x58, 0xA0, 0x2C, 0x91, 0xBB, 0xE1, 0x70, 0xEB,
    0x73, 0xA6, 0x9A, 0x44, 0x25, 0x90, 0x16, 0x62, 0x53, 0xAE, 0x08, 0xEB, 0xDC, 0xF0, 0xEE, 0x77,
    0xC2, 0xDE, 0x81, 0xE8, 0x30, 0x89, 0xDB, 0xFE, 0xBC, 0xC2, 0xDF, 0x26, 0xE9, 0x8B, 0xD6, 0x93,
    0xF0, 0xCB, 0x56, 0x90, 0xC0, 0x46, 0x68, 0x15, 0x43, 0xCB, 0xE9, 0x98, 0xE3, 0xAF, 0x31, 0x25,
    0x4D, 0x7B, 0xF3, 0xB1, 0x74, 0xE2, 0x64, 0xAC, 0xD9, 0xF6, 0xA0, 0xD5, 0x0B, 0x9B, 0x49, 0x52,
    0x69, 0x3B, 0x71, 0x00, 0x2F, 0xBB, 0xBA, 0x08, 0xB1, 0xAE, 0xBB, 0xB3, 0xE1, 0xC9, 0xA6, 0x7F,
    0x17, 0x97, 0x28, 0x72, 0x12, 0x6E, 0x91, 0xAE, 0x3A, 0xA2, 0x35, 0x46, 0x27, 0xF8, 0x12, 0x50,
];

const GSA1_T2: [u8; 256] = [
    0xD8, 0x65, 0x04, 0xC2, 0x65, 0xD5, 0xB0, 0x0C, 0xDF, 0x9D, 0xF0, 0xC3, 0x9A, 0x17, 0xC9, 0xA6,
    0xE1, 0xAC, 0x0D, 0x14, 0x2F, 0x3C, 0x2C, 0x87, 0xA2, 0xBF, 0x4D, 0x5F, 0xAC, 0x2D, 0x9D, 0xE1,
    0x0C, 0x9C, 0xE7, 0x7F, 0xFC, 0xA8, 0x66, 0x59, 0xAC, 0x18, 0xD7, 0x05, 0xF0, 0xBF, 0xD1, 0x8B,
    0x35, 0x9F, 0x59, 0xB4, 0xBA, 0x55, 0xB2, 0x85, 0xFD, 0xB1, 0x72, 0x06, 0x73, 0xA4, 0xDB, 0x48,
    0x7B, 0x5F, 0x67, 0xA5, 0x95, 0xB9, 0xA5, 0x4A, 0xCF, 0xD1, 0x44, 0xF3, 0x81, 0xF5, 0x6D, 0xF6,
    0x3A, 0xC3, 0x57, 0x83, 0xFA, 0x8E, 0x15, 0x2A, 0xA2, 0x04, 0xB2, 0x9D, 0xA8, 0x0D, 0x7F, 0xB8,
    0x0F, 0xF6, 0xAC, 0xBE, 0x97, 0xCE, 0x16, 0xE6, 0x31, 0x10, 0x60, 0x16, 0xB5, 0x83, 0x45, 0xEE,
    0xD7, 0x5F, 0x2C, 0x08, 0x58, 0xB1, 0xFD, 0x7E, 0x79, 0x00, 0x34, 0xAD, 0xB5, 0x31, 0x34, 0x39,
    0xAF, 0xA8, 0xDD, 0x52, 0x6A, 0xB0, 0x60, 0x35, 0xB8, 0x1D, 0x52, 0xF5, 0xF5, 0x30, 0x00, 0x7B,
    0xF4, 0xBA, 0x03, 0xCB, 0x3A, 0x84, 0x14, 0x8A, 0x6A, 0xEF, 0x21, 0xBD, 0x01, 0xD8, 0xA0, 0xD4,
    0x43, 0xBE, 0x23, 0xE7, 0x76, 0x27, 0x2C, 0x3F, 0x4D, 0x3F, 0x43, 0x18, 0xA7, 0xC3, 0x47, 0xA5,
    0x7A, 0x1D, 0x02, 0x55, 0x09, 0xD1, 0xFF, 0x55, 0x5E, 0x17, 0xA0, 0x56, 0xF4, 0xC9, 0x6B, 0x90,
    0xB4, 0x80, 0xA5, 0x07, 0x22, 0xFB, 0x22, 0x0D, 0xD9, 0xC0, 0x5B, 0x08, 0x35, 0x05, 0xC1, 0x75,
    0x4F, 0xD0, 0x51, 0x2D, 0x2E, 0x5E, 0x69, 0xE7, 0x3B, 0xC2, 0xDA, 0xFF, 0xF6, 0xCE, 0x3E, 0x76,
    0xE8, 0x36, 0x8C, 0x39, 0xD8, 0xF3, 0xE9, 0xA6, 0x42, 0xE6, 0xC1, 0x4C, 0x05, 0xBE, 0x17, 0xF2,
    0x5C, 0x1B, 0x19, 0xDB, 0x0F, 0xF3, 0xF8, 0x49, 0xEB, 0x36, 0xF6, 0x40, 0x6F, 0xAD, 0xC1, 0x8C,
];

const PAR3_T1: [u8; 256] = [
    0xD0, 0xFF, 0xBA, 0xE5, 0xC1, 0xC7, 0xDB, 0x5B, 0x16, 0xE3, 0x6E, 0x26, 0x62, 0x31, 0x2E, 0x2A,
    0xD1, 0xBB, 0x4A, 0xE6, 0xAE, 0x2F, 0x0A, 0x90, 0x29, 0x90, 0xB6, 0x67, 0x58, 0x2A, 0xB4, 0x45,
    0x7B, 0xCB, 0xF0, 0x73, 0x84, 0x30, 0x81, 0xC2, 0xD7, 0xBE, 0x89, 0xD7, 0x4E, 0x73, 0x5C, 0xC7,
    0x80, 0x1B, 0xE5, 0xE4, 0x43, 0xC7, 0x46, 0xD6, 0x6F, 0x7B, 0xBF, 0xED, 0xE5, 0x27, 0xD1, 0xB5,
    0xD0, 0xD8, 0xA3, 0xCB, 0x2B, 0x30, 0xA4, 0xF0, 0x84, 0x14, 0x72, 0x5C, 0xFF, 0xA4, 0xFB, 0x54,
    0x9D, 0x70, 0xE2, 0xFF, 0xBE, 0xE8, 0x24, 0x76, 0xE5, 0x15, 0xFB, 0x1A, 0xBC, 0x87, 0x02, 0x2A,
    0x58, 0x8F, 0x9A, 0x95, 0xBD, 0xAE, 0x8D, 0x0C, 0xA5, 0x4C, 0xF2, 0x5C, 0x7D, 0xAD, 0x51, 0xFB,
    0xB1, 0x22, 0x07, 0xE0, 0x29, 0x7C, 0xEB, 0x98, 0x14, 0xC6, 0x31, 0x97, 0xE4, 0x34, 0x8F, 0xCC,
    0x99, 0x56, 0x9F, 0x78, 0x43, 0x91, 0x85, 0x3F, 0xC2, 0xD0, 0xD1, 0x80, 0xD1, 0x77, 0xA7, 0xE2,
    0x43, 0x99, 0x1D, 0x2F, 0x8B, 0x6A, 0xE4, 0x66, 0x82, 0xF7, 0x2B, 0x0B, 0x65, 0x14, 0xC0, 0xC2,
    0x1D, 0x96, 0x78, 0x1C, 0xC4, 0xC3, 0xD2, 0xB1, 0x64, 0x07, 0xD7, 0x6F, 0x02, 0xE9, 0x44, 0x31,
    0xDB, 0x3C, 0xEB, 0x93, 0xED, 0x9A, 0x57, 0x05, 0xB9, 0x0E, 0xAF, 0x1F, 0x48, 0x11, 0xDC, 0x35,
    0x6C, 0xB8, 0xEE, 0x2A, 0x48, 0x2B, 0xBC, 0x89, 0x12, 0x59, 0xCB, 0xD1, 0x18, 0xEA, 0x72, 0x11,
    0x01, 0x75, 0x3B, 0xB5, 0x56, 0xF4, 0x8B, 0xA0, 0x41, 0x75, 0x86, 0x7B, 0x94, 0x12, 0x2D, 0x4C,
    0x0C, 0x22, 0xC9, 0x4A, 0xD8, 0xB1, 0x8D, 0xF0, 0x55, 0x2E, 0x77, 0x50, 0x1C, 0x64, 0x77, 0xAA,
    0x3E, 0xAC, 0xD3, 0x3D, 0xCE, 0x60, 0xCA, 0x5D, 0xA0, 0x92, 0x78, 0xC6, 0x51, 0xFE, 0xF9, 0x30,
];

const PAR3_T2: [u8; 256] = [
    0xAA, 0xAF, 0xF0, 0x72, 0x90, 0xF7, 0x71, 0x27, 0x06, 0x11, 0xEB, 0x9C, 0x37, 0x12, 0x72, 0xAA,
    0x65, 0xBC, 0x0D, 0x4A, 0x76, 0xF6, 0x5C, 0xAA, 0xB0, 0x7A, 0x7D, 0x81, 0xC1, 0xCE, 0x2F, 0x9F,
    0x02, 0x75, 0x38, 0xC8, 0xFC, 0x66, 0x05, 0xC2, 0x2C, 0xBD, 0x91, 0xAD, 0x03, 0xB1, 0x88, 0x93,
    0x31, 0xC6, 0xAB, 0x40, 0x23, 0x43, 0x76, 0x54, 0xCA, 0xE7, 0x00, 0x96, 0x9F, 0xD8, 0x24, 0x8B,
    0xE4, 0xDC, 0xDE, 0x48, 0x2C, 0xCB, 0xF7, 0x84, 0x1D, 0x45, 0xE5, 0xF1, 0x75, 0xA0, 0xED, 0xCD,
    0x4B, 0x24, 0x8A, 0xB3, 0x98, 0x7B, 0x12, 0xB8, 0xF5, 0x63, 0x97, 0xB3, 0xA6, 0xA6, 0x0B, 0xDC,
    0xD8, 0x4C, 0xA8, 0x99, 0x27, 0x0F, 0x8F, 0x94, 0x63, 0x0F, 0xB0, 0x11, 0x94, 0xC7, 0xE9, 0x7F,
    0x3B, 0x40, 0x72, 0x4C, 0xDB, 0x84, 0x78, 0xFE, 0xB8, 0x56, 0x08, 0x80, 0xDF, 0x20, 0x2F, 0xB9,
    0x66, 0x2D, 0x60, 0x63, 0xF5, 0x18, 0x15, 0x1B, 0x86, 0x85, 0xB9, 0xB4, 0x68, 0x0E, 0xC6, 0xD1,
    0x8A, 0x81, 0x2B, 0xB3, 0xF6, 0x48, 0xF0, 0x4F, 0x9C, 0x28, 0x1C, 0xA4, 0x51, 0x2F, 0xD7, 0x4B,
    0x17, 0xE7, 0xCC, 0x50, 0x9F, 0xD0, 0xD1, 0x40, 0x0C, 0x0D, 0xCA, 0x83, 0xFA, 0x5E, 0xCA, 0xEC,
    0xBF, 0x4E, 0x7C, 0x8F, 0xF0, 0xAE, 0xC2, 0xD3, 0x28, 0x41, 0x9B, 0xC8, 0x04, 0xB9, 0x4A, 0xBA,
    0x72, 0xE2, 0xB5, 0x06, 0x2C, 0x1E, 0x0B, 0x2C, 0x7F, 0x11, 0xA9, 0x26, 0x51, 0x9D, 0x3F, 0xF8,
    0x62, 0x11, 0x2E, 0x89, 0xD2, 0x9D, 0x35, 0xB1, 0xE4, 0x0A, 0x4D, 0x93, 0x01, 0xA7, 0xD1, 0x2D,
    0x00, 0x87, 0xE2, 0x2D, 0xA4, 0xE9, 0x0A, 0x06, 0x66, 0xF8, 0x1F, 0x44, 0x75, 0xB5, 0x6B, 0x1C,
    0xFC, 0x31, 0x09, 0x48, 0xA3, 0xFF, 0x92, 0x12, 0x58, 0xE9, 0xFA, 0xAE, 0x4F, 0xE2, 0xB4, 0xCC,
];

/// struct GBACheatSet's GBA-specific half. `CheatSet` is the embedded `d`
/// field; the two are produced together by `gba_cheat_set_create` and travel
/// as a tuple (device.list/decoding state) because the shared
/// `rgba_core::cheats::CheatDevice` stores plain `CheatSet`s.
pub struct GbaCheatSet {
    pub gsa_version: i32,
    gsa_seeds: [u32; 4],
    cb_rng_state: u32,
    cb_master: u32,
    cb_table: [u8; 0x30],
    cb_seeds: [u32; 4],
    /// Index of the list entry awaiting a second line, or COMPLETE.
    incomplete_cheat: usize,
    /// Index into rom_patches awaiting its value line.
    incomplete_patch: Option<usize>,
    current_block: usize,
    remaining_addresses: i32,
    /// struct GBACheatHook reduced to "hooked ROM address" — the breakpoint
    /// application behind it (GBASetBreakpoint) is not ported. Field order
    /// per the C struct.
    hook: Option<u32>,
}

impl GbaCheatSet {
    /// The GBA-field half of GBACheatSetCreate (mCheatSetInit is
    /// CheatSet::new). C memset-family defaults map to Rust zeroes.
    fn new() -> Self {
        Self {
            gsa_version: GBA_GS_NOT_SET,
            gsa_seeds: [0; 4],
            cb_rng_state: 0,
            cb_master: 0,
            cb_table: [0; 0x30],
            cb_seeds: [0; 4],
            incomplete_cheat: COMPLETE,
            incomplete_patch: None,
            current_block: COMPLETE,
            remaining_addresses: 0,
            hook: None,
        }
    }

    /// GBACheatSetGameSharkVersion
    pub fn set_game_shark_version(&mut self, version: i32) {
        self.gsa_version = version;
        match version {
            GBA_GS_GSAV1 | GBA_GS_GSAV1_RAW => self.gsa_seeds = GBA_CHEAT_GAME_SHARK_SEEDS,
            GBA_GS_PARV3 | GBA_GS_PARV3_RAW => self.gsa_seeds = GBA_CHEAT_PRO_ACTION_REPLAY_SEEDS,
            _ => {}
        }
    }

    /// GBACheatSetCopyProperties
    pub fn copy_properties(&mut self, old: &GbaCheatSet) {
        self.gsa_version = old.gsa_version;
        self.gsa_seeds = old.gsa_seeds;
        self.cb_rng_state = old.cb_rng_state;
        self.cb_master = old.cb_master;
        self.cb_seeds = old.cb_seeds;
        self.cb_table = old.cb_table;
        // The C refcounts and shares the hook; it's plain data here.
        if old.hook.is_some() {
            self.hook = old.hook;
        }
    }

    /// GBACheatParseDirectives: "!GSAv1" / "!PARv3 raw" style lines.
    pub fn parse_directives(&mut self, directives: &[String]) {
        for directive in directives {
            match directive.as_str() {
                "GSAv1" => self.set_game_shark_version(GBA_GS_GSAV1),
                "GSAv1 raw" => self.set_game_shark_version(GBA_GS_GSAV1_RAW),
                "PARv3" => self.set_game_shark_version(GBA_GS_PARV3),
                "PARv3 raw" => self.set_game_shark_version(GBA_GS_PARV3_RAW),
                _ => {}
            }
        }
    }
}

impl Default for GbaCheatSet {
    fn default() -> Self {
        Self::new()
    }
}

/// GBACheatDeviceCreate. The C hooks device->createSet up to
/// GBACheatSetCreate; here the set/state pair comes from
/// `gba_cheat_set_create` and enters the console via `Gba::cheat_add_set`,
/// which keeps `gba.cheats` and `gba.gba_cheat_sets` aligned.
pub fn gba_cheat_device_create() -> CheatDevice {
    CheatDevice::new()
}

/// GBACheatSetCreate: returns (set, GBA-side state), mirroring the
/// malloc'd struct that embeds mCheatSet.
pub fn gba_cheat_set_create(name: &str) -> (CheatSet, GbaCheatSet) {
    (CheatSet::new(name), GbaCheatSet::new())
}

/* hex helpers (hex8/hex16/32 of mgba-util/string.c, GB cheats.rs shape) */

fn hex_digit(digit: u8) -> i32 {
    match digit {
        b'0'..=b'9' => (digit - b'0') as i32,
        b'a'..=b'f' => (digit - b'a' + 10) as i32,
        b'A'..=b'F' => (digit - b'A' + 10) as i32,
        _ => -1,
    }
}

fn hex8(line: &[u8], pos: usize) -> Option<(u8, usize)> {
    let mut value = 0u8;
    for i in 0..2 {
        let digit = *line.get(pos + i)?;
        let nybble = hex_digit(digit);
        if nybble < 0 {
            return None;
        }
        value = value << 4 | nybble as u8;
    }
    Some((value, pos + 2))
}

fn hex16(line: &[u8], pos: usize) -> Option<(u16, usize)> {
    let mut value = 0u16;
    for i in 0..4 {
        let digit = *line.get(pos + i)?;
        let nybble = hex_digit(digit);
        if nybble < 0 {
            return None;
        }
        value = value << 4 | nybble as u16;
    }
    Some((value, pos + 4))
}

fn hex32(line: &[u8], pos: usize) -> Option<(u32, usize)> {
    let mut value = 0u32;
    for i in 0..8 {
        let digit = *line.get(pos + i)?;
        let nybble = hex_digit(digit);
        if nybble < 0 {
            return None;
        }
        value = value << 4 | nybble as u32;
    }
    Some((value, pos + 8))
}

/// C locale isspace (also \v). Used by the GBACheatAddLine parser and
/// mCheatParseFile's rtrim/leading-skip.
fn is_c_space(b: u8) -> bool {
    b == b' ' || (0x09..=0x0D).contains(&b)
}

/// mCheatListAppend, index-returning (the C returns a pointer; the port is
/// index-based because incompleteCheat/currentBlock track entries). The C
/// leaves appended entries uninitialized and fills them piecemeal; the port
/// zero-initializes like the GB decoder does (the offsets a C garbage value
/// would otherwise give are never read before being set).
fn push_cheat(cheats: &mut CheatSet) -> usize {
    cheats.list.push(Cheat {
        typ: CheatType::Assign,
        width: 0,
        address: 0,
        operand: 0,
        repeat: 0,
        negative_repeat: 0,
        address_offset: 0,
        operand_offset: 0,
    });
    cheats.list.len() - 1
}

/* ================= GameShark (mgba/src/gba/cheats/gameshark.c) ================ */

/// GBACheatEncryptGameShark
/// (http://en.wikipedia.org/wiki/Tiny_Encryption_Algorithm)
pub fn gba_cheat_encrypt_game_shark(op1: &mut u32, op2: &mut u32, seeds: &[u32; 4]) {
    let mut sum = 0u32;
    for _ in 0..32 {
        sum = sum.wrapping_add(0x9E3779B9);
        *op1 = op1.wrapping_add(
            (op2.wrapping_shl(4).wrapping_add(seeds[0]))
                ^ (op2.wrapping_add(sum))
                ^ (op2.wrapping_shr(5).wrapping_add(seeds[1])),
        );
        *op2 = op2.wrapping_add(
            (op1.wrapping_shl(4).wrapping_add(seeds[2]))
                ^ (op1.wrapping_add(sum))
                ^ (op1.wrapping_shr(5).wrapping_add(seeds[3])),
        );
    }
    // Dead final store, preserved from the C.
    let _ = sum.wrapping_add(0xC6EF3720);
}

/// GBACheatDecryptGameShark
pub fn gba_cheat_decrypt_game_shark(op1: &mut u32, op2: &mut u32, seeds: &[u32; 4]) {
    let mut sum = 0xC6EF3720u32;
    for _ in 0..32 {
        *op2 = op2.wrapping_sub(
            (op1.wrapping_shl(4).wrapping_add(seeds[2]))
                ^ (op1.wrapping_add(sum))
                ^ (op1.wrapping_shr(5).wrapping_add(seeds[3])),
        );
        *op1 = op1.wrapping_sub(
            (op2.wrapping_shl(4).wrapping_add(seeds[0]))
                ^ (op2.wrapping_add(sum))
                ^ (op2.wrapping_shr(5).wrapping_add(seeds[1])),
        );
        sum = sum.wrapping_sub(0x9E3779B9);
    }
}

/// GBACheatReseedGameShark
pub fn gba_cheat_reseed_game_shark(
    seeds: &mut [u32; 4],
    params: u16,
    t1: &[u8; 256],
    t2: &[u8; 256],
) {
    let s0 = (params >> 8) as usize;
    let s1 = (params & 0xFF) as usize;
    for y in 0..4 {
        for x in 0..4u32 {
            let s_x = x as usize;
            // uint8_t z = t1[...] + t2[...] — wraps like the C.
            let z = t1[(s0 + s_x) & 0xFF].wrapping_add(t2[(s1 + y) & 0xFF]);
            seeds[y] = seeds[y].wrapping_shl(8) | z as u32;
        }
    }
}

/// GBACheatAddGameSharkRaw
pub fn gba_cheat_add_game_shark_raw(
    cheats: &mut CheatSet,
    state: &mut GbaCheatSet,
    op1: u32,
    op2: u32,
) -> bool {
    let typ = op1 >> 28;

    if state.incomplete_cheat != COMPLETE {
        let incomplete_operand = cheats.list[state.incomplete_cheat].operand;
        if state.remaining_addresses > 0 {
            let idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::Assign;
            cheats.list[idx].width = 4;
            cheats.list[idx].address = op1;
            cheats.list[idx].operand = incomplete_operand;
            cheats.list[idx].repeat = 1;
            state.remaining_addresses -= 1;
        }
        if state.remaining_addresses > 0 {
            let idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::Assign;
            cheats.list[idx].width = 4;
            cheats.list[idx].address = op2;
            cheats.list[idx].operand = incomplete_operand;
            cheats.list[idx].repeat = 1;
            state.remaining_addresses -= 1;
        }
        if state.remaining_addresses == 0 {
            state.incomplete_cheat = COMPLETE;
        }
        return true;
    }

    let idx;
    match typ {
        GSA_ASSIGN_1 => {
            idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::Assign;
            cheats.list[idx].width = 1;
            cheats.list[idx].address = op1 & 0x0FFFFFFF;
        }
        GSA_ASSIGN_2 => {
            idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::Assign;
            cheats.list[idx].width = 2;
            cheats.list[idx].address = op1 & 0x0FFFFFFF;
        }
        GSA_ASSIGN_4 => {
            idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::Assign;
            cheats.list[idx].width = 4;
            cheats.list[idx].address = op1 & 0x0FFFFFFF;
        }
        GSA_ASSIGN_LIST => {
            state.remaining_addresses = (op1 & 0xFFFF) as i32 - 1;
            idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::Assign;
            cheats.list[idx].width = 4;
            cheats.list[idx].address = op2;
            state.incomplete_cheat = idx;
        }
        GSA_PATCH => {
            cheats.rom_patches.push(CheatPatch {
                address: GBA_BASE_ROM0 | ((op1 & 0xFFFFFF) << 1),
                segment: 0,
                value: op2,
                width: 2,
                applied: false,
                check_value: 0,
                check: false,
            });
            return true;
        }
        GSA_BUTTON => match op1 & 0x00F00000 {
            0x00100000 => {
                let idx_b = push_cheat(cheats);
                cheats.list[idx_b].typ = CheatType::IfButton;
                cheats.list[idx_b].repeat = 1;
                cheats.list[idx_b].negative_repeat = 0;
                idx = push_cheat(cheats);
                cheats.list[idx].typ = CheatType::Assign;
                cheats.list[idx].width = 1;
                cheats.list[idx].address = op1 & 0x0F0FFFFF;
            }
            0x00200000 => {
                let idx_b = push_cheat(cheats);
                cheats.list[idx_b].typ = CheatType::IfButton;
                cheats.list[idx_b].repeat = 1;
                cheats.list[idx_b].negative_repeat = 0;
                idx = push_cheat(cheats);
                cheats.list[idx].typ = CheatType::Assign;
                cheats.list[idx].width = 2;
                cheats.list[idx].address = op1 & 0x0F0FFFFF;
            }
            _ => {
                mlog!(
                    Level::Stub,
                    rgba_core::log::GBA_CHEAT,
                    "GameShark button type unimplemented"
                );
                return false;
            }
        },
        GSA_IF => {
            if op1 == 0xDEADFACE {
                gba_cheat_reseed_game_shark(&mut state.gsa_seeds, op2 as u16, &GSA1_T1, &GSA1_T2);
                return true;
            }
            let idx_c = push_cheat(cheats);
            match op2 >> 20 {
                GSA_IF_EQ => cheats.list[idx_c].typ = CheatType::IfEq,
                GSA_IF_NE => cheats.list[idx_c].typ = CheatType::IfNe,
                GSA_IF_LE => cheats.list[idx_c].typ = CheatType::IfLe,
                GSA_IF_GE => cheats.list[idx_c].typ = CheatType::IfGe,
                _ => {} // C leaves the uninitialized type as-is; keep the zeroed default.
            }
            cheats.list[idx_c].width = 2;
            cheats.list[idx_c].repeat = 1;
            cheats.list[idx_c].negative_repeat = 0;
            cheats.list[idx_c].address = op1 & 0x0FFFFFFF;
            cheats.list[idx_c].operand = op2 & 0xFFFF;
            return true;
        }
        GSA_IF_RANGE => {
            let idx_c = push_cheat(cheats);
            cheats.list[idx_c].typ = CheatType::IfEq;
            cheats.list[idx_c].width = 2;
            cheats.list[idx_c].address = op2 & 0x0FFFFFFF;
            cheats.list[idx_c].operand = op1 & 0xFFFF;
            cheats.list[idx_c].repeat = (op1 >> 16) & 0xFF;
            cheats.list[idx_c].negative_repeat = 0;
            return true;
        }
        GSA_HOOK => {
            if state.hook.is_some() {
                return false;
            }
            // GBACheatHook alloc: address/mode/refs/reentries; PORT: address
            // only (breakpoint application not ported).
            state.hook = Some(GBA_BASE_ROM0 | (op1 & (GBA_SIZE_ROM0 as u32 - 1)));
            return true;
        }
        _ => return false,
    }
    cheats.list[idx].operand = op2;
    cheats.list[idx].repeat = 1;
    cheats.list[idx].negative_repeat = 0;
    true
}

/// GBACheatAddGameShark
pub fn gba_cheat_add_game_shark(
    cheats: &mut CheatSet,
    state: &mut GbaCheatSet,
    op1: u32,
    op2: u32,
) -> bool {
    let mut o1 = op1;
    let mut o2 = op2;

    // C switch with fall-through: default → set GSAV1 first, GSAV1 decrypts,
    // GSAV1_RAW goes straight to the raw layout.
    match state.gsa_version {
        GBA_GS_GSAV1_RAW => return gba_cheat_add_game_shark_raw(cheats, state, o1, o2),
        GBA_GS_GSAV1 => {}
        _ => state.set_game_shark_version(GBA_GS_GSAV1),
    }
    gba_cheat_decrypt_game_shark(&mut o1, &mut o2, &state.gsa_seeds);
    gba_cheat_add_game_shark_raw(cheats, state, o1, o2)
}

/// GBACheatAddGameSharkLine: "XXXXXXXX XXXXXXXX".
pub fn gba_cheat_add_game_shark_line(
    cheats: &mut CheatSet,
    state: &mut GbaCheatSet,
    line: &str,
) -> bool {
    let b = line.as_bytes();
    let Some((op1, mut pos)) = hex32(b, 0) else {
        return false;
    };
    while pos < b.len() && b[pos] == b' ' {
        pos += 1;
    }
    let Some((op2, _)) = hex32(b, pos) else {
        return false;
    };
    gba_cheat_add_game_shark(cheats, state, op1, op2)
}

/// GBACheatGameSharkProbability
pub fn gba_cheat_game_shark_probability(op1: u32, op2: u32) -> i32 {
    let mut probability = 0;
    if op2 == 0x001DC0DE {
        return 0x100;
    }
    let address = op1 & 0x0FFFFFFF;
    match op1 >> 28 {
        GSA_ASSIGN_1 => {
            probability += 0x20;
            if op2 & 0xFFFFFF00 != 0 {
                probability -= 0x10;
            }
            probability += gba_cheat_address_is_real(address);
        }
        GSA_ASSIGN_2 => {
            probability += 0x20;
            if op2 & 0xFFFF0000 != 0 {
                probability -= 0x10;
            }
            probability += gba_cheat_address_is_real(address);
        }
        GSA_ASSIGN_4 => {
            probability += 0x20;
            probability += gba_cheat_address_is_real(address);
        }
        GSA_PATCH => {
            probability += 0x20;
            if op2 & 0xCFFF0000 != 0 {
                probability -= 0x10;
            }
        }
        GSA_BUTTON => {
            probability += 0x10;
        }
        GSA_IF => {
            probability += 0x20;
            if op2 & 0xFFCF0000 != 0 {
                probability -= 0x10;
            }
            probability += gba_cheat_address_is_real(address);
        }
        GSA_IF_RANGE => {
            probability += 0x20;
            probability += gba_cheat_address_is_real(op2);
            if op1 & 0x0F000000 != 0 {
                probability -= 0x10;
            }
        }
        GSA_HOOK => {
            probability += 0x20;
            if op2 & 0xFFFF0000 != 0 {
                probability -= 0x10;
            }
        }
        _ => {
            probability -= 0x40;
        }
    }
    probability
}

/* ================ Pro Action Replay v3 (gba/cheats/parv3.c) ================= */

/// _parAddr
fn par_addr(x: u32) -> u32 {
    (x & 0xFFFFF) | ((x << 4) & 0x0F000000)
}

/// The C masks `op2`/`op1` by `0xFFFFFFFFU >> ((4 - width) * 8)`. For the
/// out-of-range widths the C shift is negative (undefined, in practice x86's
/// masked-shifts give 0xFFFFFFFF); keeping wrapping_shr matches that.
fn width_mask(width: i32) -> u32 {
    0xFFFF_FFFFu32.wrapping_shr(((4 - width) * 8) as u32)
}

/// _parEndBlock
fn par_end_block(cheats: &mut CheatSet, state: &mut GbaCheatSet) {
    let size = (cheats.list.len() - state.current_block - 1) as u32;
    let block = &mut cheats.list[state.current_block];
    if block.repeat != 0 {
        block.negative_repeat = size.wrapping_sub(block.repeat);
    } else {
        block.repeat = size;
    }
    state.current_block = COMPLETE;
}

/// _parElseBlock
fn par_else_block(cheats: &mut CheatSet, state: &mut GbaCheatSet) {
    let size = (cheats.list.len() - state.current_block - 1) as u32;
    cheats.list[state.current_block].repeat = size;
}

/// _addPAR3Cond
fn add_par3_cond(cheats: &mut CheatSet, state: &mut GbaCheatSet, op1: u32, op2: u32) -> bool {
    let condition = op1 & PAR3_COND;
    let width = 1i32 << ((op1 & PAR3_WIDTH) >> PAR3_WIDTH_BASE);
    if op1 & PAR3_ACTION == PAR3_ACTION_DISABLE {
        // TODO: Codes that disable
        mlog!(
            Level::Stub,
            rgba_core::log::GBA_CHEAT,
            "Disable-type PARv3 codes not yet supported"
        );
        return false;
    }

    let idx = push_cheat(cheats);
    cheats.list[idx].address = par_addr(op1);
    cheats.list[idx].width = width;
    cheats.list[idx].operand = op2 & width_mask(width);
    cheats.list[idx].address_offset = 0;
    cheats.list[idx].operand_offset = 0;

    match op1 & PAR3_ACTION {
        PAR3_ACTION_NEXT => {
            cheats.list[idx].repeat = 1;
            cheats.list[idx].negative_repeat = 0;
        }
        PAR3_ACTION_NEXT_TWO => {
            cheats.list[idx].repeat = 2;
            cheats.list[idx].negative_repeat = 0;
        }
        _ => {
            // PAR3_ACTION_BLOCK (PAR3_ACTION_DISABLE returned above)
            cheats.list[idx].repeat = 0;
            cheats.list[idx].negative_repeat = 0;
            if state.current_block != COMPLETE {
                par_end_block(cheats, state);
            }
            state.current_block = idx;
        }
    }

    cheats.list[idx].typ = match condition {
        PAR3_COND_OTHER => {
            // We shouldn't be able to get here
            mlog!(
                Level::Error,
                rgba_core::log::GBA_CHEAT,
                "Unexpectedly created 'other' PARv3 code"
            );
            cheats.list[idx].operand = 0;
            CheatType::IfLand
        }
        PAR3_COND_EQ => CheatType::IfEq,
        PAR3_COND_NE => CheatType::IfNe,
        PAR3_COND_LT => CheatType::IfLt,
        PAR3_COND_GT => CheatType::IfGt,
        PAR3_COND_ULT => CheatType::IfUlt,
        PAR3_COND_UGT => CheatType::IfUgt,
        _ => CheatType::IfAnd, // PAR3_COND_AND
    };

    if width > 4 {
        cheats.list[idx].width = 0;
        cheats.list[idx].typ = CheatType::Never;
    }
    true
}

/// _addPAR3Special
fn add_par3_special(cheats: &mut CheatSet, state: &mut GbaCheatSet, op2: u32) -> bool {
    let mut rom_patch = -1;
    match op2 & 0xFF000000 {
        PAR3_OTHER_SLOWDOWN => {
            // TODO: Slowdown
            mlog!(
                Level::Stub,
                rgba_core::log::GBA_CHEAT,
                "Unimplemented PARv3 slowdown"
            );
            return false;
        }
        PAR3_OTHER_BUTTON_1 => {
            let idx_b = push_cheat(cheats);
            cheats.list[idx_b].typ = CheatType::IfButton;
            cheats.list[idx_b].repeat = 1;
            cheats.list[idx_b].negative_repeat = 0;
            let idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::Assign;
            cheats.list[idx].width = 1;
            cheats.list[idx].repeat = 1;
            cheats.list[idx].address = par_addr(op2);
            state.incomplete_cheat = idx;
        }
        PAR3_OTHER_BUTTON_2 => {
            let idx_b = push_cheat(cheats);
            cheats.list[idx_b].typ = CheatType::IfButton;
            cheats.list[idx_b].repeat = 1;
            cheats.list[idx_b].negative_repeat = 0;
            let idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::Assign;
            cheats.list[idx].width = 2;
            cheats.list[idx].repeat = 1;
            cheats.list[idx].address = par_addr(op2);
            state.incomplete_cheat = idx;
        }
        PAR3_OTHER_BUTTON_4 => {
            let idx_b = push_cheat(cheats);
            cheats.list[idx_b].typ = CheatType::IfButton;
            cheats.list[idx_b].repeat = 1;
            cheats.list[idx_b].negative_repeat = 0;
            let idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::Assign;
            cheats.list[idx].width = 4;
            cheats.list[idx].repeat = 1;
            cheats.list[idx].address = par_addr(op2);
            state.incomplete_cheat = idx;
        }
        PAR3_OTHER_PATCH_1 => rom_patch = 0,
        PAR3_OTHER_PATCH_2 => rom_patch = 1,
        PAR3_OTHER_PATCH_3 => rom_patch = 2,
        PAR3_OTHER_PATCH_4 => rom_patch = 3,
        PAR3_OTHER_ENDIF => {
            if state.current_block != COMPLETE {
                par_end_block(cheats, state);
                return true;
            }
            return false;
        }
        PAR3_OTHER_ELSE => {
            if state.current_block != COMPLETE {
                par_else_block(cheats, state);
                return true;
            }
            return false;
        }
        PAR3_OTHER_FILL_1 => {
            let idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::Assign;
            cheats.list[idx].address = par_addr(op2);
            cheats.list[idx].width = 1;
            state.incomplete_cheat = idx;
        }
        PAR3_OTHER_FILL_2 => {
            let idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::Assign;
            cheats.list[idx].address = par_addr(op2);
            cheats.list[idx].width = 2;
            state.incomplete_cheat = idx;
        }
        PAR3_OTHER_FILL_4 => {
            let idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::Assign;
            cheats.list[idx].address = par_addr(op2);
            cheats.list[idx].width = 4;
            state.incomplete_cheat = idx;
        }
        _ => {} // PAR3_OTHER_END and unlisted shapes are inert
    }
    if rom_patch >= 0 {
        cheats.rom_patches.push(CheatPatch {
            address: GBA_BASE_ROM0 | ((op2 & 0xFFFFFF) << 1),
            segment: 0,
            value: 0,
            width: 2,
            applied: false,
            check_value: 0,
            check: false,
        });
        state.incomplete_patch = Some(cheats.rom_patches.len() - 1);
    }
    true
}

/// GBACheatAddProActionReplayRaw
pub fn gba_cheat_add_pro_action_replay_raw(
    cheats: &mut CheatSet,
    state: &mut GbaCheatSet,
    op1: u32,
    op2: u32,
) -> bool {
    if let Some(patch_idx) = state.incomplete_patch {
        cheats.rom_patches[patch_idx].value = op1;
        state.incomplete_patch = None;
        return true;
    }
    if state.incomplete_cheat != COMPLETE {
        let ic = state.incomplete_cheat;
        let width = cheats.list[ic].width;
        cheats.list[ic].operand = op1 & width_mask(width);
        if ic > 0 && cheats.list[ic - 1].typ == CheatType::IfButton {
            state.incomplete_cheat = COMPLETE;
            return true;
        }
        cheats.list[ic].operand_offset = (op2 >> 24) as i32;
        cheats.list[ic].repeat = (op2 >> 16) & 0xFF;
        cheats.list[ic].address_offset = ((op2 & 0xFFFF).wrapping_mul(width as u32)) as i32;
        state.incomplete_cheat = COMPLETE;
        return true;
    }

    if op2 == 0x001DC0DE {
        return true;
    }

    match op1 {
        0x00000000 => return add_par3_special(cheats, state, op2),
        0xDEADFACE => {
            gba_cheat_reseed_game_shark(&mut state.gsa_seeds, op2 as u16, &PAR3_T1, &PAR3_T2);
            return true;
        }
        _ => {}
    }

    if op1 >> 24 == 0xC4 {
        if state.hook.is_some() {
            return false;
        }
        // GBACheatHook alloc; PORT: address only.
        state.hook = Some(GBA_BASE_ROM0 | (op1 & (GBA_SIZE_ROM0 as u32 - 2)));
        return true;
    }

    if op1 & PAR3_COND != 0 {
        return add_par3_cond(cheats, state, op1, op2);
    }

    let mut width = 1i32 << ((op1 & PAR3_WIDTH) >> PAR3_WIDTH_BASE);
    let idx = push_cheat(cheats);
    cheats.list[idx].address = par_addr(op1);
    cheats.list[idx].operand_offset = 0;
    cheats.list[idx].address_offset = 0;
    cheats.list[idx].repeat = 1;

    match op1 & PAR3_BASE {
        PAR3_BASE_ASSIGN => {
            cheats.list[idx].typ = CheatType::Assign;
            cheats.list[idx].address_offset = width;
            if width < 4 {
                cheats.list[idx].repeat = (op2 >> (width as u32 * 8)).wrapping_add(1);
            }
        }
        PAR3_BASE_INDIRECT => {
            cheats.list[idx].typ = CheatType::AssignIndirect;
            if width < 4 {
                cheats.list[idx].address_offset =
                    ((op2 >> (width as u32 * 8)).wrapping_mul(width as u32)) as i32;
            }
        }
        PAR3_BASE_ADD => {
            cheats.list[idx].typ = CheatType::Add;
        }
        _ => {
            // PAR3_BASE_OTHER
            width = ((op1 >> 24) & 1) as i32 + 1;
            cheats.list[idx].typ = CheatType::Assign;
            cheats.list[idx].address = GBA_BASE_IO | (op1 & OFFSET_MASK);
        }
    }
    if op1 & 0x01000000 != 0 && op1 & 0xFE000000 != 0xC6000000 {
        // C bails here leaving the appended cheat in the list; keep that.
        return false;
    }

    cheats.list[idx].width = width;
    cheats.list[idx].operand = op2 & width_mask(width);
    true
}

/// GBACheatAddProActionReplay
pub fn gba_cheat_add_pro_action_replay(
    cheats: &mut CheatSet,
    state: &mut GbaCheatSet,
    op1: u32,
    op2: u32,
) -> bool {
    let mut o1 = op1;
    let mut o2 = op2;

    // Same fall-through shape as GameShark, but PARv3.
    match state.gsa_version {
        GBA_GS_PARV3_RAW => return gba_cheat_add_pro_action_replay_raw(cheats, state, o1, o2),
        GBA_GS_PARV3 => {}
        _ => state.set_game_shark_version(GBA_GS_PARV3),
    }
    gba_cheat_decrypt_game_shark(&mut o1, &mut o2, &state.gsa_seeds);
    gba_cheat_add_pro_action_replay_raw(cheats, state, o1, o2)
}

/// GBACheatAddProActionReplayLine: "XXXXXXXX XXXXXXXX".
pub fn gba_cheat_add_pro_action_replay_line(
    cheats: &mut CheatSet,
    state: &mut GbaCheatSet,
    line: &str,
) -> bool {
    let b = line.as_bytes();
    let Some((op1, mut pos)) = hex32(b, 0) else {
        return false;
    };
    while pos < b.len() && b[pos] == b' ' {
        pos += 1;
    }
    let Some((op2, _)) = hex32(b, pos) else {
        return false;
    };
    gba_cheat_add_pro_action_replay(cheats, state, op1, op2)
}

/// GBACheatProActionReplayProbability
pub fn gba_cheat_pro_action_replay_probability(op1: u32, op2: u32) -> i32 {
    let mut probability = 0;
    if op2 == 0x001DC0DE {
        return 0x100;
    }
    if op1 == 0xDEADFACE && op2 & 0xFFFF0000 == 0 {
        return 0x100;
    }
    if op1 == 0 {
        probability += 0x40;
        let address = par_addr(op2);
        match op2 & 0xFE000000 {
            PAR3_OTHER_FILL_1 | PAR3_OTHER_FILL_2 | PAR3_OTHER_FILL_4 => {
                probability += gba_cheat_address_is_real(address);
            }
            PAR3_OTHER_PATCH_1 | PAR3_OTHER_PATCH_2 | PAR3_OTHER_PATCH_3 | PAR3_OTHER_PATCH_4 => {
                // TODO: Detect ROM address
            }
            PAR3_OTHER_END | PAR3_OTHER_SLOWDOWN | PAR3_OTHER_BUTTON_1 | PAR3_OTHER_BUTTON_2
            | PAR3_OTHER_BUTTON_4 | PAR3_OTHER_ENDIF | PAR3_OTHER_ELSE => {
                if op2 & 0x01000000 != 0 {
                    probability -= 0x40;
                }
            }
            _ => {
                probability -= 0x40;
            }
        }
        return probability;
    }
    let width = (op1 & PAR3_WIDTH) >> (PAR3_WIDTH_BASE - 3);
    if op1 & PAR3_COND != 0 {
        probability += 0x20;
        if width >= 24 {
            return 0;
        }
        if op2 & !((1u32 << width) - 1) != 0 {
            probability -= 0x10;
        }
    } else {
        let address = par_addr(op1);
        probability += 0x20;
        match op1 & PAR3_BASE {
            PAR3_BASE_ADD => {
                if op2 & !((1u32 << width) - 1) != 0 {
                    probability -= 0x10;
                }
                // Fall through
                probability += gba_cheat_address_is_real(address);
                if op1 & 0x01000000 != 0 {
                    return 0;
                }
            }
            PAR3_BASE_ASSIGN | PAR3_BASE_INDIRECT => {
                probability += gba_cheat_address_is_real(address);
                if op1 & 0x01000000 != 0 {
                    return 0;
                }
            }
            _ => {} // PAR3_BASE_OTHER
        }
    }
    probability
}

/* ================= CodeBreaker (gba/cheats/codebreaker.c) ================= */

/// _cbLoadByteswap
fn cb_load_byteswap(buffer: &mut [u8; 6], op1: u32, op2: u16) {
    buffer[0] = (op1 >> 24) as u8;
    buffer[1] = (op1 >> 16) as u8;
    buffer[2] = (op1 >> 8) as u8;
    buffer[3] = op1 as u8;
    buffer[4] = (op2 >> 8) as u8;
    buffer[5] = op2 as u8;
}

/// _cbStoreByteswap
fn cb_store_byteswap(buffer: &[u8; 6], op1: &mut u32, op2: &mut u16) {
    *op1 = ((buffer[0] as u32) << 24)
        | ((buffer[1] as u32) << 16)
        | ((buffer[2] as u32) << 8)
        | buffer[3] as u32;
    *op2 = (((buffer[4] as u16) << 8) | buffer[5] as u16) as u16;
}

/// _cbDecrypt
fn cb_decrypt(state: &GbaCheatSet, op1: &mut u32, op2: &mut u16) {
    let mut buffer = [0u8; 6];

    cb_load_byteswap(&mut buffer, *op1, *op2);
    for i in (0..state.cb_table.len()).rev() {
        let offset_x = i >> 3;
        let offset_y = (state.cb_table[i] >> 3) as usize;
        let bit_x = (i & 7) as u32;
        let bit_y = (state.cb_table[i] & 7) as u32;

        let x = (buffer[offset_x] >> bit_x) & 1;
        let y = (buffer[offset_y] >> bit_y) & 1;
        let mut x2 = buffer[offset_x] & !(1u8 << bit_x);
        if y != 0 {
            x2 |= 1 << bit_x;
        }
        buffer[offset_x] = x2;

        // This can't be moved earlier due to pointer aliasing
        let mut y2 = buffer[offset_y] & !(1u8 << bit_y);
        if x != 0 {
            y2 |= 1 << bit_y;
        }
        buffer[offset_y] = y2;
    }
    cb_store_byteswap(&buffer, op1, op2);

    *op1 ^= state.cb_seeds[0];
    *op2 = (*op2 as u32 ^ state.cb_seeds[1]) as u16;

    cb_load_byteswap(&mut buffer, *op1, *op2);
    let master = state.cb_master;
    for i in 0..5 {
        buffer[i] ^= ((master >> 8) as u8) ^ buffer[i + 1];
    }
    buffer[5] ^= (master >> 8) as u8;

    for i in (1..=5).rev() {
        buffer[i] ^= (master as u8) ^ buffer[i - 1];
    }
    buffer[0] ^= master as u8;
    cb_store_byteswap(&buffer, op1, op2);

    *op1 ^= state.cb_seeds[2];
    *op2 = (*op2 as u32 ^ state.cb_seeds[3]) as u16;
}

/// _cbRand
fn cb_rand(state: &mut GbaCheatSet) -> u32 {
    // Roll LCG three times to get enough bits of entropy
    let roll = state
        .cb_rng_state
        .wrapping_mul(0x41C64E6D)
        .wrapping_add(0x3039);
    let roll2 = roll.wrapping_mul(0x41C64E6D).wrapping_add(0x3039);
    let roll3 = roll2.wrapping_mul(0x41C64E6D).wrapping_add(0x3039);
    let mut mix = roll << 14 & 0xC0000000;
    mix |= (roll2 >> 1) & 0x3FFF8000;
    mix |= (roll3 >> 16) & 0x7FFF;
    state.cb_rng_state = roll3;
    mix
}

/// _cbSwapIndex
fn cb_swap_index(state: &mut GbaCheatSet) -> usize {
    let mut roll = cb_rand(state);
    let mut count = state.cb_table.len() as u32;

    if roll == count {
        roll = 0;
    }

    if roll < count {
        return roll as usize;
    }

    let mut bit = 1u32;

    while count < 0x10000000 && count < roll {
        count <<= 4;
        bit <<= 4;
    }

    while count < 0x80000000 && count < roll {
        count <<= 1;
        bit <<= 1;
    }

    let mut mask;
    loop {
        mask = 0;
        if roll >= count {
            roll -= count;
        }
        if roll >= count >> 1 {
            roll -= count >> 1;
            mask |= bit.rotate_right(1);
        }
        if roll >= count >> 2 {
            roll -= count >> 2;
            mask |= bit.rotate_right(2);
        }
        if roll >= count >> 3 {
            roll -= count >> 3;
            mask |= bit.rotate_right(3);
        }
        if roll == 0 || bit >> 4 == 0 {
            break;
        }
        bit >>= 4;
        count >>= 4;
    }

    mask &= 0xE0000000;
    if mask == 0 || bit & 7 == 0 {
        return roll as usize;
    }

    if mask & bit.rotate_right(3) != 0 {
        roll += count >> 3;
    }
    if mask & bit.rotate_right(2) != 0 {
        roll += count >> 2;
    }
    if mask & bit.rotate_right(1) != 0 {
        roll += count >> 1;
    }

    roll as usize
}

/// _cbReseed
fn cb_reseed(state: &mut GbaCheatSet, op1: u32, op2: u16) {
    state.cb_rng_state = (op2 as u32 & 0xFF) ^ 0x1111;
    // Populate the initial seed table
    for i in 0..state.cb_table.len() {
        state.cb_table[i] = i as u8;
    }
    // Swap pseudo-random table entries based on the input code
    for _ in 0..0x50 {
        let x = cb_swap_index(state);
        let y = cb_swap_index(state);
        state.cb_table.swap(x, y);
    }

    // Spin the RNG some to make the initial seed
    state.cb_rng_state = 0x4EFAD1C3;
    for _ in 0..((op1 >> 24) & 0xF) {
        state.cb_rng_state = cb_rand(state);
    }

    state.cb_seeds[2] = cb_rand(state);
    state.cb_seeds[3] = cb_rand(state);

    state.cb_rng_state = (op2 as u32 >> 8) ^ 0xF254;
    for _ in 0..(op2 >> 8) {
        state.cb_rng_state = cb_rand(state);
    }

    state.cb_seeds[0] = cb_rand(state);
    state.cb_seeds[1] = cb_rand(state);

    state.cb_master = op1;
}

/// GBACheatAddCodeBreaker
pub fn gba_cheat_add_code_breaker(
    cheats: &mut CheatSet,
    state: &mut GbaCheatSet,
    op1: u32,
    op2: u16,
) -> bool {
    let mut op1 = op1;
    let mut op2 = op2;

    if state.cb_master != 0 {
        cb_decrypt(state, &mut op1, &mut op2);
    }

    let typ = op1 >> 28;

    if state.incomplete_cheat != COMPLETE {
        if state.remaining_addresses != 0 {
            let halfword = op1 >> 16;
            cheats.list[state.incomplete_cheat].operand =
                ((halfword >> 8) | (halfword << 8)) & 0xFFFF;
            state.remaining_addresses -= 1;

            if state.remaining_addresses != 0 {
                let halfword = op1 & 0xFFFF;
                let idx = push_cheat(cheats);
                cheats.list[idx].typ = CheatType::Assign;
                cheats.list[idx].width = 2;
                cheats.list[idx].address =
                    cheats.list[state.incomplete_cheat].address.wrapping_add(2);
                cheats.list[idx].operand = ((halfword >> 8) | (halfword << 8)) & 0xFFFF;
                cheats.list[idx].repeat = 1;
                state.remaining_addresses -= 1;
                state.incomplete_cheat = idx;
            }

            if state.remaining_addresses != 0 {
                let halfword = op2 as u32;
                let idx = push_cheat(cheats);
                cheats.list[idx].typ = CheatType::Assign;
                cheats.list[idx].width = 2;
                cheats.list[idx].address =
                    cheats.list[state.incomplete_cheat].address.wrapping_add(2);
                cheats.list[idx].operand = ((halfword >> 8) | (halfword << 8)) & 0xFFFF;
                cheats.list[idx].repeat = 1;
                state.remaining_addresses -= 1;
                state.incomplete_cheat = idx;
            }

            if state.remaining_addresses == 0 {
                state.incomplete_cheat = COMPLETE;
            } else {
                let idx = push_cheat(cheats);
                cheats.list[idx].typ = CheatType::Assign;
                cheats.list[idx].width = 2;
                cheats.list[idx].address =
                    cheats.list[state.incomplete_cheat].address.wrapping_add(2);
                cheats.list[idx].repeat = 1;
                state.incomplete_cheat = idx;
            }
        } else {
            let ic = state.incomplete_cheat;
            cheats.list[ic].repeat = op1 & 0xFFFF;
            cheats.list[ic].address_offset = op2 as i32;
            cheats.list[ic].operand_offset = (op1 >> 16) as i32;
            state.incomplete_cheat = COMPLETE;
        }
        return true;
    }

    let idx;
    match typ {
        CB_GAME_ID => {
            // TODO: Run checksum
            return true;
        }
        CB_HOOK => {
            if state.hook.is_some() {
                return false;
            }
            // GBACheatHook alloc; PORT: address only.
            state.hook = Some(GBA_BASE_ROM0 | (op1 & (GBA_SIZE_ROM0 as u32 - 1)));
            return true;
        }
        CB_OR_2 => {
            idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::Or;
            cheats.list[idx].width = 2;
        }
        CB_ASSIGN_1 => {
            idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::Assign;
            cheats.list[idx].width = 1;
        }
        CB_FILL => {
            idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::Assign;
            cheats.list[idx].width = 2;
            state.incomplete_cheat = idx;
        }
        CB_FILL_LIST => {
            idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::Assign;
            cheats.list[idx].width = 2;
            state.incomplete_cheat = idx;
            state.remaining_addresses = op2 as i32;
        }
        CB_AND_2 => {
            idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::And;
            cheats.list[idx].width = 2;
        }
        CB_IF_EQ => {
            idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::IfEq;
            cheats.list[idx].width = 2;
        }
        CB_ASSIGN_2 => {
            idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::Assign;
            cheats.list[idx].width = 2;
        }
        CB_ENCRYPT => {
            cb_reseed(state, op1, op2);
            return true;
        }
        CB_IF_NE => {
            idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::IfNe;
            cheats.list[idx].width = 2;
        }
        CB_IF_GT => {
            idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::IfGt;
            cheats.list[idx].width = 2;
        }
        CB_IF_LT => {
            idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::IfLt;
            cheats.list[idx].width = 2;
        }
        CB_IF_SPECIAL => match op1 & 0x0FFFFFFF {
            0x20 => {
                idx = push_cheat(cheats);
                cheats.list[idx].typ = CheatType::IfNand;
                cheats.list[idx].width = 2;
                cheats.list[idx].address = GBA_BASE_IO | GBA_REG_KEYINPUT;
                cheats.list[idx].operand = op2 as u32;
                cheats.list[idx].repeat = 1;
                return true;
            }
            _ => {
                mlog!(
                    Level::Stub,
                    rgba_core::log::GBA_CHEAT,
                    "CodeBreaker code {:08X} {:04X} not supported",
                    op1,
                    op2
                );
                return false;
            }
        },
        CB_ADD_2 => {
            idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::Add;
            cheats.list[idx].width = 2;
        }
        CB_IF_AND => {
            idx = push_cheat(cheats);
            cheats.list[idx].typ = CheatType::IfAnd;
            cheats.list[idx].width = 2;
        }
        _ => return false,
    }

    cheats.list[idx].address = op1 & 0x0FFFFFFF;
    cheats.list[idx].operand = op2 as u32;
    cheats.list[idx].repeat = 1;
    cheats.list[idx].negative_repeat = 0;
    true
}

/// GBACheatAddCodeBreakerLine: "XXXXXXXX XXXX".
pub fn gba_cheat_add_code_breaker_line(
    cheats: &mut CheatSet,
    state: &mut GbaCheatSet,
    line: &str,
) -> bool {
    let b = line.as_bytes();
    let Some((op1, mut pos)) = hex32(b, 0) else {
        return false;
    };
    while pos < b.len() && b[pos] == b' ' {
        pos += 1;
    }
    let Some((op2, _)) = hex16(b, pos) else {
        return false;
    };
    gba_cheat_add_code_breaker(cheats, state, op1, op2)
}

/* ===================== gba/cheats.c: dispatch + VBA ===================== */

/// GBACheatAddAutodetect
fn gba_cheat_add_autodetect(
    cheats: &mut CheatSet,
    state: &mut GbaCheatSet,
    op1: u32,
    op2: u32,
) -> bool {
    let mut next_probability;
    let mut max_probability = i32::MIN;
    match state.gsa_version {
        GBA_GS_NOT_SET => {
            // Try to detect GameShark version
            let mut o1 = op1;
            let mut o2 = op2;
            gba_cheat_decrypt_game_shark(&mut o1, &mut o2, &GBA_CHEAT_GAME_SHARK_SEEDS);
            next_probability = gba_cheat_game_shark_probability(o1, o2);
            if next_probability > max_probability {
                max_probability = next_probability;
                state.set_game_shark_version(GBA_GS_GSAV1);
            }

            o1 = op1;
            o2 = op2;
            gba_cheat_decrypt_game_shark(&mut o1, &mut o2, &GBA_CHEAT_PRO_ACTION_REPLAY_SEEDS);
            next_probability = gba_cheat_pro_action_replay_probability(o1, o2);
            if next_probability > max_probability {
                max_probability = next_probability;
                state.set_game_shark_version(GBA_GS_PARV3);
            }

            next_probability = gba_cheat_game_shark_probability(op1, op2);
            if next_probability > max_probability {
                max_probability = next_probability;
                state.set_game_shark_version(GBA_GS_GSAV1_RAW);
            }

            next_probability = gba_cheat_pro_action_replay_probability(op1, op2);
            if next_probability > max_probability {
                let _ = max_probability;
                state.set_game_shark_version(GBA_GS_PARV3_RAW);
            }

            if state.gsa_version < 3 {
                return gba_cheat_add_game_shark(cheats, state, op1, op2);
            } else {
                return gba_cheat_add_pro_action_replay(cheats, state, op1, op2);
            }
        }
        GBA_GS_GSAV1 | GBA_GS_GSAV1_RAW => {
            return gba_cheat_add_game_shark(cheats, state, op1, op2);
        }
        GBA_GS_PARV3 | GBA_GS_PARV3_RAW => {
            return gba_cheat_add_pro_action_replay(cheats, state, op1, op2);
        }
        _ => {}
    }
    false
}

/// GBACheatAddVBALine: "AAAAAAAA:VV" (VV up to 4 bytes).
pub fn gba_cheat_add_vba_line(cheats: &mut CheatSet, line: &str) -> bool {
    let b = line.as_bytes();
    let Some((address, pos)) = hex32(b, 0) else {
        return false;
    };
    if pos >= b.len() || b[pos] != b':' {
        return false;
    }
    let mut pos = pos + 1;
    let mut value = 0u32;
    let mut width = 0i32;
    while width < 4 {
        let Some((op, p)) = hex8(b, pos) else {
            break;
        };
        value <<= 8;
        value |= op as u32;
        width += 1;
        pos = p;
    }
    if width == 0 || width == 3 {
        return false;
    }

    if address < GBA_BASE_ROM0 || address >= GBA_BASE_SRAM {
        cheats.list.push(Cheat {
            typ: CheatType::Assign,
            width,
            address,
            operand: value,
            repeat: 1,
            negative_repeat: 0,
            address_offset: 0,
            operand_offset: 0,
        });
    } else {
        cheats.rom_patches.push(CheatPatch {
            address,
            segment: 0,
            value,
            width,
            applied: false,
            check_value: 0,
            check: false,
        });
    }
    true
}

/// GBACheatAddLine (the set->addLine hook), wrapped in mCheatAddLine's core
/// bookkeeping: on success the raw line is kept for save/dump.
pub fn gba_cheat_add_line(
    set: &mut CheatSet,
    state: &mut GbaCheatSet,
    line: &str,
    typ: i32,
) -> bool {
    set.add_line(line, |set| gba_cheat_add_line_impl(set, state, line, typ))
}

fn gba_cheat_add_line_impl(
    cheats: &mut CheatSet,
    state: &mut GbaCheatSet,
    line: &str,
    typ: i32,
) -> bool {
    match typ {
        GBA_CHEAT_AUTODETECT => {}
        GBA_CHEAT_CODEBREAKER => return gba_cheat_add_code_breaker_line(cheats, state, line),
        GBA_CHEAT_GAMESHARK => return gba_cheat_add_game_shark_line(cheats, state, line),
        GBA_CHEAT_PRO_ACTION_REPLAY => {
            return gba_cheat_add_pro_action_replay_line(cheats, state, line)
        }
        GBA_CHEAT_VBA => return gba_cheat_add_vba_line(cheats, line),
        _ => return false,
    }

    let b = line.as_bytes();
    let Some((op1, mut pos)) = hex32(b, 0) else {
        return false;
    };
    if pos < b.len() && b[pos] == b':' {
        return gba_cheat_add_vba_line(cheats, line);
    }
    while pos < b.len() && is_c_space(b[pos]) {
        pos += 1;
    }
    let Some((op2, p)) = hex16(b, pos) else {
        return false;
    };
    pos = p;
    if pos >= b.len() || is_c_space(b[pos]) {
        return gba_cheat_add_code_breaker(cheats, state, op1, op2);
    }
    let Some((op3, _)) = hex16(b, pos) else {
        return false;
    };
    let real_op2 = ((op2 as u32) << 16) | op3 as u32;
    gba_cheat_add_autodetect(cheats, state, op1, real_op2)
}

// GBACheatSetDeinit / GBACheatAddSet / GBACheatRemoveSet in the C only manage
// the breakpoint hook, which is not ported (see GbaCheatSet::hook).

/// GBACheatAddressIsReal
pub fn gba_cheat_address_is_real(address: u32) -> i32 {
    match address >> BASE_OFFSET {
        GBA_REGION_BIOS => -0x80,
        GBA_REGION_EWRAM => {
            if address & OFFSET_MASK > GBA_SIZE_EWRAM as u32 {
                -0x40
            } else {
                0x20
            }
        }
        GBA_REGION_IWRAM => {
            if address & OFFSET_MASK > GBA_SIZE_IWRAM as u32 {
                -0x40
            } else {
                0x20
            }
        }
        GBA_REGION_IO => {
            if address & OFFSET_MASK > GBA_SIZE_IO as u32 {
                -0x80
            } else {
                0x10
            }
        }
        GBA_REGION_OAM => {
            if address & OFFSET_MASK > GBA_SIZE_OAM as u32 {
                -0x80
            } else {
                -0x8
            }
        }
        GBA_REGION_VRAM => {
            if address & OFFSET_MASK > GBA_SIZE_VRAM as u32 {
                -0x80
            } else {
                -0x8
            }
        }
        GBA_REGION_PALETTE_RAM => {
            if address & OFFSET_MASK > GBA_SIZE_PALETTE_RAM as u32 {
                -0x80
            } else {
                -0x8
            }
        }
        GBA_REGION_ROM0 | GBA_REGION_ROM0_EX | GBA_REGION_ROM1 | GBA_REGION_ROM1_EX
        | GBA_REGION_ROM2 | GBA_REGION_ROM2_EX => -0x8,
        GBA_REGION_SRAM | GBA_REGION_SRAM_MIRROR => {
            if address & OFFSET_MASK > GBA_SIZE_FLASH512 as u32 {
                -0x80
            } else {
                -0x8
            }
        }
        _ => -0xC0,
    }
}

/// The mCore vtable subset the cheat engine drives, over the GBA bus
/// (_GBACoreBusRead8/16/32, _GBACoreBusWrite8/16/32, _GBACoreRawRead8/16/32,
/// _GBACoreRawWrite8/16/32).
impl CheatBus for Gba {
    fn read_mem(&mut self, address: u32, width: i32) -> i32 {
        // _readMem → cpu->memory.load8/16/32 with mACCESS_CHEAT. The port's
        // load family doesn't take an accessSource; noted and continuing.
        let cycle_counter = &mut 0;
        match width {
            1 => self.load8(address, cycle_counter) as i32,
            2 => self.load16(address, cycle_counter) as i32,
            4 => self.load32(address, cycle_counter) as i32,
            _ => 0,
        }
    }

    fn write_mem(&mut self, address: u32, width: i32, value: i32) {
        // _writeMem → cpu->memory.store8/16/32 (mACCESS_CHEAT not modeled).
        let cycle_counter = &mut 0;
        match width {
            1 => self.store8(address, value, cycle_counter),
            2 => self.store16(address, value, cycle_counter),
            4 => self.store32(address, value, cycle_counter),
            _ => {}
        }
    }

    fn read_mem_segment(&mut self, address: u32, segment: i32, width: i32) -> i32 {
        // _GBACoreRawRead8/16/32 = GBAView8/16/32 (UNUSED segment) = the same
        // loads with a dead waitstate counter.
        let _ = segment;
        self.read_mem(address, width)
    }

    fn patch_mem(&mut self, address: u32, segment: i32, width: i32, value: i32) {
        // _GBACoreRawWrite8/16/32 → GBAPatch8/16/32 (UNUSED segment).
        let _ = segment;
        match width {
            1 => self.patch8(address, value as i8, None),
            2 => self.patch16(address, value as i16, None),
            4 => self.patch32(address, value, None),
            _ => {}
        }
    }

    fn memory_block_max_segment(&self, _address: u32) -> Option<i32> {
        // mCoreGetMemoryBlockInfo over the _GBAMemoryBlocks tables: every
        // block listed there carries maxSegment 0 (a check-value scan would
        // never match), and no GBA code shape ever sets mCheatPatch.check,
        // so this scan is never reached in practice.
        None
    }
}

impl Gba {
    /// mCheatAddSet + the GBACheatAddSet hook (which only installs the
    /// breakpoint; not ported). Keeps the GBACheatSet state list in lockstep
    /// with the shared device list, one entry per set.
    pub fn cheat_add_set(&mut self, set: CheatSet, state: GbaCheatSet) {
        self.cheats.add_set(set);
        self.gba_cheat_sets.push(state);
    }

    /// mCheatRemoveSet + the GBACheatRemoveSet hook (not ported).
    pub fn cheat_remove_set(&mut self, index: usize) {
        self.cheats.remove_set(index);
        if index < self.gba_cheat_sets.len() {
            self.gba_cheat_sets.remove(index);
        }
    }

    /// The cheat portion of GBAFrameEnded: mCheatRefresh on every set that
    /// isn't hook-driven, once per frame. Hooked sets refresh at their
    /// breakpoint in the C; breakpoints aren't ported, so they're skipped
    /// here like the "else" of GBAFrameEnded's `if (!cheats->hook)`.
    pub fn cheat_apply(&mut self) {
        if self.cheats.cheats.is_empty() {
            return;
        }
        let mut device = std::mem::take(&mut self.cheats);
        for i in 0..device.cheats.len() {
            if self
                .gba_cheat_sets
                .get(i)
                .is_some_and(|state| state.hook.is_some())
            {
                continue;
            }
            device.cheat_refresh(self, i);
        }
        self.cheats = device;
    }

    /// mCheatParseFile: the native mGBA ".cheats" format (sets introduced by
    /// `# name` comment lines, `!disabled`/engine directives via `!`, one
    /// code per line). The libretro .cht and EZ Flash .CHT sub-parsers are
    /// not ported; loading one of those logs a warning and returns Ok(false).
    /// MAX_LINE_LENGTH's 512-byte readline cap is not emulated (cheat files
    /// never approach it); overlong lines are handled whole instead of split.
    pub fn load_cheats(&mut self, path: impl AsRef<Path>) -> io::Result<bool> {
        let file = std::fs::File::open(path)?;
        let mut reader = io::BufReader::new(file);
        let mut bytes: Vec<u8> = Vec::new();
        let mut set: Option<(CheatSet, GbaCheatSet)> = None;
        let mut next_disabled = false;
        let mut directives: Vec<String> = Vec::new();

        loop {
            bytes.clear();
            if reader.read_until(b'\n', &mut bytes)? == 0 {
                break;
            }
            // rtrim
            let mut end = bytes.len();
            while end > 0 && is_c_space(bytes[end - 1]) {
                end -= 1;
            }
            let cheat = &bytes[..end];
            let mut i = 0;
            while i < cheat.len() && is_c_space(cheat[i]) {
                i += 1;
            }
            let kind = cheat.get(i).copied().unwrap_or(0);
            if kind == b'#' {
                let mut j = i + 1;
                while j < cheat.len() && is_c_space(cheat[j]) {
                    j += 1;
                }
                let name = String::from_utf8_lossy(&cheat[j..]).into_owned();
                let (mut new_set, mut new_state) = gba_cheat_set_create(&name);
                new_set.enabled = !next_disabled;
                next_disabled = false;
                if let Some((old_set, old_state)) = set.take() {
                    self.cheat_add_set(old_set, old_state);
                    // newSet->copyProperties(newSet, set): the just-added set
                    // is the previous one in the device list.
                    let last = self.gba_cheat_sets.len() - 1;
                    new_state.copy_properties(&self.gba_cheat_sets[last]);
                }
                new_state.parse_directives(&directives);
                set = Some((new_set, new_state));
            } else if kind == b'!' {
                let mut j = i + 1;
                while j < cheat.len() && is_c_space(cheat[j]) {
                    j += 1;
                }
                let directive = &cheat[j..];
                if directive.eq_ignore_ascii_case(b"disabled") {
                    next_disabled = true;
                } else if directive.eq_ignore_ascii_case(b"reset") {
                    directives.clear();
                } else {
                    directives.push(String::from_utf8_lossy(directive).into_owned());
                }
            } else {
                if set.is_none() {
                    if cheat.starts_with(b"cheats = ") {
                        // mCheatParseLibretroFile is not ported.
                        mlog!(
                            Level::Warn,
                            rgba_core::log::GBA_CHEAT,
                            "libretro .cht cheat files are not supported"
                        );
                        return Ok(false);
                    }
                    if !cheat.is_empty() && cheat[0] == b'[' {
                        // mCheatParseEZFChtFile is not ported.
                        mlog!(
                            Level::Warn,
                            rgba_core::log::GBA_CHEAT,
                            "EZ Flash .CHT cheat files are not supported"
                        );
                        return Ok(false);
                    }
                    let (mut new_set, new_state) = gba_cheat_set_create("");
                    new_set.enabled = !next_disabled;
                    next_disabled = false;
                    set = Some((new_set, new_state));
                }
                if let Ok(line) = std::str::from_utf8(cheat) {
                    let (set_part, state_part) = set.as_mut().unwrap();
                    // mCheatAddLine(set, cheat, 0): a bad line is just skipped.
                    gba_cheat_add_line(set_part, state_part, line, GBA_CHEAT_AUTODETECT);
                }
            }
        }
        if let Some((set_part, state_part)) = set.take() {
            self.cheat_add_set(set_part, state_part);
        }
        Ok(true)
    }
}
