// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gb/mbc/tama5.c (TAMA5 with TAMA6 RTC).

use rgba_core::mlog;
use rgba_core::Level;

use crate::gb::Gb;
use crate::memory::M_SAVEDATA_DIRT_NEW;

// enum GBTAMA5Register
pub const GBTAMA5_BANK_LO: u8 = 0x0;
pub const GBTAMA5_BANK_HI: u8 = 0x1;
pub const GBTAMA5_WRITE_LO: u8 = 0x4;
pub const GBTAMA5_WRITE_HI: u8 = 0x5;
pub const GBTAMA5_ADDR_HI: u8 = 0x6;
pub const GBTAMA5_ADDR_LO: u8 = 0x7;
pub const GBTAMA5_MAX: u8 = 0x8;
pub const GBTAMA5_ACTIVE: u8 = 0xA;
pub const GBTAMA5_READ_LO: u8 = 0xC;
pub const GBTAMA5_READ_HI: u8 = 0xD;

// enum GBTAMA6RTCRegister
pub const GBTAMA6_RTC_PA0_SECOND_1: usize = 0x0;
pub const GBTAMA6_RTC_PA0_SECOND_10: usize = 0x1;
pub const GBTAMA6_RTC_PA0_MINUTE_1: usize = 0x2;
pub const GBTAMA6_RTC_PA0_MINUTE_10: usize = 0x3;
pub const GBTAMA6_RTC_PA0_HOUR_1: usize = 0x4;
pub const GBTAMA6_RTC_PA0_HOUR_10: usize = 0x5;
pub const GBTAMA6_RTC_PA0_WEEK: usize = 0x6;
pub const GBTAMA6_RTC_PA0_DAY_1: usize = 0x7;
pub const GBTAMA6_RTC_PA0_DAY_10: usize = 0x8;
pub const GBTAMA6_RTC_PA0_MONTH_1: usize = 0x9;
pub const GBTAMA6_RTC_PA0_MONTH_10: usize = 0xA;
pub const GBTAMA6_RTC_PA0_YEAR_1: usize = 0xB;
pub const GBTAMA6_RTC_PA0_YEAR_10: usize = 0xC;
pub const GBTAMA6_RTC_PA1_24_HOUR: usize = 0xA;
pub const GBTAMA6_RTC_PA1_LEAP_YEAR: usize = 0xB;
pub const GBTAMA6_RTC_PAGE: usize = 0xD;

// enum GBTAMA6Command
pub const GBTAMA6_DISABLE_TIMER: u8 = 0x0;
pub const GBTAMA6_ENABLE_TIMER: u8 = 0x1;
pub const GBTAMA6_MINUTE_WRITE: u8 = 0x4;
pub const GBTAMA6_HOUR_WRITE: u8 = 0x5;
pub const GBTAMA6_MINUTE_READ: u8 = 0x6;
pub const GBTAMA6_HOUR_READ: u8 = 0x7;
pub const GBTAMA6_DISABLE_ALARM: u8 = 0x10;
pub const GBTAMA6_ENABLE_ALARM: u8 = 0x11;

#[derive(Default)]
pub struct Tama5State {
    pub reg: u8,
    pub disabled: bool,
    pub registers: [u8; 8],
    pub rtc_timer_page: [u8; 16],
    pub rtc_alarm_page: [u8; 16],
    pub rtc_free_page0: [u8; 16],
    pub rtc_free_page1: [u8; 16],
}

const TAMA6_RTC_MASK: [u8; 32] = [
    //0    1    2    3    4    5    6    7    8    9    A    B    C    D    E    F
    0xF, 0x7, 0xF, 0x7, 0xF, 0x3, 0x7, 0xF, 0x3, 0xF, 0x1, 0xF, 0xF, 0x0, 0x0, 0x0, //
    0x0, 0x0, 0xF, 0x7, 0xF, 0x3, 0x7, 0xF, 0x3, 0x0, 0x1, 0x3, 0x0, 0x0, 0x0, 0x0,
];

/// _daysToMonth; index 0 is unused, like the C designated initializers.
const DAYS_TO_MONTH: [i32; 13] = [
    0,
    0,
    31,
    31 + 28,
    31 + 28 + 31,
    31 + 28 + 31 + 30,
    31 + 28 + 31 + 30 + 31,
    31 + 28 + 31 + 30 + 31 + 30,
    31 + 28 + 31 + 30 + 31 + 30 + 31,
    31 + 28 + 31 + 30 + 31 + 30 + 31 + 31,
    31 + 28 + 31 + 30 + 31 + 30 + 31 + 31 + 30,
    31 + 28 + 31 + 30 + 31 + 30 + 31 + 31 + 30 + 31,
    31 + 28 + 31 + 30 + 31 + 30 + 31 + 31 + 30 + 31 + 30,
];

/// _tama6DMYToDayOfYear
fn tama6_dmy_to_day_of_year(mut day: i32, month: i32, year: i32) -> i32 {
    if !(1..=12).contains(&month) {
        return -1;
    }
    day += DAYS_TO_MONTH[month as usize];
    if month > 2 && year & 3 == 0 {
        day += 1;
    }
    day
}

/// _tama6DayOfYearToMonth
fn tama6_day_of_year_to_month(mut day: i32, year: i32) -> i32 {
    let mut month = 1;
    while month < 12 {
        if day <= DAYS_TO_MONTH[(month + 1) as usize] {
            return month;
        }
        if month == 2 && year & 3 == 0 {
            if day == 60 {
                return 2;
            }
            day -= 1;
        }
        month += 1;
    }
    12
}

/// _tama6DayOfYearToDayOfMonth
fn tama6_day_of_year_to_day_of_month(mut day: i32, year: i32) -> i32 {
    let mut month = 1;
    while month < 12 {
        if day <= DAYS_TO_MONTH[(month + 1) as usize] {
            return day - DAYS_TO_MONTH[month as usize];
        }
        if month == 2 && year & 3 == 0 {
            if day == 60 {
                return 29;
            }
            day -= 1;
        }
        month += 1;
    }
    day - DAYS_TO_MONTH[12]
}

/// _latchTAMA6Rtc (RTC source replaced by the unix timestamp `now`).
pub(crate) fn latch_tama6_rtc(now: i64, tama5: &mut Tama5State, rtc_last_latch: &mut i64) {
    let mut t = now;
    let current_latch = t;
    t -= *rtc_last_latch;
    *rtc_last_latch = current_latch;
    if t == 0 || tama5.disabled {
        return;
    }

    let timer = &mut tama5.rtc_timer_page;
    let is24hour = tama5.rtc_alarm_page[GBTAMA6_RTC_PA1_24_HOUR] != 0;

    let mut diff: i64;
    diff = timer[GBTAMA6_RTC_PA0_SECOND_1] as i64
        + timer[GBTAMA6_RTC_PA0_SECOND_10] as i64 * 10
        + t % 60;
    if diff < 0 {
        diff += 60;
        t -= 60;
    }
    timer[GBTAMA6_RTC_PA0_SECOND_1] = (diff % 10) as u8;
    timer[GBTAMA6_RTC_PA0_SECOND_10] = ((diff % 60) / 10) as u8;
    t /= 60;
    t += diff / 60;

    diff = timer[GBTAMA6_RTC_PA0_MINUTE_1] as i64
        + timer[GBTAMA6_RTC_PA0_MINUTE_10] as i64 * 10
        + t % 60;
    if diff < 0 {
        diff += 60;
        t -= 60;
    }
    timer[GBTAMA6_RTC_PA0_MINUTE_1] = (diff % 10) as u8;
    timer[GBTAMA6_RTC_PA0_MINUTE_10] = ((diff % 60) / 10) as u8;
    t /= 60;
    t += diff / 60;

    diff = timer[GBTAMA6_RTC_PA0_HOUR_1] as i64;
    if is24hour {
        diff += timer[GBTAMA6_RTC_PA0_HOUR_10] as i64 * 10;
    } else {
        let hour10 = timer[GBTAMA6_RTC_PA0_HOUR_10] as i64;
        diff += (hour10 & 1) * 10;
        diff += (hour10 & 2) * 12;
    }
    diff += t % 24;
    if diff < 0 {
        diff += 24;
        t -= 24;
    }
    if is24hour {
        timer[GBTAMA6_RTC_PA0_HOUR_1] = ((diff % 24) % 10) as u8;
        timer[GBTAMA6_RTC_PA0_HOUR_10] = ((diff % 24) / 10) as u8;
    } else {
        timer[GBTAMA6_RTC_PA0_HOUR_1] = ((diff % 12) % 10) as u8;
        timer[GBTAMA6_RTC_PA0_HOUR_10] = ((diff % 12) / 10 + (diff / 12) * 2) as u8;
    }
    t /= 24;
    t += diff / 24;

    let mut year = timer[GBTAMA6_RTC_PA0_YEAR_1] as i32 + timer[GBTAMA6_RTC_PA0_YEAR_10] as i32 * 10;
    let day = timer[GBTAMA6_RTC_PA0_DAY_1] as i32 + timer[GBTAMA6_RTC_PA0_DAY_10] as i32 * 10;
    let month = timer[GBTAMA6_RTC_PA0_MONTH_1] as i32 + timer[GBTAMA6_RTC_PA0_MONTH_10] as i32 * 10;
    let mut leap_year = tama5.rtc_alarm_page[GBTAMA6_RTC_PA1_LEAP_YEAR] as i32;
    let mut day_of_week = timer[GBTAMA6_RTC_PA0_WEEK] as i64;
    let day_in_year = tama6_dmy_to_day_of_year(day, month, leap_year) as i64;
    diff = day_in_year + t;
    while diff <= 0 {
        // Previous year
        if leap_year & 3 != 0 {
            diff += 365;
        } else {
            diff += 366;
        }
        year -= 1;
        leap_year -= 1;
    }
    while diff > if leap_year & 3 != 0 { 365 } else { 366 } {
        // Future year
        if year % 4 != 0 {
            diff -= 365;
        } else {
            diff -= 366;
        }
        year += 1;
        leap_year += 1;
    }
    day_of_week = (day_of_week + diff) % 7;
    year %= 100;
    leap_year &= 3;

    let day = tama6_day_of_year_to_day_of_month(diff as i32, leap_year);
    let month = tama6_day_of_year_to_month(diff as i32, leap_year);

    timer[GBTAMA6_RTC_PA0_WEEK] = day_of_week as u8;
    tama5.rtc_alarm_page[GBTAMA6_RTC_PA1_LEAP_YEAR] = leap_year as u8;

    timer[GBTAMA6_RTC_PA0_DAY_1] = (day % 10) as u8;
    timer[GBTAMA6_RTC_PA0_DAY_10] = (day / 10) as u8;

    timer[GBTAMA6_RTC_PA0_MONTH_1] = (month % 10) as u8;
    timer[GBTAMA6_RTC_PA0_MONTH_10] = (month / 10) as u8;

    timer[GBTAMA6_RTC_PA0_YEAR_1] = (year % 10) as u8;
    timer[GBTAMA6_RTC_PA0_YEAR_10] = (year / 10) as u8;
}

/// _GBTAMA5
pub fn tama5(gb: &mut Gb, address: u16, value: u8) {
    match address >> 13 {
        0x5 => {
            if address & 1 != 0 {
                gb.memory.mbc_state.tama5.reg = value;
                return;
            }
            let value = value & 0xF;
            let reg = gb.memory.mbc_state.tama5.reg;
            if reg < GBTAMA5_MAX {
                mlog!(Level::Debug, rgba_core::log::GB_MBC, "TAMA5 write: {:02X}:{:X}", reg, value);
                gb.memory.mbc_state.tama5.registers[reg as usize] = value;
                let tama_addr: u8 = ((gb.memory.mbc_state.tama5.registers[GBTAMA5_ADDR_HI as usize]
                    << 4)
                    & 0x10)
                    | gb.memory.mbc_state.tama5.registers[GBTAMA5_ADDR_LO as usize];
                let out: u8 = (gb.memory.mbc_state.tama5.registers[GBTAMA5_WRITE_HI as usize] << 4)
                    | gb.memory.mbc_state.tama5.registers[GBTAMA5_WRITE_LO as usize];
                match reg {
                    GBTAMA5_BANK_LO | GBTAMA5_BANK_HI => {
                        let bank = gb.memory.mbc_state.tama5.registers[GBTAMA5_BANK_LO as usize]
                            as i32
                            | (gb.memory.mbc_state.tama5.registers[GBTAMA5_BANK_HI as usize] as i32)
                                << 4;
                        gb.mbc_switch_bank(bank);
                    }
                    GBTAMA5_WRITE_LO | GBTAMA5_WRITE_HI | GBTAMA5_ADDR_HI => {}
                    GBTAMA5_ADDR_LO => {
                        match gb.memory.mbc_state.tama5.registers[GBTAMA5_ADDR_HI as usize] >> 1 {
                            0x0 => {
                                // RAM write (TAMA5 forces a 0x20-byte SRAM)
                                if let Some(b) = gb.memory.sram.get_mut(tama_addr as usize) {
                                    *b = out;
                                }
                                gb.sram_dirty |= M_SAVEDATA_DIRT_NEW;
                            }
                            0x1 => {
                                // RAM read
                            }
                            0x2 => {
                                // Other commands
                                let st = &mut gb.memory.mbc_state.tama5;
                                match tama_addr {
                                    GBTAMA6_DISABLE_TIMER => {
                                        st.disabled = true;
                                        st.rtc_timer_page[GBTAMA6_RTC_PAGE] &= 0x7;
                                        st.rtc_alarm_page[GBTAMA6_RTC_PAGE] &= 0x7;
                                        st.rtc_free_page0[GBTAMA6_RTC_PAGE] &= 0x7;
                                        st.rtc_free_page1[GBTAMA6_RTC_PAGE] &= 0x7;
                                    }
                                    GBTAMA6_ENABLE_TIMER => {
                                        st.disabled = false;
                                        st.rtc_timer_page[GBTAMA6_RTC_PA0_SECOND_1] = 0;
                                        st.rtc_timer_page[GBTAMA6_RTC_PA0_SECOND_10] = 0;
                                        st.rtc_timer_page[GBTAMA6_RTC_PAGE] |= 0x8;
                                        st.rtc_alarm_page[GBTAMA6_RTC_PAGE] |= 0x8;
                                        st.rtc_free_page0[GBTAMA6_RTC_PAGE] |= 0x8;
                                        st.rtc_free_page1[GBTAMA6_RTC_PAGE] |= 0x8;
                                    }
                                    GBTAMA6_MINUTE_WRITE => {
                                        st.rtc_timer_page[GBTAMA6_RTC_PA0_MINUTE_1] = out & 0xF;
                                        st.rtc_timer_page[GBTAMA6_RTC_PA0_MINUTE_10] = out >> 4;
                                    }
                                    GBTAMA6_HOUR_WRITE => {
                                        st.rtc_timer_page[GBTAMA6_RTC_PA0_HOUR_1] = out & 0xF;
                                        st.rtc_timer_page[GBTAMA6_RTC_PA0_HOUR_10] = out >> 4;
                                    }
                                    GBTAMA6_DISABLE_ALARM => {
                                        st.rtc_timer_page[GBTAMA6_RTC_PAGE] &= 0xB;
                                        st.rtc_alarm_page[GBTAMA6_RTC_PAGE] &= 0xB;
                                        st.rtc_free_page0[GBTAMA6_RTC_PAGE] &= 0xB;
                                        st.rtc_free_page1[GBTAMA6_RTC_PAGE] &= 0xB;
                                    }
                                    GBTAMA6_ENABLE_ALARM => {
                                        st.rtc_timer_page[GBTAMA6_RTC_PAGE] |= 0x4;
                                        st.rtc_alarm_page[GBTAMA6_RTC_PAGE] |= 0x4;
                                        st.rtc_free_page0[GBTAMA6_RTC_PAGE] |= 0x4;
                                        st.rtc_free_page1[GBTAMA6_RTC_PAGE] |= 0x4;
                                    }
                                    _ => {}
                                }
                            }
                            0x4 => {
                                // RTC access
                                let st = &mut gb.memory.mbc_state.tama5;
                                let rtc_addr = st.registers[GBTAMA5_WRITE_LO as usize];
                                if rtc_addr < GBTAMA6_RTC_PAGE as u8 {
                                    let out2 = st.registers[GBTAMA5_WRITE_HI as usize];
                                    match st.registers[GBTAMA5_ADDR_LO as usize] {
                                        0 => {
                                            let m = TAMA6_RTC_MASK[rtc_addr as usize];
                                            st.rtc_timer_page[rtc_addr as usize] = out2 & m;
                                        }
                                        2 => {
                                            let m = TAMA6_RTC_MASK[rtc_addr as usize | 0x10];
                                            st.rtc_alarm_page[rtc_addr as usize] = out2 & m;
                                        }
                                        4 => {
                                            st.rtc_free_page0[rtc_addr as usize] = out2;
                                        }
                                        6 => {
                                            st.rtc_free_page1[rtc_addr as usize] = out2;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            _ => {
                                mlog!(
                                    Level::Stub,
                                    rgba_core::log::GB_MBC,
                                    "TAMA5 unknown address: {:02X}:{:02X}",
                                    tama_addr,
                                    out
                                );
                            }
                        }
                    }
                    _ => {
                        mlog!(
                            Level::Stub,
                            rgba_core::log::GB_MBC,
                            "TAMA5 unknown write: {:02X}:{:X}",
                            reg,
                            value
                        );
                    }
                }
            } else {
                mlog!(Level::Stub, rgba_core::log::GB_MBC, "TAMA5 unknown write: {:02X}", reg);
            }
        }
        _ => {
            mlog!(
                Level::Stub,
                rgba_core::log::GB_MBC,
                "TAMA5 unknown address: {:04X}:{:02X}",
                address,
                value
            );
        }
    }
}

/// _GBTAMA5Read
pub fn tama5_read(gb: &mut Gb, address: u16) -> u8 {
    if address & 0x1FFF > 1 {
        mlog!(
            Level::Stub,
            rgba_core::log::GB_MBC,
            "TAMA5 unknown address: {:04X}",
            address
        );
    }
    if address & 1 != 0 {
        return 0xFF;
    }
    let mut value: u8 = 0xF0;
    let reg = gb.memory.mbc_state.tama5.reg;
    let addr_byte: u8 = ((gb.memory.mbc_state.tama5.registers[GBTAMA5_ADDR_HI as usize] << 4) & 0x10)
        | gb.memory.mbc_state.tama5.registers[GBTAMA5_ADDR_LO as usize];
    match reg {
        GBTAMA5_ACTIVE => 0xF1,
        GBTAMA5_READ_LO | GBTAMA5_READ_HI => {
            match gb.memory.mbc_state.tama5.registers[GBTAMA5_ADDR_HI as usize] >> 1 {
                0x1 => {
                    value = gb.memory.sram_byte(addr_byte as usize);
                }
                0x2 => {
                    mlog!(
                        Level::Stub,
                        rgba_core::log::GB_MBC,
                        "TAMA5 unknown read {}: {:02X}",
                        if reg == GBTAMA5_READ_HI { "hi" } else { "lo" },
                        addr_byte
                    );
                    let now = gb.unix_time();
                    latch_tama6_rtc(
                        now,
                        &mut gb.memory.mbc_state.tama5,
                        &mut gb.memory.rtc_last_latch,
                    );
                    let timer = &gb.memory.mbc_state.tama5.rtc_timer_page;
                    match addr_byte {
                        GBTAMA6_MINUTE_READ => {
                            value = (timer[GBTAMA6_RTC_PA0_MINUTE_10] << 4)
                                | timer[GBTAMA6_RTC_PA0_MINUTE_1];
                        }
                        GBTAMA6_HOUR_READ => {
                            value = (timer[GBTAMA6_RTC_PA0_HOUR_10] << 4)
                                | timer[GBTAMA6_RTC_PA0_HOUR_1];
                        }
                        _ => {
                            value = addr_byte;
                        }
                    }
                }
                0x4 => {
                    if reg == GBTAMA5_READ_HI {
                        mlog!(
                            Level::GameError,
                            rgba_core::log::GB_MBC,
                            "TAMA5 reading RTC incorrectly"
                        );
                        // break: value stays 0xF0
                    } else {
                        let now = gb.unix_time();
                        latch_tama6_rtc(
                            now,
                            &mut gb.memory.mbc_state.tama5,
                            &mut gb.memory.rtc_last_latch,
                        );
                        let st = &gb.memory.mbc_state.tama5;
                        let rtc_addr = st.registers[GBTAMA5_WRITE_LO as usize];
                        if rtc_addr > GBTAMA6_RTC_PAGE as u8 {
                            value = 0;
                        } else {
                            match st.registers[GBTAMA5_ADDR_LO as usize] {
                                1 | 3 | 5 | 7 => {
                                    value = st.rtc_timer_page[rtc_addr as usize];
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {
                    mlog!(
                        Level::Stub,
                        rgba_core::log::GB_MBC,
                        "TAMA5 unknown read {}: {:02X}",
                        if reg == GBTAMA5_READ_HI { "hi" } else { "lo" },
                        addr_byte
                    );
                }
            }
            if reg == GBTAMA5_READ_HI {
                value >>= 4;
            }
            value | 0xF0
        }
        _ => {
            mlog!(
                Level::Stub,
                rgba_core::log::GB_MBC,
                "TAMA5 unknown read: {:02X}",
                reg
            );
            0xF1
        }
    }
}
