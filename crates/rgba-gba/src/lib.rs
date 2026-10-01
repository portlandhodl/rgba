//! rgba-gba: Game Boy Advance (ARM7TDMI) core, a Rust port of mGBA's
//! `src/arm` and `src/gba` modules. mGBA is Copyright (c) 2013-2026 Jeffrey
//! Pfau and contributors, MPL-2.0. This port is a derived work, also MPL-2.0.

pub mod arm;
pub mod debugger;
pub mod audio;
pub mod bios;
pub mod cart;
pub mod cheats;
pub mod dma;
pub mod gba;
pub mod io;
pub mod memory;
pub mod savedata;
pub mod serialize;
pub mod sio;
pub mod timers;
pub mod video;

pub use gba::{EventId, Gba};

/// doCrc32 (mgba-util/crc32.c)
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB88320 } else { crc >> 1 };
        }
    }
    !crc
}
