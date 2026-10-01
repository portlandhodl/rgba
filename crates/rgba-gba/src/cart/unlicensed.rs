// Copyright (c) 2013-2024 Jeffrey Pfau (mGBA), MPL-2.0.
// Copyright (c) 2016 taizou (mGBA), MPL-2.0.
// Ported from mgba/src/gba/cart/unlicensed.c and mgba/src/gba/cart/vfame.c.

use rgba_core::{mlog, Level};

use crate::gba::Gba;
use crate::memory::{GBA_SIZE_ROM0, GBA_SIZE_SRAM};

pub const MULTI_SETTLE: i32 = 300;
pub const MULTI_BLOCK: usize = 0x80000;
pub const MULTI_BANK: usize = 0x2000000;

// enum GBMulticartCfgOffset
pub const GBA_MULTICART_CFG_BANK: u32 = 0x2;
pub const GBA_MULTICART_CFG_OFFSET: u32 = 0x3;
pub const GBA_MULTICART_CFG_SIZE: u32 = 0x4;
pub const GBA_MULTICART_CFG_SRAM: u32 = 0x5;
pub const GBA_MULTICART_CFG_UNK: u32 = 0x6;

/// enum GBAVFameCartType
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VfameCartType {
    Standard = 0,
    George = 1,
    Alternate = 2,
}

/// enum GBAUnlCartType
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnlCartType {
    None = 0,
    Vfame = 1,
    Multicart = 2,
}

/// struct GBAVFameCart
pub struct VfameCart {
    pub cart_type: VfameCartType,
    pub sram_mode: i32,
    pub rom_mode: i32,
    pub write_sequence: [u8; 5],
    pub accepting_mode_change: bool,
}

/// struct GBAMulticart
pub struct Multicart {
    /// Full ROM image backing the cart (C: romVf-backed mapping); the
    /// `memory.rom` window contents are copied out of this on settle.
    pub rom: Vec<u8>,

    pub bank: u8,
    pub offset: u8,
    pub size: u8,
    pub sram_active: bool,
    pub locked: bool,
    pub unk: u8,
}

/// struct GBAUnlCart
pub struct UnlCart {
    pub cart_type: UnlCartType,
    pub vfame: VfameCart,
    pub multi: Multicart,
}

impl UnlCart {
    pub fn new() -> Self {
        UnlCart {
            cart_type: UnlCartType::None,
            vfame: VfameCart {
                cart_type: VfameCartType::Standard,
                sram_mode: 0,
                rom_mode: 0,
                write_sequence: [0; 5],
                accepting_mode_change: false,
            },
            multi: Multicart {
                rom: Vec::new(),
                bank: 0,
                offset: 0,
                size: 0,
                sram_active: false,
                locked: false,
                unk: 0,
            },
        }
    }
}

impl Default for UnlCart {
    fn default() -> Self {
        Self::new()
    }
}

const DIGIMON_SAPPHIRE_CHINESE_CRC32: u32 = 0x793A328F;

const ADDRESS_REORDERING: [[[u8; 16]; 3]; 3] = [
    [
        [15, 14, 9, 1, 8, 10, 7, 3, 5, 11, 4, 0, 13, 12, 2, 6],
        [15, 7, 13, 5, 11, 6, 0, 9, 12, 2, 10, 14, 3, 1, 8, 4],
        [15, 0, 3, 12, 2, 4, 14, 13, 1, 8, 6, 7, 9, 5, 11, 10],
    ],
    [
        [15, 7, 13, 1, 11, 10, 14, 9, 12, 2, 4, 0, 3, 5, 8, 6],
        [15, 14, 3, 12, 8, 4, 0, 13, 5, 11, 6, 7, 9, 1, 2, 10],
        [15, 0, 9, 5, 2, 6, 7, 3, 1, 8, 10, 14, 13, 12, 11, 4],
    ],
    [
        [15, 0, 13, 5, 8, 4, 7, 3, 1, 2, 10, 14, 9, 12, 11, 6],
        [15, 7, 9, 1, 2, 6, 14, 13, 12, 11, 4, 0, 3, 5, 8, 10],
        [15, 14, 3, 12, 11, 10, 0, 9, 5, 8, 6, 7, 13, 1, 2, 4],
    ],
];

const VALUE_REORDERING: [[[u8; 8]; 3]; 3] = [
    [
        [5, 4, 3, 2, 1, 0, 7, 6],
        [3, 2, 1, 0, 7, 6, 5, 4],
        [1, 0, 7, 6, 5, 4, 3, 2],
    ],
    [
        [3, 0, 7, 2, 1, 4, 5, 6],
        [1, 4, 3, 0, 5, 6, 7, 2],
        [5, 2, 1, 6, 7, 0, 3, 4],
    ],
    [
        [5, 4, 7, 2, 1, 0, 3, 6],
        [1, 2, 3, 0, 5, 6, 7, 4],
        [3, 0, 1, 6, 7, 4, 5, 2],
    ],
];

// A portion of the initialisation routine that gets copied into RAM - Always seems to be present at 0x15C in VFame game ROM
const INIT_SEQUENCE: [u8; 16] = [
    0xB4, 0x00, 0x9F, 0xE5, 0x99, 0x10, 0xA0, 0xE3, 0x00, 0x10, 0xC0, 0xE5, 0xAC, 0x00, 0x9F, 0xE5,
];

impl Gba {
    pub fn unl_cart_is_vfame(&self) -> bool {
        self.unl.cart_type == UnlCartType::Vfame
    }
    pub fn unl_cart_is_present(&self) -> bool {
        self.unl.cart_type != UnlCartType::None
    }

    /// GBAUnlCartDetect (the multicart branch is handled at load time in
    /// gba.rs, where the full >32MiB image is still around — see
    /// UnlCart::init_multicart).
    pub fn unl_cart_detect(&mut self) {
        if self.memory.rom.is_empty() {
            return;
        }

        if vfame_detect(
            &mut self.unl.vfame,
            &self.memory.rom,
            self.memory.rom_size,
            self.rom_crc32,
        ) {
            self.unl.cart_type = UnlCartType::Vfame;
        }
    }

    /// GBAUnlCartReset
    pub fn unl_cart_reset(&mut self) {
        if self.unl.cart_type == UnlCartType::Multicart {
            self.unl.multi.bank = 0;
            self.unl.multi.offset = 0;
            self.unl.multi.size = 0;
            self.unl.multi.locked = false;
            // C: gba->memory.rom = multi.rom; romSize = GBA_SIZE_ROM0 — the
            // window starts at the base of the full image.
            self.multi_apply_window(0, GBA_SIZE_ROM0);
        }
    }

    /// GBAUnlCartWriteSRAM
    pub fn unl_cart_write_sram(&mut self, address: u32, value: u8) {
        match self.unl.cart_type {
            UnlCartType::Vfame => {
                self.vfame_sram_write(address, value);
                return;
            }
            UnlCartType::Multicart => {
                mlog!(
                    Level::Debug,
                    rgba_core::log::GBA_MEM,
                    "Multicart writing SRAM {:06X}:{:02X}",
                    address,
                    value
                );
                match address {
                    GBA_MULTICART_CFG_BANK => {
                        if !self.unl.multi.locked {
                            self.unl.multi.bank = value >> 4;
                            // TODO(timing): needs EventId::UnlCartSettle (C: "GBA
                            // Unlicensed Multicart Settle", priority 0x71):
                            //   deschedule + schedule in MULTI_SETTLE cycles.
                        }
                    }
                    GBA_MULTICART_CFG_OFFSET => {
                        if !self.unl.multi.locked {
                            // note: C schedules here before the lock check below.
                            self.unl.multi.offset = value;
                            // TODO(timing): EventId::UnlCartSettle deschedule+schedule(MULTI_SETTLE)
                            if self.unl.multi.offset & 0x80 != 0 {
                                self.unl.multi.locked = true;
                            }
                        }
                    }
                    GBA_MULTICART_CFG_SIZE => {
                        self.unl.multi.size = 0x40 - (value & 0x3F);
                        if !self.unl.multi.locked {
                            // TODO(timing): EventId::UnlCartSettle deschedule+schedule(MULTI_SETTLE)
                        }
                    }
                    GBA_MULTICART_CFG_SRAM => {
                        if value == 0 && self.unl.multi.sram_active {
                            self.unl.multi.sram_active = false;
                        } else if value == 1 && !self.unl.multi.sram_active {
                            self.unl.multi.sram_active = true;
                        }
                    }
                    GBA_MULTICART_CFG_UNK => {
                        // TODO: What does this do?
                        self.unl.multi.unk = value;
                    }
                    _ => {}
                }
            }
            UnlCartType::None => {}
        }

        if !self.savedata.data.is_empty() {
            self.savedata.data[(address as usize) & (GBA_SIZE_SRAM - 1)] = value;
        }
    }

    /// GBAUnlCartWriteROM
    pub fn unl_cart_write16(&mut self, address: u32, value: u16) {
        match self.unl.cart_type {
            UnlCartType::Vfame | UnlCartType::None => {}
            UnlCartType::Multicart => {
                mlog!(
                    Level::Stub,
                    rgba_core::log::GBA_MEM,
                    "Unimplemented writing to ROM {:07X}:{:04X}",
                    address,
                    value
                );
            }
        }
    }

    /// _multicartSettle — the "GBA Unlicensed Multicart Settle" timing event
    /// callback. TODO(timing): dispatch from process_event via
    /// EventId::UnlCartSettle (priority 0x71).
    pub fn multicart_settle(&mut self, _cycles_late: u32) {
        mlog!(
            Level::Info,
            rgba_core::log::GBA_MEM,
            "Switching to bank {} offset {}, size {}",
            self.unl.multi.bank,
            self.unl.multi.offset & 0x3F,
            self.unl.multi.size
        );
        let offset = self.unl.multi.bank as usize * (MULTI_BANK >> 2)
            + (self.unl.multi.offset & 0x3F) as usize * (MULTI_BLOCK >> 2);
        let size = self.unl.multi.size as usize * MULTI_BLOCK;
        if offset * 4 >= self.unl.multi.rom.len() || offset * 4 + size > self.unl.multi.rom.len() {
            mlog!(
                Level::GameError,
                rgba_core::log::GBA_MEM,
                "Bank switch was out of bounds, {:07X} + {:X} > {:07X}",
                offset * 4,
                size,
                self.unl.multi.rom.len()
            );
            return;
        }
        // C: gba->memory.rom = multi.rom + offset; gba->memory.romSize = size;
        // The window contents are the same bytes.
        self.multi_apply_window(offset * 4, size);
    }

    /// Swap the memory.rom window to `src..src+size` of the full image.
    fn multi_apply_window(&mut self, src: usize, size: usize) {
        let n = size
            .min(self.unl.multi.rom.len().saturating_sub(src))
            .min(self.memory.rom.len());
        if n > 0 {
            let data = self.unl.multi.rom[src..src + n].to_vec();
            self.memory.rom[..n].copy_from_slice(&data);
        }
        self.memory.rom_size = size;
    }

    /// GBAVFameModifyRomAddress. Not currently used (same as the C).
    pub fn vfame_modify_rom_address(&self, mut address: u32) -> u32 {
        if self.unl.vfame.rom_mode == -1 && (address & 0x01000000) == 0 {
            // When ROM mode is uninitialised, it just mirrors the first 0x80000 bytes
            // All known games set the ROM mode to 00 which enables full range of reads, it's currently unknown what other values do
            address &= 0x7FFFF;
        } else if is_in_mirrored_area(address, self.memory.rom_size) {
            address = address.wrapping_sub(0x800000);
        }
        address
    }

    /// GBAVFameGetPatternValue
    pub fn vfame_get_pattern_value(&self, address: u32, bits: i32) -> u32 {
        vfame_get_pattern_value(address, bits)
    }

    /// GBAVFameSramWrite
    fn vfame_sram_write(&mut self, address: u32, value: u8) {
        // A certain sequence of writes to SRAM FFF8->FFFC can enable or disable "mode change" mode
        // Currently unknown if these writes have to be sequential, or what happens if you write different values, if anything
        if (0xFFF8..=0xFFFC).contains(&address) {
            self.unl.vfame.write_sequence[(address - 0xFFF8) as usize] = value;
            if address == 0xFFFC {
                const MODE_CHANGE_START_SEQUENCE: [u8; 5] = [0x99, 0x02, 0x05, 0x02, 0x03];
                const MODE_CHANGE_END_SEQUENCE: [u8; 5] = [0x99, 0x03, 0x62, 0x02, 0x56];
                if self.unl.vfame.write_sequence == MODE_CHANGE_START_SEQUENCE {
                    self.unl.vfame.accepting_mode_change = true;
                }
                if self.unl.vfame.write_sequence == MODE_CHANGE_END_SEQUENCE {
                    self.unl.vfame.accepting_mode_change = false;
                }
            }
        }

        // If we are in "mode change mode" we can change either SRAM or ROM modes
        // Currently unknown if other SRAM writes in this mode should have any effect
        if self.unl.vfame.accepting_mode_change {
            if address == 0xFFFE {
                self.unl.vfame.sram_mode = value as i32;
            } else if address == 0xFFFD {
                self.unl.vfame.rom_mode = value as i32;
            }
        }

        if self.unl.vfame.sram_mode == -1 {
            // when SRAM mode is uninitialised you can't write to it
            return;
        }

        // if mode has been set - the address and value of the SRAM write will be modified
        let cart_type = self.unl.vfame.cart_type;
        let address = modify_sram_address(cart_type, address, self.unl.vfame.sram_mode);
        let value = modify_sram_value(cart_type, value, self.unl.vfame.sram_mode);
        let address = address & (GBA_SIZE_SRAM as u32 - 1);
        if !self.savedata.data.is_empty() {
            self.savedata.data[address as usize] = value;
        }
    }
}

impl Multicart {
    /// GBAUnlCartDetect's multicart branch: stash the full ROM image.
    pub fn init_multicart(&mut self, rom: Vec<u8>) {
        self.rom = rom;
    }
}

/// The GBMulticart id check from GBAUnlCartDetect (6 bytes at 0xAC).
pub fn is_multicart_id(rom: &[u8]) -> bool {
    rom.len() >= 0xB2 && (&rom[0xAC..0xB2] == b"AXVJ01" || &rom[0xAC..0xB2] == b"BI3P52")
}

/// GBAVFameDetect
fn vfame_detect(cart: &mut VfameCart, rom: &[u8], rom_size: usize, crc32: u32) -> bool {
    // The initialisation code is also present & run in the dumps of Digimon Ruby & Sapphire from hacked/deprotected reprint carts,
    // which would break if run in "proper" VFame mode so we need to exclude those..
    if rom_size == 0x2000000 {
        // the deprotected dumps are 32MB but no real VF games are this size
        return false;
    }

    let mut detected = false;

    // Most games have the same init sequence in the same place
    // but LOTR/Mo Jie Qi Bing doesn't, probably because it's based on the Kiki KaiKai engine, so just detect based on its title
    if (rom.len() >= 0x15C + 16 && rom[0x15C..0x15C + 16] == INIT_SEQUENCE)
        || (rom.len() >= 0xB0 && rom[0xA0..0xB0] == *b"\0LORD\0WORD\0\0AKIJ")
    {
        detected = true;
        cart.cart_type = VfameCartType::Standard;
        mlog!(
            Level::Info,
            rgba_core::log::GBA_MEM,
            "Vast Fame game detected"
        );
    }

    // These games additionally operates with a different set of SRAM modes
    // Their initialisation seems to be identical so the difference must be in the cart HW itself
    // Other undumped games may have similar differences
    if rom.len() >= 0xAC && rom[0xA0..0xAC] == *b"George Sango" {
        detected = true;
        cart.cart_type = VfameCartType::George;
        mlog!(Level::Info, rgba_core::log::GBA_MEM, "George mode");
    } else if crc32 == DIGIMON_SAPPHIRE_CHINESE_CRC32 {
        // Chinese version of Digimon Sapphire; header is identical to the English version which uses the normal reordering
        // so we have to use some other way to detect it
        detected = true;
        cart.cart_type = VfameCartType::Alternate;
    }

    if detected {
        cart.sram_mode = -1;
        cart.rom_mode = -1;
        cart.accepting_mode_change = false;
    }

    detected
}

fn is_in_mirrored_area(address: u32, rom_size: usize) -> bool {
    let address = address & 0x01FFFFFF;
    // For some reason known 4m games e.g. Zook, Sango repeat the game at 800000 but the 8m Digimon R. does not
    if rom_size != 0x400000 {
        return false;
    }
    if address < 0x800000 {
        return false;
    }
    if address >= 0x800000 + rom_size as u32 {
        return false;
    }
    true
}

/// GBAVFameGetPatternValue. Looks like only 16-bit reads are done by games but others are possible...
pub fn vfame_get_pattern_value(address: u32, bits: i32) -> u32 {
    match bits {
        8 => {
            if address & 1 != 0 {
                get_pattern_value(address) & 0xFF
            } else {
                (get_pattern_value(address) & 0xFF00) >> 8
            }
        }
        16 => get_pattern_value(address),
        32 => (get_pattern_value(address) << 2)
            .wrapping_add(get_pattern_value(address.wrapping_add(2))),
        _ => 0,
    }
}

// when you read from a ROM location outside the actual ROM data or its mirror, it returns a value based on some 16-bit transformation of the address
// which the game relies on to run
fn get_pattern_value(addr: u32) -> u32 {
    let addr = addr & 0x1FFFFF;
    let mut value: u32 = 0;
    match addr & 0x1F0000 {
        0x000000 | 0x010000 => {
            value = (addr >> 1) & 0xFFFF;
        }
        0x020000 => {
            value = addr & 0xFFFF;
        }
        0x030000 => {
            value = (addr & 0xFFFF) + 1;
        }
        0x040000 => {
            value = 0xFFFF - (addr & 0xFFFF);
        }
        0x050000 => {
            value = (0xFFFFu32 - (addr & 0xFFFF)).wrapping_sub(1);
        }
        0x060000 => {
            value = (addr & 0xFFFF) ^ 0xAAAA;
        }
        0x070000 => {
            value = ((addr & 0xFFFF) ^ 0xAAAA) + 1;
        }
        0x080000 => {
            value = (addr & 0xFFFF) ^ 0x5555;
        }
        0x090000 => {
            value = ((addr & 0xFFFF) ^ 0x5555).wrapping_sub(1);
        }
        0x0A0000 | 0x0B0000 => {
            value = pattern_right_shift2(addr);
        }
        0x0C0000 | 0x0D0000 => {
            value = 0xFFFF_u32.wrapping_sub(pattern_right_shift2(addr));
        }
        0x0E0000 | 0x0F0000 => {
            value = pattern_right_shift2(addr) ^ 0xAAAA;
        }
        0x100000 | 0x110000 => {
            value = pattern_right_shift2(addr) ^ 0x5555;
        }
        0x120000 => {
            value = 0xFFFF - ((addr & 0xFFFF) >> 1);
        }
        0x130000 => {
            value = (0xFFFFu32 - ((addr & 0xFFFF) >> 1)).wrapping_sub(0x8000);
        }
        0x140000 | 0x150000 => {
            value = ((addr >> 1) & 0xFFFF) ^ 0xAAAA;
        }
        0x160000 | 0x170000 => {
            value = ((addr >> 1) & 0xFFFF) ^ 0x5555;
        }
        0x180000 | 0x190000 => {
            value = ((addr >> 1) & 0xFFFF) ^ 0xF0F0;
        }
        0x1A0000 | 0x1B0000 => {
            value = ((addr >> 1) & 0xFFFF) ^ 0x0F0F;
        }
        0x1C0000 | 0x1D0000 => {
            value = ((addr >> 1) & 0xFFFF) ^ 0xFF00;
        }
        0x1E0000 | 0x1F0000 => {
            value = ((addr >> 1) & 0xFFFF) ^ 0x00FF;
        }
        _ => {}
    }

    value & 0xFFFF
}

fn pattern_right_shift2(addr: u32) -> u32 {
    let mut value = addr & 0xFFFF;
    value >>= 2;
    value += if (addr & 3) == 2 { 0x8000 } else { 0 };
    value += if addr & 0x10000 != 0 { 0x4000 } else { 0 };
    value
}

fn modify_sram_address(cart_type: VfameCartType, address: u32, mode: i32) -> u32 {
    let mode = mode & 0x3;
    if mode == 0 {
        return address;
    }
    reorder_bits(
        address,
        &ADDRESS_REORDERING[cart_type as usize][(mode - 1) as usize],
        16,
    )
}

fn modify_sram_value(cart_type: VfameCartType, value: u8, mode: i32) -> u8 {
    let reorder_type = (mode & 0xF) >> 2;
    let mut value = value;
    if reorder_type != 0 {
        value = reorder_bits(
            value as u32,
            &VALUE_REORDERING[cart_type as usize][(reorder_type - 1) as usize],
            8,
        ) as u8;
    }
    if mode & 0x80 != 0 {
        value ^= 0xAA;
    }
    value
}

// Reorder bits in a byte according to the reordering given
fn reorder_bits(value: u32, reordering: &[u8], reorder_length: i32) -> u32 {
    let mut retval = value;

    let mut x = reorder_length;
    while x > 0 {
        let reorder_place = reordering[(reorder_length - x) as usize] as u32; // get the reorder position

        let mask = 1u32 << reorder_place; // move the bit to the position we want
        let mut val = value & mask; // AND it with the original value
        val >>= reorder_place; // move the bit back, so we have the correct 0 or 1

        let destination_place = (x - 1) as u32;

        let new_mask = 1u32 << destination_place;
        if val == 1 {
            retval |= new_mask;
        } else {
            retval &= !new_mask;
        }
        x -= 1;
    }

    retval
}
