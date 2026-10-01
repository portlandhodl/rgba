// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gb/mbc.c (_GBMBCLatchRTC, GBMBCRTCRead/Write),
// huc-3.c (GBMBCHuC3Read/Write) and tama5.c (GBMBCTAMA5Read/Write).
//
// The C appends RTC save data past the end of the SRAM image (via
// _GBMBCAppendSaveSuffix). The Rust port keeps that suffix as an in-memory
// Vec on Memory (rtc_save_suffix); the frontend persists it next to the .sav.

use crate::gb::Gb;
use crate::memory::MbcType;
use crate::mbc::huc3;
use crate::mbc::tama5;

/// _GBMBCLatchRTC: advance the 5-byte RTC register image by (now -
/// *rtc_last_latch) seconds of wall-clock time.
pub fn latch_rtc(now: i64, rtc_regs: &mut [u8; 5], rtc_last_latch: &mut i64) {
    let t_now = now;
    let mut t = now - *rtc_last_latch;
    *rtc_last_latch = t_now;

    let mut diff: i64;
    diff = rtc_regs[0] as i64 + t % 60;
    if diff < 0 {
        diff += 60;
        t -= 60;
    }
    rtc_regs[0] = (diff % 60) as u8;
    t /= 60;
    t += diff / 60;

    diff = rtc_regs[1] as i64 + t % 60;
    if diff < 0 {
        diff += 60;
        t -= 60;
    }
    rtc_regs[1] = (diff % 60) as u8;
    t /= 60;
    t += diff / 60;

    diff = rtc_regs[2] as i64 + t % 24;
    if diff < 0 {
        diff += 24;
        t -= 24;
    }
    rtc_regs[2] = (diff % 24) as u8;
    t /= 24;
    t += diff / 24;

    diff = rtc_regs[3] as i64 + (((rtc_regs[4] & 1) as i64) << 8) + (t & 0x1FF);
    rtc_regs[3] = diff as u8;
    rtc_regs[4] &= 0xFE;
    rtc_regs[4] |= ((diff >> 8) & 1) as u8;
    if diff & 0x200 != 0 {
        rtc_regs[4] |= 0x80;
    }
}

// GBMBCTAMA5SaveBuffer pages order (4 × 0x8 packed nibbles + u64 time = 40 bytes).
const TAMA5_SAVE_LEN: usize = 0x8 * 4 + 8;
// GBMBCHuC3SaveBuffer: 0x80 packed halves + u64 = 0x88 bytes.
const HUC3_SAVE_LEN: usize = 0x80 + 8;
// GBMBCRTCSaveBuffer: 10 u32 + u64 = 48 bytes.
const RTC_SAVE_LEN: usize = 10 * 4 + 8;

fn get_u32(d: &mut &[u8]) -> u32 {
    let (b, rest) = d.split_at(4);
    *d = rest;
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn get_u64(d: &mut &[u8]) -> u64 {
    let (b, rest) = d.split_at(8);
    *d = rest;
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// GBMBCTAMA5Read
fn tama5_read(gb: &mut Gb, data: &[u8]) {
    if data.len() < TAMA5_SAVE_LEN {
        gb.memory.mbc_state.tama5.disabled = false;
        return;
    }
    let mut d = data;
    let mut pages = [[0u8; 0x8]; 4];
    for p in pages.iter_mut() {
        for b in p.iter_mut() {
            *b = d[0];
            d = &d[1..];
        }
    }
    let rtc_last_latch = get_u64(&mut d) as i64;
    let st = &mut gb.memory.mbc_state.tama5;
    for i in 0..8 {
        st.rtc_timer_page[i * 2] = pages[0][i] & 0xF;
        st.rtc_timer_page[i * 2 + 1] = pages[0][i] >> 4;
        st.rtc_alarm_page[i * 2] = pages[1][i] & 0xF;
        st.rtc_alarm_page[i * 2 + 1] = pages[1][i] >> 4;
        st.rtc_free_page0[i * 2] = pages[2][i] & 0xF;
        st.rtc_free_page0[i * 2 + 1] = pages[2][i] >> 4;
        st.rtc_free_page1[i * 2] = pages[3][i] & 0xF;
        st.rtc_free_page1[i * 2 + 1] = pages[3][i] >> 4;
    }
    gb.memory.rtc_last_latch = rtc_last_latch;

    st.disabled = (st.rtc_timer_page[tama5::GBTAMA6_RTC_PAGE] & 0x8) == 0;

    st.rtc_timer_page[tama5::GBTAMA6_RTC_PAGE] &= 0xC;
    st.rtc_alarm_page[tama5::GBTAMA6_RTC_PAGE] &= 0xC;
    st.rtc_alarm_page[tama5::GBTAMA6_RTC_PAGE] |= 1;
    st.rtc_free_page0[tama5::GBTAMA6_RTC_PAGE] &= 0xC;
    st.rtc_free_page0[tama5::GBTAMA6_RTC_PAGE] |= 2;
    st.rtc_free_page1[tama5::GBTAMA6_RTC_PAGE] &= 0xC;
    st.rtc_free_page1[tama5::GBTAMA6_RTC_PAGE] |= 3;
}

/// GBMBCTAMA5Write
fn tama5_write(gb: &mut Gb) -> Vec<u8> {
    let mut out = Vec::with_capacity(TAMA5_SAVE_LEN);
    let st = &gb.memory.mbc_state.tama5;
    for i in 0..8 {
        out.push(
            (st.rtc_timer_page[i * 2] & 0xF) | (st.rtc_timer_page[i * 2 + 1] << 4),
        );
    }
    for i in 0..8 {
        out.push(
            (st.rtc_alarm_page[i * 2] & 0xF) | (st.rtc_alarm_page[i * 2 + 1] << 4),
        );
    }
    for i in 0..8 {
        out.push(
            (st.rtc_free_page0[i * 2] & 0xF) | (st.rtc_free_page0[i * 2 + 1] << 4),
        );
    }
    for i in 0..8 {
        out.push(
            (st.rtc_free_page1[i * 2] & 0xF) | (st.rtc_free_page1[i * 2 + 1] << 4),
        );
    }
    put_u64(&mut out, gb.memory.rtc_last_latch as u64);
    out
}

/// GBMBCHuC3Read
fn huc3_read(gb: &mut Gb, data: &[u8]) {
    if data.len() < HUC3_SAVE_LEN {
        return;
    }
    let mut d = data;
    for i in 0..0x80usize {
        let v = d[0];
        d = &d[1..];
        gb.memory.mbc_state.huc3.registers[i * 2] = v & 0xF;
        gb.memory.mbc_state.huc3.registers[i * 2 + 1] = v >> 4;
    }
    gb.memory.rtc_last_latch = get_u64(&mut d) as i64;
}

/// GBMBCHuC3Write
fn huc3_write(gb: &mut Gb) -> Vec<u8> {
    let mut out = Vec::with_capacity(HUC3_SAVE_LEN);
    for i in 0..0x80usize {
        let lo = gb.memory.mbc_state.huc3.registers[i * 2] & 0xF;
        let hi = gb.memory.mbc_state.huc3.registers[i * 2 + 1] << 4;
        out.push(lo | hi);
    }
    put_u64(&mut out, gb.memory.rtc_last_latch as u64);
    out
}

/// GBMBCRTCRead
fn mbc3_rtc_read(gb: &mut Gb, data: &[u8]) {
    if data.len() < RTC_SAVE_LEN - 4 {
        return;
    }
    // Layout: sec,min,hour,days,daysHi (5×u32), latched* (5×u32), unixTime (u64)
    let mut d = &data[5 * 4..];
    gb.memory.rtc_regs[0] = get_u32(&mut d) as u8;
    gb.memory.rtc_regs[1] = get_u32(&mut d) as u8;
    gb.memory.rtc_regs[2] = get_u32(&mut d) as u8;
    gb.memory.rtc_regs[3] = get_u32(&mut d) as u8;
    gb.memory.rtc_regs[4] = get_u32(&mut d) as u8;
    gb.memory.rtc_last_latch = get_u64(&mut d) as i64;
}

/// GBMBCRTCWrite
fn mbc3_rtc_write(gb: &mut Gb) -> Vec<u8> {
    let mut rtc_regs = gb.memory.rtc_regs;
    let mut rtc_last_latch = gb.memory.rtc_last_latch;
    let now = gb.unix_time();
    latch_rtc(now, &mut rtc_regs, &mut rtc_last_latch);

    let mut out = Vec::with_capacity(RTC_SAVE_LEN);
    put_u32(&mut out, rtc_regs[0] as u32);
    put_u32(&mut out, rtc_regs[1] as u32);
    put_u32(&mut out, rtc_regs[2] as u32);
    put_u32(&mut out, rtc_regs[3] as u32);
    put_u32(&mut out, rtc_regs[4] as u32);
    put_u32(&mut out, gb.memory.rtc_regs[0] as u32);
    put_u32(&mut out, gb.memory.rtc_regs[1] as u32);
    put_u32(&mut out, gb.memory.rtc_regs[2] as u32);
    put_u32(&mut out, gb.memory.rtc_regs[3] as u32);
    put_u32(&mut out, gb.memory.rtc_regs[4] as u32);
    put_u64(&mut out, gb.memory.rtc_last_latch as u64);
    out
}

/// Returns the RTC suffix length for cart types that persist one.
pub fn rtc_suffix_len(gb: &Gb) -> Option<usize> {
    match gb.memory.mbc_type {
        MbcType::Mbc3Rtc => Some(RTC_SAVE_LEN),
        MbcType::HuC3 => Some(HUC3_SAVE_LEN),
        MbcType::Tama5 => Some(TAMA5_SAVE_LEN),
        _ => None,
    }
}

/// Load RTC state from the save-file suffix, if the cart has an RTC.
pub fn read_rtc_if_needed(gb: &mut Gb) {
    let data = match &gb.memory.rtc_save_suffix {
        Some(d) if !d.is_empty() => d.clone(),
        _ => return,
    };
    match gb.memory.mbc_type {
        MbcType::Mbc3Rtc => mbc3_rtc_read(gb, &data),
        MbcType::HuC3 => huc3_read(gb, &data),
        MbcType::Tama5 => tama5_read(gb, &data),
        _ => {}
    }
}

/// Store RTC state to the in-memory save-file suffix (flushed to disk by the
/// frontend together with SRAM).
pub fn write_rtc_if_needed(gb: &mut Gb) {
    let suffix = match gb.memory.mbc_type {
        MbcType::Mbc3Rtc => Some(mbc3_rtc_write(gb)),
        MbcType::HuC3 => Some(huc3_write(gb)),
        MbcType::Tama5 => Some(tama5_write(gb)),
        _ => None,
    };
    if let Some(s) = suffix {
        gb.memory.rtc_save_suffix = Some(s);
    }
}

/// Frontend hook: RTC bytes to append after the SRAM image on disk.
pub fn rtc_save_bytes(gb: &Gb) -> Option<Vec<u8>> {
    gb.memory.rtc_save_suffix.clone()
}

/// Frontend hook: load the previously-stored RTC suffix.
pub fn rtc_load_bytes(gb: &mut Gb, data: &[u8]) {
    gb.memory.rtc_save_suffix = Some(data.to_vec());
    read_rtc_if_needed(gb);
}
