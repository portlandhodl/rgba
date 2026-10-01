// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gb/mbc.c (GBMBCInit/GBMBCReset, shared
// GBMBCSwitch*Bank, GBMBCFromGBX, detection helpers) and the dispatch part of
// mgba/src/gb/mbc/mbc.c, plus mgba/include/mgba/internal/gb/mbc.h.

pub mod huc3;
pub mod licensed;
pub mod pocket_cam;
pub mod rtc;
pub mod tama5;
pub mod unlicensed;

use rgba_core::mlog;
use rgba_core::Level;

use crate::gb::Gb;
use crate::memory::{
    MbcState, MbcType, GB_BASE_VRAM, GB_SIZE_CART_BANK0, GB_SIZE_CART_HALFBANK,
    GB_SIZE_EXTERNAL_RAM, GB_SIZE_EXTERNAL_RAM_HALFBANK, GB_SIZE_MBC6_FLASH,
};

pub use unlicensed::{GB_SACHEN_LOCKED_CGB, GB_SACHEN_LOCKED_DMG, GB_SACHEN_UNLOCKED};

pub const GB_LOGO_HASH: u32 = 0x46195417;

/// toPow2 (mgba-util/math.h): rounds up to the nearest power of two.
fn to_pow2(bits: u32) -> u32 {
    if bits == 0 {
        return 0;
    }
    1u32 << (32 - (bits - 1).leading_zeros())
}

/// GBMBCFromGBX
pub fn mbc_from_gbx(fourcc: &[u8]) -> MbcType {
    // Three-letter codes are NUL-padded, like memcmp against the C literals.
    const TABLE: &[(&[u8; 4], MbcType)] = &[
        (b"ROM\0", MbcType::None),
        (b"MBC1", MbcType::Mbc1),
        (b"MBC2", MbcType::Mbc2),
        (b"MBC3", MbcType::Mbc3),
        (b"MBC5", MbcType::Mbc5),
        (b"MBC6", MbcType::Mbc6),
        (b"MBC7", MbcType::Mbc7),
        (b"MB1M", MbcType::Mbc1),
        (b"MMM1", MbcType::Mmm01),
        (b"CAMR", MbcType::PocketCam),
        (b"HUC1", MbcType::HuC1),
        (b"HUC3", MbcType::HuC3),
        (b"TAM5", MbcType::Tama5),
        (b"M161", MbcType::M161),
        (b"BBD\0", MbcType::UnlBbd),
        (b"HITK", MbcType::UnlHitek),
        (b"SNTX", MbcType::UnlSintax),
        (b"NTO1", MbcType::UnlNtOld1),
        (b"NTO2", MbcType::UnlNtOld2),
        (b"NTN\0", MbcType::UnlNtNew),
        (b"LICH", MbcType::UnlLiCheng),
        (b"LBMC", MbcType::Autodetect), // TODO
        (b"LIBA", MbcType::Autodetect), // TODO
        (b"PKJD", MbcType::UnlPkjd),
        (b"WISD", MbcType::UnlWisdomTree),
        (b"SAM1", MbcType::UnlSachenMmc1),
        (b"SAM2", MbcType::UnlSachenMmc2),
        (b"ROCK", MbcType::Autodetect), // TODO
        (b"NGHK", MbcType::Autodetect), // TODO
        (b"GB81", MbcType::UnlGgb81),
        (b"TPP1", MbcType::Autodetect), // TODO
        (b"VF01", MbcType::Autodetect), // TODO
        (b"SKL8", MbcType::Autodetect), // TODO
    ];
    let first4 = fourcc.get(..4).unwrap_or(&[]);
    for &(cc, mbc) in TABLE {
        if first4 == cc.as_slice() {
            return mbc;
        }
    }
    MbcType::Autodetect
}

/// _isMulticart (MBC1M detection): embedded headers probed with GBIsROM.
fn is_multicart(mem: &[u8]) -> bool {
    fn check(mem: &[u8], bank: usize) -> bool {
        let off = GB_SIZE_CART_BANK0 * bank;
        match mem.get(off..off + 1024) {
            Some(slice) => Gb::is_rom(slice),
            None => false,
        }
    }
    if !check(mem, 0x10) {
        return false;
    }
    if !check(mem, 0x20) {
        return check(mem, 0x30);
    }
    true
}

/// _isWisdomTree
fn is_wisdom_tree(mem: &[u8], size: usize) -> bool {
    let read32 = |i: usize| -> u32 {
        match mem.get(i..i + 4) {
            Some(b) => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            None => 0,
        }
    };
    let mut i = 0x134;
    while i < 0x14C {
        if read32(i) != 0 {
            return false;
        }
        i += 4;
    }
    let mut i = 0xF0;
    while i < 0x100 {
        if read32(i) != 0 {
            return false;
        }
        i += 4;
    }
    if mem.get(0x14D).copied() != Some(0xE7) {
        return false;
    }
    let mut i = 0x300;
    while i + 11 < size {
        if &mem[i..i + 6] == b"WISDOM" && &mem[i + 7..i + 11] == b"TREE" {
            return true;
        }
        i += 1;
    }
    false
}

/// _detectUnlMBC
fn detect_unl_mbc(mem: &[u8], size: usize) -> MbcType {
    let cart_type = mem.get(0x147).copied().unwrap_or(0);

    if cart_type == 0 && is_wisdom_tree(mem, size) {
        return MbcType::UnlWisdomTree;
    }

    let secondary_logo = crate::crc32(&mem[0x184..0x184 + 0x30]);
    match secondary_logo {
        0x4fdab691 => {
            return MbcType::UnlHitek;
        }
        0xc7d8c1df | 0x6d1ea662 => {
            // Garou
            if mem.get(0x7FFF).copied().unwrap_or(0) != 0x01 {
                // Make sure we're not using a "fixed" version
                return MbcType::UnlBbd;
            }
        }
        0x79f34594 | 0x7e8c539b => {
            // DATA. / TD-SOFT
            return MbcType::UnlGgb81;
        }
        0x20d092e2 | 0xd2b57657 => {
            if cart_type == 0x01 {
                // Make sure we're not using a "fixed" version
                return MbcType::UnlLiCheng;
            }
            let rom_size = mem.get(0x148).copied().unwrap_or(0) as u32;
            if 0x8000u32.wrapping_shl(rom_size) != size as u32 {
                return MbcType::UnlLiCheng;
            }
        }
        0x6c1dcf2d | 0x99e3449d => {
            if mem.get(0x7FFF).copied().unwrap_or(0) != 0x01 {
                // Make sure we're not using a "fixed" version
                return MbcType::UnlSintax;
            }
        }
        _ => {}
    }

    if mem.get(0x104).copied() == Some(0xCE)
        && mem.get(0x144).copied() == Some(0xED)
        && mem.get(0x114).copied() == Some(0x66)
    {
        return MbcType::UnlSachenMmc1;
    }

    if mem.get(0x184).copied() == Some(0xCE)
        && mem.get(0x1C4).copied() == Some(0xED)
        && mem.get(0x194).copied() == Some(0x66)
    {
        return MbcType::UnlSachenMmc2;
    }

    MbcType::Autodetect
}

impl Gb {
    /// GBMBCReset
    pub fn mbc_reset(&mut self) {
        self.memory.current_bank0 = 0;
        self.memory.rom_bank_off = GB_SIZE_CART_BANK0;
        self.memory.rom_bank_is_sram = false;
        self.memory.cart_bus = 0xFF;
        self.memory.cart_bus_pc = 0;
        self.memory.cart_bus_decay = 1;
        self.memory.rumble_state = false;

        self.memory.mbc_state = MbcState::default();
        self.mbc_init();
        match self.memory.mbc_type {
            MbcType::Mbc1 => {
                self.memory.mbc_state.mbc1.mode = 0;
                self.memory.mbc_state.mbc1.bank_lo = 1;
            }
            MbcType::Mbc6 => {
                self.mbc_switch_half_bank(0, 2);
                self.mbc_switch_half_bank(1, 3);
                self.mbc_switch_sram_half_bank(0, 0);
                self.mbc_switch_sram_half_bank(0, 1);
            }
            MbcType::Mmm01 => {
                let banks = (self.memory.rom_size / GB_SIZE_CART_BANK0) as i32;
                self.mbc_switch_bank0(banks.wrapping_sub(2));
                self.mbc_switch_bank(banks.wrapping_sub(1));
            }
            MbcType::UnlSintax => {
                self.memory.mbc_state.sintax.mode = 0xF;
            }
            _ => {}
        }
        self.memory.sram_bank_off = 0;
    }

    /// GBMBCInit
    pub fn mbc_init(&mut self) {
        let mut sram_size: usize = 0;
        // GBCartridge offset within rom; normally 0x100, re-scanned below.
        let mut cart: usize = 0x100;
        if !self.memory.rom.is_empty() && self.memory.rom_size != 0 {
            if self.memory.rom_size >= 0x8000 {
                let cart_footer = self.memory.rom_size - 0x7F00;
                let logo_ok = self
                    .memory
                    .rom
                    .get(cart_footer + 4..cart_footer + 4 + 48)
                    .map(|logo| crate::crc32(logo) == GB_LOGO_HASH)
                    .unwrap_or(false);
                let footer_type = self.memory.rom_byte(cart_footer + 0x47);
                if logo_ok && (0x0B..=0x0D).contains(&footer_type) {
                    cart = cart_footer;
                }
            }
            if self.gbx.rom_size != 0 {
                sram_size = self.gbx.ram_size as usize;
                self.memory.mbc_type = self.gbx.mbc;
            } else {
                sram_size = match self.memory.rom_byte(cart + 0x49) {
                    0 => 0,
                    3 => 0x8000,
                    4 => 0x20000,
                    5 => 0x10000,
                    _ => 0x2000,
                };
            }
            if self.memory.mbc_type == MbcType::Autodetect {
                let t = detect_unl_mbc(&self.memory.rom, self.memory.rom_size);
                self.memory.mbc_type = t;
            }

            if self.memory.mbc_type == MbcType::Autodetect {
                let ctype = self.memory.rom_byte(cart + 0x47);
                self.memory.mbc_type = match ctype {
                    0 | 8 | 9 => MbcType::None,
                    1 | 2 | 3 => MbcType::Mbc1,
                    5 | 6 => MbcType::Mbc2,
                    0x0B | 0x0C | 0x0D => MbcType::Mmm01,
                    0x0F | 0x10 => MbcType::Mbc3Rtc,
                    0x11 | 0x12 | 0x13 => MbcType::Mbc3,
                    0x1C | 0x1D | 0x1E => MbcType::Mbc5Rumble,
                    0x20 => MbcType::Mbc6,
                    0x22 => MbcType::Mbc7,
                    0xFC => MbcType::PocketCam,
                    0xFD => MbcType::Tama5,
                    0xFE => MbcType::HuC3,
                    0xFF => MbcType::HuC1,
                    _ => {
                        mlog!(Level::Warn, rgba_core::log::GB_MBC, "Unknown MBC type: {:02X}", ctype);
                        MbcType::Mbc5
                    }
                };
            }
        } else {
            self.memory.mbc_type = MbcType::None;
        }
        self.memory.direct_sram_access = true;
        self.memory.mbc_read_bank0 = false;
        self.memory.mbc_read_bank1 = false;
        self.memory.mbc_read_high = false;
        self.memory.mbc_write_high = false;
        self.memory.cart_bus_decay = 4;
        match self.memory.mbc_type {
            MbcType::None | MbcType::Mbc3 | MbcType::Mbc3Rtc | MbcType::Mbc5 | MbcType::Mbc5Rumble
            | MbcType::Mmm01 | MbcType::HuC1 | MbcType::M161 | MbcType::UnlWisdomTree
            | MbcType::UnlNtOld1 | MbcType::UnlNtOld2 | MbcType::UnlNtNew | MbcType::UnlLiCheng => {}
            MbcType::Mbc1 => {
                if self.gbx.mapper_vars[0] != 0 {
                    self.memory.mbc_state.mbc1.multicart_stride = self.gbx.mapper_vars[0] as i32;
                } else if self.memory.rom_size >= GB_SIZE_CART_BANK0 * 0x31
                    && is_multicart(&self.memory.rom)
                {
                    self.memory.mbc_state.mbc1.multicart_stride = 4;
                } else {
                    self.memory.mbc_state.mbc1.multicart_stride = 5;
                }
            }
            MbcType::Mbc2 => {
                self.memory.direct_sram_access = false;
                sram_size = 0x100;
            }
            MbcType::Mbc6 => {
                self.memory.direct_sram_access = false;
                if sram_size == 0 {
                    sram_size = GB_SIZE_EXTERNAL_RAM; // Force minimum size for convenience
                }
                sram_size += GB_SIZE_MBC6_FLASH; // Flash is concatenated at the end
            }
            MbcType::Mbc7 => {
                sram_size = 0x100;
            }
            MbcType::HuC3 => {}
            MbcType::Tama5 => {
                self.memory.mbc_state.tama5.rtc_alarm_page[tama5::GBTAMA6_RTC_PAGE] = 1;
                self.memory.mbc_state.tama5.rtc_free_page0[tama5::GBTAMA6_RTC_PAGE] = 2;
                self.memory.mbc_state.tama5.rtc_free_page1[tama5::GBTAMA6_RTC_PAGE] = 3;
                sram_size = 0x20;
            }
            MbcType::PocketCam => {
                if sram_size == 0 {
                    sram_size = GB_SIZE_EXTERNAL_RAM; // Force minimum size for convenience
                }
                // No camera (mImageSource) in this port: the C calls
                // cam->startRequestImage here, which is NULL-gated anyway.
            }
            MbcType::UnlPkjd => {}
            MbcType::UnlBbd => {
                self.memory.mbc_read_bank1 = true;
            }
            MbcType::UnlHitek => {
                self.memory.mbc_state.bbd.data_swap_mode = 7;
                self.memory.mbc_state.bbd.bank_swap_mode = 7;
                self.memory.mbc_read_bank1 = true;
            }
            MbcType::UnlGgb81 => {
                self.memory.mbc_read_bank1 = true;
            }
            MbcType::UnlSachenMmc1 => {
                self.memory.mbc_read_bank0 = true;
                self.memory.mbc_read_bank1 = true;
            }
            MbcType::UnlSachenMmc2 => {
                self.memory.mbc_read_bank0 = true;
                self.memory.mbc_read_bank1 = true;
                self.memory.mbc_read_high = true;
                self.memory.mbc_write_high = true;
                if sram_size != 0 {
                    self.memory.sram_access = true;
                }
            }
            MbcType::UnlSintax => {
                self.memory.mbc_read_bank1 = true;
                if sram_size != 0 {
                    self.memory.sram_access = true;
                }
            }
            MbcType::Autodetect => {
                mlog!(
                    Level::Warn,
                    rgba_core::log::GB_MBC,
                    "Unknown MBC type: {:02X}",
                    self.memory.rom_byte(cart + 0x47)
                );
            }
        }
        if self.memory.mbc_type == MbcType::Mbc3Rtc {
            self.memory.rtc_regs = [0; 5];
        }

        self.memory.current_bank = 1;
        self.memory.sram_current_bank = 0;
        self.memory.sram_access = false;
        self.memory.rtc_access = false;
        self.memory.active_rtc_reg = 0;
        self.memory.rtc_latched = false;
        self.memory.rtc_last_latch = 0;
        self.memory.rtc_last_latch = self.unix_time();
        self.memory.rtc_regs = [0; 5];

        self.resize_sram(sram_size);

        // GBMBCInit reads the RTC save suffix from the open save file for
        // MBC3-RTC/HuC-3/TAMA5; the frontend loads it via rtc::rtc_load_bytes.
        rtc::read_rtc_if_needed(self);
    }

    /// GBMBCSwitchBank
    pub fn mbc_switch_bank(&mut self, bank: i32) {
        let mut bank = bank;
        let mut bank_start = (bank as i64).wrapping_mul(GB_SIZE_CART_BANK0 as i64) as usize;
        if bank_start + GB_SIZE_CART_BANK0 > self.memory.rom_size {
            mlog!(
                Level::GameError,
                rgba_core::log::GB_MBC,
                "Attempting to switch to an invalid ROM bank: {:X}",
                bank
            );
            if self.memory.rom_size == 0 {
                return;
            }
            bank_start &= (to_pow2(self.memory.rom_size as u32) - 1) as usize;
            if bank_start + GB_SIZE_CART_BANK0 > self.memory.rom_size {
                return;
            }
            bank = (bank_start / GB_SIZE_CART_BANK0) as i32;
        }
        self.memory.rom_bank_off = bank_start;
        self.memory.rom_bank_is_sram = false;
        self.memory.current_bank = bank;
        if self.cpu.pc < GB_BASE_VRAM {
            self.set_active_region(self.cpu.pc);
        }
    }

    /// GBMBCSwitchBank0
    pub fn mbc_switch_bank0(&mut self, bank: i32) {
        let mut bank = bank;
        let mut bank_start = (bank as i64).wrapping_mul(GB_SIZE_CART_BANK0 as i64) as usize;
        if bank_start + GB_SIZE_CART_BANK0 > self.memory.rom_size {
            mlog!(
                Level::GameError,
                rgba_core::log::GB_MBC,
                "Attempting to switch to an invalid ROM bank: {:X}",
                bank
            );
            if self.memory.rom_size == 0 {
                return;
            }
            bank_start &= (to_pow2(self.memory.rom_size as u32) - 1) as usize;
            if bank_start + GB_SIZE_CART_BANK0 > self.memory.rom_size {
                return;
            }
            bank = (bank_start / GB_SIZE_CART_BANK0) as i32;
        }
        self.memory.rom_base_off = bank_start;
        self.memory.current_bank0 = bank;
        if self.cpu.pc < GB_SIZE_CART_BANK0 as u16 {
            self.set_active_region(self.cpu.pc);
        }
    }

    /// GBMBCSwitchHalfBank (MBC6 flash pages + NT NEW split mode)
    pub fn mbc_switch_half_bank(&mut self, half: i32, bank: i32) {
        let mut bank = bank;
        let mut bank_start = (bank as i64).wrapping_mul(GB_SIZE_CART_HALFBANK as i64) as usize;
        let is_flash = self.memory.mbc_type == MbcType::Mbc6
            && if half != 0 {
                self.memory.mbc_state.mbc6.flash_bank1
            } else {
                self.memory.mbc_state.mbc6.flash_bank0
            };
        if is_flash {
            if bank_start + GB_SIZE_CART_HALFBANK > GB_SIZE_MBC6_FLASH {
                mlog!(
                    Level::GameError,
                    rgba_core::log::GB_MBC,
                    "Attempting to switch to an invalid Flash bank: {:X}",
                    bank
                );
                bank_start &= GB_SIZE_MBC6_FLASH - 1;
                bank = (bank_start / GB_SIZE_CART_HALFBANK) as i32;
            }
            bank_start = bank_start.wrapping_add(self.sram_size.wrapping_sub(GB_SIZE_MBC6_FLASH));
        } else {
            if bank_start + GB_SIZE_CART_HALFBANK > self.memory.rom_size {
                mlog!(
                    Level::GameError,
                    rgba_core::log::GB_MBC,
                    "Attempting to switch to an invalid ROM bank: {:X}",
                    bank
                );
                if self.memory.rom_size == 0 {
                    return;
                }
                bank_start &= (to_pow2(self.memory.rom_size as u32) - 1) as usize;
                if bank_start + GB_SIZE_CART_HALFBANK > self.memory.rom_size {
                    return;
                }
                bank = (bank_start / GB_SIZE_CART_HALFBANK) as i32;
                // NB: bankStart is NOT recomputed from the bumped bank, like
                // the C; the window stays mapped at the masked offset.
                if bank == 0 {
                    bank += 1;
                }
            }
        }
        if half == 0 {
            self.memory.rom_bank_off = bank_start;
            self.memory.rom_bank_is_sram = is_flash;
            self.memory.current_bank = bank;
        } else {
            self.memory.rom_bank1_off = bank_start;
            self.memory.rom_bank1_is_sram = is_flash;
            self.memory.current_bank1 = bank;
        }
        if self.cpu.pc < GB_BASE_VRAM {
            self.set_active_region(self.cpu.pc);
        }
    }

    /// GBMBCSwitchSramBank
    pub fn mbc_switch_sram_bank(&mut self, bank: i32) {
        let mut bank = bank;
        let mut bank_start = (bank as i64).wrapping_mul(GB_SIZE_EXTERNAL_RAM as i64) as usize;
        if bank_start + GB_SIZE_EXTERNAL_RAM > self.sram_size {
            if self.sram_size == 0 {
                return;
            }
            if self.sram_size < GB_SIZE_EXTERNAL_RAM {
                bank = 0;
                // The C leaves bankStart pointing past the buffer here; pin
                // the offset to bank 0 so the window stays in bounds.
                bank_start = 0;
            } else {
                mlog!(
                    Level::GameError,
                    rgba_core::log::GB_MBC,
                    "Attempting to switch to an invalid RAM bank: {:X}",
                    bank
                );
                bank_start &= (to_pow2(self.sram_size as u32) - 1) as usize;
                if bank_start + GB_SIZE_EXTERNAL_RAM > self.sram_size {
                    bank = 0;
                    // Same OOB-pointer clamp as above.
                    bank_start = 0;
                } else {
                    bank = (bank_start / GB_SIZE_EXTERNAL_RAM) as i32;
                }
            }
        }
        self.memory.sram_bank_off = bank_start;
        self.memory.sram_current_bank = bank;
    }

    /// GBMBCSwitchSramHalfBank (MBC6: SRAM lives before the flash region)
    pub fn mbc_switch_sram_half_bank(&mut self, half: i32, bank: i32) {
        let mut bank = bank;
        let halfbank = GB_SIZE_EXTERNAL_RAM_HALFBANK as i64;
        let mut bank_start = (bank as i64) * halfbank;
        let sram_sz = self.sram_size as i64 - GB_SIZE_MBC6_FLASH as i64;
        if bank_start + halfbank > sram_sz {
            if sram_sz <= 0 {
                return;
            }
            if sram_sz < halfbank {
                bank = 0;
                // C keeps the (now unused) OOB pointer; clamp to bank 0.
                bank_start = 0;
            } else {
                mlog!(
                    Level::GameError,
                    rgba_core::log::GB_MBC,
                    "Attempting to switch to an invalid RAM bank: {:X}",
                    bank
                );
                // Yes, the C masks against the FULL sramSize (incl. flash).
                bank_start &= (to_pow2(self.sram_size as u32) - 1) as i64;
                if bank_start + halfbank > self.sram_size as i64 {
                    bank = 0;
                    bank_start = 0;
                } else {
                    bank = (bank_start / halfbank) as i32;
                }
            }
        }
        let bank_start = bank_start.max(0) as usize;
        if half == 0 {
            self.memory.sram_bank_off = bank_start;
            self.memory.sram_current_bank = bank;
        } else {
            self.memory.sram_bank1_off = bank_start;
            self.memory.current_sram_bank1 = bank;
        }
    }

    /// Port of the installed mbcWrite function pointer (GBMemory.mbcWrite).
    pub fn mbc_write_dispatched(&mut self, address: u16, value: u8) {
        match self.memory.mbc_type {
            MbcType::None => mbc_none(self, address, value),
            MbcType::Mbc1 => licensed::mbc1(self, address, value),
            MbcType::Mbc2 => licensed::mbc2(self, address, value),
            MbcType::Mbc3 | MbcType::Mbc3Rtc => licensed::mbc3(self, address, value),
            // NB: the C installs _GBMBC5 for anything unrecognized too.
            MbcType::Autodetect | MbcType::Mbc5 | MbcType::Mbc5Rumble => {
                licensed::mbc5(self, address, value)
            }
            MbcType::Mbc6 => licensed::mbc6(self, address, value),
            MbcType::Mbc7 => licensed::mbc7(self, address, value),
            MbcType::Mmm01 => licensed::mmm01(self, address, value),
            MbcType::HuC1 => licensed::huc1(self, address, value),
            MbcType::HuC3 => huc3::huc3(self, address, value),
            MbcType::Tama5 => tama5::tama5(self, address, value),
            MbcType::PocketCam => pocket_cam::pocket_cam(self, address, value),
            MbcType::M161 => unlicensed::m161(self, address, value),
            MbcType::UnlWisdomTree => unlicensed::wisdom_tree(self, address, value),
            MbcType::UnlPkjd => unlicensed::pkjd(self, address, value),
            MbcType::UnlNtOld1 => unlicensed::nt_old1(self, address, value),
            MbcType::UnlNtOld2 => unlicensed::nt_old2(self, address, value),
            MbcType::UnlNtNew => unlicensed::nt_new(self, address, value),
            MbcType::UnlBbd => unlicensed::bbd(self, address, value),
            MbcType::UnlHitek => unlicensed::hitek(self, address, value),
            MbcType::UnlLiCheng => unlicensed::li_cheng(self, address, value),
            MbcType::UnlGgb81 => unlicensed::ggb81(self, address, value),
            MbcType::UnlSachenMmc1 | MbcType::UnlSachenMmc2 => {
                unlicensed::sachen(self, address, value)
            }
            MbcType::UnlSintax => unlicensed::sintax(self, address, value),
        }
    }

    /// Port of the installed mbcRead function pointer (GBMemory.mbcRead).
    /// Only reachable for types with a read handler (see Memory::mbc_has_read).
    pub fn mbc_read_dispatched(&mut self, address: u16) -> u8 {
        match self.memory.mbc_type {
            MbcType::Mbc2 => licensed::mbc2_read(self, address),
            MbcType::Mbc6 => licensed::mbc6_read(self, address),
            MbcType::Mbc7 => licensed::mbc7_read(self, address),
            MbcType::HuC3 => huc3::huc3_read(self, address),
            MbcType::Tama5 => tama5::tama5_read(self, address),
            MbcType::PocketCam => pocket_cam::pocket_cam_read(self, address),
            MbcType::UnlPkjd => unlicensed::pkjd_read(self, address),
            MbcType::UnlBbd => unlicensed::bbd_read(self, address),
            MbcType::UnlHitek => unlicensed::hitek_read(self, address),
            MbcType::UnlGgb81 => unlicensed::ggb81_read(self, address),
            MbcType::UnlSachenMmc1 => unlicensed::sachen_mmc1_read(self, address),
            MbcType::UnlSachenMmc2 => unlicensed::sachen_mmc2_read(self, address),
            MbcType::UnlSintax => unlicensed::sintax_read(self, address),
            _ => 0xFF,
        }
    }

    /// In-RAM GBResizeSram: grow/shrink memory.sram, filling new bytes with
    /// 0xFF, and update gb.sram_size. The VFile footwork of the C is the
    /// frontend's job.
    pub fn resize_sram(&mut self, size: usize) {
        if !self.memory.sram.is_empty() && size <= self.sram_size {
            return;
        }
        let keep = self.memory.sram.len().min(size);
        let mut new_sram = vec![0xFF; size];
        new_sram[..keep].copy_from_slice(&self.memory.sram[..keep]);
        self.memory.sram = new_sram;
        self.sram_size = size;
    }
}

/// _GBMBCNone
fn mbc_none(gb: &mut Gb, _address: u16, _value: u8) {
    if gb.yanked_rom_size == 0 {
        mlog!(Level::GameError, rgba_core::log::GB_MBC, "Wrote to invalid MBC");
    }
}
