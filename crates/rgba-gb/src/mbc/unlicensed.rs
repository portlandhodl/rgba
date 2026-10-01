// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gb/mbc/unlicensed.c (Wisdom Tree, PKJD, NT old/new,
// BBD, Hitek, Li Cheng, GGB81, Sachen, Sintax), plus _GBM161 from
// mgba/src/gb/mbc/mbc.c.

use rgba_core::mlog;
use rgba_core::Level;

use crate::gb::Gb;
use crate::mbc::licensed;
use crate::memory::{
    GB_BASE_CART_BANK1, GB_BASE_VRAM, GB_SIZE_CART_BANK0, GB_SIZE_EXTERNAL_RAM, MbcType,
};

#[derive(Default)]
pub struct M161State {
    pub locked: bool,
    pub bank: u8,
}

#[derive(Default)]
pub struct NtOldState {
    pub swapped: bool,
    pub base_bank: u8,
    pub bank_count: u8,
    pub rumble: bool,
}

#[derive(Default)]
pub struct NtNewState {
    pub split_mode: bool,
}

#[derive(Default)]
pub struct PkjdState {
    pub reg: [u8; 2],
}

#[derive(Default)]
pub struct BbdState {
    pub data_swap_mode: i32,
    pub bank_swap_mode: i32,
}

// enum GBSachenLockMode
pub const GB_SACHEN_LOCKED_DMG: i32 = 0;
pub const GB_SACHEN_LOCKED_CGB: i32 = 1;
pub const GB_SACHEN_UNLOCKED: i32 = 2;

#[derive(Default)]
pub struct SachenState {
    pub locked: i32,
    pub transition: i32,
    pub mask: u8,
    pub unmasked_bank: u8,
    pub base_bank: u8,
}

#[derive(Default)]
pub struct SintaxState {
    pub mode: u8,
    pub xor_values: [u8; 4],
    pub bank_no: u8,
    pub rom_bank_xor: u8,
}

/// _GBM161
pub fn m161(gb: &mut Gb, _address: u16, value: u8) {
    let m161 = &mut gb.memory.mbc_state.m161;
    if m161.locked {
        return;
    }

    let bank = value & 0x7;
    m161.bank = bank;
    m161.locked = true;

    gb.mbc_switch_bank0(bank as i32 * 2);
    gb.mbc_switch_bank(bank as i32 * 2 + 1);
}

/// _GBWisdomTree
pub fn wisdom_tree(gb: &mut Gb, address: u16, value: u8) {
    let bank = (address & 0x3F) as i32;
    match address >> 14 {
        0x0 => {
            gb.mbc_switch_bank0(bank * 2);
            gb.mbc_switch_bank(bank * 2 + 1);
        }
        _ => {
            // TODO
            mlog!(
                Level::Stub,
                rgba_core::log::GB_MBC,
                "Wisdom Tree unknown address: {:04X}:{:02X}",
                address,
                value
            );
        }
    }
}

/// _GBPKJD
pub fn pkjd(gb: &mut Gb, address: u16, value: u8) {
    match address >> 13 {
        0x2 => {
            if value < 8 {
                gb.memory.direct_sram_access = true;
                gb.memory.active_rtc_reg = 0;
            } else if (0xD..=0xF).contains(&value) {
                gb.memory.direct_sram_access = false;
                gb.memory.rtc_access = false;
                gb.memory.active_rtc_reg = (value - 8) as i32;
            }
        }
        0x5 => {
            if !gb.memory.sram_access {
                return;
            }
            match gb.memory.active_rtc_reg {
                0 => {
                    let off = gb.memory.sram_bank_off
                        + (address as usize & (GB_SIZE_EXTERNAL_RAM - 1));
                    if let Some(b) = gb.memory.sram.get_mut(off) {
                        *b = value;
                    }
                }
                5 | 6 => {
                    gb.memory.mbc_state.pkjd.reg[(gb.memory.active_rtc_reg - 5) as usize] = value;
                }
                7 => match value {
                    0x11 => {
                        gb.memory.mbc_state.pkjd.reg[0] =
                            gb.memory.mbc_state.pkjd.reg[0].wrapping_sub(1);
                    }
                    0x12 => {
                        gb.memory.mbc_state.pkjd.reg[1] =
                            gb.memory.mbc_state.pkjd.reg[1].wrapping_sub(1);
                    }
                    0x41 => {
                        gb.memory.mbc_state.pkjd.reg[0] = gb.memory.mbc_state.pkjd.reg[0]
                            .wrapping_add(gb.memory.mbc_state.pkjd.reg[1]);
                    }
                    0x42 => {
                        gb.memory.mbc_state.pkjd.reg[1] = gb.memory.mbc_state.pkjd.reg[1]
                            .wrapping_add(gb.memory.mbc_state.pkjd.reg[0]);
                    }
                    0x51 => {
                        gb.memory.mbc_state.pkjd.reg[0] =
                            gb.memory.mbc_state.pkjd.reg[0].wrapping_add(1);
                    }
                    0x52 => {
                        gb.memory.mbc_state.pkjd.reg[1] =
                            gb.memory.mbc_state.pkjd.reg[1].wrapping_sub(1);
                    }
                    _ => {}
                },
                _ => {}
            }
            return;
        }
        _ => {}
    }
    licensed::mbc3(gb, address, value);
}

/// _GBPKJDRead
pub fn pkjd_read(gb: &mut Gb, address: u16) -> u8 {
    if !gb.memory.sram_access {
        return 0xFF;
    }
    match gb.memory.active_rtc_reg {
        0 => gb.memory.sram_byte(
            gb.memory.sram_bank_off + (address as usize & (GB_SIZE_EXTERNAL_RAM - 1)),
        ),
        5 | 6 => gb.memory.mbc_state.pkjd.reg[(gb.memory.active_rtc_reg - 5) as usize],
        _ => 0,
    }
}

/// _reorderBits
fn reorder_bits(input: u8, reorder: &[u8; 8]) -> u8 {
    let mut newbyte = 0u8;
    for (i, &oldbit) in reorder.iter().enumerate() {
        newbyte |= ((input >> oldbit) & 1) << i;
    }
    newbyte
}

const NT_OLD1_REORDER: [u8; 8] = [0, 2, 1, 4, 3, 5, 6, 7];

/// _ntOldMulticart
fn nt_old_multicart(gb: &mut Gb, address: u16, value: u8, reorder: &[u8; 8]) {
    match address & 3 {
        0 => {
            mlog!(
                Level::Stub,
                rgba_core::log::GB_MBC,
                "Unimplemented NT Old 1 address 0"
            );
        }
        1 => {
            let value = value & 0x3F;
            gb.memory.mbc_state.nt_old.base_bank = value * 2;
            if gb.memory.mbc_state.nt_old.base_bank != 0 {
                let base = gb.memory.mbc_state.nt_old.base_bank as i32;
                gb.mbc_switch_bank0(base);
                gb.mbc_switch_bank(base + 1);
            }
        }
        2 => {
            if value & 0xF0 == 0xE0 {
                gb.sram_size = 0x2000;
                gb.resize_sram(gb.sram_size);
            }
            gb.memory.mbc_state.nt_old.bank_count = match value & 0xF {
                0x00 => 32,
                0x08 => 16,
                0x0C => 8,
                0x0E => 4,
                0x0F => 2,
                _ => 32,
            };
        }
        _ => {
            gb.memory.mbc_state.nt_old.swapped = value & 0x10 != 0;

            let mut bank = gb.memory.current_bank;
            if gb.memory.mbc_state.nt_old.swapped {
                bank = reorder_bits(bank as u8, reorder) as i32;
            }
            gb.mbc_switch_bank(bank);
        }
    }
}

/// _GBNTOld1
pub fn nt_old1(gb: &mut Gb, address: u16, value: u8) {
    let mut bank = value as i32;

    match address >> 12 {
        0x0 | 0x1 => {
            licensed::mbc3(gb, address, value);
        }
        0x2 | 0x3 => {
            bank &= 0x1F;
            if bank == 0 {
                bank = 1;
            }
            let st = &gb.memory.mbc_state.nt_old;
            if st.swapped {
                bank = reorder_bits(bank as u8, &NT_OLD1_REORDER) as i32;
            }
            if st.bank_count != 0 {
                bank &= st.bank_count as i32 - 1;
            }
            let base = st.base_bank as i32;
            gb.mbc_switch_bank(bank + base);
        }
        0x5 => {
            nt_old_multicart(gb, address, value, &NT_OLD1_REORDER);
        }
        _ => {}
    }
}

const NT_OLD2_REORDER: [u8; 8] = [1, 2, 0, 3, 4, 5, 6, 7];

/// The rumble tail shared by the _GBNTOld2 0x4 and 0x5 cases.
fn nt_old2_rumble(gb: &mut Gb, address: u16, value: u8) {
    if address == 0x5001 {
        gb.memory.mbc_state.nt_old.rumble = value & 0x80 != 0;
    }

    if gb.memory.mbc_state.nt_old.rumble {
        // The C conditionally calls memory->rumble->setRumble and integrates
        // lastRumble; this port exposes the state directly.
        let swapped = gb.memory.mbc_state.nt_old.swapped;
        gb.memory.rumble_state = value & if swapped { 0x08 } else { 0x02 } != 0;
    }
}

/// _GBNTOld2
pub fn nt_old2(gb: &mut Gb, address: u16, value: u8) {
    let mut bank = value as i32;

    match address >> 12 {
        0x0 | 0x1 => {
            licensed::mbc3(gb, address, value);
        }
        0x2 | 0x3 => {
            if bank == 0 {
                bank = 1;
            }
            let st = &gb.memory.mbc_state.nt_old;
            if st.swapped {
                bank = reorder_bits(bank as u8, &NT_OLD2_REORDER) as i32;
            }
            if st.bank_count != 0 {
                bank &= st.bank_count as i32 - 1;
            }
            let base = st.base_bank as i32;
            gb.mbc_switch_bank(bank + base);
        }
        0x5 => {
            nt_old_multicart(gb, address, value, &NT_OLD2_REORDER);
            // Fall through
            nt_old2_rumble(gb, address, value);
        }
        0x4 => {
            nt_old2_rumble(gb, address, value);
        }
        _ => {}
    }
}

/// _GBNTNew
pub fn nt_new(gb: &mut Gb, address: u16, value: u8) {
    if address >> 8 == 0x14 {
        gb.memory.mbc_state.nt_new.split_mode = true;
        return;
    }
    if gb.memory.mbc_state.nt_new.split_mode {
        let mut bank = value as i32;
        if bank < 2 {
            bank = 2;
        }
        match address >> 10 {
            8 => {
                gb.mbc_switch_half_bank(0, bank);
                return;
            }
            9 => {
                gb.mbc_switch_half_bank(1, bank);
                return;
            }
            _ => {}
        }
    }
    licensed::mbc5(gb, address, value);
}

const BBD_DATA_REORDERING: [[u8; 8]; 8] = [
    [0, 1, 2, 3, 4, 5, 6, 7], // 00 - Normal
    [0, 1, 2, 3, 4, 5, 6, 7], // 01 - NOT KNOWN YET
    [0, 1, 2, 3, 4, 5, 6, 7], // 02 - NOT KNOWN YET
    [0, 1, 2, 3, 4, 5, 6, 7], // 03 - NOT KNOWN YET
    [0, 5, 1, 3, 4, 2, 6, 7], // 04 - Garou
    [0, 4, 2, 3, 1, 5, 6, 7], // 05 - Harry
    [0, 1, 2, 3, 4, 5, 6, 7], // 06 - NOT KNOWN YET
    [0, 1, 5, 3, 4, 2, 6, 7], // 07 - Digimon
];

const BBD_BANK_REORDERING: [[u8; 8]; 8] = [
    [0, 1, 2, 3, 4, 5, 6, 7], // 00 - Normal
    [0, 1, 2, 3, 4, 5, 6, 7], // 01 - NOT KNOWN YET
    [0, 1, 2, 3, 4, 5, 6, 7], // 02 - NOT KNOWN YET
    [3, 4, 2, 0, 1, 5, 6, 7], // 03 - 0,1 unconfirmed. Digimon/Garou
    [0, 1, 2, 3, 4, 5, 6, 7], // 04 - NOT KNOWN YET
    [1, 2, 3, 4, 0, 5, 6, 7], // 05 - 0,1 unconfirmed. Harry
    [0, 1, 2, 3, 4, 5, 6, 7], // 06 - NOT KNOWN YET
    [0, 1, 2, 3, 4, 5, 6, 7], // 07 - NOT KNOWN YET
];

/// Current ROM bank byte for the reordering reads (romBank[addr & 0x3FFF]).
fn rom_bank_byte(gb: &Gb, address: u16) -> u8 {
    gb.memory
        .rom_byte(gb.memory.rom_bank_off + (address as usize & (GB_SIZE_CART_BANK0 - 1)))
}

///  _GBBBD
pub fn bbd(gb: &mut Gb, address: u16, value: u8) {
    let mut value = value;
    match address & 0xF0FF {
        0x2000 => {
            value = reorder_bits(
                value,
                &BBD_BANK_REORDERING[gb.memory.mbc_state.bbd.bank_swap_mode as usize & 7],
            );
        }
        0x2001 => {
            gb.memory.mbc_state.bbd.data_swap_mode = (value & 0x07) as i32;
            let mode = gb.memory.mbc_state.bbd.data_swap_mode;
            if !(mode == 0x07 || mode == 0x05 || mode == 0x04 || mode == 0x00) {
                mlog!(
                    Level::Stub,
                    rgba_core::log::GB_MBC,
                    "Bitswap mode unsupported: {:X}",
                    mode
                );
            }
        }
        0x2080 => {
            gb.memory.mbc_state.bbd.bank_swap_mode = (value & 0x07) as i32;
            let mode = gb.memory.mbc_state.bbd.bank_swap_mode;
            if !(mode == 0x03 || mode == 0x05 || mode == 0x00) {
                mlog!(
                    Level::Stub,
                    rgba_core::log::GB_MBC,
                    "Bankswap mode unsupported: {:X}",
                    gb.memory.mbc_state.bbd.data_swap_mode
                );
            }
        }
        _ => {}
    }
    licensed::mbc5(gb, address, value);
}

/// _GBBBDRead
pub fn bbd_read(gb: &mut Gb, address: u16) -> u8 {
    match address >> 14 {
        1 => reorder_bits(
            rom_bank_byte(gb, address),
            &BBD_DATA_REORDERING[gb.memory.mbc_state.bbd.data_swap_mode as usize & 7],
        ),
        _ => rom_bank_byte(gb, address),
    }
}

const HITEK_DATA_REORDERING: [[u8; 8]; 8] = [
    [0, 1, 2, 3, 4, 5, 6, 7],
    [0, 6, 5, 3, 4, 1, 2, 7],
    [0, 5, 6, 3, 4, 2, 1, 7],
    [0, 6, 2, 3, 4, 5, 1, 7],
    [0, 6, 1, 3, 4, 5, 2, 7],
    [0, 1, 6, 3, 4, 5, 2, 7],
    [0, 2, 6, 3, 4, 1, 5, 7],
    [0, 6, 2, 3, 4, 1, 5, 7],
];

const HITEK_BANK_REORDERING: [[u8; 8]; 8] = [
    [0, 1, 2, 3, 4, 5, 6, 7],
    [3, 2, 1, 0, 4, 5, 6, 7],
    [2, 1, 0, 3, 4, 5, 6, 7],
    [1, 0, 3, 2, 4, 5, 6, 7],
    [0, 3, 2, 1, 4, 5, 6, 7],
    [2, 3, 0, 1, 4, 5, 6, 7],
    [3, 0, 1, 2, 4, 5, 6, 7],
    [2, 0, 3, 1, 4, 5, 6, 7],
];

///  _GBHitek
pub fn hitek(gb: &mut Gb, address: u16, value: u8) {
    let mut value = value;
    match address & 0xF0FF {
        0x2000 => {
            value = reorder_bits(
                value,
                &HITEK_BANK_REORDERING[gb.memory.mbc_state.bbd.bank_swap_mode as usize & 7],
            );
        }
        0x2001 => {
            gb.memory.mbc_state.bbd.data_swap_mode = (value & 0x07) as i32;
        }
        0x2080 => {
            gb.memory.mbc_state.bbd.bank_swap_mode = (value & 0x07) as i32;
        }
        0x300 => {
            // See hhugboy src/memory/mbc/MbcUnlHitek.cpp for commentary on this return
            return;
        }
        _ => {}
    }
    licensed::mbc5(gb, address, value);
}

/// _GBHitekRead
pub fn hitek_read(gb: &mut Gb, address: u16) -> u8 {
    match address >> 14 {
        1 => reorder_bits(
            rom_bank_byte(gb, address),
            &HITEK_DATA_REORDERING[gb.memory.mbc_state.bbd.data_swap_mode as usize & 7],
        ),
        _ => rom_bank_byte(gb, address),
    }
}

const GGB81_DATA_REORDERING: [[u8; 8]; 8] = [
    [0, 1, 2, 3, 4, 5, 6, 7],
    [0, 2, 1, 3, 4, 6, 5, 7],
    [0, 6, 5, 3, 4, 2, 1, 7],
    [0, 5, 1, 3, 4, 2, 6, 7],
    [0, 5, 2, 3, 4, 1, 6, 7],
    [0, 2, 6, 3, 4, 5, 1, 7],
    [0, 1, 6, 3, 4, 2, 5, 7],
    [0, 2, 5, 3, 4, 6, 1, 7],
];

///  _GBGGB81
pub fn ggb81(gb: &mut Gb, address: u16, value: u8) {
    if address & 0xF0FF == 0x2001 {
        gb.memory.mbc_state.bbd.data_swap_mode = (value & 0x07) as i32;
    }
    licensed::mbc5(gb, address, value);
}

/// _GBGGB81Read
pub fn ggb81_read(gb: &mut Gb, address: u16) -> u8 {
    match address >> 14 {
        1 => reorder_bits(
            rom_bank_byte(gb, address),
            &GGB81_DATA_REORDERING[gb.memory.mbc_state.bbd.data_swap_mode as usize & 7],
        ),
        _ => rom_bank_byte(gb, address),
    }
}

///  _GBLiCheng
pub fn li_cheng(gb: &mut Gb, address: u16, value: u8) {
    if address > 0x2100 && address < 0x3000 {
        return;
    }
    licensed::mbc5(gb, address, value);
}

/// _GBSachen
pub fn sachen(gb: &mut Gb, address: u16, value: u8) {
    let mut bank = value;
    match address >> 13 {
        0 => {
            let st = &mut gb.memory.mbc_state.sachen;
            if st.unmasked_bank & 0x30 == 0x30 {
                st.base_bank = bank;
                let b = (st.base_bank & st.mask) as i32;
                gb.mbc_switch_bank0(b);
            }
        }
        1 => {
            if bank == 0 {
                bank = 1;
            }
            let st = &mut gb.memory.mbc_state.sachen;
            st.unmasked_bank = bank;
            bank = (bank & !st.mask) | (st.base_bank & st.mask);
            let bank = bank;
            let _ = st;
            gb.mbc_switch_bank(bank as i32);
        }
        2 => {
            let st = &mut gb.memory.mbc_state.sachen;
            if st.unmasked_bank & 0x30 == 0x30 {
                st.mask = value;
                bank = (st.unmasked_bank & !st.mask) | (st.base_bank & st.mask);
                let b0 = (st.base_bank & st.mask) as i32;
                let bank = bank;
                let _ = st;
                gb.mbc_switch_bank(bank as i32);
                gb.mbc_switch_bank0(b0);
            }
        }
        6 => {
            if gb.memory.mbc_type == MbcType::UnlSachenMmc2
                && gb.memory.mbc_state.sachen.locked == GB_SACHEN_LOCKED_DMG
            {
                gb.memory.mbc_state.sachen.locked = GB_SACHEN_LOCKED_CGB;
                gb.memory.mbc_state.sachen.transition = 0;
            }
        }
        _ => {}
    }
}

/// _unscrambleSachen
fn unscramble_sachen(address: u16) -> u16 {
    let mut unscrambled = address & 0xFFAC;
    unscrambled |= (address & 0x40) >> 6;
    unscrambled |= (address & 0x10) >> 3;
    unscrambled |= (address & 0x02) << 3;
    unscrambled |= (address & 0x01) << 6;
    unscrambled
}

/// _GBSachenMMC1Read
pub fn sachen_mmc1_read(gb: &mut Gb, address: u16) -> u8 {
    let mut address = address;
    let st = &mut gb.memory.mbc_state.sachen;
    if st.locked != GB_SACHEN_UNLOCKED && address & 0xFF00 == 0x100 {
        st.transition += 1;
        if st.transition == 0x31 {
            st.locked = GB_SACHEN_UNLOCKED;
        } else {
            address |= 0x80;
        }
    }

    if address & 0xFF00 == 0x0100 {
        address = unscramble_sachen(address);
    }

    if address < GB_BASE_CART_BANK1 {
        gb.memory.rom_byte(gb.memory.rom_base_off + address as usize)
    } else if address < GB_BASE_VRAM {
        gb.memory
            .rom_byte(gb.memory.rom_bank_off + (address as usize & (GB_SIZE_CART_BANK0 - 1)))
    } else {
        0xFF
    }
}

/// _GBSachenMMC2Read
pub fn sachen_mmc2_read(gb: &mut Gb, address: u16) -> u8 {
    let mut address = address;
    let st = &mut gb.memory.mbc_state.sachen;
    if address >= 0xC000 && st.locked == GB_SACHEN_LOCKED_DMG {
        st.transition = 0;
        st.locked = GB_SACHEN_LOCKED_CGB;
    }

    if st.locked != GB_SACHEN_UNLOCKED && address & 0x8700 == 0x0100 {
        st.transition += 1;
        if st.transition == 0x31 {
            st.locked += 1;
            st.transition = 0;
        }
    }

    if address & 0xFF00 == 0x0100 {
        if st.locked == GB_SACHEN_LOCKED_CGB {
            address |= 0x80;
        }
        address = unscramble_sachen(address);
    }

    if address < GB_BASE_CART_BANK1 {
        gb.memory.rom_byte(gb.memory.rom_base_off + address as usize)
    } else if address < GB_BASE_VRAM {
        gb.memory
            .rom_byte(gb.memory.rom_bank_off + (address as usize & (GB_SIZE_CART_BANK0 - 1)))
    } else {
        0xFF
    }
}

const SINTAX_REORDERING: [[u8; 8]; 16] = [
    [2, 1, 4, 3, 6, 5, 0, 7],
    [3, 2, 5, 4, 7, 6, 1, 0],
    [0, 1, 2, 3, 4, 5, 6, 7], // unknown
    [0, 1, 2, 3, 4, 5, 6, 7], // unknown
    [0, 1, 2, 3, 4, 5, 6, 7], // unknown
    [4, 5, 2, 3, 0, 1, 6, 7],
    [0, 1, 2, 3, 4, 5, 6, 7], // unknown
    [6, 7, 4, 5, 1, 3, 0, 2],
    [0, 1, 2, 3, 4, 5, 6, 7], // unknown
    [7, 6, 1, 0, 3, 2, 5, 4],
    [0, 1, 2, 3, 4, 5, 6, 7], // unknown
    [5, 4, 7, 6, 1, 0, 3, 2],
    [0, 1, 2, 3, 4, 5, 6, 7], // unknown
    [2, 3, 4, 5, 6, 7, 0, 1],
    [0, 1, 2, 3, 4, 5, 6, 7], // unknown
    [0, 1, 2, 3, 4, 5, 6, 7],
];

///  _GBSintax
pub fn sintax(gb: &mut Gb, address: u16, value: u8) {
    let mut value = value;

    if (0x2000..0x3000).contains(&address) {
        let st = &mut gb.memory.mbc_state.sintax;
        st.bank_no = value;
        value = reorder_bits(value, &SINTAX_REORDERING[st.mode as usize]);
        st.rom_bank_xor = st.xor_values[(st.bank_no & 0x3) as usize];
    }

    if address & 0xF0F0 == 0x5010 {
        // contrary to previous belief it IS possible to change the mode after setting it initially
        // The reason Metal Max was breaking is because it only recognises writes to 5x1x
        // and that game writes to a bunch of other 5xxx addresses before battles
        gb.memory.mbc_state.sintax.mode = value & 0xF;
        let mode = gb.memory.mbc_state.sintax.mode;

        mlog!(
            Level::Debug,
            rgba_core::log::GB_MBC,
            "Sintax bank reorder mode: {:X}",
            mode
        );

        match mode {
            // Supported modes
            0x00 // Lion King, Golden Sun
            | 0x01 // Langrisser
            | 0x05 // Maple Story, Pokemon Platinum
            | 0x07 // Bynasty Warriors 5
            | 0x09 // ???
            | 0x0B // Shaolin Legend
            | 0x0D // Older games
            | 0x0F => {} // Default mode, no reordering
            _ => {
                mlog!(
                    Level::Debug,
                    rgba_core::log::GB_MBC,
                    "Bank reorder mode unsupported - {:X}",
                    mode
                );
            }
        }

        let bank_no = gb.memory.mbc_state.sintax.bank_no;
        sintax(gb, 0x2000, bank_no); // fake a bank switch to select the correct bank
        return;
    }

    if (0x7000..0x8000).contains(&address) {
        let xor_no = (address & 0x00F0) >> 4;
        let st = &mut gb.memory.mbc_state.sintax;
        match xor_no {
            2 => {
                st.xor_values[0] = value;
                mlog!(Level::Debug, rgba_core::log::GB_MBC, "Sintax XOR 0: {:X}", value);
            }
            3 => {
                st.xor_values[1] = value;
                mlog!(Level::Debug, rgba_core::log::GB_MBC, "Sintax XOR 1: {:X}", value);
            }
            4 => {
                st.xor_values[2] = value;
                mlog!(Level::Debug, rgba_core::log::GB_MBC, "Sintax XOR 2: {:X}", value);
            }
            5 => {
                st.xor_values[3] = value;
                mlog!(Level::Debug, rgba_core::log::GB_MBC, "Sintax XOR 3: {:X}", value);
            }
            _ => {}
        }

        // xor is applied immediately to the current bank
        gb.memory.mbc_state.sintax.rom_bank_xor =
            gb.memory.mbc_state.sintax.xor_values[(gb.memory.mbc_state.sintax.bank_no & 0x3) as usize];
    }
    licensed::mbc5(gb, address, value);
}

/// _GBSintaxRead
pub fn sintax_read(gb: &mut Gb, address: u16) -> u8 {
    match address >> 13 {
        0x2 | 0x3 => {
            rom_bank_byte(gb, address) ^ gb.memory.mbc_state.sintax.rom_bank_xor
        }
        0x5 => {
            if gb.memory.sram_access && !gb.memory.sram.is_empty() {
                gb.memory.sram_byte(
                    gb.memory.sram_bank_off + (address as usize & (GB_SIZE_EXTERNAL_RAM - 1)),
                )
            } else {
                0xFF
            }
        }
        _ => 0xFF,
    }
}
