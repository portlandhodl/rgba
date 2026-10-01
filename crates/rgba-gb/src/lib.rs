//! rgba-gb: Game Boy (SM83) core, a Rust port of mGBA's `src/sm83` and
//! `src/gb` modules. mGBA is Copyright (c) 2013-2026 Jeffrey Pfau and
//! contributors, MPL-2.0. This port is a derived work, also MPL-2.0.

pub mod audio;
pub mod cheats;
pub mod cpu;
pub mod debugger;
pub mod gb;
pub mod io;
pub mod mbc;
pub mod memory;
pub mod overrides;
pub mod serialize;
pub mod sio;
pub mod timer;
pub mod video;
pub mod video_log;

pub use gb::{EventId, Gb, GbModel, GameInfo};

/// doCrc32 (mgba-util/crc32.c): CRC-32/ISO-HDLC.
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
