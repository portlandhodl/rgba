// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/include/mgba/internal/gba/memory.h and
// mgba/src/gba/memory.c. Cartridge side-channel systems (GPIO/RTC, rumble,
// tilt, matrix, e-Reader, unlicensed carts) get their own modules; this file
// wires them into the bus.
//
// Renderer write callback ABI (GBAVideoRenderer.writePalette / writeVRAM /
// writeOAM in the C): palette and VRAM callbacks receive a BYTE address into
// the buffer, OAM receives a HALFWORD index (offset >> 1). These feed the
// mVL video log (proxy.c) verbatim, so the packet stream stays in the C's
// address space.

use rgba_core::timing::Timing;
use rgba_core::{mlog, Level};

use crate::arm::*;
use crate::gba::{Gba, GBA_MEMORY_MAGIC_UNUSED};
use crate::io::*;
use crate::video;

pub const GBA_REGION_BIOS: u32 = 0x0;
pub const GBA_REGION_EWRAM: u32 = 0x2;
pub const GBA_REGION_IWRAM: u32 = 0x3;
pub const GBA_REGION_IO: u32 = 0x4;
pub const GBA_REGION_PALETTE_RAM: u32 = 0x5;
pub const GBA_REGION_VRAM: u32 = 0x6;
pub const GBA_REGION_OAM: u32 = 0x7;
pub const GBA_REGION_ROM0: u32 = 0x8;
pub const GBA_REGION_ROM0_EX: u32 = 0x9;
pub const GBA_REGION_ROM1: u32 = 0xA;
pub const GBA_REGION_ROM1_EX: u32 = 0xB;
pub const GBA_REGION_ROM2: u32 = 0xC;
pub const GBA_REGION_ROM2_EX: u32 = 0xD;
pub const GBA_REGION_SRAM: u32 = 0xE;
pub const GBA_REGION_SRAM_MIRROR: u32 = 0xF;

pub const GBA_BASE_BIOS: u32 = 0x00000000;
pub const GBA_BASE_EWRAM: u32 = 0x02000000;
pub const GBA_BASE_IWRAM: u32 = 0x03000000;
pub const GBA_BASE_IO: u32 = 0x04000000;
pub const GBA_BASE_PALETTE_RAM: u32 = 0x05000000;
pub const GBA_BASE_VRAM: u32 = 0x06000000;
pub const GBA_BASE_OAM: u32 = 0x07000000;
pub const GBA_BASE_ROM0: u32 = 0x08000000;
pub const GBA_BASE_ROM0_EX: u32 = 0x09000000;
pub const GBA_BASE_ROM1: u32 = 0x0A000000;
pub const GBA_BASE_ROM1_EX: u32 = 0x0B000000;
pub const GBA_BASE_ROM2: u32 = 0x0C000000;
pub const GBA_BASE_ROM2_EX: u32 = 0x0D000000;
pub const GBA_BASE_SRAM: u32 = 0x0E000000;
pub const GBA_BASE_SRAM_MIRROR: u32 = 0x0F000000;

pub const GBA_SIZE_BIOS: usize = 0x00004000;
pub const GBA_SIZE_EWRAM: usize = 0x00040000;
pub const GBA_SIZE_IWRAM: usize = 0x00008000;
pub const GBA_SIZE_IO: usize = 0x00000400;
pub const GBA_SIZE_PALETTE_RAM: usize = 0x00000400;
pub const GBA_SIZE_VRAM: usize = 0x00018000;
pub const GBA_SIZE_OAM: usize = 0x00000400;
pub const GBA_SIZE_ROM0: usize = 0x02000000;
pub const GBA_SIZE_SRAM: usize = 0x00008000;
pub const GBA_SIZE_SRAM512: usize = 0x00010000;
pub const GBA_SIZE_FLASH512: usize = 0x00010000;
pub const GBA_SIZE_FLASH1M: usize = 0x00020000;
pub const GBA_SIZE_EEPROM: usize = 0x00002000;
pub const GBA_SIZE_EEPROM512: usize = 0x00000200;
pub const GBA_SIZE_AGB_PRINT: usize = 0x10000;

pub const OFFSET_MASK: u32 = 0x00FFFFFF;
pub const BASE_OFFSET: u32 = 24;

// AGBPrint magic addresses
pub const AGB_PRINT_PROTECT: u32 = 0x09FE2FFE;
pub const AGB_PRINT_STRUCT: u32 = 0x09FE20F8;
pub const AGB_PRINT_FLUSH_ADDR: u32 = 0x09FE209E;
pub const AGB_PRINT_TOP: u32 = 0x09FC0000;
pub const AGB_PRINT_BASE: u32 = AGB_PRINT_STRUCT & !0xFFFF;
pub const AGB_PRINT_FLUSH: u32 = AGB_PRINT_TOP;

pub const SAVEDATA_FLASH_BASE: u32 = GBA_BASE_SRAM;

const GBA_BASE_WAITSTATES: [i32; 16] = [0, 0, 2, 0, 0, 0, 0, 0, 4, 4, 4, 4, 4, 4, 4, 0];
const GBA_BASE_WAITSTATES_32: [i32; 16] = [0, 0, 5, 0, 0, 1, 1, 0, 7, 7, 9, 9, 13, 13, 9, 0];
const GBA_BASE_WAITSTATES_SEQ: [i32; 16] = [0, 0, 2, 0, 0, 0, 0, 0, 2, 2, 4, 4, 8, 8, 4, 0];
const GBA_BASE_WAITSTATES_SEQ_32: [i32; 16] = [0, 0, 5, 0, 0, 1, 1, 0, 5, 5, 9, 9, 17, 17, 9, 0];
const GBA_ROM_WAITSTATES: [i32; 4] = [4, 3, 2, 8];
const GBA_ROM_WAITSTATES_SEQ: [i32; 6] = [2, 1, 4, 1, 8, 1];

pub const IDLE_LOOP_IGNORE: i32 = -1;
pub const IDLE_LOOP_REMOVE: i32 = 0;
pub const IDLE_LOOP_DETECT: i32 = 1;

/// The cached instruction-fetch region (replaces the C's raw pointer+mask).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActiveMemoryRegion {
    None,
    Bios { mask: u32 },
    Ewram,
    Iwram,
    Palette,
    Vram { high: bool },
    Oam,
    Rom { mask: u32 },
    AgbPrintFunc,
}

pub struct Memory {
    pub bios: Vec<u8>,
    pub full_bios: bool,
    pub wram: Vec<u8>,  // EWRAM
    pub iwram: Vec<u8>, // IWRAM (separate vec; C overlays one mapping)
    pub rom: Vec<u8>,
    pub rom_size: usize,
    pub rom_mask: u32,
    pub rom_id: u16,
    /// First 0x4000 bytes are the HLE BIOS trampoline otherwise.
    pub io: [u16; 518], // (GBA_REG_INTERNAL_MAX / 2 + extra)

    pub waitstates_nonseq16: [i32; 256],
    pub waitstates_seq16: [i32; 256],
    pub waitstates_nonseq32: [i32; 256],
    pub waitstates_seq32: [i32; 256],
    pub active_region: i32,
    pub prefetch: bool,
    pub last_prefetched_pc: u32,
    pub bios_prefetch: u32,

    pub agb_print_base: u32,
    pub agb_print_protect: u16,
    pub agb_print_ctx: AgbPrintContext,
    pub agb_print_buffer: Option<Vec<u8>>,
    pub agb_print_protect_backup: u16,
    pub agb_print_ctx_backup: AgbPrintContext,
    pub agb_print_func_backup: u32,
    pub agb_print_buffer_backup: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Default)]
pub struct AgbPrintContext {
    pub request: u16,
    pub bank: u16,
    pub get: u16,
    pub put: u16,
}

pub static DEADBEEF: [u8; 4] = [0x10, 0xB7, 0x10, 0xE7]; // Illegal instruction on both ARM and Thumb
const AGB_PRINT_FUNC: [u8; 4] = [0xFA, 0xDF, 0x70, 0x47]; // swi 0xFA; bx lr

impl Memory {
    pub fn new() -> Self {
        Memory {
            bios: Vec::new(),
            full_bios: false,
            wram: vec![0; GBA_SIZE_EWRAM],
            iwram: vec![0; GBA_SIZE_IWRAM],
            rom: Vec::new(),
            rom_size: 0,
            rom_mask: 0,
            rom_id: 0,
            io: [0; 518],
            waitstates_nonseq16: [0; 256],
            waitstates_seq16: [0; 256],
            waitstates_nonseq32: [0; 256],
            waitstates_seq32: [0; 256],
            active_region: -1,
            prefetch: false,
            last_prefetched_pc: 0,
            bios_prefetch: 0,
            agb_print_base: 0,
            agb_print_protect: 0,
            agb_print_ctx: AgbPrintContext::default(),
            agb_print_buffer: None,
            agb_print_protect_backup: 0,
            agb_print_ctx_backup: AgbPrintContext::default(),
            agb_print_func_backup: 0,
            agb_print_buffer_backup: None,
        }
    }

    pub fn init_waitstates(&mut self) {
        for i in 0..16 {
            self.waitstates_nonseq16[i] = GBA_BASE_WAITSTATES[i];
            self.waitstates_seq16[i] = GBA_BASE_WAITSTATES_SEQ[i];
            self.waitstates_nonseq32[i] = GBA_BASE_WAITSTATES_32[i];
            self.waitstates_seq32[i] = GBA_BASE_WAITSTATES_SEQ_32[i];
        }
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn ror32(v: u32, r: u32) -> u32 {
    v.rotate_right(r)
}

impl Gba {
    /// _analyzeForIdleLoop (only called in Thumb mode; see the C).
    fn analyze_for_idle_loop(&mut self, address: u32) {
        use crate::arm::decoder::*;
        self.tainted_registers = [false; 16];
        self.cached_registers = [0; 16];
        if self.cpu.execution_mode != crate::arm::ExecutionMode::Thumb {
            self.idle_detection_step = -1;
            return;
        }
        let mut next_address = address;
        loop {
            let opcode = {
                // LOAD_16 from the active region, like the C
                let mut cc = 0i32;
                self.load16(next_address, &mut cc) as u16
            };
            let info = arm_decode_thumb(opcode);
            match info.branch_type {
                ARM_BRANCH_NONE => {
                    if info.operand_format & ARM_OPERAND_MEMORY_2 != 0 {
                        if info.mnemonic == ARMMnemonic::Str
                            || self.tainted_registers[info.memory.base_reg as usize]
                        {
                            self.idle_detection_step = -1;
                            return;
                        }
                        let mut load_address =
                            self.cached_registers[info.memory.base_reg as usize] as u32;
                        let mut offset: u32 = 0;
                        if info.memory.format & ARM_MEMORY_IMMEDIATE_OFFSET != 0 {
                            offset = info.memory.offset.immediate as u32;
                        } else if info.memory.format & ARM_MEMORY_REGISTER_OFFSET != 0 {
                            let reg = info.memory.offset.reg as usize;
                            if reg < 16 && self.cached_registers[reg] != 0 {
                                self.idle_detection_step = -1;
                                return;
                            }
                            offset = self.cached_registers[reg] as u32;
                        }
                        if info.memory.format & ARM_MEMORY_OFFSET_SUBTRACT != 0 {
                            load_address = load_address.wrapping_sub(offset);
                        } else {
                            load_address = load_address.wrapping_add(offset);
                        }
                        if (load_address >> BASE_OFFSET) == GBA_REGION_IO
                            && !Gba::io_is_read_constant(load_address)
                        {
                            self.idle_detection_step = -1;
                            return;
                        }
                        if (load_address >> BASE_OFFSET) < GBA_REGION_ROM0
                            || (load_address >> BASE_OFFSET) > GBA_REGION_ROM2_EX
                        {
                            self.tainted_registers[info.op1.reg as usize] = true;
                        } else {
                            let mut cc = 0i32;
                            let op = info.op1.reg as usize;
                            match info.memory.width {
                                1 => self.cached_registers[op] = self.load8(load_address, &mut cc) as i32,
                                2 => self.cached_registers[op] = self.load16(load_address, &mut cc) as i32,
                                4 => self.cached_registers[op] = self.load32(load_address, &mut cc) as i32,
                                _ => {}
                            }
                        }
                    } else if info.operand_format & ARM_OPERAND_AFFECTED_1 != 0 {
                        self.tainted_registers[info.op1.reg as usize] = true;
                    }
                    next_address += WORD_SIZE_THUMB as u32;
                }
                bt if bt & ARM_BRANCH != 0 => {
                    // C: `if (info.op1.immediate + nextAddress +
                    //    WORD_SIZE_THUMB*2 == address)` — any branch kind.
                    if info.branch_type == ARM_BRANCH
                        && info.op1.immediate.wrapping_add(
                            next_address.wrapping_add(WORD_SIZE_THUMB as u32 * 2) as i32,
                        ) == address as i32
                    {
                        self.idle_loop = address;
                        self.idle_optimization = IDLE_LOOP_REMOVE;
                    }
                    self.idle_detection_step = -1;
                    return;
                }
                _ => {
                    self.idle_detection_step = -1;
                    return;
                }
            }
        }
    }

    /// GBASetActiveRegion
    pub fn set_active_region(&mut self, address: u32) {
        let new_region = address >> BASE_OFFSET;

        // Idle-loop detection block (C's GBASetActiveRegion, gated on
        // idleOptimization >= REMOVE and the active region not being BIOS).
        if self.idle_optimization >= IDLE_LOOP_REMOVE
            && self.memory.active_region != GBA_REGION_BIOS as i32
        {
            if address == self.idle_loop {
                if self.halt_pending {
                    self.halt_pending = false;
                    self.halt();
                } else {
                    self.halt_pending = true;
                }
            } else if self.idle_optimization >= IDLE_LOOP_DETECT
                && new_region as i32 == self.memory.active_region
            {
                if address == self.last_jump {
                    match self.idle_detection_step {
                        0 => {
                            self.cached_registers = self.cpu.gprs;
                            self.idle_detection_step = 1;
                        }
                        1 => {
                            if self.cached_registers != self.cpu.gprs {
                                self.idle_detection_step = -1;
                                self.idle_detection_failures += 1;
                                if self.idle_detection_failures > 10000 {
                                    self.idle_optimization = IDLE_LOOP_IGNORE;
                                }
                            } else {
                                self.analyze_for_idle_loop(address);
                            }
                        }
                        _ => {}
                    }
                } else {
                    self.idle_detection_step = 0;
                }
            }
        }
        self.last_jump = address;

        // memory->lastPrefetchedPc = 0 in C; our prefetch is decoupled.

        if new_region as i32 == self.memory.active_region {
            if self.cpu.cpsr.t() {
                self.cpu.active_mask |= WORD_SIZE_THUMB as u32;
            } else {
                self.cpu.active_mask &= (0u32).wrapping_sub(WORD_SIZE_ARM as u32);
            }
            if new_region < GBA_REGION_ROM0
                || (address & (GBA_SIZE_ROM0 as u32 - 1)) < self.memory.rom_size as u32
            {
                return;
            }
        }

        if self.memory.active_region == GBA_REGION_BIOS as i32 {
            self.memory.bios_prefetch = self.cpu.prefetch[1];
        }
        self.memory.active_region = new_region as i32;
        match new_region {
            GBA_REGION_BIOS => {
                self.cpu.active_region = ActiveMemoryRegion::Bios {
                    mask: (GBA_SIZE_BIOS - 1) as u32,
                };
                self.cpu.active_mask = (GBA_SIZE_BIOS - 1) as u32;
            }
            GBA_REGION_EWRAM => {
                self.cpu.active_region = ActiveMemoryRegion::Ewram;
                self.cpu.active_mask = (GBA_SIZE_EWRAM - 1) as u32;
            }
            GBA_REGION_IWRAM => {
                self.cpu.active_region = ActiveMemoryRegion::Iwram;
                self.cpu.active_mask = (GBA_SIZE_IWRAM - 1) as u32;
            }
            GBA_REGION_PALETTE_RAM => {
                self.cpu.active_region = ActiveMemoryRegion::Palette;
                self.cpu.active_mask = (GBA_SIZE_PALETTE_RAM - 1) as u32;
            }
            GBA_REGION_VRAM => {
                self.cpu.active_region = ActiveMemoryRegion::Vram {
                    high: address & 0x10000 != 0,
                };
                self.cpu.active_mask = if address & 0x10000 != 0 { 0x7FFF } else { 0xFFFF };
            }
            GBA_REGION_OAM => {
                self.cpu.active_region = ActiveMemoryRegion::Oam;
                self.cpu.active_mask = (video::GBA_SIZE_OAM - 1) as u32;
            }
            GBA_REGION_ROM0
            | GBA_REGION_ROM0_EX
            | GBA_REGION_ROM1
            | GBA_REGION_ROM1_EX
            | GBA_REGION_ROM2
            |             GBA_REGION_ROM2_EX => {
                self.cpu.active_region = ActiveMemoryRegion::Rom {
                    mask: self.memory.rom_mask,
                };
                self.cpu.active_mask = self.memory.rom_mask;
                if (address & (GBA_SIZE_ROM0 as u32 - 1)) < self.memory.rom_size as u32 {
                    // fits — continue to waitstate block below
                } else if (address & 0x00FFFFFE) == AGB_PRINT_FLUSH_ADDR
                    && self.memory.agb_print_protect == 0x20
                {
                    self.cpu.active_region = ActiveMemoryRegion::AgbPrintFunc;
                    self.cpu.active_mask = (AGB_PRINT_FUNC.len() - 1) as u32;
                } else {
                    mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Jumped to invalid address: {:08X}", address);
                    self.memory.active_region = -1;
                    self.cpu.active_region = ActiveMemoryRegion::None;
                    self.cpu.active_mask = 0;
                    return;
                }
            }
            _ => {
                self.memory.active_region = -1;
                self.cpu.active_region = ActiveMemoryRegion::None;
                self.cpu.active_mask = 0;

                mlog!(Level::Warn, rgba_core::log::GBA_MEM, "Jumped to invalid address: {:08X}", address);
                return;
            }
        }
        // Same-region shortcut re-applied mask note: reaches here after the
        // early-return check above.
        let r = self.memory.active_region.max(0) as usize;
        self.cpu.active_seq_cycles32 = self.memory.waitstates_seq32[r];
        self.cpu.active_seq_cycles16 = self.memory.waitstates_seq16[r];
        self.cpu.active_nonseq_cycles32 = self.memory.waitstates_nonseq32[r];
        self.cpu.active_nonseq_cycles16 = self.memory.waitstates_nonseq16[r];
        // activeMask &= -(cpsr.t ? WORD_SIZE_THUMB : WORD_SIZE_ARM)
        if self.cpu.cpsr.t() {
            self.cpu.active_mask |= WORD_SIZE_THUMB as u32;
        } else {
            self.cpu.active_mask &= (0u32).wrapping_sub(WORD_SIZE_ARM as u32);
        }
    }

    /// ActiveRegion loads (prefetch fetches)
    pub fn active_region_load32(&self, offset: u32) -> u32 {
        let off = offset as usize;
        match self.cpu.active_region {
            ActiveMemoryRegion::Bios { mask } => {
                let i = (off & mask as usize) & !3;
                u32::from_le_bytes(self.memory.bios.get(i..i + 4).map(|s| [s[0], s[1], s[2], s[3]]).unwrap_or([0; 4]))
            }
            ActiveMemoryRegion::Ewram => {
                let i = off & (GBA_SIZE_EWRAM - 4);
                u32::from_le_bytes(self.memory.wram[i..i + 4].try_into().unwrap())
            }
            ActiveMemoryRegion::Iwram => {
                let i = off & (GBA_SIZE_IWRAM - 4);
                u32::from_le_bytes(self.memory.iwram[i..i + 4].try_into().unwrap())
            }
            ActiveMemoryRegion::Palette => {
                let i = off & (GBA_SIZE_PALETTE_RAM - 4);
                u32::from_le_bytes(self.video.palette[i..i + 4].try_into().unwrap())
            }
            ActiveMemoryRegion::Vram { high } => {
                let base = if high { 0x8000 } else { 0 };
                let i = base + (off & if high { 0x7FFF } else { 0xFFFF });
                u32::from_le_bytes(self.video.vram[i..i + 4].try_into().unwrap())
            }
            ActiveMemoryRegion::Oam => {
                let i = off & (video::GBA_SIZE_OAM - 4);
                u32::from_le_bytes(self.video.oam[i..i + 4].try_into().unwrap())
            }
            ActiveMemoryRegion::Rom { mask } => {
                let i = off & mask as usize;
                if i + 4 > self.memory.rom.len() {
                    // open-busish; only reachable out-of-range
                    return (((off >> 1) & 0xFFFF) as u32) | ((((off + 2) >> 1) as u32) << 16);
                }
                u32::from_le_bytes(self.memory.rom[i..i + 4].try_into().unwrap())
            }
            ActiveMemoryRegion::AgbPrintFunc => u32::from_le_bytes(AGB_PRINT_FUNC),
            ActiveMemoryRegion::None => u32::from_le_bytes(DEADBEEF),
        }
    }

    pub fn active_region_load16(&self, offset: u32) -> u32 {
        let off = offset as usize;
        let half: u32 = match self.cpu.active_region {
            ActiveMemoryRegion::Bios { mask } => {
                let i = off & (mask as usize & !1);
                u16::from_le_bytes([self.memory.bios.get(i).copied().unwrap_or(0), self.memory.bios.get(i+1).copied().unwrap_or(0)]) as u32
            }
            ActiveMemoryRegion::Ewram => {
                let i = off & (GBA_SIZE_EWRAM - 2);
                u16::from_le_bytes([self.memory.wram[i], self.memory.wram[i + 1]]) as u32
            }
            ActiveMemoryRegion::Iwram => {
                let i = off & (GBA_SIZE_IWRAM - 2);
                u16::from_le_bytes([self.memory.iwram[i], self.memory.iwram[i + 1]]) as u32
            }
            ActiveMemoryRegion::Palette => {
                let i = off & (GBA_SIZE_PALETTE_RAM - 2);
                u16::from_le_bytes([self.video.palette[i], self.video.palette[i + 1]]) as u32
            }
            ActiveMemoryRegion::Vram { high } => {
                let base = if high { 0x8000 } else { 0 };
                let i = base + (off & if high { 0x7FEF.min(0x7FFF) } else { 0xFFFE.min(0xFFFF) });
                u16::from_le_bytes([self.video.vram[i], self.video.vram[i + 1]]) as u32
            }
            ActiveMemoryRegion::Oam => {
                let i = off & (video::GBA_SIZE_OAM - 2);
                u16::from_le_bytes([self.video.oam[i], self.video.oam[i + 1]]) as u32
            }
            ActiveMemoryRegion::Rom { mask } => {
                let i = off & (mask as usize & !1);
                if i + 2 > self.memory.rom.len() {
                    return ((off >> 1) & 0xFFFF) as u32;
                }
                u16::from_le_bytes([self.memory.rom[i], self.memory.rom[i + 1]]) as u32
            }
            ActiveMemoryRegion::AgbPrintFunc => {
                let i = off & 2;
                u16::from_le_bytes([AGB_PRINT_FUNC[i], AGB_PRINT_FUNC[i + 1]]) as u32
            }
            ActiveMemoryRegion::None => {
                let i = off & 2;
                u16::from_le_bytes([DEADBEEF[i], DEADBEEF[i + 1]]) as u32
            }
        };
        half
    }
}

impl Gba {
    /// GBALoadBad (open bus)
    pub fn load_bad(&self) -> u32 {
        let mut value;
        if self.performing_dma != 0
            || (self.cpu.gprs[ARM_PC] as u32).wrapping_sub(self.dma_pc)
                == (if self.cpu.execution_mode == ExecutionMode::Thumb {
                    WORD_SIZE_THUMB
                } else {
                    WORD_SIZE_ARM
                }) as u32
        {
            value = self.bus;
        } else {
            value = self.cpu.prefetch[1];
            if self.cpu.execution_mode == ExecutionMode::Thumb {
                // http://ngemu.com/threads/gba-open-bus.170809/
                match (self.cpu.gprs[ARM_PC] as u32) >> BASE_OFFSET {
                    GBA_REGION_BIOS | GBA_REGION_OAM => {
                        // This isn't right half the time, but we don't have $+6 handy
                        value <<= 16;
                        value |= self.cpu.prefetch[0];
                    }
                    GBA_REGION_IWRAM => {
                        if self.cpu.gprs[ARM_PC] & 2 != 0 {
                            value <<= 16;
                            value |= self.cpu.prefetch[0];
                        } else {
                            value |= self.cpu.prefetch[0] << 16;
                        }
                    }
                    _ => {
                        value |= value << 16;
                    }
                }
            }
        }
        value
    }

    fn dispcnt_mode(&self) -> u16 {
        self.memory.io[(GBA_REG_DISPCNT >> 1) as usize] & 7
    }

    fn load_bios_32(&self, address: u32) -> u32 {
        if address < GBA_SIZE_BIOS as u32 {
            if self.memory.active_region == GBA_REGION_BIOS as i32 {
                let i = (address & !3) as usize;
                return u32::from_le_bytes(self.memory.bios.get(i..i + 4).map(|s| [s[0], s[1], s[2], s[3]]).unwrap_or([0; 4]));
            }
            mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad BIOS Load32: 0x{:08X}", address);
            self.memory.bios_prefetch
        } else {
            mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad memory Load32: 0x{:08X}", address);
            self.load_bad()
        }
    }

    /// GBALoad32
    pub fn load32(&mut self, address: u32, cycle_counter: &mut i32) -> u32 {
        if self.dbg_watchpoints_active {
            self.dbg_watch_load(address, 4, 0);
        }
        let mut wait: i32 = 0;
        let mut value: u32;
        match address >> BASE_OFFSET {
            GBA_REGION_BIOS => {
                value = self.load_bios_32(address);
            }
            GBA_REGION_EWRAM => {
                let i = (address as usize) & (GBA_SIZE_EWRAM - 4);
                value = u32::from_le_bytes(self.memory.wram[i..i + 4].try_into().unwrap());
                wait += self.memory.waitstates_nonseq32[GBA_REGION_EWRAM as usize];
            }
            GBA_REGION_IWRAM => {
                let i = (address as usize) & (GBA_SIZE_IWRAM - 4);
                value = u32::from_le_bytes(self.memory.iwram[i..i + 4].try_into().unwrap());
            }
            GBA_REGION_IO => {
                value = self.io_read(address & OFFSET_MASK & !3) as u32
                    | ((self.io_read((address & OFFSET_MASK & !1) | 2) as u32) << 16);
            }
            GBA_REGION_PALETTE_RAM => {
                let i = (address as usize) & (GBA_SIZE_PALETTE_RAM - 4);
                value = u32::from_le_bytes(self.video.palette[i..i + 4].try_into().unwrap());
                wait += self.memory.waitstates_nonseq32[GBA_REGION_PALETTE_RAM as usize];
            }
            GBA_REGION_VRAM => {
                if (address & 0x0001FFFF) >= GBA_SIZE_VRAM as u32 {
                    if (address & ((GBA_SIZE_VRAM | 0x00014000) as u32)) == GBA_SIZE_VRAM as u32 && self.dispcnt_mode() >= 3 {
                        mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad VRAM Load32: 0x{:08X}", address);
                        value = 0;
                    } else {
                        let i = (address & 0x00017FFC) as usize;
                        value = u32::from_le_bytes(self.video.vram[i..i + 4].try_into().unwrap());
                    }
                } else {
                    let i = (address & 0x0001FFFC) as usize;
                    value = u32::from_le_bytes(self.video.vram[i..i + 4].try_into().unwrap());
                }
                wait += 1;
                if self.video.stall_mask != 0
                    && (address & 0x0001FFFF) < if self.dispcnt_mode() >= 3 { 0x00014000 } else { 0x00010000 }
                {
                    wait += self.memory_stall_vram(wait, 1);
                }
            }
            GBA_REGION_OAM => {
                let i = (address as usize) & (video::GBA_SIZE_OAM - 4);
                value = u32::from_le_bytes(self.video.oam[i..i + 4].try_into().unwrap());
            }
            GBA_REGION_ROM0 | GBA_REGION_ROM0_EX | GBA_REGION_ROM1 | GBA_REGION_ROM1_EX
            | GBA_REGION_ROM2 | GBA_REGION_ROM2_EX => {
                wait += self.memory.waitstates_nonseq32[(address >> BASE_OFFSET) as usize];
                if ((address & (GBA_SIZE_ROM0 as u32 - 4)) as usize) < self.memory.rom_size {
                    let i = (address & (GBA_SIZE_ROM0 as u32 - 4)) as usize;
                    value = u32::from_le_bytes(self.memory.rom[i..i + 4].try_into().unwrap());
                } else if self.unl_cart_is_vfame() {
                    value = self.vfame_get_pattern_value(address, 32);
                } else {
                    mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Out of bounds ROM Load32: 0x{:08X}", address);
                    value = ((address & !3) >> 1) & 0xFFFF;
                    value |= (((address & !3) + 2) >> 1) << 16;
                }
            }
            GBA_REGION_SRAM | GBA_REGION_SRAM_MIRROR => {
                wait = self.memory.waitstates_nonseq16[(address >> BASE_OFFSET) as usize];
                value = self.load8_inner(address);
                value |= value << 8;
                value |= value << 16;
            }
            _ => {
                mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad memory Load32: 0x{:08X}", address);
                value = self.load_bad();
            }
        }

        wait += 2;
        if address < GBA_BASE_ROM0 {
            wait = self.memory_stall(wait);
        }
        *cycle_counter += wait;
        ror32(value, (address & 3) << 3)
    }

    /// GBALoad16
    pub fn load16(&mut self, address: u32, cycle_counter: &mut i32) -> u32 {
        if self.dbg_watchpoints_active {
            self.dbg_watch_load(address, 2, 0);
        }
        let mut wait: i32 = 0;
        let mut value: u32;
        match address >> BASE_OFFSET {
            GBA_REGION_BIOS => {
                if address < GBA_SIZE_BIOS as u32 {
                    if self.memory.active_region == GBA_REGION_BIOS as i32 {
                        let i = (address & !1) as usize;
                        value = u16::from_le_bytes([self.memory.bios[i], self.memory.bios[i + 1]]) as u32;
                    } else {
                        mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad BIOS Load16: 0x{:08X}", address);
                        value = (self.memory.bios_prefetch >> ((address & 2) * 8)) & 0xFFFF;
                    }
                } else {
                    mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad memory Load16: 0x{:08X}", address);
                    value = (self.load_bad() >> ((address & 2) * 8)) & 0xFFFF;
                }
            }
            GBA_REGION_EWRAM => {
                let i = (address as usize) & (GBA_SIZE_EWRAM - 2);
                value = u16::from_le_bytes([self.memory.wram[i], self.memory.wram[i + 1]]) as u32;
                wait = self.memory.waitstates_nonseq16[GBA_REGION_EWRAM as usize];
            }
            GBA_REGION_IWRAM => {
                let i = (address as usize) & (GBA_SIZE_IWRAM - 2);
                value = u16::from_le_bytes([self.memory.iwram[i], self.memory.iwram[i + 1]]) as u32;
            }
            GBA_REGION_IO => {
                value = self.io_read(address & (OFFSET_MASK - 1)) as u32;
            }
            GBA_REGION_PALETTE_RAM => {
                let i = (address as usize) & (GBA_SIZE_PALETTE_RAM - 2);
                value = u16::from_le_bytes([self.video.palette[i], self.video.palette[i + 1]]) as u32;
            }
            GBA_REGION_VRAM => {
                if (address & 0x0001FFFF) >= GBA_SIZE_VRAM as u32 {
                    if (address & ((GBA_SIZE_VRAM | 0x00014000) as u32)) == GBA_SIZE_VRAM as u32 && self.dispcnt_mode() >= 3 {
                        mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad VRAM Load16: 0x{:08X}", address);
                        value = 0;
                    } else {
                        let i = (address & 0x00017FFE) as usize;
                        value = u16::from_le_bytes([self.video.vram[i], self.video.vram[i + 1]]) as u32;
                    }
                } else {
                    let i = (address & 0x0001FFFE) as usize;
                    value = u16::from_le_bytes([self.video.vram[i], self.video.vram[i + 1]]) as u32;
                }
                if self.video.stall_mask != 0
                    && (address & 0x0001FFFF) < if self.dispcnt_mode() >= 3 { 0x00014000 } else { 0x00010000 }
                {
                    wait += self.memory_stall_vram(wait, 0);
                }
            }
            GBA_REGION_OAM => {
                let i = (address as usize) & (video::GBA_SIZE_OAM - 2);
                value = u16::from_le_bytes([self.video.oam[i], self.video.oam[i + 1]]) as u32;
            }
            GBA_REGION_ROM0 | GBA_REGION_ROM0_EX | GBA_REGION_ROM1 | GBA_REGION_ROM1_EX
            | GBA_REGION_ROM2 => {
                wait = self.memory.waitstates_nonseq16[(address >> BASE_OFFSET) as usize];
                if ((address & (GBA_SIZE_ROM0 as u32 - 2)) as usize) < self.memory.rom_size {
                    let i = (address & (GBA_SIZE_ROM0 as u32 - 2)) as usize;
                    value = u16::from_le_bytes([self.memory.rom[i], self.memory.rom[i + 1]]) as u32;
                } else if self.unl_cart_is_vfame() {
                    value = self.vfame_get_pattern_value(address, 16);
                } else if (address & (GBA_SIZE_ROM0 as u32 - 2)) >= AGB_PRINT_BASE {
                    let agb_addr = address & 0x00FFFFFF;
                    if agb_addr == AGB_PRINT_PROTECT {
                        value = self.memory.agb_print_protect as u32;
                    } else if agb_addr < AGB_PRINT_TOP || (agb_addr & 0x00FFFFF8) == AGB_PRINT_STRUCT {
                        value = self.agb_print_load(address) as u32;
                    } else {
                        mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Out of bounds ROM Load16: 0x{:08X}", address);
                        value = (address >> 1) & 0xFFFF;
                    }
                } else {
                    mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Out of bounds ROM Load16: 0x{:08X}", address);
                    value = (address >> 1) & 0xFFFF;
                }
            }
            GBA_REGION_ROM2_EX => {
                wait = self.memory.waitstates_nonseq16[(address >> BASE_OFFSET) as usize];
                if self.savedata_type_is_eeprom() {
                    value = self.savedata_read_eeprom() as u32;
                } else if (address & 0x0DFC0000) >= 0x0DF80000 && self.hw_has_ereader() {
                    value = self.ereader_read(address) as u32;
                } else if ((address & (GBA_SIZE_ROM0 as u32 - 2)) as usize) < self.memory.rom_size {
                    let i = (address & (GBA_SIZE_ROM0 as u32 - 2)) as usize;
                    value = u16::from_le_bytes([self.memory.rom[i], self.memory.rom[i + 1]]) as u32;
                } else if self.unl_cart_is_vfame() {
                    value = self.vfame_get_pattern_value(address, 16);
                } else {
                    mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Out of bounds ROM Load16: 0x{:08X}", address);
                    value = (address >> 1) & 0xFFFF;
                }
            }
            GBA_REGION_SRAM | GBA_REGION_SRAM_MIRROR => {
                wait = self.memory.waitstates_nonseq16[(address >> BASE_OFFSET) as usize];
                value = self.load8_inner(address);
                value |= value << 8;
            }
            _ => {
                mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad memory Load16: 0x{:08X}", address);
                value = (self.load_bad() >> ((address & 2) * 8)) & 0xFFFF;
            }
        }
        wait += 2;
        if address < GBA_BASE_ROM0 {
            wait = self.memory_stall(wait);
        }
        *cycle_counter += wait;
        ror32(value, (address & 1) << 3)
    }

    /// GBALoad8
    pub fn load8(&mut self, address: u32, cycle_counter: &mut i32) -> u32 {
        if self.dbg_watchpoints_active {
            self.dbg_watch_load(address, 1, 0);
        }
        let value = self.load8_inner(address);
        let mut wait: i32 = match address >> BASE_OFFSET {
            GBA_REGION_EWRAM | GBA_REGION_ROM0 | GBA_REGION_ROM0_EX | GBA_REGION_ROM1
            | GBA_REGION_ROM1_EX | GBA_REGION_ROM2 | GBA_REGION_ROM2_EX | GBA_REGION_SRAM
            | GBA_REGION_SRAM_MIRROR => self.memory.waitstates_nonseq16[(address >> BASE_OFFSET) as usize],
            GBA_REGION_VRAM => {
                if self.video.stall_mask != 0 {
                    self.memory_stall_vram(0, 0)
                } else {
                    0
                }
            }
            _ => 0,
        };
        wait += 2;
        if address < GBA_BASE_ROM0 {
            wait = self.memory_stall(wait);
        }
        *cycle_counter += wait;
        value
    }

    // GBALoad8's value half, separated so cycle accounting can reuse the C's
    // structure. Kept inline with the same decision order.
    fn load8_inner(&mut self, address: u32) -> u32 {
        match address >> BASE_OFFSET {
            GBA_REGION_BIOS => {
                if address < GBA_SIZE_BIOS as u32 {
                    if self.memory.active_region == GBA_REGION_BIOS as i32 {
                        self.memory.bios.get(address as usize).copied().unwrap_or(0) as u32
                    } else {
                        mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad BIOS Load8: 0x{:08X}", address);
                        (self.memory.bios_prefetch >> ((address & 3) * 8)) & 0xFF
                    }
                } else {
                    mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad memory Load8: 0x{:08X}", address);
                    (self.load_bad() >> ((address & 3) * 8)) & 0xFF
                }
            }
            GBA_REGION_EWRAM => self.memory.wram[(address as usize) & (GBA_SIZE_EWRAM - 1)] as u32,
            GBA_REGION_IWRAM => self.memory.iwram[(address as usize) & (GBA_SIZE_IWRAM - 1)] as u32,
            GBA_REGION_IO => {
                ((self.io_read(address & 0xFFFE) >> (((address & 1) as u32) << 3)) & 0xFF) as u32
            }
            GBA_REGION_PALETTE_RAM => {
                self.video.palette[(address as usize) & (GBA_SIZE_PALETTE_RAM - 1)] as u32
            }
            GBA_REGION_VRAM => {
                let value;
                if (address & 0x0001FFFF) >= GBA_SIZE_VRAM as u32 {
                    if (address & ((GBA_SIZE_VRAM | 0x00014000) as u32)) == GBA_SIZE_VRAM as u32 && self.dispcnt_mode() >= 3 {
                        mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad VRAM Load8: 0x{:08X}", address);
                        value = 0;
                    } else {
                        value = self.video.vram[(address & 0x00017FFF) as usize] as u32;
                    }
                } else {
                    value = self.video.vram[(address & 0x0001FFFF) as usize] as u32;
                }
                value
            }
            GBA_REGION_OAM => self.video.oam[(address as usize) & (video::GBA_SIZE_OAM - 1)] as u32,
            GBA_REGION_ROM0 | GBA_REGION_ROM0_EX | GBA_REGION_ROM1 | GBA_REGION_ROM1_EX
            | GBA_REGION_ROM2 | GBA_REGION_ROM2_EX => {
                if ((address & (GBA_SIZE_ROM0 as u32 - 1)) as usize) < self.memory.rom_size {
                    self.memory.rom[(address & (GBA_SIZE_ROM0 as u32 - 1)) as usize] as u32
                } else if self.unl_cart_is_vfame() {
                    self.vfame_get_pattern_value(address, 8)
                } else {
                    mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Out of bounds ROM Load8: 0x{:08X}", address);
                    ((address >> 1) >> ((address & 1) * 8)) & 0xFF
                }
            }
            GBA_REGION_SRAM | GBA_REGION_SRAM_MIRROR => self.sram_region_read8(address),
            _ => {
                mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad memory Load8: 0x{:08X}", address);
                (self.load_bad() >> ((address & 3) * 8)) & 0xFF
            }
        }
    }

    /// GBAMemoryStall (prefetch modeling)
    pub fn memory_stall(&mut self, mut wait: i32) -> i32 {
        if self.memory.active_region < GBA_REGION_ROM0 as i32 || !self.memory.prefetch {
            // The wait is the stall
            return wait;
        }

        let mut previous_loads: i32 = 0;
        // Don't prefetch too much if we're overlapping with a previous prefetch
        let dist = self
            .memory
            .last_prefetched_pc
            .wrapping_sub(self.cpu.gprs[ARM_PC] as u32);
        let mut max_loads: i32 = 8;
        if dist < 16 {
            previous_loads = (dist >> 1) as i32;
            max_loads -= previous_loads;
        }

        let s = self.cpu.active_seq_cycles16;
        let mut stall = s + 1;
        let mut loads = 1;

        while stall < wait && loads < max_loads {
            stall += s;
            loads += 1;
        }
        self.memory.last_prefetched_pc = (self.cpu.gprs[ARM_PC] as u32)
            .wrapping_add((WORD_SIZE_THUMB * (loads + previous_loads - 1)) as u32);

        if stall > wait {
            // The wait cannot take less time than the prefetch stalls
            wait = stall;
        }

        // This instruction used to have an N, convert it to an S.
        wait -= self.cpu.active_nonseq_cycles16 - s;

        // The next |loads|S waitstates disappear entirely, so long as they're all in a row
        wait - stall
    }

    /// GBAMemoryStallVRAM
    pub fn memory_stall_vram(&mut self, wait: i32, extra: i32) -> i32 {
        let stall_lut: [u16; 32] = compute_vstall_lut();
        let until = self.until(crate::gba::EventId::Video);
        let period = ((-until) & 0x1F) as i32;

        let mut stall = until;
        let mut extra_left = extra;
        for i in 0..16 {
            if stall_lut[((period + i) & 0x1F) as usize] & self.video.stall_mask as u16 == 0 {
                if extra_left == 0 {
                    stall = i;
                    break;
                }
                extra_left -= 1;
            }
        }

        stall -= wait;
        if stall < 0 {
            return 0;
        }
        stall
    }

    /// GBAAdjustWaitstates (gated by WAITCNT write)
    pub fn adjust_waitstates(&mut self, parameters: u16) {
        let sram = (parameters & 0x0003) as usize;
        let ws0 = ((parameters & 0x000C) >> 2) as usize;
        let ws0seq = ((parameters & 0x0010) >> 4) as usize;
        let ws1 = ((parameters & 0x0060) >> 5) as usize;
        let ws1seq = ((parameters & 0x0080) >> 7) as usize;
        let ws2 = ((parameters & 0x0300) >> 8) as usize;
        let ws2seq = ((parameters & 0x0400) >> 10) as usize;
        let prefetch = parameters & 0x4000 != 0;

        let m = &mut self.memory;
        m.waitstates_nonseq16[GBA_REGION_SRAM as usize] = GBA_ROM_WAITSTATES[sram];
        m.waitstates_nonseq16[GBA_REGION_SRAM_MIRROR as usize] = GBA_ROM_WAITSTATES[sram];
        m.waitstates_seq16[GBA_REGION_SRAM as usize] = GBA_ROM_WAITSTATES[sram];
        m.waitstates_seq16[GBA_REGION_SRAM_MIRROR as usize] = GBA_ROM_WAITSTATES[sram];
        m.waitstates_nonseq32[GBA_REGION_SRAM as usize] = 2 * GBA_ROM_WAITSTATES[sram] + 1;
        m.waitstates_nonseq32[GBA_REGION_SRAM_MIRROR as usize] = 2 * GBA_ROM_WAITSTATES[sram] + 1;
        m.waitstates_seq32[GBA_REGION_SRAM as usize] = 2 * GBA_ROM_WAITSTATES[sram] + 1;
        m.waitstates_seq32[GBA_REGION_SRAM_MIRROR as usize] = 2 * GBA_ROM_WAITSTATES[sram] + 1;

        m.waitstates_nonseq16[GBA_REGION_ROM0 as usize] = GBA_ROM_WAITSTATES[ws0];
        m.waitstates_nonseq16[GBA_REGION_ROM0_EX as usize] = GBA_ROM_WAITSTATES[ws0];
        m.waitstates_nonseq16[GBA_REGION_ROM1 as usize] = GBA_ROM_WAITSTATES[ws1];
        m.waitstates_nonseq16[GBA_REGION_ROM1_EX as usize] = GBA_ROM_WAITSTATES[ws1];
        m.waitstates_nonseq16[GBA_REGION_ROM2 as usize] = GBA_ROM_WAITSTATES[ws2];
        m.waitstates_nonseq16[GBA_REGION_ROM2_EX as usize] = GBA_ROM_WAITSTATES[ws2];

        m.waitstates_seq16[GBA_REGION_ROM0 as usize] = GBA_ROM_WAITSTATES_SEQ[ws0seq];
        m.waitstates_seq16[GBA_REGION_ROM0_EX as usize] = GBA_ROM_WAITSTATES_SEQ[ws0seq];
        m.waitstates_seq16[GBA_REGION_ROM1 as usize] = GBA_ROM_WAITSTATES_SEQ[ws1seq + 2];
        m.waitstates_seq16[GBA_REGION_ROM1_EX as usize] = GBA_ROM_WAITSTATES_SEQ[ws1seq + 2];
        m.waitstates_seq16[GBA_REGION_ROM2 as usize] = GBA_ROM_WAITSTATES_SEQ[ws2seq + 4];
        m.waitstates_seq16[GBA_REGION_ROM2_EX as usize] = GBA_ROM_WAITSTATES_SEQ[ws2seq + 4];

        for pair in [GBA_REGION_ROM0 as usize, GBA_REGION_ROM0_EX as usize] {
            m.waitstates_nonseq32[pair] = m.waitstates_nonseq16[pair] + 1 + m.waitstates_seq16[pair];
        }
        for pair in [GBA_REGION_ROM1 as usize, GBA_REGION_ROM1_EX as usize] {
            m.waitstates_nonseq32[pair] = m.waitstates_nonseq16[pair] + 1 + m.waitstates_seq16[pair];
        }
        for pair in [GBA_REGION_ROM2 as usize, GBA_REGION_ROM2_EX as usize] {
            m.waitstates_nonseq32[pair] = m.waitstates_nonseq16[pair] + 1 + m.waitstates_seq16[pair];
        }
        m.waitstates_seq32[GBA_REGION_ROM0 as usize] = 2 * m.waitstates_seq16[GBA_REGION_ROM0 as usize] + 1;
        m.waitstates_seq32[GBA_REGION_ROM0_EX as usize] = m.waitstates_seq32[GBA_REGION_ROM0 as usize];
        m.waitstates_seq32[GBA_REGION_ROM1 as usize] = 2 * m.waitstates_seq16[GBA_REGION_ROM1 as usize] + 1;
        m.waitstates_seq32[GBA_REGION_ROM1_EX as usize] = m.waitstates_seq32[GBA_REGION_ROM1 as usize];
        m.waitstates_seq32[GBA_REGION_ROM2 as usize] = 2 * m.waitstates_seq16[GBA_REGION_ROM2 as usize] + 1;
        m.waitstates_seq32[GBA_REGION_ROM2_EX as usize] = m.waitstates_seq32[GBA_REGION_ROM2 as usize];

        m.prefetch = prefetch;

        if m.active_region >= 0 {
            let r = m.active_region as usize;
            self.cpu.active_seq_cycles32 = m.waitstates_seq32[r];
            self.cpu.active_seq_cycles16 = m.waitstates_seq16[r];
            self.cpu.active_nonseq_cycles32 = m.waitstates_nonseq32[r];
            self.cpu.active_nonseq_cycles16 = m.waitstates_nonseq16[r];
        }

        if self.memory.agb_print_buffer_backup.is_some() {
            self.agb_print_restore_by_phi((parameters >> 11) & 3);
        }

        if self.performing_dma != 0 {
            self.dma_recalculate_cycles();
        }
    }

    /// GBAAdjustEWRAMWaitstates
    pub fn adjust_ewram_waitstates(&mut self, parameters: u16) {
        let wait = 15 - ((parameters >> 8) & 0xF) as i32;
        if wait != 0 {
            let m = &mut self.memory;
            m.waitstates_nonseq16[GBA_REGION_EWRAM as usize] = wait;
            m.waitstates_seq16[GBA_REGION_EWRAM as usize] = wait;
            m.waitstates_nonseq32[GBA_REGION_EWRAM as usize] = 2 * wait + 1;
            m.waitstates_seq32[GBA_REGION_EWRAM as usize] = 2 * wait + 1;
            if m.active_region >= 0 {
                let r = m.active_region as usize;
                self.cpu.active_seq_cycles32 = m.waitstates_seq32[r];
                self.cpu.active_seq_cycles16 = m.waitstates_seq16[r];
                self.cpu.active_nonseq_cycles32 = m.waitstates_nonseq32[r];
                self.cpu.active_nonseq_cycles16 = m.waitstates_nonseq16[r];
            }
        } else {
            mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Cannot set EWRAM to 0 waitstates");
        }
    }
}

fn compute_vstall_lut() -> [u16; 32] {
    // exact copy of the C stallLUT
    [
    0x0211, 0x0222, 0x0144, 0x0588,
    0x0211, 0x0222, 0x0144, 0x0588,
    0x0200, 0x0200, 0x0100, 0x0500,
    0x0210, 0x0220, 0x0140, 0x0580,
    0x0211, 0x0222, 0x0144, 0x0588,
    0x0211, 0x0222, 0x0144, 0x0588,
    0x0200, 0x0200, 0x0100, 0x0500,
    0x0210, 0x0220, 0x0140, 0x0580,
    ]
}

impl Gba {
    /// GBAStore32
    pub fn store32(&mut self, address: u32, value: i32, cycle_counter: &mut i32) {
        if self.dbg_watchpoints_active {
            self.dbg_watch_store(address, 4, value as u32);
        }
        let mut wait: i32 = 0;
        let mut expected_wait = None::<&mut i32>;
        match address >> BASE_OFFSET {
            GBA_REGION_EWRAM => {
                let i = (address as usize) & (GBA_SIZE_EWRAM - 4);
                self.memory.wram[i..i + 4].copy_from_slice(&value.to_le_bytes());
                wait += self.memory.waitstates_nonseq32[GBA_REGION_EWRAM as usize];
            }
            GBA_REGION_IWRAM => {
                let i = (address as usize) & (GBA_SIZE_IWRAM - 4);
                self.memory.iwram[i..i + 4].copy_from_slice(&value.to_le_bytes());
            }
            GBA_REGION_IO => {
                self.io_write32(address & (OFFSET_MASK - 3), value as u32);
            }
            GBA_REGION_PALETTE_RAM => {
                let i = (address as usize) & (GBA_SIZE_PALETTE_RAM - 4);
                let old = u32::from_le_bytes(self.video.palette[i..i + 4].try_into().unwrap());
                if old != value as u32 {
                    self.video.palette[i..i + 4].copy_from_slice(&value.to_le_bytes());
                    self.renderer_write_palette((i + 2) as u32, ((value as u32) >> 16) as u16);
                    self.renderer_write_palette(i as u32, value as u16);
                }
                wait += self.memory.waitstates_nonseq32[GBA_REGION_PALETTE_RAM as usize];
            }
            GBA_REGION_VRAM => {
                let mode3_obj_limit: u32 = if self.dispcnt_mode() >= 3 { 0x00014000 } else { 0x00010000 };
                if (address & 0x0001FFFF) >= GBA_SIZE_VRAM as u32 {
                    if (address & ((GBA_SIZE_VRAM | 0x00014000) as u32)) == GBA_SIZE_VRAM as u32 && self.dispcnt_mode() >= 3 {
                        mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad VRAM Store32: 0x{:08X}", address);
                    } else {
                        let i = (address & 0x00017FFC) as usize;
                        let old = u32::from_le_bytes(self.video.vram[i..i + 4].try_into().unwrap());
                        if old != value as u32 {
                            self.video.vram[i..i + 4].copy_from_slice(&value.to_le_bytes());
                            self.renderer_write_vram((i + 2) as u32);
                            self.renderer_write_vram(i as u32);
                        }
                    }
                } else {
                    let i = (address & 0x0001FFFC) as usize;
                    let old = u32::from_le_bytes(self.video.vram[i..i + 4].try_into().unwrap());
                    if old != value as u32 {
                        self.video.vram[i..i + 4].copy_from_slice(&value.to_le_bytes());
                        self.renderer_write_vram((i + 2) as u32);
                        self.renderer_write_vram(i as u32);
                    }
                }
                let _ = mode3_obj_limit;
                wait += 1;
                if self.video.stall_mask != 0
                    && (address & 0x0001FFFF) < if self.dispcnt_mode() >= 3 { 0x00014000 } else { 0x00010000 }
                {
                    wait += self.memory_stall_vram(wait, 1);
                }
            }
            GBA_REGION_OAM => {
                let i = (address as usize) & (video::GBA_SIZE_OAM - 4);
                let old = u32::from_le_bytes(self.video.oam[i..i + 4].try_into().unwrap());
                if old != value as u32 {
                    self.video.oam[i..i + 4].copy_from_slice(&value.to_le_bytes());
                    self.renderer_write_oam((i >> 1) as u32);
                    self.renderer_write_oam(((i >> 1) + 1) as u32);
                }
            }
            GBA_REGION_ROM0 | GBA_REGION_ROM0_EX | GBA_REGION_ROM1 | GBA_REGION_ROM1_EX
            | GBA_REGION_ROM2 | GBA_REGION_ROM2_EX => {
                wait += self.memory.waitstates_nonseq32[(address >> BASE_OFFSET) as usize];
                if self.matrix_active() && (address & 0x01FFFF00) == 0x00800100 {
                    self.matrix_write(address & 0x3C, value as u32);
                } else {
                    mlog!(Level::Stub, rgba_core::log::GBA_MEM, "Unimplemented memory Store32: 0x{:08X}", address);
                }
            }
            GBA_REGION_SRAM | GBA_REGION_SRAM_MIRROR => {
                let mut shifts = (8 * (address & 3)) as i32;
                let v8 = (value >> shifts) as i8 as i32;
                self.store8(address, v8, cycle_counter);
            }
            _ => {
                mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad memory Store32: 0x{:08X}", address);
            }
        }
        let _ = expected_wait;
        wait += 1;
        if address < GBA_BASE_ROM0 {
            wait = self.memory_stall(wait);
        }
        *cycle_counter += wait;
    }

    /// GBAStore16
    pub fn store16(&mut self, address: u32, value: i32, cycle_counter: &mut i32) {
        if self.dbg_watchpoints_active {
            self.dbg_watch_store(address, 2, value as u32);
        }
        let mut wait: i32 = 0;
        let value = value as u16;
        match address >> BASE_OFFSET {
            GBA_REGION_EWRAM => {
                let i = (address as usize) & (GBA_SIZE_EWRAM - 2);
                self.memory.wram[i..i + 2].copy_from_slice(&value.to_le_bytes());
                wait = self.memory.waitstates_nonseq16[GBA_REGION_EWRAM as usize];
            }
            GBA_REGION_IWRAM => {
                let i = (address as usize) & (GBA_SIZE_IWRAM - 2);
                self.memory.iwram[i..i + 2].copy_from_slice(&value.to_le_bytes());
            }
            GBA_REGION_IO => {
                self.io_write(address & (OFFSET_MASK - 1), value);
            }
            GBA_REGION_PALETTE_RAM => {
                let i = (address as usize) & (GBA_SIZE_PALETTE_RAM - 2);
                let old = u16::from_le_bytes([self.video.palette[i], self.video.palette[i + 1]]);
                if old != value {
                    self.video.palette[i] = value as u8;
                    self.video.palette[i + 1] = (value >> 8) as u8;
                    self.renderer_write_palette(i as u32, value);
                }
            }
            GBA_REGION_VRAM => {
                if (address & 0x0001FFFF) >= GBA_SIZE_VRAM as u32 {
                    if (address & ((GBA_SIZE_VRAM | 0x00014000) as u32)) == GBA_SIZE_VRAM as u32 && self.dispcnt_mode() >= 3 {
                        mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad VRAM Store16: 0x{:08X}", address);
                    } else {
                        let i = (address & 0x00017FFE) as usize;
                        let old = u16::from_le_bytes([self.video.vram[i], self.video.vram[i + 1]]);
                        if value != old {
                            self.video.vram[i] = value as u8;
                            self.video.vram[i + 1] = (value >> 8) as u8;
                            self.renderer_write_vram(i as u32);
                        }
                    }
                } else {
                    let i = (address & 0x0001FFFE) as usize;
                    let old = u16::from_le_bytes([self.video.vram[i], self.video.vram[i + 1]]);
                    if value != old {
                        self.video.vram[i] = value as u8;
                        self.video.vram[i + 1] = (value >> 8) as u8;
                        self.renderer_write_vram(i as u32);
                    }
                }
                if self.video.stall_mask != 0
                    && (address & 0x0001FFFF) < if self.dispcnt_mode() >= 3 { 0x00014000 } else { 0x00010000 }
                {
                    wait += self.memory_stall_vram(wait, 0);
                }
            }
            GBA_REGION_OAM => {
                let i = (address as usize) & (video::GBA_SIZE_OAM - 2);
                let old = u16::from_le_bytes([self.video.oam[i], self.video.oam[i + 1]]);
                if value != old {
                    self.video.oam[i] = value as u8;
                    self.video.oam[i + 1] = (value >> 8) as u8;
                    self.renderer_write_oam((i >> 1) as u32);
                }
            }
            GBA_REGION_ROM0 => {
                if is_gpio_register(address & 0xFFFFFE) {
                    if !self.hw_has_gpio() {
                        mlog!(Level::Warn, rgba_core::log::GBA_HW, "Write to GPIO address {:08X} on cartridge without GPIO", address);
                        return;
                    }
                    let reg = address & 0xFFFFFE;
                    self.gpio_write(reg, value);
                    return;
                }
                if self.matrix_active() && (address & 0x01FFFF00) == 0x00800100 {
                    self.matrix_write16(address & 0x3C, value);
                    return;
                }
                // Fall through (to the ROM0_EX shared tail)
                self.store16_rom0_ex_tail(address, value);
            }
            GBA_REGION_ROM0_EX => {
                self.store16_rom0_ex_tail(address, value);
            }
            GBA_REGION_ROM2_EX => {
                if (address & 0x0DFC0000) >= 0x0DF80000 && self.hw_has_ereader() {
                    self.ereader_write(address, value);
                    return;
                } else if self.savedata_type_is_autodetect() {
                    mlog!(Level::Info, rgba_core::log::GBA_MEM, "Detected EEPROM savegame");
                    self.savedata_init_eeprom();
                }
                if self.savedata_type_is_eeprom() {
                    self.savedata_write_eeprom(value, 1);
                    return;
                }
                mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad memory Store16: 0x{:08X}", address);
            }
            GBA_REGION_ROM1 | GBA_REGION_ROM1_EX | GBA_REGION_ROM2 => {
                mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad memory Store16: 0x{:08X}", address);
            }
            _ => {
                mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad memory Store16: 0x{:08X}", address);
            }
        }
        wait += 1;
        if address < GBA_BASE_ROM0 {
            wait = self.memory_stall(wait);
        }
        *cycle_counter += wait;
    }

    /// The shared tail of STORE16's cart cases.
    fn store16_rom0_ex_tail(&mut self, address: u32, value: u16) {
        if (address & 0x00FFFFFF) >= AGB_PRINT_BASE {
            self.agb_print_write16(address, value);
            return;
        }
        if self.unl_cart_is_present() {
            self.unl_cart_write16(address & (GBA_SIZE_ROM0 as u32 - 1), value);
            return;
        }
        mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad cartridge Store16: 0x{:08X}", address);
    }

}

// Additional glue referenced by the ISA and IO layers.
impl Gba {
    /// cpu->memory.stall == GBAMemoryStall
    pub fn cpu_stall(&mut self, wait: i32) -> i32 {
        self.memory_stall(wait)
    }
}

impl Gba {
    pub fn io_read(&mut self, address: u32) -> u16 {
        self.io_read_real(address)
    }
    pub fn io_write(&mut self, address: u32, value: u16) {
        self.io_write_real(address, value);
    }
    pub fn io_write32(&mut self, address: u32, value: u32) {
        self.io_write32_real(address, value);
    }
    pub fn io_write8(&mut self, address: u32, value: u8) {
        self.io_write8_real(address, value);
    }
    pub fn io_read32(&mut self, address: u32) -> u32 {
        self.io_read(address & !3) as u32 | ((self.io_read(address | 2) as u32) << 16)
    }
}

// Remaining pieces of memory.c.
impl Gba {
    /// GBAStore8
    pub fn store8(&mut self, address: u32, value: i32, cycle_counter: &mut i32) {
        if self.dbg_watchpoints_active {
            self.dbg_watch_store(address, 1, value as u32);
        }
        let mut wait: i32 = 0;
        let value = value as u8;
        match address >> BASE_OFFSET {
            GBA_REGION_EWRAM => {
                self.memory.wram[(address as usize) & (GBA_SIZE_EWRAM - 1)] = value;
                wait = self.memory.waitstates_nonseq16[GBA_REGION_EWRAM as usize];
            }
            GBA_REGION_IWRAM => {
                self.memory.iwram[(address as usize) & (GBA_SIZE_IWRAM - 1)] = value;
            }
            GBA_REGION_IO => {
                self.io_write8(address & OFFSET_MASK, value);
            }
            GBA_REGION_PALETTE_RAM => {
                self.store16(address & !1, ((value as u16) | ((value as u16) << 8)) as i32, cycle_counter);
            }
            GBA_REGION_VRAM => {
                let obj_limit: u32 = if self.dispcnt_mode() >= 3 { 0x00014000 } else { 0x00010000 };
                if (address & 0x0001FFFF) >= obj_limit {
                    mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Cannot Store8 to OBJ: 0x{:08X}", address);
                } else {
                    let i = (address & 0x1FFFE) as usize;
                    let old = u16::from_le_bytes([self.video.vram[i], self.video.vram[i + 1]]);
                    let new = (value as u16) | ((value as u16) << 8);
                    if old != new {
                        self.video.vram[i] = value;
                        self.video.vram[i + 1] = value;
                        self.renderer_write_vram(i as u32);
                    }
                }
                if self.video.stall_mask != 0 {
                    wait += self.memory_stall_vram(wait, 0);
                }
            }
            GBA_REGION_OAM => {
                mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Cannot Store8 to OAM: 0x{:08X}", address);
            }
            GBA_REGION_ROM0 => {
                mlog!(Level::Stub, rgba_core::log::GBA_MEM, "Unimplemented memory Store8: 0x{:08X}", address);
            }
            GBA_REGION_SRAM | GBA_REGION_SRAM_MIRROR => {
                self.sram_region_write8(address, value);
                wait = self.memory.waitstates_nonseq16[GBA_REGION_SRAM as usize];
            }
            _ => {
                mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad memory Store8: 0x{:08X}", address);
            }
        }
        wait += 1;
        if address < GBA_BASE_ROM0 {
            wait = self.memory_stall(wait);
        }
        *cycle_counter += wait;
    }
}

impl Gba {
    /// GBALoadMultiple (LDM/STM bus)
    pub fn load_multiple(&mut self, address: u32, mask: i32, direction: i32, cycle_counter: &mut i32) -> u32 {
        if self.dbg_watchpoints_active {
            self.dbg_watch_multiple(address, mask, direction, false);
        }
        let mut address = address;
        let mut value: u32 = 0;
        let mut offset: i32 = 4;
        let mut popcount = 0i32;
        if direction & LSM_DA_D != 0 {
            offset = -4;
            popcount = (mask as u32).count_ones() as i32;
            address = address.wrapping_sub(((popcount << 2) - 4) as u32);
        }
        if direction & LSM_B != 0 {
            address = address.wrapping_add(offset as u32);
        }
        let address_misalign = address & 0x3;
        let region = address >> BASE_OFFSET;
        if region < GBA_REGION_SRAM {
            address &= 0xFFFFFFFC;
        }
        let mut wait = self.memory.waitstates_seq32[region as usize] - self.memory.waitstates_nonseq32[region as usize];

        macro_rules! ldm_loop_body {
            ($ldm:expr) => {
                if mask == 0 {
                    let v: u32 = $ldm(self, address, &mut wait);
                    self.cpu.gprs[ARM_PC] = v as i32;
                    wait += 16;
                    address = address.wrapping_add(64);
                }
                for i in (0..16).step_by(4) {
                    if mask & (1 << i) != 0 {
                        value = $ldm(self, address, &mut wait);
                        self.cpu.gprs[i] = value as i32;
                        wait += 1;
                        address = address.wrapping_add(4);
                    }
                    if mask & (2 << i) != 0 {
                        value = $ldm(self, address, &mut wait);
                        self.cpu.gprs[i + 1] = value as i32;
                        wait += 1;
                        address = address.wrapping_add(4);
                    }
                    if mask & (4 << i) != 0 {
                        value = $ldm(self, address, &mut wait);
                        self.cpu.gprs[i + 2] = value as i32;
                        wait += 1;
                        address = address.wrapping_add(4);
                    }
                    if mask & (8 << i) != 0 {
                        value = $ldm(self, address, &mut wait);
                        self.cpu.gprs[i + 3] = value as i32;
                        wait += 1;
                        address = address.wrapping_add(4);
                    }
                }
            };
        }

        match region {
            GBA_REGION_BIOS => {
                ldm_loop_body!(|g: &mut Gba, a: u32, w: &mut i32| { let _ = w; g.load_bios_32(a) });
            }
            GBA_REGION_EWRAM => {
                ldm_loop_body!(|g: &mut Gba, a: u32, w: &mut i32| {
                    let i = (a as usize) & (GBA_SIZE_EWRAM - 4);
                    *w += g.memory.waitstates_nonseq32[GBA_REGION_EWRAM as usize];
                    u32::from_le_bytes(g.memory.wram[i..i + 4].try_into().unwrap())
                });
            }
            GBA_REGION_IWRAM => {
                ldm_loop_body!(|g: &mut Gba, a: u32, _w: &mut i32| {
                    let i = (a as usize) & (GBA_SIZE_IWRAM - 4);
                    u32::from_le_bytes(g.memory.iwram[i..i + 4].try_into().unwrap())
                });
            }
            GBA_REGION_IO => {
                ldm_loop_body!(|g: &mut Gba, a: u32, _w: &mut i32| {
                    g.io_read32(a)
                });
            }
            GBA_REGION_PALETTE_RAM => {
                ldm_loop_body!(|g: &mut Gba, a: u32, w: &mut i32| {
                    let i = (a as usize) & (GBA_SIZE_PALETTE_RAM - 4);
                    *w += g.memory.waitstates_nonseq32[GBA_REGION_PALETTE_RAM as usize];
                    u32::from_le_bytes(g.video.palette[i..i + 4].try_into().unwrap())
                });
            }
            GBA_REGION_VRAM => {
                ldm_loop_body!(|g: &mut Gba, a: u32, w: &mut i32| {
                    let v;
                    if (a & 0x0001FFFF) >= GBA_SIZE_VRAM as u32 {
                        if (a & ((GBA_SIZE_VRAM | 0x00014000) as u32)) == GBA_SIZE_VRAM as u32 && g.dispcnt_mode() >= 3 {
                            mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad VRAM Load32: 0x{:08X}", a);
                            v = 0;
                        } else {
                            let i = (a & 0x00017FFC) as usize;
                            v = u32::from_le_bytes(g.video.vram[i..i + 4].try_into().unwrap());
                        }
                    } else {
                        let i = (a & 0x0001FFFC) as usize;
                        v = u32::from_le_bytes(g.video.vram[i..i + 4].try_into().unwrap());
                    }
                    *w += 1;
                    if g.video.stall_mask != 0 && (a & 0x0001FFFF) < if g.dispcnt_mode() >= 3 { 0x00014000 } else { 0x00010000 } {
                        let ww = *w;
                        *w += g.memory_stall_vram(ww, 1);
                    }
                    v
                });
            }
            GBA_REGION_OAM => {
                ldm_loop_body!(|g: &mut Gba, a: u32, _w: &mut i32| {
                    let i = (a as usize) & (crate::video::GBA_SIZE_OAM as u32 as usize - 4) as usize;
                    u32::from_le_bytes(g.video.oam[i..i + 4].try_into().unwrap())
                });
            }
            GBA_REGION_ROM0 | GBA_REGION_ROM0_EX | GBA_REGION_ROM1 | GBA_REGION_ROM1_EX | GBA_REGION_ROM2 | GBA_REGION_ROM2_EX => {
                ldm_loop_body!(|g: &mut Gba, a: u32, w: &mut i32| {
                    *w += g.memory.waitstates_nonseq32[(a >> BASE_OFFSET) as usize];
                    if ((a & (GBA_SIZE_ROM0 as u32 - 4)) as usize) < g.memory.rom_size {
                        let i = (a & (GBA_SIZE_ROM0 as u32 - 4)) as usize;
                        u32::from_le_bytes(g.memory.rom[i..i + 4].try_into().unwrap())
                    } else if g.unl_cart_is_vfame() {
                        g.vfame_get_pattern_value(a, 32)
                    } else {
                        mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Out of bounds ROM Load32: 0x{:08X}", a);
                        let mut v = ((a & !3) >> 1) & 0xFFFF;
                        v |= (((a & !3) + 2) >> 1) << 16;
                        v
                    }
                });
            }
            GBA_REGION_SRAM | GBA_REGION_SRAM_MIRROR => {
                ldm_loop_body!(|g: &mut Gba, a: u32, w: &mut i32| {
                    *w = g.memory.waitstates_nonseq16[(a >> BASE_OFFSET) as usize];
                    let mut v = g.load8_inner(a);
                    v |= v << 8;
                    v |= v << 16;
                    v
                });
            }
            _ => {
                ldm_loop_body!(|g: &mut Gba, _a: u32, _w: &mut i32| g.load_bad());
            }
        }

        wait += 1;
        if address < GBA_BASE_ROM0 {
            wait = self.memory_stall(wait);
        }
        *cycle_counter += wait;
        if direction & LSM_B != 0 {
            address = address.wrapping_sub(offset as u32);
        }
        if direction & LSM_DA_D != 0 {
            address = address.wrapping_sub(((popcount << 2) + 4) as u32);
        }
        address | address_misalign
    }

    /// GBAStoreMultiple
    pub fn store_multiple(&mut self, address: u32, mask: i32, direction: i32, cycle_counter: &mut i32) -> u32 {
        if self.dbg_watchpoints_active {
            self.dbg_watch_multiple(address, mask, direction, true);
        }
        let mut address = address;
        let mut offset: i32 = 4;
        let mut popcount = 0i32;
        if direction & LSM_DA_D != 0 {
            offset = -4;
            popcount = (mask as u32).count_ones() as i32;
            address = address.wrapping_sub(((popcount << 2) - 4) as u32);
        }
        if direction & LSM_B != 0 {
            address = address.wrapping_add(offset as u32);
        }
        let address_misalign = address & 0x3;
        let region = address >> BASE_OFFSET;
        if region < GBA_REGION_SRAM {
            address &= 0xFFFFFFFC;
        }
        let mut wait = self.memory.waitstates_seq32[region as usize] - self.memory.waitstates_nonseq32[region as usize];

        macro_rules! stm_loop_body {
            ($stm:expr) => {
                if mask == 0 {
                    let value = (self.cpu.gprs[ARM_PC] as u32).wrapping_add(if self.cpu.execution_mode == ExecutionMode::Arm { WORD_SIZE_ARM } else { WORD_SIZE_THUMB } as u32);
                    $stm(self, address, value, &mut wait);
                    wait += 16;
                    address = address.wrapping_add(64);
                }
                for i in (0..16).step_by(4) {
                    if mask & (1 << i) != 0 {
                        let value = self.cpu.gprs[i] as u32;
                        $stm(self, address, value, &mut wait);
                        address = address.wrapping_add(4);
                    }
                    if mask & (2 << i) != 0 {
                        let value = self.cpu.gprs[i + 1] as u32;
                        $stm(self, address, value, &mut wait);
                        address = address.wrapping_add(4);
                    }
                    if mask & (4 << i) != 0 {
                        let value = self.cpu.gprs[i + 2] as u32;
                        $stm(self, address, value, &mut wait);
                        address = address.wrapping_add(4);
                    }
                    if mask & (8 << i) != 0 {
                        let mut value = self.cpu.gprs[i + 3] as u32;
                        if i + 3 == ARM_PC {
                            value = value.wrapping_add(WORD_SIZE_ARM as u32);
                        }
                        $stm(self, address, value, &mut wait);
                        address = address.wrapping_add(4);
                    }
                }
            };
        }

        match region {
            GBA_REGION_EWRAM => {
                stm_loop_body!(|g: &mut Gba, a: u32, v: u32, w: &mut i32| {
                    let i = (a as usize) & (GBA_SIZE_EWRAM - 4);
                    g.memory.wram[i..i + 4].copy_from_slice(&v.to_le_bytes());
                    *w += g.memory.waitstates_nonseq32[GBA_REGION_EWRAM as usize];
                });
            }
            GBA_REGION_IWRAM => {
                stm_loop_body!(|g: &mut Gba, a: u32, v: u32, _w: &mut i32| {
                    let i = (a as usize) & (GBA_SIZE_IWRAM - 4);
                    g.memory.iwram[i..i + 4].copy_from_slice(&v.to_le_bytes());
                });
            }
            GBA_REGION_IO => {
                stm_loop_body!(|g: &mut Gba, a: u32, v: u32, _w: &mut i32| {
                    g.io_write32(a, v);
                });
            }
            GBA_REGION_PALETTE_RAM => {
                stm_loop_body!(|g: &mut Gba, a: u32, v: u32, w: &mut i32| {
                    let i = (a as usize) & (GBA_SIZE_PALETTE_RAM - 4);
                    *w += g.memory.waitstates_nonseq32[GBA_REGION_PALETTE_RAM as usize];
                    let old = u32::from_le_bytes(g.video.palette[i..i + 4].try_into().unwrap());
                    if old != v {
                        g.video.palette[i..i + 4].copy_from_slice(&v.to_le_bytes());
                        g.renderer_write_palette((i + 2) as u32, (v >> 16) as u16);
                        g.renderer_write_palette(i as u32, v as u16);
                    }
                });
            }
            GBA_REGION_VRAM => {
                stm_loop_body!(|g: &mut Gba, a: u32, v: u32, w: &mut i32| {
                    *w += 1;
                    if g.video.stall_mask != 0 && (a & 0x0001FFFF) < if g.dispcnt_mode() >= 3 { 0x00014000 } else { 0x00010000 } {
                        let ww = *w;
                        *w += g.memory_stall_vram(ww, 1);
                    }
                    let limit: u32 = if g.dispcnt_mode() >= 3 { 0x00014000 } else { 0x00010000 };
                    let i = (a & if (a & 0x0001FFFF) >= GBA_SIZE_VRAM as u32 { 0x00017FFC } else { 0x0001FFFC }) as usize;
                    let v2 = v.to_le_bytes();
                    let old = u32::from_le_bytes(g.video.vram[i..i + 4].try_into().unwrap());
                    if old != v {
                        g.video.vram[i..i + 4].copy_from_slice(&v2);
                        g.renderer_write_vram((i + 2) as u32);
                        g.renderer_write_vram(i as u32);
                    }
                    let _ = limit;
                });
            }
            GBA_REGION_OAM => {
                // STORE_OAM: store + both writeOAM callbacks (the port was
                // missing these; oam_dirty never got set on STM-to-OAM).
                stm_loop_body!(|g: &mut Gba, a: u32, v: u32, _w: &mut i32| {
                    let i = (a as usize) & (crate::video::GBA_SIZE_OAM as usize - 4);
                    let old = u32::from_le_bytes(g.video.oam[i..i + 4].try_into().unwrap());
                    if old != v {
                        g.video.oam[i..i + 4].copy_from_slice(&v.to_le_bytes());
                        g.renderer_write_oam((i >> 1) as u32);
                        g.renderer_write_oam(((i >> 1) + 1) as u32);
                    }
                });
            }
            GBA_REGION_ROM0 | GBA_REGION_ROM0_EX | GBA_REGION_ROM1 | GBA_REGION_ROM1_EX | GBA_REGION_ROM2 | GBA_REGION_ROM2_EX => {
                mlog!(Level::Stub, rgba_core::log::GBA_MEM, "Unimplemented STM to ROM");
            }
            GBA_REGION_SRAM | GBA_REGION_SRAM_MIRROR => {
                stm_loop_body!(|g: &mut Gba, a: u32, v: u32, _w: &mut i32| {
                    { let mut discard = 0i32; g.store8(a, (v >> (8 * (a & 3))) as i32, &mut discard); }
                });
            }
            _ => {
                mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Bad memory STM");
            }
        }

        if address < GBA_BASE_ROM0 {
            wait = self.memory_stall(wait);
        }
        *cycle_counter += wait;
        if direction & LSM_B != 0 {
            address = address.wrapping_sub(offset as u32);
        }
        if direction & LSM_DA_D != 0 {
            address = address.wrapping_sub(((popcount << 2) + 4) as u32);
        }
        address | address_misalign
    }

    /// GBAPatch family is used by cheats / save editors — port as part of memory.
    pub fn patch16(&mut self, address: u32, value: i16, old: Option<&mut i16>) {
        let mut old_value: i16 = -1;
        match address >> BASE_OFFSET {
            GBA_REGION_EWRAM => {
                let i = (address as usize) & (GBA_SIZE_EWRAM - 2);
                old_value = i16::from_le_bytes([self.memory.wram[i], self.memory.wram[i + 1]]);
                self.memory.wram[i] = value as u8;
                self.memory.wram[i + 1] = (value >> 8) as u8;
            }
            GBA_REGION_IWRAM => {
                let i = (address as usize) & (GBA_SIZE_IWRAM - 2);
                old_value = i16::from_le_bytes([self.memory.iwram[i], self.memory.iwram[i + 1]]);
                self.memory.iwram[i] = value as u8;
                self.memory.iwram[i + 1] = (value >> 8) as u8;
            }
            GBA_REGION_ROM0 | GBA_REGION_ROM0_EX | GBA_REGION_ROM1 | GBA_REGION_ROM1_EX
            | GBA_REGION_ROM2 | GBA_REGION_ROM2_EX => {
                if (address as usize) + 2 > self.memory.rom_size {
                    // C grows romSize + romMask here
                    self.memory.rom_size = ((address & 0xFFFFF) + 2) as usize;
                }
                let i = (address as usize) & (GBA_SIZE_ROM0 - 2);
                old_value = i16::from_le_bytes([self.memory.rom[i], self.memory.rom[i + 1]]);
                self.memory.rom[i] = value as u8;
                self.memory.rom[i + 1] = (value >> 8) as u8;
            }
            GBA_REGION_VRAM => {
                let i = if (address & 0x0001FFFF) < GBA_SIZE_VRAM as u32 { (address & 0x0001FFFE) as usize } else { (address & 0x00017FFE) as usize };
                old_value = i16::from_le_bytes([self.video.vram[i], self.video.vram[i + 1]]);
                self.video.vram[i] = value as u8;
                self.video.vram[i + 1] = (value >> 8) as u8;
                self.renderer_write_vram(i as u32);
            }
            GBA_REGION_OAM => {
                let i = (address as usize) & (video::GBA_SIZE_OAM - 2);
                old_value = i16::from_le_bytes([self.video.oam[i], self.video.oam[i + 1]]);
                self.video.oam[i] = value as u8;
                self.video.oam[i + 1] = (value >> 8) as u8;
                self.renderer_write_oam((i >> 1) as u32);
            }
            GBA_REGION_PALETTE_RAM => {
                // The C's GBAPatch16 palette case (memory.c) also notifies the
                // renderer; the port was missing it.
                let i = (address as usize) & (GBA_SIZE_PALETTE_RAM - 2);
                old_value = i16::from_le_bytes([self.video.palette[i], self.video.palette[i + 1]]);
                self.video.palette[i] = value as u8;
                self.video.palette[i + 1] = (value >> 8) as u8;
                self.renderer_write_palette(i as u32, value as u16);
            }
            _ => {
                mlog!(Level::Warn, rgba_core::log::GBA_MEM, "Bad memory Patch16: 0x{:08X}", address);
            }
        }
        if let Some(o) = old {
            *o = old_value;
        }
    }

    pub fn patch32(&mut self, address: u32, value: i32, old: Option<&mut i32>) {
        let mut old_value: i32 = -1;
        match address >> BASE_OFFSET {
            GBA_REGION_EWRAM => {
                let i = (address as usize) & (GBA_SIZE_EWRAM - 4);
                old_value = i32::from_le_bytes(self.memory.wram[i..i + 4].try_into().unwrap());
                self.memory.wram[i..i + 4].copy_from_slice(&value.to_le_bytes());
            }
            GBA_REGION_IWRAM => {
                let i = (address as usize) & (GBA_SIZE_IWRAM - 4);
                old_value = i32::from_le_bytes(self.memory.iwram[i..i + 4].try_into().unwrap());
                self.memory.iwram[i..i + 4].copy_from_slice(&value.to_le_bytes());
            }
            GBA_REGION_PALETTE_RAM => {
                let i = (address as usize) & (GBA_SIZE_PALETTE_RAM - 4);
                old_value = i32::from_le_bytes(self.video.palette[i..i + 4].try_into().unwrap());
                self.video.palette[i..i + 4].copy_from_slice(&value.to_le_bytes());
                self.renderer_write_palette(i as u32, value as u16);
                self.renderer_write_palette((i + 2) as u32, ((value >> 16) & 0xFFFF) as u16);
            }
            GBA_REGION_VRAM => {
                let i = if (address & 0x0001FFFF) < GBA_SIZE_VRAM as u32 { (address & 0x0001FFFC) as usize } else { (address & 0x00017FFC) as usize };
                old_value = i32::from_le_bytes(self.video.vram[i..i + 4].try_into().unwrap());
                self.video.vram[i..i + 4].copy_from_slice(&value.to_le_bytes());
                self.renderer_write_vram(i as u32);
                self.renderer_write_vram((i + 2) as u32);
            }
            GBA_REGION_OAM => {
                let i = (address as usize) & (video::GBA_SIZE_OAM - 4);
                old_value = i32::from_le_bytes(self.video.oam[i..i + 4].try_into().unwrap());
                self.video.oam[i..i + 4].copy_from_slice(&value.to_le_bytes());
                self.renderer_write_oam((i >> 1) as u32);
                self.renderer_write_oam(((i >> 1) + 1) as u32);
            }
            GBA_REGION_ROM0 | GBA_REGION_ROM0_EX | GBA_REGION_ROM1 | GBA_REGION_ROM1_EX
            | GBA_REGION_ROM2 | GBA_REGION_ROM2_EX => {
                if (address as usize) + 4 > self.memory.rom_size {
                    self.memory.rom_size = ((address as usize) + 4) as usize;
                }
                let i = (address as usize) & (GBA_SIZE_ROM0 - 4);
                let old = i32::from_le_bytes(self.memory.rom[i..i + 4].try_into().unwrap());
                old_value = old;
                self.memory.rom[i..i + 4].copy_from_slice(&value.to_le_bytes());
            }
            GBA_REGION_SRAM | GBA_REGION_SRAM_MIRROR => {
                let i = (address as usize) & (self.savedata.data.len() - 1);
                if self.savedata.data.len() >= 4 {
                    old_value = i32::from_le_bytes(self.savedata.data[i..i + 4].try_into().unwrap());
                    self.savedata.data[i..i + 4].copy_from_slice(&value.to_le_bytes());
                }
            }
            _ => {
                mlog!(Level::Stub, rgba_core::log::GBA_MEM, "Unimplemented memory Patch32: 0x{:08X}", address);
            }
        }
        if let Some(o) = old {
            *o = old_value;
        }
    }

    pub fn patch8(&mut self, address: u32, value: i8, old: Option<&mut i8>) {
        let mut old_value: i8 = -1;
        match address >> BASE_OFFSET {
            GBA_REGION_EWRAM => {
                let i = (address as usize) & (GBA_SIZE_EWRAM - 1);
                old_value = self.memory.wram[i] as i8;
                self.memory.wram[i] = value as u8;
            }
            GBA_REGION_IWRAM => {
                let i = (address as usize) & (GBA_SIZE_IWRAM - 1);
                old_value = self.memory.iwram[i] as i8;
                self.memory.iwram[i] = value as u8;
            }
            _ => {
                mlog!(Level::Warn, rgba_core::log::GBA_MEM, "Bad memory Patch8: 0x{:08X}", address);
            }
        }
        if let Some(o) = old {
            *o = old_value;
        }
    }
    /// GBAView8: side-effect-free raw read (debugger).
    pub fn view8(&mut self, address: u32) -> u8 {
        let old_guard = self.debugger_view_guard(true);
        let v = self.view8_inner(address);
        self.debugger_view_guard(old_guard);
        v
    }

    fn view8_inner(&mut self, address: u32) -> u8 {
        let mut cycle_counter = 0i32;
        match address >> BASE_OFFSET {
            GBA_REGION_BIOS => {
                if address < GBA_SIZE_BIOS as u32 {
                    self.memory.bios[address as usize]
                } else {
                    0
                }
            }
            GBA_REGION_EWRAM | GBA_REGION_IWRAM | GBA_REGION_ROM0 | GBA_REGION_ROM0_EX
            | GBA_REGION_ROM1 | GBA_REGION_ROM1_EX | GBA_REGION_ROM2 | GBA_REGION_ROM2_EX
            | GBA_REGION_SRAM => self.load8(address, &mut cycle_counter) as u8,
            GBA_REGION_IO | GBA_REGION_PALETTE_RAM | GBA_REGION_VRAM | GBA_REGION_OAM => {
                (self.view16(address) >> ((address & 1) * 8)) as u8
            }
            _ => 0,
        }
    }

    /// GBAView16
    pub fn view16(&mut self, address: u32) -> u16 {
        let old_guard = self.debugger_view_guard(true);
        let v = self.view16_inner(address);
        self.debugger_view_guard(old_guard);
        v
    }

    fn view16_inner(&mut self, address: u32) -> u16 {
        let mut cycle_counter = 0i32;
        let address = address & !1;
        match address >> BASE_OFFSET {
            GBA_REGION_BIOS => {
                if address < GBA_SIZE_BIOS as u32 {
                    u16::from_le_bytes([
                        self.memory.bios[address as usize],
                        self.memory.bios[address as usize + 1],
                    ])
                } else {
                    0
                }
            }
            GBA_REGION_EWRAM | GBA_REGION_IWRAM | GBA_REGION_PALETTE_RAM | GBA_REGION_VRAM
            | GBA_REGION_OAM | GBA_REGION_ROM0 | GBA_REGION_ROM0_EX | GBA_REGION_ROM1
            | GBA_REGION_ROM1_EX | GBA_REGION_ROM2 | GBA_REGION_ROM2_EX => {
                self.load16(address, &mut cycle_counter) as u16
            }
            GBA_REGION_IO => {
                let off = address & OFFSET_MASK;
                if off < crate::io::regs::GBA_REG_MAX || off == crate::io::regs::GBA_REG_POSTFLG {
                    self.memory.io[(off >> 1) as usize]
                } else if off == crate::io::regs::GBA_REG_EXWAITCNT_LO
                    || off == crate::io::regs::GBA_REG_EXWAITCNT_HI
                {
                    let off = off.wrapping_add(
                        crate::io::regs::GBA_REG_INTERNAL_EXWAITCNT_LO
                            .wrapping_sub(crate::io::regs::GBA_REG_EXWAITCNT_LO),
                    );
                    self.memory.io[(off >> 1) as usize]
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    /// GBAView32
    pub fn view32(&mut self, address: u32) -> u32 {
        let old_guard = self.debugger_view_guard(true);
        let v = self.view32_inner(address);
        self.debugger_view_guard(old_guard);
        v
    }

    fn view32_inner(&mut self, address: u32) -> u32 {
        let mut cycle_counter = 0i32;
        let address = address & !3;
        match address >> BASE_OFFSET {
            GBA_REGION_BIOS => {
                if address < GBA_SIZE_BIOS as u32 {
                    u32::from_le_bytes(
                        self.memory.bios[address as usize..address as usize + 4]
                            .try_into()
                            .unwrap(),
                    )
                } else {
                    0
                }
            }
            GBA_REGION_EWRAM | GBA_REGION_IWRAM | GBA_REGION_PALETTE_RAM | GBA_REGION_VRAM
            | GBA_REGION_OAM | GBA_REGION_ROM0 | GBA_REGION_ROM0_EX | GBA_REGION_ROM1
            | GBA_REGION_ROM1_EX | GBA_REGION_ROM2 | GBA_REGION_ROM2_EX => {
                self.load32(address, &mut cycle_counter)
            }
            GBA_REGION_IO => {
                self.view16(address) as u32 | ((self.view16(address + 2) as u32) << 16)
            }
            GBA_REGION_SRAM => {
                self.load8(address, &mut cycle_counter)
                    | self.load8(address + 1, &mut cycle_counter) << 8
                    | self.load8(address + 2, &mut cycle_counter) << 16
                    | self.load8(address + 3, &mut cycle_counter) << 24
            }
            _ => 0,
        }
    }
}

/// IS_GPIO_REGISTER

pub fn is_gpio_register(reg: u32) -> bool {
    reg == 0xC4 || reg == 0xC6 || reg == 0xC8
}

pub const LSM_DA_D: i32 = 2; // LSM_D
pub const LSM_B: i32 = 1;



impl Gba {
    /// GBAMemoryReset
    pub fn memory_reset(&mut self) {
        for b in self.memory.wram.iter_mut() { *b = 0; }
        for b in self.memory.iwram.iter_mut() { *b = 0; }
        self.memory.io = [0; 518];
        self.memory.init_waitstates();
        let rcnt = RCNT_INITIAL;
        self.memory.io[(GBA_REG_RCNT >> 1) as usize] = rcnt;
        self.memory.prefetch = false;
        self.memory.last_prefetched_pc = 0;
        // GBAMemoryReset's GBAUnlCartReset (bootleg carts).
        self.unl_cart_reset();
        self.savedata_reset();
        self.dma_reset();
    }
}
