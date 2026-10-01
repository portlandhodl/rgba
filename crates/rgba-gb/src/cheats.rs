// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gb/cheats.c and include/mgba/internal/gb/cheats.h —
// the Game Boy Game Genie / GameShark / Codebreaker / VBA code decoders.
//
// NOTE: this vendored mGBA snapshot (0.11.0-dev) predates the upstream
// "GBCheatDeviceGameShark" rework that emulates the physical GameShark
// cartridge (C0/C1/D0/D1/D8/DD/E0/E8 code types); here 8-digit codes are
// plain "idle" writes of one byte to one address, refreshed every frame.
// Ported as-is per docs/PORTING.md.

use rgba_core::cheats::{Cheat, CheatBus, CheatDevice, CheatPatch, CheatSet, CheatType};

use crate::gb::Gb;
use crate::memory::GB_SIZE_CART_MAX;

/// enum GBCheatType
pub const GB_CHEAT_AUTODETECT: i32 = 0;
pub const GB_CHEAT_GAMESHARK: i32 = 1;
pub const GB_CHEAT_GAME_GENIE: i32 = 2;
pub const GB_CHEAT_VBA: i32 = 3;

/// GBCheatDeviceCreate: the GB variant is a plain CheatDevice; its
/// GBCheatSetCreate hook produces plain sets whose addLine hook is
/// `gb_cheat_add_line` below.
pub fn gb_cheat_device_create() -> CheatDevice {
    CheatDevice::new()
}

/// hexDigit (mgba-util/string.c)
fn hex_digit(digit: u8) -> i32 {
    match digit {
        b'0'..=b'9' => (digit - b'0') as i32,
        b'a'..=b'f' => (digit - b'a' + 10) as i32,
        b'A'..=b'F' => (digit - b'A' + 10) as i32,
        _ => -1,
    }
}

/// hex8
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

/// hex12
fn hex12(line: &[u8], pos: usize) -> Option<(u16, usize)> {
    let mut value = 0u16;
    for i in 0..3 {
        let digit = *line.get(pos + i)?;
        let nybble = hex_digit(digit);
        if nybble < 0 {
            return None;
        }
        value = value << 4 | nybble as u16;
    }
    Some((value, pos + 3))
}

/// hex16
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

/// hex32
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

/// GBCheatAddCodebreaker: one byte written every frame (idle refresh).
fn gb_cheat_add_codebreaker(cheats: &mut CheatSet, address: u16, data: u8) -> bool {
    cheats.list.push(Cheat {
        typ: CheatType::Assign,
        width: 1,
        address: address as u32,
        operand: data as u32,
        repeat: 1,
        negative_repeat: 0,
        address_offset: 0,
        operand_offset: 0,
    });
    true
}

/// GBCheatAddGameShark: yyxxLLHH op, applied as xx -> 0xHHLL.
fn gb_cheat_add_game_shark(cheats: &mut CheatSet, op: u32) -> bool {
    gb_cheat_add_codebreaker(
        cheats,
        (((op & 0xFF) << 8) | ((op >> 8) & 0xFF)) as u16,
        ((op >> 16) & 0xFF) as u8,
    )
}

/// GBCheatAddGameSharkLine: an 8-hex-digit GameShark code.
fn gb_cheat_add_game_shark_line(cheats: &mut CheatSet, line: &str) -> bool {
    let Some((op, _)) = hex32(line.as_bytes(), 0) else {
        return false;
    };
    gb_cheat_add_game_shark(cheats, op)
}

/// GBCheatAddGameGenieLine: XXX-XXX or XXX-XXX-XXX Game Genie code, decoded
/// into a static ROM patch (with an old-value check for the 9-digit form).
fn gb_cheat_add_game_genie_line(cheats: &mut CheatSet, line: &str) -> bool {
    let b = line.as_bytes();
    let Some((op1, pos)) = hex12(b, 0) else {
        return false;
    };
    let mut pos = pos;
    if pos >= b.len() || b[pos] != b'-' {
        return false;
    }
    pos += 1;
    let Some((op2, p)) = hex12(b, pos) else {
        return false;
    };
    pos = p;
    let mut op3: u16 = 0x1000;
    if pos < b.len() && b[pos] == b'-' {
        pos += 1;
        let Some((v, p)) = hex12(b, pos) else {
            return false;
        };
        op3 = v;
        pos = p;
    }
    if pos != b.len() {
        return false;
    }
    let mut address = (op1 & 0xF) << 8;
    address |= (op2 >> 4) & 0xFF;
    address |= ((op2 & 0xF) ^ 0xF) << 12;
    let mut patch = CheatPatch {
        address: address as u32,
        value: (op1 >> 4) as u32,
        applied: false,
        width: 1,
        segment: -1,
        check_value: 0,
        check: false,
    };
    if op3 < 0x1000 {
        let mut value = ((op3 as u32 & 0xF00) << 20) | (op3 as u32 & 0xF);
        value = value.rotate_right(2); // ROR(value, 2)
        value |= value >> 24;
        value ^= 0xBA;
        patch.check_value = value & 0xFF;
        patch.check = true;
    }
    cheats.rom_patches.push(patch);
    true
}

/// GBCheatAddVBALine: AAAA:VV. The C parses the value as `hex8(line)` --
/// the first two digits of the whole line, not the digits after the colon --
/// so the stored operand is the address's high byte. Ported as-is.
fn gb_cheat_add_vba_line(cheats: &mut CheatSet, line: &str) -> bool {
    let b = line.as_bytes();
    let Some((address, pos)) = hex16(b, 0) else {
        return false;
    };
    if pos >= b.len() || b[pos] != b':' {
        return false;
    }
    let Some((value, _)) = hex8(b, 0) else {
        return false;
    };
    gb_cheat_add_codebreaker(cheats, address, value)
}

/// GBCheatAddLine (the set->addLine hook), wrapped in mCheatAddLine's core
/// bookkeeping: on success the raw line is kept for save/dump.
pub fn gb_cheat_add_line(set: &mut CheatSet, line: &str, typ: i32) -> bool {
    set.add_line(line, |set| gb_cheat_add_line_impl(set, line, typ))
}

fn gb_cheat_add_line_impl(cheats: &mut CheatSet, line: &str, typ: i32) -> bool {
    match typ {
        GB_CHEAT_GAME_GENIE => return gb_cheat_add_game_genie_line(cheats, line),
        GB_CHEAT_GAMESHARK => return gb_cheat_add_game_shark_line(cheats, line),
        GB_CHEAT_VBA => return gb_cheat_add_vba_line(cheats, line),
        GB_CHEAT_AUTODETECT => {}
        _ => return false,
    }

    let b = line.as_bytes();
    let Some((op1, mut pos)) = hex16(b, 0) else {
        return gb_cheat_add_game_genie_line(cheats, line);
    };
    if pos < b.len() && b[pos] == b':' {
        return gb_cheat_add_vba_line(cheats, line);
    }
    let Some((op2, p)) = hex8(b, pos) else {
        return false;
    };
    pos = p;
    let mut codebreaker = false;
    if pos < b.len() && b[pos] == b'-' {
        codebreaker = true;
        pos += 1;
    }
    let Some((op3, _)) = hex8(b, pos) else {
        return false;
    };
    if codebreaker {
        let address = (op1 << 8) | op2 as u16;
        gb_cheat_add_codebreaker(cheats, address, op3)
    } else {
        let real_op = (op1 as u32) << 16 | (op2 as u32) << 8 | op3 as u32;
        gb_cheat_add_game_shark(cheats, real_op)
    }
}

// GBCheatSetCopyProperties, GBCheatParseDirectives, GBCheatDumpDirectives are
// all no-ops in the C and are omitted.

/// The mCore vtable subset the cheat engine drives, over the GB bus
/// (_GBCoreBusRead8/16/32, _GBCoreRawRead8/16/32, _GBCoreRawWrite8/16/32).
impl CheatBus for Gb {
    fn read_mem(&mut self, address: u32, width: i32) -> i32 {
        let address = address as u16;
        match width {
            1 => self.load8(address) as i32,
            2 => {
                let lo = self.load8(address) as i32;
                lo | (self.load8(address.wrapping_add(1)) as i32) << 8
            }
            4 => {
                let b0 = self.load8(address) as i32;
                let b1 = self.load8(address.wrapping_add(1)) as i32;
                let b2 = self.load8(address.wrapping_add(2)) as i32;
                let b3 = self.load8(address.wrapping_add(3)) as i32;
                b0 | b1 << 8 | b2 << 16 | b3 << 24
            }
            _ => 0,
        }
    }

    fn write_mem(&mut self, address: u32, width: i32, value: i32) {
        let address = address as u16;
        match width {
            1 => self.store8(address, value as u8),
            2 => {
                self.store8(address, value as u8);
                self.store8(address.wrapping_add(1), (value >> 8) as u8);
            }
            4 => {
                self.store8(address, value as u8);
                self.store8(address.wrapping_add(1), (value >> 8) as u8);
                self.store8(address.wrapping_add(2), (value >> 16) as u8);
                self.store8(address.wrapping_add(3), (value >> 24) as u8);
            }
            _ => {}
        }
    }

    fn read_mem_segment(&mut self, address: u32, segment: i32, width: i32) -> i32 {
        let address = address as u16;
        match width {
            1 => self.view8(address, segment) as i32,
            2 => {
                let lo = self.view8(address, segment) as i32;
                lo | (self.view8(address.wrapping_add(1), segment) as i32) << 8
            }
            4 => {
                let b0 = self.view8(address, segment) as i32;
                let b1 = self.view8(address.wrapping_add(1), segment) as i32;
                let b2 = self.view8(address.wrapping_add(2), segment) as i32;
                let b3 = self.view8(address.wrapping_add(3), segment) as i32;
                b0 | b1 << 8 | b2 << 16 | b3 << 24
            }
            _ => 0,
        }
    }

    fn patch_mem(&mut self, address: u32, segment: i32, width: i32, value: i32) {
        let address = address as u16;
        match width {
            1 => self.patch8(address, value as u8, None, segment),
            2 => {
                self.patch8(address, value as u8, None, segment);
                self.patch8(address.wrapping_add(1), (value >> 8) as u8, None, segment);
            }
            4 => {
                self.patch8(address, value as u8, None, segment);
                self.patch8(address.wrapping_add(1), (value >> 8) as u8, None, segment);
                self.patch8(address.wrapping_add(2), (value >> 16) as u8, None, segment);
                self.patch8(address.wrapping_add(3), (value >> 24) as u8, None, segment);
            }
            _ => {}
        }
    }

    fn memory_block_max_segment(&self, address: u32) -> Option<i32> {
        // mCoreGetMemoryBlockInfo over _GBMemoryBlocks/_GBCMemoryBlocks: the
        // first MAPPED block is "cart0" (start 0, size GB_SIZE_CART_MAX,
        // maxSegment 511), which matches every GB cart address. Only ROM
        // blocks ever have segments for Game Genie checks.
        if address < GB_SIZE_CART_MAX as u32 {
            Some(511)
        } else {
            None
        }
    }
}

impl Gb {
    /// The cheat portion of GBFrameEnded: mCheatRefresh on every set, once
    /// per frame. GameShark/Codebreaker codes are idle writes done here;
    /// Game Genie codes are static ROM patches applied (and re-applied) here.
    pub fn cheat_apply(&mut self) {
        if self.cheats.cheats.is_empty() {
            return;
        }
        let mut device = std::mem::take(&mut self.cheats);
        for i in 0..device.cheats.len() {
            device.cheat_refresh(self, i);
        }
        self.cheats = device;
    }
}
