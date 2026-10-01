// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gb/mbc/huc-3.c (HuC-3 with IR + RTC).

use rgba_core::mlog;
use rgba_core::Level;

use crate::gb::Gb;
use crate::memory::GB_SIZE_EXTERNAL_RAM;

// enum GBHuC3Register
pub const GBHUC3_RTC_MINUTES_LO: usize = 0x10;
pub const GBHUC3_RTC_MINUTES_MI: usize = 0x11;
pub const GBHUC3_RTC_MINUTES_HI: usize = 0x12;
pub const GBHUC3_RTC_DAYS_LO: usize = 0x13;
pub const GBHUC3_RTC_DAYS_MI: usize = 0x14;
pub const GBHUC3_RTC_DAYS_HI: usize = 0x15;
pub const GBHUC3_SPEAKER_TONE: usize = 0x26;
pub const GBHUC3_SPEAKER_ENABLE: usize = 0x27;

// enum GBHuC3Mode
pub const GBHUC3_MODE_SRAM_RO: u8 = 0x0;
pub const GBHUC3_MODE_SRAM_RW: u8 = 0xA;
pub const GBHUC3_MODE_IN: u8 = 0xB;
pub const GBHUC3_MODE_OUT: u8 = 0xC;
pub const GBHUC3_MODE_COMMIT: u8 = 0xD;

// enum GBHuC3Command
pub const GBHUC3_CMD_LATCH: u8 = 0x0;
pub const GBHUC3_CMD_SET_RTC: u8 = 0x1;
pub const GBHUC3_CMD_RO: u8 = 0x2;
pub const GBHUC3_CMD_TONE: u8 = 0xE;

#[derive(Clone)]
pub struct HuC3State {
    pub index: u8,
    pub value: u8,
    pub mode: u8,
    pub registers: [u8; 256],
}

/// _latchHuC3Rtc (RTC source replaced by the unix timestamp `now`).
pub(crate) fn latch_huc3_rtc(now: i64, regs: &mut [u8; 256], rtc_last_latch: &mut i64) {
    let mut t = now;
    t -= *rtc_last_latch;
    t /= 60;

    if t == 0 {
        return;
    }
    *rtc_last_latch += t * 60;

    let mut minutes: i64 = (regs[GBHUC3_RTC_MINUTES_HI] as i64) << 8;
    minutes |= (regs[GBHUC3_RTC_MINUTES_MI] as i64) << 4;
    minutes |= regs[GBHUC3_RTC_MINUTES_LO] as i64;
    minutes += t % 1440;
    t /= 1440;
    if minutes >= 1440 {
        minutes -= 1440;
        t += 1;
    } else if minutes < 0 {
        minutes += 1440;
        t -= 1;
    }
    regs[GBHUC3_RTC_MINUTES_LO] = (minutes & 0xF) as u8;
    regs[GBHUC3_RTC_MINUTES_MI] = ((minutes >> 4) & 0xF) as u8;
    regs[GBHUC3_RTC_MINUTES_HI] = ((minutes >> 8) & 0xF) as u8;

    let mut days: i64 = regs[GBHUC3_RTC_DAYS_LO] as i64;
    days |= (regs[GBHUC3_RTC_DAYS_MI] as i64) << 4;
    days |= (regs[GBHUC3_RTC_DAYS_HI] as i64) << 8;

    days += t;

    regs[GBHUC3_RTC_DAYS_LO] = (days & 0xF) as u8;
    regs[GBHUC3_RTC_DAYS_MI] = ((days >> 4) & 0xF) as u8;
    regs[GBHUC3_RTC_DAYS_HI] = ((days >> 8) & 0xF) as u8;
}

/// _huc3Commit
fn huc3_commit(gb: &mut Gb) {
    let value = gb.memory.mbc_state.huc3.value;
    match value & 0x70 {
        0x10 => {
            let index = gb.memory.mbc_state.huc3.index;
            if index & 0xF8 == 0x10 {
                let now = gb.unix_time();
                latch_huc3_rtc(
                    now,
                    &mut gb.memory.mbc_state.huc3.registers,
                    &mut gb.memory.rtc_last_latch,
                );
            }
            let st = &mut gb.memory.mbc_state.huc3;
            st.value &= 0xF0;
            st.value |= st.registers[st.index as usize] & 0xF;
            mlog!(
                Level::Debug,
                rgba_core::log::GB_MBC,
                "HuC-3 read: {:02X}:{:X}",
                st.index,
                st.value & 0xF
            );
            if st.value & 0x10 != 0 {
                st.index = st.index.wrapping_add(1);
            }
        }
        0x30 => {
            let st = &mut gb.memory.mbc_state.huc3;
            mlog!(
                Level::Debug,
                rgba_core::log::GB_MBC,
                "HuC-3 write: {:02X}:{:X}",
                st.index,
                st.value & 0xF
            );
            st.registers[st.index as usize] = st.value & 0xF;
            if st.value & 0x10 != 0 {
                st.index = st.index.wrapping_add(1);
            }
        }
        0x40 => {
            let st = &mut gb.memory.mbc_state.huc3;
            st.index &= 0xF0;
            st.index |= st.value & 0xF;
            mlog!(Level::Debug, rgba_core::log::GB_MBC, "HuC-3 index (low): {:02X}", st.index);
        }
        0x50 => {
            let st = &mut gb.memory.mbc_state.huc3;
            st.index &= 0x0F;
            st.index |= (st.value & 0xF) << 4;
            mlog!(Level::Debug, rgba_core::log::GB_MBC, "HuC-3 index (high): {:02X}", st.index);
        }
        0x60 => {
            match value & 0xF {
                GBHUC3_CMD_LATCH => {
                    let now = gb.unix_time();
                    latch_huc3_rtc(
                        now,
                        &mut gb.memory.mbc_state.huc3.registers,
                        &mut gb.memory.rtc_last_latch,
                    );
                    let st = &mut gb.memory.mbc_state.huc3;
                    st.registers.copy_within(GBHUC3_RTC_MINUTES_LO..GBHUC3_RTC_MINUTES_LO + 6, 0);
                    mlog!(Level::Debug, rgba_core::log::GB_MBC, "HuC-3 RTC latch");
                }
                GBHUC3_CMD_SET_RTC => {
                    let st = &mut gb.memory.mbc_state.huc3;
                    st.registers.copy_within(0..6, GBHUC3_RTC_MINUTES_LO);
                    mlog!(Level::Debug, rgba_core::log::GB_MBC, "HuC-3 set RTC");
                }
                GBHUC3_CMD_RO => {
                    mlog!(Level::Stub, rgba_core::log::GB_MBC, "HuC-3 unimplemented read-only mode");
                }
                GBHUC3_CMD_TONE => {
                    let huc3 = &gb.memory.mbc_state.huc3;
                    if huc3.registers[GBHUC3_SPEAKER_ENABLE] == 1 {
                        // mCALLBACKS_INVOKE(gb, alarm): no alarm callback in
                        // this port.
                        mlog!(
                            Level::Debug,
                            rgba_core::log::GB_MBC,
                            "HuC-3 tone {}",
                            huc3.registers[GBHUC3_SPEAKER_TONE] & 3
                        );
                    }
                }
                _ => {
                    mlog!(
                        Level::Stub,
                        rgba_core::log::GB_MBC,
                        "HuC-3 unknown command: {:X}",
                        value & 0xF
                    );
                }
            }
            gb.memory.mbc_state.huc3.value = 0xE1;
        }
        _ => {
            mlog!(
                Level::Stub,
                rgba_core::log::GB_MBC,
                "HuC-3 unknown mode commit: {:02X}:{:02X}",
                gb.memory.mbc_state.huc3.index,
                value
            );
        }
    }
}

/// _GBHuC3
pub fn huc3(gb: &mut Gb, address: u16, value: u8) {
    let bank = (value & 0x7F) as i32;
    if address & 0x1FFF != 0 {
        mlog!(
            Level::Stub,
            rgba_core::log::GB_MBC,
            "HuC-3 unknown value {:04X}:{:02X}",
            address,
            value
        );
    }

    match address >> 13 {
        0x0 => {
            match value {
                0xA => {
                    gb.memory.sram_access = true;
                    let b = gb.memory.sram_current_bank;
                    gb.mbc_switch_sram_bank(b);
                }
                _ => {
                    gb.memory.sram_access = false;
                }
            }
            gb.memory.mbc_state.huc3.mode = value;
        }
        0x1 => {
            gb.mbc_switch_bank(bank);
        }
        0x2 => {
            gb.mbc_switch_sram_bank(bank);
        }
        0x5 => match gb.memory.mbc_state.huc3.mode {
            GBHUC3_MODE_IN => {
                gb.memory.mbc_state.huc3.value = 0x80 | value;
            }
            GBHUC3_MODE_COMMIT => {
                huc3_commit(gb);
            }
            _ => {
                mlog!(
                    Level::Stub,
                    rgba_core::log::GB_MBC,
                    "HuC-3 unknown mode write: {:02X}:{:02X}",
                    gb.memory.mbc_state.huc3.mode,
                    value
                );
            }
        },
        _ => {
            // TODO
            mlog!(
                Level::Stub,
                rgba_core::log::GB_MBC,
                "HuC-3 unknown address: {:04X}:{:02X}",
                address,
                value
            );
        }
    }
}

/// _GBHuC3Read
pub fn huc3_read(gb: &mut Gb, address: u16) -> u8 {
    match gb.memory.mbc_state.huc3.mode {
        GBHUC3_MODE_SRAM_RO | GBHUC3_MODE_SRAM_RW => gb
            .memory
            .sram_byte(gb.memory.sram_bank_off + (address as usize & (GB_SIZE_EXTERNAL_RAM - 1))),
        GBHUC3_MODE_IN | GBHUC3_MODE_OUT => 0x80 | gb.memory.mbc_state.huc3.value,
        _ => 0xFF,
    }
}

impl Default for HuC3State {
    fn default() -> Self {
        HuC3State { index: 0, value: 0, mode: 0, registers: [0; 256] }
    }
}
