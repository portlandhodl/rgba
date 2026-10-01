// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Ported from mgba/src/gba/sio/gbp.c (Game Boy Player virtual controller).
// hash32 is from mgba/src/util/hash.c (MurmurHash3 x86_32).

use crate::gba::Gba;
use crate::io::regs::*;
use crate::sio::{SioDriver, SioMode};

// hash32 (mgba/src/util/hash.c) — MurmurHash3 x86_32, seed 0.
fn hash32(key: &[u8], seed: u32) -> u32 {
    let nblocks = key.len() / 4;
    let mut h1 = seed;
    const C1: u32 = 0xcc9e2d51;
    const C2: u32 = 0x1b873593;

    for i in 0..nblocks {
        let mut k1 = u32::from_le_bytes(key[i * 4..i * 4 + 4].try_into().unwrap());
        k1 = k1.wrapping_mul(C1);
        k1 = k1.rotate_left(15);
        k1 = k1.wrapping_mul(C2);
        h1 ^= k1;
        h1 = h1.rotate_left(13);
        h1 = h1.wrapping_mul(5).wrapping_add(0xe6546b64);
    }

    let tail = &key[nblocks * 4..];
    let mut k1: u32 = 0;
    match key.len() & 3 {
        3 => {
            k1 ^= (tail[2] as u32) << 16;
            k1 ^= (tail[1] as u32) << 8;
            k1 ^= tail[0] as u32;
        }
        2 => {
            k1 ^= (tail[1] as u32) << 8;
            k1 ^= tail[0] as u32;
        }
        1 => {
            k1 ^= tail[0] as u32;
        }
        _ => {}
    }
    if key.len() & 3 != 0 {
        k1 = k1.wrapping_mul(C1);
        k1 = k1.rotate_left(15);
        k1 = k1.wrapping_mul(C2);
        h1 ^= k1;
    }

    h1 ^= key.len() as u32;
    // fmix32
    h1 ^= h1 >> 16;
    h1 = h1.wrapping_mul(0x85ebca6b);
    h1 ^= h1 >> 13;
    h1 = h1.wrapping_mul(0xc2b2ae35);
    h1 ^= h1 >> 16;
    h1
}

const LOGO_PALETTE: [u8; 0x80] = [
    0xDF, 0xFF, 0x0C, 0x64, 0x0C, 0xE4, 0x2D, 0xE4, 0x4E, 0x64, 0x4E, 0xE4, 0x6E, 0xE4, 0xAF, 0x68,
    0xB0, 0xE8, 0xD0, 0x68, 0xF0, 0x68, 0x11, 0x69, 0x11, 0xE9, 0x32, 0x6D, 0x32, 0xED, 0x73, 0xED,
    0x93, 0x6D, 0x94, 0xED, 0xB4, 0x6D, 0xD5, 0xF1, 0xF5, 0x71, 0xF6, 0xF1, 0x16, 0x72, 0x57, 0x72,
    0x57, 0xF6, 0x78, 0x76, 0x78, 0xF6, 0x99, 0xF6, 0xB9, 0xF6, 0xD9, 0x76, 0xDA, 0xF6, 0x1B, 0x7B,
    0x1B, 0xFB, 0x3C, 0xFB, 0x5C, 0x7B, 0x7D, 0x7B, 0x7D, 0xFF, 0x9D, 0x7F, 0xBE, 0x7F, 0xFF, 0x7F,
    0x2D, 0x64, 0x8E, 0x64, 0x8F, 0xE8, 0xF1, 0xE8, 0x52, 0x6D, 0x73, 0x6D, 0xB4, 0xF1, 0x16, 0xF2,
    0x37, 0x72, 0x98, 0x76, 0xFA, 0x7A, 0xFA, 0xFA, 0x5C, 0xFB, 0xBE, 0xFF, 0xDE, 0x7F, 0xFF, 0xFF,
    0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

const LOGO_HASH: u32 = 0xEEDA6963;

const GBP_TX_DATA: [u32; 13] = [
    0x0000494E, 0x0000494E,
    0xB6B1494E, 0xB6B1544E,
    0xABB1544E, 0xABB14E45,
    0xB1BA4E45, 0xB1BA4F44,
    0xB0BB4F44, 0xB0BB8002,
    0x10000010, 0x20000013,
    0x30000003,
];

/// GBASIOPlayerUpdate: run at end of frame; detects the GBP boot screen and
/// drives the handshake. The key callback (`_gbpRead`) is folded in via
/// `gbp_key_override`, consulted by the frontend's key write path.
pub fn gbp_update(gba: &mut Gba) {
    let gbp_active = gba.hw.devices & crate::gba::HW_GB_PLAYER != 0;
    if gbp_active {
        if gbp_check_screen(gba) {
            gba.sio.gbp.inputs_posted += 1;
            gba.sio.gbp.inputs_posted %= 3;
        } else {
            // key callback restored to the frontend path
            gba.sio.gbp.key_override = false;
        }
        gba.sio.gbp.tx_position = 0;
        return;
    }
    if gba.sio.gbp.key_override {
        return;
    }
    if gbp_check_screen(gba) {
        gba.hw.devices |= crate::gba::HW_GB_PLAYER;
        gba.sio.gbp.inputs_posted = 0;
        gba.sio.gbp.key_override = true;
        if matches!(gba.sio.driver, SioDriver::None) {
            gba.sio_set_driver(Some(SioDriver::Gbp));
        }
    }
}

/// GBASIOPlayerCheckScreen
fn gbp_check_screen(gba: &Gba) -> bool {
    if gba.video.palette[..0x80] != LOGO_PALETTE {
        return false;
    }
    hash32(&gba.video.vram[0x4000..0x8000], 0) == LOGO_HASH
}

/// _gbpRead: the virtual key callback. Returns 0xF0 (all keys pressed)
/// during the third posted-input slot of the boot animation.
pub fn gbp_read_keys(gba: &Gba) -> u16 {
    if gba.sio.gbp.inputs_posted == 2 {
        0xF0
    } else {
        0
    }
}

/// _gbpSioWriteSIOCNT
pub fn gbp_write_siocnt(_gba: &mut Gba, value: u16) -> u16 {
    value & 0x78FB
}

/// _gbpSioStart
pub fn gbp_start(gba: &mut Gba) -> bool {
    let rx = gba.memory.io[(GBA_REG_SIODATA32_LO >> 1) as usize] as u32
        | ((gba.memory.io[(GBA_REG_SIODATA32_HI >> 1) as usize] as u32) << 16);
    let pos = gba.sio.gbp.tx_position;
    if pos < 12 && pos > 0 {
        // TODO: Check expected (as in the C)
    } else if pos >= 12 {
        // 0x00 = Stop, 0x11 = Hard Stop, 0x22 = Start
        // Rumble is not wired to anything in the SDL frontend; the C gates
        // on gba->rumble. Keep the timing math so a future rumble
        // integration drops in.
        let _current_time = gba.timing.current_time();
        let _rumble_on = (rx & 0x33) == 0x22;
    }
    true
}

/// _gbpSioFinishNormal32: feed the keyboard handshake values.
pub fn gbp_finish_normal32(gba: &mut Gba) -> u32 {
    let mut tx_position = gba.sio.gbp.tx_position;
    if tx_position > 16 {
        gba.sio.gbp.tx_position = 0;
        tx_position = 0;
    } else if tx_position > 12 {
        tx_position = 12;
    }
    let tx = GBP_TX_DATA[tx_position as usize];
    gba.sio.gbp.tx_position += 1;
    tx
}

/// Entry into the GBP driver from the SIO register write paths
/// (driver vtable): handles NORMAL32 only, 1 "connected" device.
pub mod driver {
    use crate::gba::Gba;
    use crate::sio::SioMode;

    pub fn handles_mode(mode: SioMode) -> bool {
        mode == SioMode::Normal32
    }

    pub fn connected_devices() -> i32 {
        1
    }

    pub fn set_mode(_gba: &mut Gba, _mode: SioMode) {}
}
