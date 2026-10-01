// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gb/memory.c and include/mgba/internal/gb/memory.h.
//
// The C's `SM83Memory` fetch cache (cpuLoad8/activeRegion/activeMask/
// activeRegionEnd) is modeled by `ActiveFetch`; `load8/store8/cpu_load8`
// are the GBLoad8/GBStore8/GBCartLoad8 ports.

use rgba_core::mlog;
use rgba_core::Level;

use crate::gb::{EventId, Gb, GbModel};
use crate::video::GB_SIZE_VRAM_BANK0;
use crate::io::GB_REG_IE;

pub const GB_BASE_CART_BANK0: u16 = 0x0000;
pub const GB_BASE_CART_BANK1: u16 = 0x4000;
pub const GB_BASE_CART_HALFBANK1: u16 = 0x4000;
pub const GB_BASE_CART_HALFBANK2: u16 = 0x6000;
pub const GB_BASE_VRAM: u16 = 0x8000;
pub const GB_BASE_EXTERNAL_RAM: u16 = 0xA000;
pub const GB_BASE_WORKING_RAM_BANK0: u16 = 0xC000;
pub const GB_BASE_WORKING_RAM_BANK1: u16 = 0xD000;
pub const GB_BASE_OAM: u16 = 0xFE00;
pub const GB_BASE_UNUSABLE: u16 = 0xFEA0;
pub const GB_BASE_IO: u16 = 0xFF00;
pub const GB_BASE_HRAM: u16 = 0xFF80;
pub const GB_BASE_IE: u16 = 0xFFFF;

pub const GB_REGION_CART_BANK0: u16 = 0x0;
pub const GB_REGION_CART_BANK1: u16 = 0x4;
pub const GB_REGION_VRAM: u16 = 0x8;
pub const GB_REGION_EXTERNAL_RAM: u16 = 0xA;
pub const GB_REGION_WORKING_RAM_BANK0: u16 = 0xC;
pub const GB_REGION_WORKING_RAM_BANK1: u16 = 0xD;
pub const GB_REGION_WORKING_RAM_BANK1_MIRROR: u16 = 0xE;

pub const GB_SIZE_CART_BANK0: usize = 0x4000;
pub const GB_SIZE_CART_HALFBANK: usize = 0x2000;
pub const GB_SIZE_CART_MAX: usize = 0x800000;
pub const GB_SIZE_EXTERNAL_RAM: usize = 0x2000;
pub const GB_SIZE_EXTERNAL_RAM_HALFBANK: usize = 0x1000;
pub const GB_SIZE_WORKING_RAM: usize = 0x8000;
pub const GB_SIZE_WORKING_RAM_BANK0: usize = 0x1000;
pub const GB_SIZE_IO: usize = 0x80;
pub const GB_SIZE_HRAM: usize = 0x7F;

pub const GB_SIZE_MBC6_FLASH: usize = 0x100000;

pub const M_SAVEDATA_DIRT_NEW: i32 = 1;

/// GBMemoryBankControllerType
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MbcType {
    #[default]
    Autodetect = -1,
    None = 0,
    Mbc1 = 1,
    Mbc2 = 2,
    Mbc3 = 3,
    Mbc5 = 5,
    Mbc6 = 6,
    Mbc7 = 7,
    Mmm01 = 0x10,
    HuC1 = 0x11,
    HuC3 = 0x12,
    PocketCam = 0x13,
    Tama5 = 0x14,
    M161 = 0x15,
    Mbc3Rtc = 0x103,
    Mbc5Rumble = 0x105,
    UnlWisdomTree = 0x200,
    UnlPkjd = 0x203,
    UnlNtOld1 = 0x210,
    UnlNtOld2 = 0x211,
    UnlNtNew = 0x212,
    UnlBbd = 0x220,
    UnlHitek = 0x221,
    UnlLiCheng = 0x222,
    UnlGgb81 = 0x223,
    UnlSachenMmc1 = 0x230,
    UnlSachenMmc2 = 0x231,
    UnlSintax = 0x240,
}

/// MBC union states, flattened into one struct (the C is a tagged-union-by-
/// mbcType; extra memory is negligible and safe).
#[derive(Default)]
pub struct MbcState {
    pub mbc1: crate::mbc::licensed::Mbc1State,
    pub mbc6: crate::mbc::licensed::Mbc6State,
    pub mbc7: crate::mbc::licensed::Mbc7State,
    pub mmm01: crate::mbc::licensed::Mmm01State,
    pub pocket_cam: crate::mbc::pocket_cam::PocketCamState,
    pub tama5: crate::mbc::tama5::Tama5State,
    pub huc3: crate::mbc::huc3::HuC3State,
    pub m161: crate::mbc::unlicensed::M161State,
    pub nt_old: crate::mbc::unlicensed::NtOldState,
    pub nt_new: crate::mbc::unlicensed::NtNewState,
    pub pkjd: crate::mbc::unlicensed::PkjdState,
    pub bbd: crate::mbc::unlicensed::BbdState,
    pub sachen: crate::mbc::unlicensed::SachenState,
    pub sintax: crate::mbc::unlicensed::SintaxState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GbBus {
    Cpu,
    Main,
    Vram,
    Ram,
}

const OAM_BLOCK_DMG: [GbBus; 8] = [
    GbBus::Main,
    GbBus::Main,
    GbBus::Main,
    GbBus::Main,
    GbBus::Vram,
    GbBus::Main,
    GbBus::Main,
    GbBus::Cpu,
];

const OAM_BLOCK_CGB: [GbBus; 8] = [
    GbBus::Main,
    GbBus::Main,
    GbBus::Main,
    GbBus::Main,
    GbBus::Vram,
    GbBus::Main,
    GbBus::Ram,
    GbBus::Cpu,
];

/// The fetch path cache for instruction loads (cpuLoad8), mirroring the C's
/// activeRegion/activeMask/activeRegionEnd triple.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ActiveFetch {
    /// GBLoad8 (full dispatch)
    Direct,
    /// GBCartLoad8 on a memory window: `base` indexes memory.rom, or
    /// memory.sram when `sram` is set (MBC6 flash pages).
    Rom {
        base: usize,
        end: u16,
        mask: u16,
        sram: bool,
    },
    /// GBCartLoad8 with the BIOS buffer mapped over bank0.
    Bios { end: u16, mask: u16 },
    /// _yankBuffer/_blockedRegion: reads 0xFF until `end` (end = previous
    /// region end, matching the C which leaves activeRegionEnd unchanged).
    Blocked { end: u16 },
}

pub struct Memory {
    pub rom: Vec<u8>,
    pub rom_size: usize,
    /// Offset of romBase within rom (bank0 window), when not BIOS-mapped.
    pub rom_base_off: usize,
    /// Offset of romBank within rom (the 0x4000-0x7FFF window).
    pub rom_bank_off: usize,
    pub rom_bank1_off: usize,
    /// Sram-backed "rom" window source flags for MBC6 flash.
    pub rom_bank_is_sram: bool,
    pub rom_bank1_is_sram: bool,
    pub mbc_type: MbcType,
    pub mbc_state: MbcState,
    pub current_bank: i32,
    pub current_bank0: i32,
    pub current_bank1: i32,
    pub current_sram_bank1: i32,
    pub sram_bank1_off: usize,

    pub cart_bus_decay: i32,
    pub cart_bus_pc: u16,
    pub cart_bus: u8,

    pub wram: Vec<u8>,
    /// Offset of the C000-DFFF window bank within wram.
    pub wram_bank_off: usize,
    pub wram_current_bank: i32,

    pub mbc_read_bank0: bool,
    pub mbc_read_bank1: bool,
    pub mbc_read_high: bool,
    pub mbc_write_high: bool,

    pub sram_access: bool,
    pub direct_sram_access: bool,
    pub sram: Vec<u8>,
    pub sram_bank_off: usize,
    pub sram_current_bank: i32,

    pub io: [u8; GB_SIZE_IO],
    pub ime: bool,
    pub ie: u8,

    pub hram: [u8; GB_SIZE_HRAM + 1],

    pub dma_source: u16,
    pub dma_dest: u16,
    pub dma_remaining: i32,

    pub hdma_source: u16,
    pub hdma_dest: u16,
    pub hdma_remaining: i32,
    pub is_hdma: bool,

    /// BIOS image supplied by the frontend (unmapped unless bios_mapped set).
    pub bios: Option<Vec<u8>>,
    /// When set, a 16 KiB buffer replacing the bank0 window (BIOS + ROM
    /// remainder composite, per GBMapBIOS).
    pub bios_mapped: Option<Vec<u8>>,

    pub active_fetch: ActiveFetch,

    pub rtc_access: bool,
    pub active_rtc_reg: i32,
    pub rtc_latched: bool,
    /// Set by MBC rumble writes (frontend reads it to drive haptics).
    pub rumble_state: bool,
    /// RTC save-file suffix (appended after SRAM on disk), in-memory copy.
    pub rtc_save_suffix: Option<Vec<u8>>,
    /// C has exactly 5; MBC code keeps activeRtcReg within 0..=4. We clamp on
    /// index to stay panic-free on garbage state.
    pub rtc_regs: [u8; 5],
    pub rtc_last_latch: i64,
}

impl Memory {
    #[inline]
    pub fn rtc_reg(&self) -> u8 {
        self.rtc_regs[self.active_rtc_reg.clamp(0, 4) as usize]
    }
    #[inline]
    pub fn set_rtc_reg(&mut self, v: u8) {
        self.rtc_regs[self.active_rtc_reg.clamp(0, 4) as usize] = v;
    }
    /// True when the current MBC installs a read handler (C: memory->mbcRead
    /// != NULL).
    pub fn mbc_has_read(&self) -> bool {
        use MbcType::*;
        matches!(
            self.mbc_type,
            Mbc2 | Mbc6 | Mbc7 | HuC3 | Tama5 | PocketCam | UnlSachenMmc2
        ) || (self.mbc_type == UnlSachenMmc1 && true) // see mbc module for address scoping
    }
    /// True when the read handler applies to this address (C: mbcRead is only
    /// called for the 0xA000-0xBFFF region unless mbcReadBank0/1/High).
    pub fn mbc_read_for(&self, address: u16) -> bool {
        if !self.mbc_has_read() {
            return false;
        }
        match self.mbc_type {
            MbcType::UnlSachenMmc1 => (address & 0xFF00) == 0x4000 || address >= 0xA000,
            _ => true,
        }
    }
}

pub const YANK_BUFFER: u8 = 0xFF;
pub const BLOCKED_READ: u8 = 0xFF;

impl Memory {
    pub fn new() -> Self {
        Memory {
            rom: Vec::new(),
            rom_size: 0,
            rom_base_off: 0,
            rom_bank_off: 0,
            rom_bank1_off: 0,
            rom_bank_is_sram: false,
            rom_bank1_is_sram: false,
            mbc_type: MbcType::Autodetect,
            mbc_state: MbcState::default(),
            current_bank: 1,
            current_bank0: 0,
            current_bank1: 0,
            current_sram_bank1: 0,
            sram_bank1_off: 0,
            cart_bus_decay: 4,
            cart_bus_pc: 0,
            cart_bus: 0,
            wram: Vec::new(),
            wram_bank_off: GB_SIZE_WORKING_RAM_BANK0,
            wram_current_bank: 1,
            mbc_read_bank0: false,
            mbc_read_bank1: false,
            mbc_read_high: false,
            mbc_write_high: false,
            sram_access: false,
            direct_sram_access: true,
            sram: Vec::new(),
            sram_bank_off: 0,
            sram_current_bank: 0,
            io: [0; GB_SIZE_IO],
            ime: false,
            ie: 0,
            hram: [0; GB_SIZE_HRAM + 1],
            dma_source: 0,
            dma_dest: 0,
            dma_remaining: 0,
            hdma_source: 0,
            hdma_dest: 0,
            hdma_remaining: 0,
            is_hdma: false,
            bios: None,
            bios_mapped: None,
            active_fetch: ActiveFetch::Direct,
            rtc_access: false,
            active_rtc_reg: 0,
            rtc_latched: false,
            rtc_regs: [0; 5],
            rtc_last_latch: 0,
            rumble_state: false,
            rtc_save_suffix: None,
        }
    }

    /// True while the bank0 window shows the ROM (not swapped to BIOS).
    pub fn rom_base_is_mapped(&self) -> bool {
        self.bios_mapped.is_none()
    }

    pub fn rom_base_reset(&mut self) {
        self.rom_base_off = 0;
        self.bios_mapped = None;
    }

    #[inline]
    pub fn rom_byte(&self, off: usize) -> u8 {
        // The C relies on bank switching to keep pointers in range; the
        // wrapping in switch_bank keeps base valid, but guard anyway.
        self.rom.get(off).copied().unwrap_or(0xFF)
    }

    #[inline]
    pub fn sram_byte(&self, off: usize) -> u8 {
        self.sram.get(off).copied().unwrap_or(0xFF)
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

impl Gb {
    /// GBMemoryReset
    pub fn memory_reset(&mut self) {
        self.memory.wram = vec![0; GB_SIZE_WORKING_RAM];
        if self.model.is_cgb() {
            // Banks 0, 1, 3, 6, and 7 are cleared with this pattern
            cgb_wram_init_pattern(&mut self.memory.wram);
        }
        self.memory_switch_wram_bank(1);
        self.memory.ime = false;
        self.memory.ie = 0;

        self.memory.dma_remaining = 0;
        self.memory.dma_source = 0;
        self.memory.dma_dest = 0;
        self.memory.hdma_remaining = 0;
        self.memory.hdma_source = 0;
        self.memory.hdma_dest = 0;
        self.memory.is_hdma = false;

        self.memory.hram = [0; GB_SIZE_HRAM + 1];

        self.mbc_reset();
    }

    /// GBMemorySwitchWramBank
    pub fn memory_switch_wram_bank(&mut self, bank: i32) {
        let mut bank = bank & 7;
        if bank == 0 {
            bank = 1;
        }
        self.memory.wram_bank_off = GB_SIZE_WORKING_RAM_BANK0 * bank as usize;
        self.memory.wram_current_bank = bank;
    }

    #[inline]
    fn oam_block_table(&self) -> &'static [GbBus; 8] {
        if !self.model.is_cgb() {
            &OAM_BLOCK_DMG
        } else {
            &OAM_BLOCK_CGB
        }
    }

    /// GBLoad8 (also the fallthrough fetch path when ActiveFetch::Direct)
    pub fn load8(&mut self, address: u16) -> u8 {
        if self.dbg_watchpoints_active {
            self.dbg_watch_load8(address);
        }
        if self.memory.dma_remaining != 0 {
            let block = self.oam_block_table();
            let dma_bus = block[(self.memory.dma_source >> 13) as usize & 7];
            let access_bus = block[(address >> 13) as usize & 7];
            if dma_bus != GbBus::Cpu && dma_bus == access_bus {
                return 0xFF;
            }
            if (GB_BASE_OAM..GB_BASE_IO).contains(&address) {
                return 0xFF;
            }
        }
        match address >> 12 {
            0x0 | 0x1 | 0x2 | 0x3 => {
                let m = &mut self.memory;
                if let Some(bios) = &m.bios_mapped {
                    if m.rom_base_off == 0 {
                        m.cart_bus = bios.get(address as usize).copied().unwrap_or(0xFF);
                        m.cart_bus_pc = self.cpu.pc;
                        return m.cart_bus;
                    }
                }
                let cart_bus = if (address as usize) >= m.rom_size {
                    0xFF
                } else if m.mbc_read_bank0 {
                    self.mbc_read_dispatched(address)
                } else {
                    let off = m.rom_base_off + (address as usize & (GB_SIZE_CART_BANK0 - 1));
                    m.rom_byte(off)
                };
                self.memory.cart_bus = cart_bus;
                self.memory.cart_bus_pc = self.cpu.pc;
                self.memory.cart_bus
            }
            0x6 | 0x7 => {
                if self.memory.mbc_type == MbcType::Mbc6
                    || (self.memory.mbc_type == MbcType::UnlNtNew && self.memory.mbc_state.nt_new.split_mode)
                {
                    let m = &mut self.memory;
                    let off = m.rom_bank1_off + (address as usize & (GB_SIZE_CART_HALFBANK - 1));
                    m.cart_bus = if m.rom_bank1_is_sram { m.sram_byte(off) } else { m.rom_byte(off) };
                    m.cart_bus_pc = self.cpu.pc;
                    return m.cart_bus;
                }
                self.load8_rom_bank1(address)
            }
            0x4 | 0x5 => self.load8_rom_bank1(address),
            0x8 | 0x9 => {
                if self.video.mode != 3 {
                    self.video.vram[(self.video.vram_current_bank as usize * GB_SIZE_VRAM_BANK0 as usize)
                        + (address as usize & (GB_SIZE_VRAM_BANK0 as usize - 1))]
                } else {
                    0xFF
                }
            }
            0xA | 0xB => {
                let m = &mut self.memory;
                let cart_bus = if m.rtc_access {
                    m.rtc_reg()
                } else if m.mbc_read_for(address) {
                    self.mbc_read_dispatched(address)
                } else if m.sram_access && !m.sram.is_empty() {
                    m.sram_byte(m.sram_bank_off + (address as usize & (GB_SIZE_EXTERNAL_RAM - 1)))
                } else if m.mbc_type == MbcType::HuC3 {
                    0x01 // TODO: Is this supposed to be the current SRAM bank?
                } else if self.cpu.t_multiplier * (self.cpu.pc.wrapping_sub(m.cart_bus_pc)) as i32
                    >= m.cart_bus_decay
                {
                    0xFF
                } else {
                    m.cart_bus
                };
                self.memory.cart_bus = cart_bus;
                self.memory.cart_bus_pc = self.cpu.pc;
                self.memory.cart_bus
            }
            0xC | 0xE => {
                if self.memory.mbc_read_high {
                    self.mbc_read_dispatched(address);
                }
                self.memory.wram[address as usize & (GB_SIZE_WORKING_RAM_BANK0 - 1)]
            }
            0xD => {
                if self.memory.mbc_read_high {
                    self.mbc_read_dispatched(address);
                }
                self.memory.wram[self.memory.wram_bank_off + (address as usize & (GB_SIZE_WORKING_RAM_BANK0 - 1))]
            }
            _ => {
                if address < GB_BASE_OAM {
                    return self.memory.wram
                        [self.memory.wram_bank_off + (address as usize & (GB_SIZE_WORKING_RAM_BANK0 - 1))];
                }
                if address < GB_BASE_UNUSABLE {
                    if self.video.mode < 2 {
                        return self.video.oam[(address & 0xFF) as usize];
                    }
                    return 0xFF;
                }
                if address < GB_BASE_IO {
                    mlog!(
                        Level::GameError,
                        rgba_core::log::GB_MEM,
                        "Attempt to read from unusable memory: {:04X}",
                        address
                    );
                    return 0xFF;
                }
                if address < GB_BASE_HRAM {
                    return self.io_read(address & (GB_SIZE_IO as u16 - 1));
                }
                if address < GB_BASE_IE {
                    return self.memory.hram[address as usize & GB_SIZE_HRAM];
                }
                self.io_read(GB_REG_IE as u16)
            }
        }
    }

    fn load8_rom_bank1(&mut self, address: u16) -> u8 {
        let m = &mut self.memory;
        let cart_bus = if (address as usize) >= m.rom_size {
            0xFF
        } else if m.mbc_read_bank1 {
            self.mbc_read_dispatched(address)
        } else {
            let off = m.rom_bank_off + (address as usize & (GB_SIZE_CART_BANK0 - 1));
            if m.rom_bank_is_sram { m.sram_byte(off) } else { m.rom_byte(off) }
        };
        self.memory.cart_bus = cart_bus;
        self.memory.cart_bus_pc = self.cpu.pc;
        self.memory.cart_bus
    }

    /// GBStore8
    pub fn store8(&mut self, address: u16, value: u8) {
        if self.dbg_watchpoints_active {
            self.dbg_watch_store8(address, value);
        }
        if self.memory.dma_remaining != 0 {
            let block = self.oam_block_table();
            let dma_bus = block[(self.memory.dma_source >> 13) as usize & 7];
            let access_bus = block[(address >> 13) as usize & 7];
            if dma_bus != GbBus::Cpu && dma_bus == access_bus {
                return;
            }
            if (GB_BASE_OAM..GB_BASE_UNUSABLE).contains(&address) {
                return;
            }
        }
        match address >> 12 {
            0x0 | 0x1 | 0x2 | 0x3 | 0x4 | 0x5 | 0x6 | 0x7 => {
                self.mbc_write_dispatched(address, value);
                self.set_active_region(self.cpu.pc);
            }
            0x8 | 0x9 => {
                if self.video.mode != 3 {
                    let off = (address as usize & (GB_SIZE_VRAM_BANK0 as usize - 1))
                        + GB_SIZE_VRAM_BANK0 as usize * self.video.vram_current_bank as usize;
                    self.renderer_write_vram(off as u16);
                    self.video.vram[off] = value;
                }
            }
            0xA | 0xB => {
                if self.memory.rtc_access {
                    self.memory.set_rtc_reg(value);
                } else if self.memory.sram_access
                    && !self.memory.sram.is_empty()
                    && self.memory.direct_sram_access
                {
                    let off = self.memory.sram_bank_off
                        + (address as usize & (GB_SIZE_EXTERNAL_RAM - 1));
                    if self.memory.sram[off] != value {
                        self.memory.sram[off] = value;
                        self.sram_dirty |= M_SAVEDATA_DIRT_NEW;
                    }
                } else {
                    self.mbc_write_dispatched(address, value);
                }
            }
            0xC | 0xE => {
                if self.memory.mbc_write_high {
                    self.mbc_write_dispatched(address, value);
                }
                self.memory.wram[address as usize & (GB_SIZE_WORKING_RAM_BANK0 - 1)] = value;
            }
            0xD => {
                if self.memory.mbc_write_high {
                    self.mbc_write_dispatched(address, value);
                }
                self.memory.wram[self.memory.wram_bank_off
                    + (address as usize & (GB_SIZE_WORKING_RAM_BANK0 - 1))] = value;
            }
            _ => {
                if address < GB_BASE_OAM {
                    let off = self.memory.wram_bank_off
                        + (address as usize & (GB_SIZE_WORKING_RAM_BANK0 - 1));
                    self.memory.wram[off] = value;
                } else if address < GB_BASE_UNUSABLE {
                    if self.video.mode < 2 {
                        self.video.oam[(address & 0xFF) as usize] = value;
                        self.renderer_write_oam(address & 0xFF);
                    }
                } else if address < GB_BASE_IO {
                    mlog!(
                        Level::GameError,
                        rgba_core::log::GB_MEM,
                        "Attempt to write to unusable memory: {:04X}:{:02X}",
                        address,
                        value
                    );
                } else if address < GB_BASE_HRAM {
                    self.io_write(address & (GB_SIZE_IO as u16 - 1), value);
                } else if address < GB_BASE_IE {
                    self.memory.hram[address as usize & GB_SIZE_HRAM] = value;
                } else {
                    self.io_write(GB_REG_IE as u16, value);
                }
            }
        }
    }

    /// GBCartLoad8 — the fetched-opcode path, with the active-region cache.
    pub fn cpu_load8(&mut self, address: u16) -> u8 {
        match self.memory.active_fetch {
            ActiveFetch::Rom { base, end, mask, sram } => {
                if address >= end {
                    self.set_active_region(address);
                    return self.cpu_load8(address);
                }
                self.memory.cart_bus_pc = address;
                let off = base + (address & mask) as usize;
                let value = if sram {
                    self.memory.sram_byte(off)
                } else {
                    self.memory.rom_byte(off)
                };
                self.memory.cart_bus = value;
                value
            }
            ActiveFetch::Bios { end, .. } => {
                if address >= end {
                    self.set_active_region(address);
                    return self.cpu_load8(address);
                }
                self.memory.cart_bus_pc = address;
                let value = self
                    .memory
                    .bios_mapped
                    .as_deref()
                    .and_then(|b| b.get(address as usize))
                    .copied()
                    .unwrap_or(0xFF);
                self.memory.cart_bus = value;
                value
            }
            ActiveFetch::Blocked { end } => {
                if address >= end {
                    self.set_active_region(address);
                    return self.cpu_load8(address);
                }
                0xFF
            }
            ActiveFetch::Direct => self.load8(address),
        }
    }

    /// GBSetActiveRegion
    pub fn set_active_region(&mut self, address: u16) {
        match address >> 12 {
            0x0 | 0x1 | 0x2 | 0x3 => {
                if self.memory.mbc_read_bank0 {
                    self.memory.active_fetch = ActiveFetch::Direct;
                } else if self.memory.bios_mapped.is_some() && self.memory.rom_base_off == 0 {
                    self.memory.active_fetch = ActiveFetch::Bios {
                        end: GB_BASE_CART_BANK1,
                        mask: (GB_SIZE_CART_BANK0 - 1) as u16,
                    };
                } else {
                    let mut end = GB_BASE_CART_BANK1;
                    let base = self.memory.rom_base_off;
                    let mut mask = (GB_SIZE_CART_BANK0 - 1) as u16;
                    let mut yank = false;
                    if self.memory.rom_size < GB_SIZE_CART_BANK0 {
                        if address as usize >= self.memory.rom_size {
                            yank = true;
                            mask = 0;
                        } else {
                            end = self.memory.rom_size as u16;
                        }
                    }
                    self.memory.active_fetch = if yank {
                        ActiveFetch::Blocked { end }
                    } else {
                        ActiveFetch::Rom { base, end, mask, sram: false }
                    };
                }
            }
            0x4 | 0x5 | 0x6 | 0x7 => {
                if self.memory.mbc_read_bank1 {
                    self.memory.active_fetch = ActiveFetch::Direct;
                } else {
                    let normal_layout = self.memory.mbc_type != MbcType::Mbc6
                        && !(self.memory.mbc_type == MbcType::UnlNtNew
                            && self.memory.mbc_state.nt_new.split_mode);
                    let (base, mut end, mut mask, sram);
                    if normal_layout {
                        base = self.memory.rom_bank_off;
                        end = GB_BASE_VRAM;
                        mask = (GB_SIZE_CART_BANK0 - 1) as u16;
                        sram = self.memory.rom_bank_is_sram;
                    } else {
                        mask = (GB_SIZE_CART_HALFBANK - 1) as u16;
                        if address & 0x2000 != 0 {
                            base = self.memory.rom_bank1_off;
                            end = GB_BASE_VRAM;
                            sram = self.memory.rom_bank1_is_sram;
                        } else {
                            base = self.memory.rom_bank_off;
                            end = GB_BASE_CART_BANK1 + 0x2000;
                            sram = self.memory.rom_bank_is_sram;
                        }
                    }
                    let mut yank = false;
                    if self.memory.rom_size < GB_SIZE_CART_BANK0 * 2 {
                        if address as usize >= self.memory.rom_size {
                            yank = true;
                            mask = 0;
                        } else {
                            end = self.memory.rom_size as u16;
                        }
                    }
                    self.memory.active_fetch = if yank {
                        ActiveFetch::Blocked { end }
                    } else {
                        ActiveFetch::Rom { base, end, mask, sram }
                    };
                }
            }
            _ => {
                self.memory.active_fetch = ActiveFetch::Direct;
                self.memory.cart_bus = 0xFF;
            }
        }
        if self.memory.dma_remaining != 0 {
            let block = self.oam_block_table();
            let dma_bus = block[(self.memory.dma_source >> 13) as usize & 7];
            let access_bus = block[(address >> 13) as usize & 7];
            let end = match self.memory.active_fetch {
                ActiveFetch::Rom { end, .. } => end,
                ActiveFetch::Bios { end, .. } => end,
                ActiveFetch::Blocked { end } => end,
                ActiveFetch::Direct => GB_BASE_IE,
            };
            let blocked = (dma_bus != GbBus::Cpu && dma_bus == access_bus)
                || (GB_BASE_OAM..GB_BASE_UNUSABLE).contains(&address);
            if blocked && !matches!(self.memory.active_fetch, ActiveFetch::Direct) {
                // The C overrides the cached region with _blockedRegion.
                self.memory.active_fetch = ActiveFetch::Blocked { end };
            } else if blocked {
                // Direct path: GBLoad8 itself (and the DMA check therein) yields 0xFF.
            }
        }
    }

    /// GBCurrentSegment
    pub fn current_segment(&self, address: u16) -> i32 {
        match address >> 12 {
            0x0..=0x3 => 0,
            0x4..=0x7 => self.memory.current_bank,
            0x8 | 0x9 => self.video.vram_current_bank,
            0xA | 0xB => self.memory.sram_current_bank,
            0xC | 0xE => 0,
            0xD => self.memory.wram_current_bank,
            _ => 0,
        }
    }

    /// GBView8
    pub fn view8(&mut self, address: u16, segment: i32) -> u8 {
        match address >> 12 {
            0x0..=0x3 => self.memory.rom[address as usize & (GB_SIZE_CART_BANK0 - 1)],
            0x4..=0x7 => {
                if segment < 0 {
                    self.memory.rom_byte(
                        self.memory.rom_bank_off + (address as usize & (GB_SIZE_CART_BANK0 - 1)),
                    )
                } else if segment as usize * GB_SIZE_CART_BANK0 < self.memory.rom_size {
                    self.memory.rom_byte(
                        (address as usize & (GB_SIZE_CART_BANK0 - 1))
                            + segment as usize * GB_SIZE_CART_BANK0,
                    )
                } else {
                    0xFF
                }
            }
            0x8 | 0x9 => {
                if segment < 0 {
                    self.video.vram[(self.video.vram_current_bank as usize
                        * GB_SIZE_VRAM_BANK0 as usize)
                        + (address as usize & (GB_SIZE_VRAM_BANK0 as usize - 1))]
                } else if segment == 1 && !self.model.is_cgb() {
                    0xFF
                } else if segment < 2 {
                    self.video.vram[(address as usize & (GB_SIZE_VRAM_BANK0 as usize - 1))
                        + segment as usize * GB_SIZE_VRAM_BANK0 as usize]
                } else {
                    0xFF
                }
            }
            0xA | 0xB => {
                if self.memory.rtc_access {
                    self.memory.rtc_reg()
                } else if self.memory.sram_access {
                    if !self.memory.sram.is_empty() {
                        if segment < 0 {
                            self.memory.sram_byte(
                                self.memory.sram_bank_off
                                    + (address as usize & (GB_SIZE_EXTERNAL_RAM - 1)),
                            )
                        } else if (segment as usize) * GB_SIZE_EXTERNAL_RAM < self.sram_size {
                            self.memory.sram_byte(
                                (address as usize & (GB_SIZE_EXTERNAL_RAM - 1))
                                    + segment as usize * GB_SIZE_EXTERNAL_RAM,
                            )
                        } else {
                            0xFF
                        }
                    } else {
                        0xFF
                    }
                } else if self.memory.mbc_has_read() {
                    self.mbc_read_dispatched(address)
                } else if self.memory.mbc_type == MbcType::HuC3 {
                    0x01 // TODO: Is this supposed to be the current SRAM bank?
                } else {
                    0xFF
                }
            }
            0xC | 0xE => self.memory.wram[address as usize & (GB_SIZE_WORKING_RAM_BANK0 - 1)],
            0xD => {
                if segment < 0 {
                    self.memory.wram[self.memory.wram_bank_off
                        + (address as usize & (GB_SIZE_WORKING_RAM_BANK0 - 1))]
                } else {
                    return self.view8_wram_bank(address, segment);
                }
            }
            _ => {
                if address < GB_BASE_OAM {
                    return self.memory.wram[self.memory.wram_bank_off
                        + (address as usize & (GB_SIZE_WORKING_RAM_BANK0 - 1))];
                }
                if address < GB_BASE_UNUSABLE {
                    if self.video.mode < 2 {
                        return self.video.oam[(address & 0xFF) as usize];
                    }
                    return 0xFF;
                }
                if address < GB_BASE_IO {
                    mlog!(
                        Level::GameError,
                        rgba_core::log::GB_MEM,
                        "Attempt to read from unusable memory: {:04X}",
                        address
                    );
                    if self.video.mode < 2 {
                        return match self.model {
                            GbModel::Agb => (address & 0xF0) as u8 | ((address >> 4) & 0xF) as u8,
                            _ => 0x00, // TODO: R/W behavior (CGB)
                        };
                    }
                    return 0xFF;
                }
                if address < GB_BASE_HRAM {
                    return self.io_read(address & (GB_SIZE_IO as u16 - 1));
                }
                if address < GB_BASE_IE {
                    return self.memory.hram[address as usize & GB_SIZE_HRAM];
                }
                self.io_read(GB_REG_IE as u16)
            }
        }
    }

    fn view8_wram_bank(&self, address: u16, segment: i32) -> u8 {
        let mut segment = segment as usize;
        if segment < 8 {
            if segment == 0 {
                segment = 1;
            } else if segment > 1 && !self.model.is_cgb() {
                return 0xFF;
            }
            self.memory.wram
                [(address as usize & (GB_SIZE_WORKING_RAM_BANK0 - 1)) + segment * GB_SIZE_WORKING_RAM_BANK0]
        } else {
            0xFF
        }
    }

    /// GBMemoryDMA
    pub fn memory_dma(&mut self, base: u16) {
        let mut base = base;
        if base >= 0xE000 {
            base &= 0xDFFF;
        }
        self.deschedule(EventId::Dma);
        self.schedule(EventId::Dma, 8 * (2 - self.double_speed as i32));
        self.memory.dma_source = base;
        self.memory.dma_dest = 0;
        self.memory.dma_remaining = 0xA0;
    }

    /// GBMemoryWriteHDMA5
    pub fn memory_write_hdma5(&mut self, value: u8) -> u8 {
        self.memory.hdma_source = (self.memory.io[crate::io::GB_REG_HDMA1 as usize] as u16) << 8;
        self.memory.hdma_source |= self.memory.io[crate::io::GB_REG_HDMA2 as usize] as u16;
        self.memory.hdma_dest = (self.memory.io[crate::io::GB_REG_HDMA3 as usize] as u16) << 8;
        self.memory.hdma_dest |= self.memory.io[crate::io::GB_REG_HDMA4 as usize] as u16;
        self.memory.hdma_source &= 0xFFF0;
        if self.memory.hdma_source >= 0x8000 && self.memory.hdma_source < 0xA000 {
            mlog!(
                Level::GameError,
                rgba_core::log::GB_MEM,
                "Invalid HDMA source: {:04X}",
                self.memory.hdma_source
            );
            return value | 0x80;
        }
        self.memory.hdma_dest &= 0x1FF0;
        self.memory.hdma_dest |= 0x8000;
        let was_hdma = self.memory.is_hdma;
        self.memory.is_hdma = value & 0x80 != 0;
        if (!was_hdma && !self.memory.is_hdma) || self.video.mode == 0 {
            if self.memory.is_hdma {
                self.memory.hdma_remaining = 0x10;
            } else {
                self.memory.hdma_remaining = ((value as i32 & 0x7F) + 1) * 0x10;
            }
            self.cpu_blocked = true;
            self.schedule(EventId::Hdma, 0);
        }
        value & 0x7F
    }

    /// _GBMemoryDMAService
    pub fn memory_dma_service(&mut self, timing: &mut rgba_core::timing::Timing, cycles_late: u32) {
        let dma_remaining = self.memory.dma_remaining;
        self.memory.dma_remaining = 0;
        let b = self.load8(self.memory.dma_source);
        self.video.oam[self.memory.dma_dest as usize] = b;
        self.renderer_write_oam(self.memory.dma_dest);
        self.memory.dma_source = self.memory.dma_source.wrapping_add(1);
        self.memory.dma_dest = self.memory.dma_dest.wrapping_add(1);
        self.memory.dma_remaining = dma_remaining - 1;
        if self.memory.dma_remaining != 0 {
            timing.schedule(
                EventId::Dma.into(),
                EventId::Dma.priority(),
                4 * (2 - self.double_speed as i32) - cycles_late as i32,
            );
        }
    }

    /// _GBMemoryHDMAService
    pub fn memory_hdma_service(&mut self, timing: &mut rgba_core::timing::Timing, cycles_late: u32) {
        self.cpu_blocked = true;
        let b = self.load8(self.memory.hdma_source);
        self.store8(self.memory.hdma_dest, b);
        self.memory.hdma_source = self.memory.hdma_source.wrapping_add(1);
        self.memory.hdma_dest = self.memory.hdma_dest.wrapping_add(1);
        self.memory.hdma_remaining -= 1;
        if self.memory.hdma_remaining != 0 {
            timing.deschedule(EventId::Hdma.into());
            timing.schedule(EventId::Hdma.into(), EventId::Hdma.priority(), 4 - cycles_late as i32);
        } else {
            self.cpu_blocked = false;
            self.memory.io[crate::io::GB_REG_HDMA1 as usize] = (self.memory.hdma_source >> 8) as u8;
            self.memory.io[crate::io::GB_REG_HDMA2 as usize] = self.memory.hdma_source as u8;
            self.memory.io[crate::io::GB_REG_HDMA3 as usize] = (self.memory.hdma_dest >> 8) as u8;
            self.memory.io[crate::io::GB_REG_HDMA4 as usize] = self.memory.hdma_dest as u8;
            if self.memory.is_hdma {
                let h = crate::io::GB_REG_HDMA5 as usize;
                self.memory.io[h] = self.memory.io[h].wrapping_sub(1);
                if self.memory.io[h] == 0xFF {
                    self.memory.is_hdma = false;
                }
            } else {
                self.memory.io[crate::io::GB_REG_HDMA5 as usize] = 0xFF;
            }
        }
    }

    /// GBPatch8: like a store8 that ignores MBC protection, for cheats/patches.
    /// Returns the overwritten byte.
    pub fn patch8(&mut self, address: u16, value: u8, old: Option<&mut i8>, segment: i32) {
        let mut old_value: i8 = -1;
        match address >> 12 {
            0x0..=0x3 => {
                self.pristine_cow();
                let off = address as usize & (GB_SIZE_CART_BANK0 - 1);
                old_value = self.memory.rom_byte(self.memory.rom_base_off + off) as i8;
                let idx = self.memory.rom_base_off + off;
                if !self.memory.rom.is_empty() {
                    self.memory.rom[idx] = value;
                }
            }
            0x4..=0x7 => {
                self.pristine_cow();
                if segment < 0 {
                    let idx = self.memory.rom_bank_off + (address as usize & (GB_SIZE_CART_BANK0 - 1));
                    old_value = self.memory.rom_byte(idx) as i8;
                    if idx < self.memory.rom.len() {
                        self.memory.rom[idx] = value;
                    }
                } else if (segment as usize) * GB_SIZE_CART_BANK0 < self.memory.rom_size {
                    let idx = (address as usize & (GB_SIZE_CART_BANK0 - 1))
                        + segment as usize * GB_SIZE_CART_BANK0;
                    old_value = self.memory.rom[idx] as i8;
                    self.memory.rom[idx] = value;
                } else {
                    return;
                }
            }
            0x8 | 0x9 => {
                if segment < 0 {
                    let off = (address as usize & (GB_SIZE_VRAM_BANK0 as usize - 1))
                        + self.video.vram_current_bank as usize * GB_SIZE_VRAM_BANK0 as usize;
                    old_value = self.video.vram[off] as i8;
                    self.video.vram[off] = value;
                    self.renderer_write_vram(off as u16);
                } else if segment == 1 && !self.model.is_cgb() {
                    return;
                } else if segment < 2 {
                    let off = (address as usize & (GB_SIZE_VRAM_BANK0 as usize - 1))
                        + segment as usize * GB_SIZE_VRAM_BANK0 as usize;
                    old_value = self.video.vram[off] as i8;
                    self.video.vram[off] = value;
                    self.renderer_write_vram(off as u16);
                } else {
                    return;
                }
            }
            0xA | 0xB => {
                if self.memory.rtc_access {
                    self.memory.set_rtc_reg(value);
                } else if self.memory.sram_access
                    && !self.memory.sram.is_empty()
                    && self.memory.mbc_type != MbcType::Mbc2
                {
                    let off = self.memory.sram_bank_off
                        + (address as usize & (GB_SIZE_EXTERNAL_RAM - 1));
                    self.memory.sram[off] = value;
                } else {
                    self.mbc_write_dispatched(address, value);
                }
                self.sram_dirty |= M_SAVEDATA_DIRT_NEW;
                return;
            }
            0xC | 0xE => {
                let off = address as usize & (GB_SIZE_WORKING_RAM_BANK0 - 1);
                old_value = self.memory.wram[off] as i8;
                self.memory.wram[off] = value;
            }
            0xD => {
                let mut seg = segment;
                if seg < 0 {
                    let off = self.memory.wram_bank_off
                        + (address as usize & (GB_SIZE_WORKING_RAM_BANK0 - 1));
                    old_value = self.memory.wram[off] as i8;
                    self.memory.wram[off] = value;
                } else if seg < 8 {
                    if seg == 0 {
                        seg = 1;
                    } else if seg > 1 && !self.model.is_cgb() {
                        return;
                    }
                    let off = (address as usize & (GB_SIZE_WORKING_RAM_BANK0 - 1))
                        + seg as usize * GB_SIZE_WORKING_RAM_BANK0;
                    old_value = self.memory.wram[off] as i8;
                    self.memory.wram[off] = value;
                } else {
                    return;
                }
            }
            _ => {
                if address < GB_BASE_OAM {
                    let off = self.memory.wram_bank_off
                        + (address as usize & (GB_SIZE_WORKING_RAM_BANK0 - 1));
                    old_value = self.memory.wram[off] as i8;
                    self.memory.wram[off] = value;
                } else if address < GB_BASE_UNUSABLE {
                    let off = (address & 0xFF) as usize;
                    old_value = self.video.oam[off] as i8;
                    self.video.oam[off] = value;
                    self.renderer_write_oam(address & 0xFF);
                } else if address < GB_BASE_HRAM {
                    mlog!(
                        Level::Stub,
                        rgba_core::log::GB_MEM,
                        "Unimplemented memory Patch8: 0x{:08X}",
                        address
                    );
                    return;
                } else if address < GB_BASE_IE {
                    let off = address as usize & GB_SIZE_HRAM;
                    old_value = self.memory.hram[off] as i8;
                    self.memory.hram[off] = value;
                } else {
                    mlog!(
                        Level::Stub,
                        rgba_core::log::GB_MEM,
                        "Unimplemented memory Patch8: 0x{:08X}",
                        address
                    );
                    return;
                }
            }
        }
        if let Some(o) = old {
            *o = old_value;
        }
    }

    fn pristine_cow(&mut self) {
        // We own the ROM buffer, so no copy-on-write is needed; kept for
        // parity with the C call sites.
    }
}

/// GBSramClean + mSavedataClean (subset, file write-back is the frontend's job).
pub fn sram_clean(gb: &mut Gb) {
    // Periodic write-back is handled by the frontend via `savedata()`; we only
    // track dirt age like the C.
    if gb.sram_dirty == 0 {
        return;
    }
    crate::mbc::rtc::write_rtc_if_needed(gb);
    gb.sram_dirty = 0;
    gb.sram_dirt_age = 0;
}

// The C fills CGB WRAM through a u32* cast; we fill byte-wise LE, same bytes.
pub fn cgb_wram_init_pattern(wram: &mut [u8]) {
    let mut pattern: u32 = 0;
    let mut i = 0usize; // in u32 units
    let n = GB_SIZE_WORKING_RAM / 4;
    while i < n {
        if (i & 0x1FFF) == 0x800 {
            i += 0x3FC + 4;
            continue;
        }
        if (i & 0x1FFF) == 0x1000 {
            i += 0x7FC + 4;
            continue;
        }
        if (i & 0x1FF) == 0 {
            pattern = !pattern;
        }
        let vals = [pattern, pattern, !pattern, !pattern];
        for (j, v) in vals.iter().enumerate() {
            wram[(i + j) * 4..(i + j + 1) * 4].copy_from_slice(&v.to_le_bytes());
        }
        i += 4;
    }
}


impl Memory {
    pub fn sram_save_bytes(&self) -> Option<&[u8]> {
        if self.sram.is_empty() {
            None
        } else {
            Some(&self.sram)
        }
    }
    pub fn sram_save_bytes_mut(&mut self) -> Option<&mut [u8]> {
        if self.sram.is_empty() {
            None
        } else {
            Some(&mut self.sram)
        }
    }
}
