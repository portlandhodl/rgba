// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gb/mbc/mbc.c (MBC1/2/3/5/6/7 handlers and reads) and
// mgba/src/gb/mbc/licensed.c (MMM01, HuC-1).

use rgba_core::mlog;
use rgba_core::Level;

use crate::gb::Gb;
use crate::memory::{MbcType, GB_SIZE_CART_BANK0, GB_SIZE_EXTERNAL_RAM_HALFBANK, M_SAVEDATA_DIRT_NEW};
use crate::mbc::rtc;

#[derive(Default)]
pub struct Mbc1State {
    pub mode: i32,
    pub multicart_stride: i32,
    pub bank_lo: u8,
    pub bank_hi: u8,
}

#[derive(Default)]
pub struct Mbc6State {
    pub flash_bank0: bool,
    pub flash_bank1: bool,
}

/// GBMBC7Field bit packing: CS=0x80, CLK=0x40, DI=0x02, DO=0x01.
pub const MBC7_FIELD_CS: u8 = 0x80;
pub const MBC7_FIELD_CLK: u8 = 0x40;
pub const MBC7_FIELD_DI: u8 = 0x02;
pub const MBC7_FIELD_DO: u8 = 0x01;

// enum GBMBC7MachineState
pub const MBC7_STATE_IDLE: i32 = 0;
pub const MBC7_STATE_READ_COMMAND: i32 = 1;
pub const MBC7_STATE_DO: i32 = 2;
pub const MBC7_STATE_EEPROM_EWDS: i32 = 0x10;
pub const MBC7_STATE_EEPROM_WRAL: i32 = 0x11;
pub const MBC7_STATE_EEPROM_ERAL: i32 = 0x12;
pub const MBC7_STATE_EEPROM_EWEN: i32 = 0x13;
pub const MBC7_STATE_EEPROM_WRITE: i32 = 0x14;
pub const MBC7_STATE_EEPROM_READ: i32 = 0x18;
pub const MBC7_STATE_EEPROM_ERASE: i32 = 0x1C;

#[derive(Default)]
pub struct Mbc7State {
    pub state: i32,
    pub sr: u16,
    pub address: u8,
    pub writable: bool,
    pub sr_bits: i32,
    pub access: u8,
    pub latch: u8,
    /// Packs CS=0x80, CLK=0x40, DI=0x02, DO=0x01.
    pub eeprom: u8,
}

#[derive(Default)]
pub struct Mmm01State {
    pub locked: bool,
    pub current_bank0: i32,
}

/// _GBMBC1Update
fn mbc1_update(gb: &mut Gb) {
    let stride = gb.memory.mbc_state.mbc1.multicart_stride;
    let mode = gb.memory.mbc_state.mbc1.mode;
    let bank_hi = gb.memory.mbc_state.mbc1.bank_hi as i32;
    let mut bank = gb.memory.mbc_state.mbc1.bank_lo as i32;
    bank &= 1i32.wrapping_shl(stride as u32) - 1;
    bank |= bank_hi.wrapping_shl(stride as u32);
    if mode != 0 {
        gb.mbc_switch_bank0(bank_hi.wrapping_shl(stride as u32));
        gb.mbc_switch_sram_bank(bank_hi & 3);
    } else {
        gb.mbc_switch_bank0(0);
        gb.mbc_switch_sram_bank(0);
    }
    if gb.memory.mbc_state.mbc1.bank_lo & 0x1F == 0 {
        gb.memory.mbc_state.mbc1.bank_lo = gb.memory.mbc_state.mbc1.bank_lo.wrapping_add(1);
        bank += 1;
    }
    gb.mbc_switch_bank(bank);
}

/// _GBMBC1
pub fn mbc1(gb: &mut Gb, address: u16, value: u8) {
    let mut bank = (value & 0x1F) as i32;
    match address >> 13 {
        0x0 => match value & 0xF {
            0 => {
                gb.memory.sram_access = false;
            }
            0xA => {
                gb.memory.sram_access = true;
                let b = gb.memory.sram_current_bank;
                gb.mbc_switch_sram_bank(b);
            }
            _ => {
                // TODO
                mlog!(Level::Stub, rgba_core::log::GB_MBC, "MBC1 unknown value {:02X}", value);
            }
        },
        0x1 => {
            gb.memory.mbc_state.mbc1.bank_lo = bank as u8;
            mbc1_update(gb);
        }
        0x2 => {
            bank &= 3;
            gb.memory.mbc_state.mbc1.bank_hi = bank as u8;
            mbc1_update(gb);
        }
        0x3 => {
            gb.memory.mbc_state.mbc1.mode = (value & 1) as i32;
            mbc1_update(gb);
        }
        _ => {
            // TODO
            mlog!(
                Level::Stub,
                rgba_core::log::GB_MBC,
                "MBC1 unknown address: {:04X}:{:02X}",
                address,
                value
            );
        }
    }
}

/// _GBMBC2
pub fn mbc2(gb: &mut Gb, address: u16, value: u8) {
    let shift = (address & 1) * 4;
    let mut bank = (value & 0xF) as i32;
    match (address & 0xC100) >> 8 {
        0x0 => match value & 0x0F {
            0 => {
                gb.memory.sram_access = false;
            }
            0xA => {
                gb.memory.sram_access = true;
            }
            _ => {
                // TODO
                mlog!(Level::Stub, rgba_core::log::GB_MBC, "MBC2 unknown value {:02X}", value);
            }
        },
        0x1 => {
            if bank == 0 {
                bank += 1;
            }
            gb.mbc_switch_bank(bank);
        }
        0x80 | 0x81 | 0x82 | 0x83 => {
            if !gb.memory.sram_access {
                return;
            }
            let address = address & 0x1FF;
            let off = gb.memory.sram_bank_off + (address >> 1) as usize;
            // MBC2 forces a 0x100-byte SRAM; guard to stay panic-free.
            if let Some(b) = gb.memory.sram.get_mut(off) {
                *b &= 0xF0 >> shift;
                *b |= (value & 0xF) << shift;
            }
            gb.sram_dirty |= M_SAVEDATA_DIRT_NEW;
        }
        _ => {
            // TODO
            mlog!(
                Level::Stub,
                rgba_core::log::GB_MBC,
                "MBC2 unknown address: {:04X}:{:02X}",
                address,
                value
            );
        }
    }
}

/// _GBMBC2Read
pub fn mbc2_read(gb: &mut Gb, address: u16) -> u8 {
    if !gb.memory.sram_access {
        return 0xFF;
    }
    let address = address & 0x1FF;
    let shift = (address & 1) * 4;
    (gb.memory
        .sram_byte(gb.memory.sram_bank_off + (address >> 1) as usize)
        >> shift)
        | 0xF0
}

/// _GBMBC3
pub fn mbc3(gb: &mut Gb, address: u16, value: u8) {
    let mut bank = value as i32;
    match address >> 13 {
        0x0 => {
            if value & 0xF == 0xA {
                gb.memory.sram_access = true;
                let b = gb.memory.sram_current_bank;
                gb.mbc_switch_sram_bank(b);
            } else {
                gb.memory.sram_access = false;
            }
        }
        0x1 => {
            if gb.memory.rom_size < GB_SIZE_CART_BANK0 * 0x80 {
                bank &= 0x7F;
            }
            if bank == 0 {
                bank += 1;
            }
            gb.mbc_switch_bank(bank);
        }
        0x2 => {
            bank &= 0xF;
            if bank < 8 {
                // NB: the C switches using the raw value here, not bank.
                gb.mbc_switch_sram_bank(value as i32);
                gb.memory.rtc_access = false;
            } else if bank <= 0xC {
                gb.memory.active_rtc_reg = bank - 8;
                gb.memory.rtc_access = true;
            }
        }
        0x3 => {
            if gb.memory.rtc_latched && value == 0 {
                gb.memory.rtc_latched = false;
            } else if !gb.memory.rtc_latched && value == 1 {
                let now = gb.unix_time();
                rtc::latch_rtc(now, &mut gb.memory.rtc_regs, &mut gb.memory.rtc_last_latch);
                gb.memory.rtc_latched = true;
            }
        }
        _ => {}
    }
}

/// _GBMBC5
pub fn mbc5(gb: &mut Gb, address: u16, value: u8) {
    match address >> 12 {
        0x0 | 0x1 => match value {
            0 => {
                gb.memory.sram_access = false;
            }
            0xA => {
                gb.memory.sram_access = true;
                let b = gb.memory.sram_current_bank;
                gb.mbc_switch_sram_bank(b);
            }
            _ => {
                // TODO
                mlog!(Level::Stub, rgba_core::log::GB_MBC, "MBC5 unknown value {:02X}", value);
            }
        },
        0x2 => {
            let bank = (gb.memory.current_bank & 0x100) | value as i32;
            gb.mbc_switch_bank(bank);
        }
        0x3 => {
            let bank = (gb.memory.current_bank & 0xFF) | (((value & 1) as i32) << 8);
            gb.mbc_switch_bank(bank);
        }
        0x4 | 0x5 => {
            let mut value = value;
            if gb.memory.mbc_type == MbcType::Mbc5Rumble {
                // The C conditionally calls memory->rumble->setRumble when a
                // rumble callback is installed and integrates lastRumble; this
                // port exposes the state directly (see Memory::rumble_state).
                gb.memory.rumble_state = (value >> 3) & 1 != 0;
                value &= !8;
            }
            gb.mbc_switch_sram_bank((value & 0xF) as i32);
        }
        _ => {
            // TODO
            mlog!(
                Level::Stub,
                rgba_core::log::GB_MBC,
                "MBC5 unknown address: {:04X}:{:02X}",
                address,
                value
            );
        }
    }
}

/// _GBMBC6
pub fn mbc6(gb: &mut Gb, address: u16, value: u8) {
    let bank = value as i32;
    match address >> 10 {
        0 => match value {
            0 => {
                gb.memory.sram_access = false;
            }
            0xA => {
                gb.memory.sram_access = true;
            }
            _ => {
                // TODO
                mlog!(Level::Stub, rgba_core::log::GB_MBC, "MBC6 unknown value {:02X}", value);
            }
        },
        0x1 => {
            gb.mbc_switch_sram_half_bank(0, bank);
        }
        0x2 => {
            gb.mbc_switch_sram_half_bank(1, bank);
        }
        0x3 => {
            mlog!(
                Level::Stub,
                rgba_core::log::GB_MBC,
                "MBC6 unimplemented flash OE write: {:04X}:{:02X}",
                address,
                value
            );
        }
        0x4 => {
            mlog!(
                Level::Stub,
                rgba_core::log::GB_MBC,
                "MBC6 unimplemented flash WE write: {:04X}:{:02X}",
                address,
                value
            );
        }
        0x8 | 0x9 => {
            gb.mbc_switch_half_bank(0, bank);
        }
        0xA | 0xB => {
            mbc6_map_chip(gb, 0, value);
        }
        0xC | 0xD => {
            gb.mbc_switch_half_bank(1, bank);
        }
        0xE | 0xF => {
            mbc6_map_chip(gb, 1, value);
        }
        0x28 | 0x29 | 0x2A | 0x2B => {
            if gb.memory.sram_access {
                let off =
                    gb.memory.sram_bank_off + (address as usize & (GB_SIZE_EXTERNAL_RAM_HALFBANK - 1));
                if let Some(b) = gb.memory.sram.get_mut(off) {
                    *b = value;
                }
                gb.sram_dirty |= M_SAVEDATA_DIRT_NEW;
            }
        }
        0x2C | 0x2D | 0x2E | 0x2F => {
            if gb.memory.sram_access {
                // NB: the C does not mark this save data dirty.
                let off = gb.memory.sram_bank1_off
                    + (address as usize & (GB_SIZE_EXTERNAL_RAM_HALFBANK - 1));
                if let Some(b) = gb.memory.sram.get_mut(off) {
                    *b = value;
                }
            }
        }
        _ => {
            mlog!(
                Level::Stub,
                rgba_core::log::GB_MBC,
                "MBC6 unknown address: {:04X}:{:02X}",
                address,
                value
            );
        }
    }
}

/// _GBMBC6Read
pub fn mbc6_read(gb: &mut Gb, address: u16) -> u8 {
    if !gb.memory.sram_access {
        return 0xFF;
    }
    let off = address as usize & (GB_SIZE_EXTERNAL_RAM_HALFBANK - 1);
    match address >> 12 {
        0xA => gb.memory.sram_byte(gb.memory.sram_bank_off + off),
        0xB => gb.memory.sram_byte(gb.memory.sram_bank1_off + off),
        _ => 0xFF,
    }
}

/// _GBMBC6MapChip
fn mbc6_map_chip(gb: &mut Gb, half: i32, value: u8) {
    if half == 0 {
        gb.memory.mbc_state.mbc6.flash_bank0 = value & 0x08 != 0;
        let bank = gb.memory.current_bank;
        gb.mbc_switch_half_bank(half, bank);
    } else {
        gb.memory.mbc_state.mbc6.flash_bank1 = value & 0x08 != 0;
        let bank = gb.memory.current_bank1;
        gb.mbc_switch_half_bank(half, bank);
    }
}

/// _GBMBC7
pub fn mbc7(gb: &mut Gb, address: u16, value: u8) {
    let bank = (value & 0x7F) as i32;
    match address >> 13 {
        0x0 => match value {
            0xA => {
                gb.memory.mbc_state.mbc7.access |= 1;
            }
            _ => {
                gb.memory.mbc_state.mbc7.access = 0;
            }
        },
        0x1 => {
            gb.mbc_switch_bank(bank);
        }
        0x2 => {
            if value == 0x40 {
                gb.memory.mbc_state.mbc7.access |= 2;
            } else {
                gb.memory.mbc_state.mbc7.access &= !2;
            }
        }
        0x5 => {
            mbc7_write(gb, address, value);
            gb.sram_dirty |= M_SAVEDATA_DIRT_NEW;
        }
        _ => {
            // TODO
            mlog!(
                Level::Stub,
                rgba_core::log::GB_MBC,
                "MBC7 unknown address: {:04X}:{:02X}",
                address,
                value
            );
        }
    }
}

/// _GBMBC7Read (no rotation source in this port: the memory->rotation == NULL
/// branches of the C).
pub fn mbc7_read(gb: &mut Gb, address: u16) -> u8 {
    if gb.memory.mbc_state.mbc7.access != 3 {
        return 0xFF;
    }
    match address & 0xF0 {
        0x20 => 0xFF,
        0x30 => 7,
        0x40 => 0xFF,
        0x50 => 7,
        0x60 => 0,
        0x80 => gb.memory.mbc_state.mbc7.eeprom,
        _ => 0xFF,
    }
}

/// _GBMBC7Write (the EEPROM serial FSM)
fn mbc7_write(gb: &mut Gb, address: u16, value: u8) {
    let mut st = &mut gb.memory.mbc_state.mbc7;
    if st.access != 3 {
        return;
    }
    match address & 0xF0 {
        0x00 => {
            st.latch = ((value & 0x55) == 0x55) as u8;
            return;
        }
        0x10 => {
            st.latch |= value & 0xAA;
            // (no rotation source to sample in this port)
            st.latch = 0;
            return;
        }
        0x80 => {}
        _ => {
            mlog!(
                Level::Stub,
                rgba_core::log::GB_MBC,
                "MBC7 unknown register: {:04X}:{:02X}",
                address,
                value
            );
            return;
        }
    }
    let old = st.eeprom;
    let mut value = value | MBC7_FIELD_DO; // Hi-Z
    if old & MBC7_FIELD_CS == 0 && value & MBC7_FIELD_CS != 0 {
        st.state = MBC7_STATE_IDLE;
    }
    if old & MBC7_FIELD_CLK == 0 && value & MBC7_FIELD_CLK != 0 {
        if st.state == MBC7_STATE_READ_COMMAND
            || st.state == MBC7_STATE_EEPROM_WRITE
            || st.state == MBC7_STATE_EEPROM_WRAL
        {
            st.sr <<= 1;
            st.sr |= ((value & MBC7_FIELD_DI) >> 1) as u16;
            st.sr_bits += 1;
        }
        match st.state {
            MBC7_STATE_IDLE => {
                if value & MBC7_FIELD_DI != 0 {
                    st.state = MBC7_STATE_READ_COMMAND;
                    st.sr_bits = 0;
                    st.sr = 0;
                }
            }
            MBC7_STATE_READ_COMMAND => {
                if st.sr_bits == 10 {
                    st.state = 0x10 | (st.sr >> 6) as i32;
                    if st.state & 0xC != 0 {
                        st.state &= !0x3;
                    }
                    st.sr_bits = 0;
                    st.address = (st.sr & 0x7F) as u8;
                }
            }
            MBC7_STATE_DO => {
                value = (value & !MBC7_FIELD_DO) | ((st.sr >> 15) as u8 & 1);
                st.sr <<= 1;
                st.sr_bits -= 1;
                if st.sr_bits == 0 {
                    st.state = MBC7_STATE_IDLE;
                }
            }
            _ => {}
        }
        match st.state {
            MBC7_STATE_EEPROM_EWEN => {
                st.writable = true;
                st.state = MBC7_STATE_IDLE;
            }
            MBC7_STATE_EEPROM_EWDS => {
                st.writable = false;
                st.state = MBC7_STATE_IDLE;
            }
            MBC7_STATE_EEPROM_WRITE => {
                if st.sr_bits == 16 {
                    if st.writable {
                        let addr = st.address as usize * 2;
                        // MBC7 forces a 0x100-byte SRAM; guard to stay panic-free.
                        if addr + 1 < gb.memory.sram.len() {
                            gb.memory.sram[addr] = (st.sr >> 8) as u8;
                            gb.memory.sram[addr + 1] = st.sr as u8;
                        }
                    }
                    st.state = MBC7_STATE_IDLE;
                }
            }
            MBC7_STATE_EEPROM_ERASE => {
                if st.writable {
                    let addr = st.address as usize * 2;
                    if addr + 1 < gb.memory.sram.len() {
                        gb.memory.sram[addr] = 0xFF;
                        gb.memory.sram[addr + 1] = 0xFF;
                    }
                }
                st.state = MBC7_STATE_IDLE;
            }
            MBC7_STATE_EEPROM_READ => {
                let addr = st.address as usize * 2;
                let hi = gb.memory.sram_byte(addr) as u16;
                let lo = gb.memory.sram_byte(addr + 1) as u16;
                let sr = hi << 8 | lo;
                let st2 = &mut gb.memory.mbc_state.mbc7;
                st2.sr_bits = 16;
                st2.sr = sr;
                st2.state = MBC7_STATE_DO;
                value &= !MBC7_FIELD_DO;
                st = st2;
            }
            MBC7_STATE_EEPROM_WRAL => {
                if st.sr_bits == 16 {
                    if st.writable {
                        let n = gb.memory.sram.len().min(256);
                        let hi = (st.sr >> 8) as u8;
                        let lo = st.sr as u8;
                        for i in (0..n).step_by(2) {
                            gb.memory.sram[i] = hi;
                            gb.memory.sram[i + 1] = lo;
                        }
                    }
                    st.state = MBC7_STATE_IDLE;
                }
            }
            MBC7_STATE_EEPROM_ERAL => {
                if st.writable {
                    let n = gb.memory.sram.len().min(256);
                    for i in (0..n).step_by(2) {
                        gb.memory.sram[i] = 0xFF;
                        gb.memory.sram[i + 1] = 0xFF;
                    }
                }
                st.state = MBC7_STATE_IDLE;
            }
            _ => {}
        }
    } else if value & MBC7_FIELD_CS != 0 && old & MBC7_FIELD_CLK != 0 && value & MBC7_FIELD_CLK == 0
    {
        value = (value & !MBC7_FIELD_DO) | (old & MBC7_FIELD_DO);
    }
    gb.memory.mbc_state.mbc7.eeprom = value;
}

/// _GBMMM01
pub fn mmm01(gb: &mut Gb, address: u16, value: u8) {
    if !gb.memory.mbc_state.mmm01.locked {
        match address >> 13 {
            0x0 => {
                gb.memory.mbc_state.mmm01.locked = true;
                let b = gb.memory.mbc_state.mmm01.current_bank0;
                gb.mbc_switch_bank0(b);
            }
            0x1 => {
                gb.memory.mbc_state.mmm01.current_bank0 &= !0x7F;
                gb.memory.mbc_state.mmm01.current_bank0 |= (value & 0x7F) as i32;
            }
            0x2 => {
                gb.memory.mbc_state.mmm01.current_bank0 &= !0x180;
                gb.memory.mbc_state.mmm01.current_bank0 |= ((value & 0x30) as i32) << 3;
            }
            _ => {
                // TODO
                mlog!(
                    Level::Stub,
                    rgba_core::log::GB_MBC,
                    "MMM01 unknown address: {:04X}:{:02X}",
                    address,
                    value
                );
            }
        }
        return;
    }
    match address >> 13 {
        0x0 => {
            if value == 0xA {
                gb.memory.sram_access = true;
                let b = gb.memory.sram_current_bank;
                gb.mbc_switch_sram_bank(b);
            } else {
                gb.memory.sram_access = false;
            }
        }
        0x1 => {
            let b = gb.memory.mbc_state.mmm01.current_bank0;
            gb.mbc_switch_bank(value as i32 + b);
        }
        0x2 => {
            gb.mbc_switch_sram_bank(value as i32);
        }
        _ => {
            // TODO
            mlog!(
                Level::Stub,
                rgba_core::log::GB_MBC,
                "MMM01 unknown address: {:04X}:{:02X}",
                address,
                value
            );
        }
    }
}

/// _GBHuC1
pub fn huc1(gb: &mut Gb, address: u16, value: u8) {
    let bank = (value & 0x3F) as i32;
    match address >> 13 {
        0x0 => {
            if value == 0xE {
                gb.memory.sram_access = false;
            } else {
                gb.memory.sram_access = true;
                let b = gb.memory.sram_current_bank;
                gb.mbc_switch_sram_bank(b);
            }
        }
        0x1 => {
            gb.mbc_switch_bank(bank);
        }
        0x2 => {
            gb.mbc_switch_sram_bank(value as i32);
        }
        _ => {
            // TODO
            mlog!(
                Level::Stub,
                rgba_core::log::GB_MBC,
                "HuC-1 unknown address: {:04X}:{:02X}",
                address,
                value
            );
        }
    }
}

