// Copyright (c) 2013-2015 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gb/overrides.c and include/mgba/internal/gb/overrides.h
// (the static per-game override tables; config/ini overrides are not ported).

use crate::gb::{Gb, GbModel};
use crate::memory::MbcType;

/// GBColorLookup (include/mgba/gb/interface.h)
pub const GB_COLORS_NONE: u32 = 0;
pub const GB_COLORS_CGB: u32 = 1;
pub const GB_COLORS_SGB: u32 = 2;
pub const GB_COLORS_SGB_CGB_FALLBACK: u32 = GB_COLORS_CGB | GB_COLORS_SGB;

/// GBCartridgeOverride (include/mgba/gb/interface.h)
#[derive(Clone, Copy, Debug)]
pub struct GbOverride {
    pub header_crc32: u32,
    pub model: GbModel,
    pub mbc: MbcType,
    pub gb_colors: [u32; 12],
}

impl GbOverride {
    /// The reset state GBOverrideFind starts from.
    pub fn new(header_crc32: u32) -> Self {
        GbOverride {
            header_crc32,
            model: GbModel::Autodetect,
            mbc: MbcType::Autodetect,
            gb_colors: [0; 12],
        }
    }
}

/// GBColorPreset (include/mgba/gb/interface.h)
pub struct GbColorPreset {
    pub name: &'static str,
    pub colors: [u32; 12],
}

/// M_RGB5_TO_RGB8 (mgba-util/image.h)
const fn rgb5_to_rgb8(x: u32) -> u32 {
    ((x & 0x1F) << 19) | (((x >> 5) & 0x1F) << 11) | (((x >> 10) & 0x1F) << 3)
}

/// PAL_ENTRY
const fn pal_entry(a: u32, b: u32, c: u32, d: u32) -> [u32; 4] {
    [
        0xFF000000 | rgb5_to_rgb8(a),
        0xFF000000 | rgb5_to_rgb8(b),
        0xFF000000 | rgb5_to_rgb8(c),
        0xFF000000 | rgb5_to_rgb8(d),
    ]
}

/// PALETTE(X, Y, Z)
const fn palette(x: [u32; 4], y: [u32; 4], z: [u32; 4]) -> [u32; 12] {
    let mut out = [0u32; 12];
    let mut i = 0;
    while i < 4 {
        out[i] = x[i];
        out[i + 4] = y[i];
        out[i + 8] = z[i];
        i += 1;
    }
    out
}

/// UNIFORM_PAL(A, B, C, D)
const fn uniform_pal(a: u32, b: u32, c: u32, d: u32) -> [u32; 12] {
    let e = pal_entry(a, b, c, d);
    palette(e, e, e)
}

/// SGB_PAL(A)
const fn sgb_pal(a: [u32; 4]) -> [u32; 12] {
    palette(a, a, a)
}

/// A color-table entry (model/MBC stay autodetect).
const fn ov(header_crc32: u32, gb_colors: [u32; 12]) -> GbOverride {
    GbOverride {
        header_crc32,
        model: GbModel::Autodetect,
        mbc: MbcType::Autodetect,
        gb_colors,
    }
}

/// An MBC-table entry (no palette).
const fn ovm(header_crc32: u32, mbc: MbcType) -> GbOverride {
    GbOverride {
        header_crc32,
        model: GbModel::Autodetect,
        mbc,
        gb_colors: [0; 12],
    }
}

const PAL0: [u32; 4] = pal_entry(0x7FFF, 0x32BF, 0x00D0, 0x0000);
const PAL1: [u32; 4] = pal_entry(0x639F, 0x4279, 0x15B0, 0x04CB);
const PAL2: [u32; 4] = pal_entry(0x7FFF, 0x6E31, 0x454A, 0x0000);
const PAL3: [u32; 4] = pal_entry(0x7FFF, 0x1BEF, 0x0200, 0x0000);
const PAL4: [u32; 4] = pal_entry(0x7FFF, 0x421F, 0x1CF2, 0x0000);
const PAL5: [u32; 4] = pal_entry(0x7FFF, 0x5294, 0x294A, 0x0000);
const PAL6: [u32; 4] = pal_entry(0x7FFF, 0x03FF, 0x012F, 0x0000);
const PAL7: [u32; 4] = pal_entry(0x7FFF, 0x03EF, 0x01D6, 0x0000);
const PAL8: [u32; 4] = pal_entry(0x7FFF, 0x42B5, 0x3DC8, 0x0000);
const PAL9: [u32; 4] = pal_entry(0x7E74, 0x03FF, 0x0180, 0x0000);
const PAL10: [u32; 4] = pal_entry(0x67FF, 0x77AC, 0x1A13, 0x2D6B);
const PAL11: [u32; 4] = pal_entry(0x7ED6, 0x4BFF, 0x2175, 0x0000);
const PAL12: [u32; 4] = pal_entry(0x53FF, 0x4A5F, 0x7E52, 0x0000);
const PAL13: [u32; 4] = pal_entry(0x4FFF, 0x7ED2, 0x3A4C, 0x1CE0);
const PAL14: [u32; 4] = pal_entry(0x03ED, 0x7FFF, 0x255F, 0x0000);
const PAL15: [u32; 4] = pal_entry(0x036A, 0x021F, 0x03FF, 0x7FFF);
const PAL16: [u32; 4] = pal_entry(0x7FFF, 0x01DF, 0x0112, 0x0000);
const PAL17: [u32; 4] = pal_entry(0x231F, 0x035F, 0x00F2, 0x0009);
const PAL18: [u32; 4] = pal_entry(0x7FFF, 0x03EA, 0x011F, 0x0000);
const PAL19: [u32; 4] = pal_entry(0x299F, 0x001A, 0x000C, 0x0000);
const PAL20: [u32; 4] = pal_entry(0x7FFF, 0x027F, 0x001F, 0x0000);
const PAL21: [u32; 4] = pal_entry(0x7FFF, 0x03E0, 0x0206, 0x0120);
const PAL22: [u32; 4] = pal_entry(0x7FFF, 0x7EEB, 0x001F, 0x7C00);
const PAL23: [u32; 4] = pal_entry(0x7FFF, 0x3FFF, 0x7E00, 0x001F);
const PAL24: [u32; 4] = pal_entry(0x7FFF, 0x03FF, 0x001F, 0x0000);
const PAL25: [u32; 4] = pal_entry(0x03FF, 0x001F, 0x000C, 0x0000);
const PAL26: [u32; 4] = pal_entry(0x7FFF, 0x033F, 0x0193, 0x0000);
const PAL27: [u32; 4] = pal_entry(0x0000, 0x4200, 0x037F, 0x7FFF);
const PAL28: [u32; 4] = pal_entry(0x7FFF, 0x7E8C, 0x7C00, 0x0000);
const PAL29: [u32; 4] = pal_entry(0x7FFF, 0x1BEF, 0x6180, 0x0000);
const PAL30: [u32; 4] = pal_entry(0x7C00, 0x7FFF, 0x3FFF, 0x7E00);
const PAL31: [u32; 4] = pal_entry(0x7FFF, 0x7FFF, 0x7E8C, 0x7C00);
const PAL32: [u32; 4] = pal_entry(0x0000, 0x7FFF, 0x421F, 0x1CF2);
const PAL1A: [u32; 4] = pal_entry(0x67BF, 0x265B, 0x10B5, 0x2866);
const PAL1B: [u32; 4] = pal_entry(0x637B, 0x3AD9, 0x0956, 0x0000);
const PAL1C: [u32; 4] = pal_entry(0x7F1F, 0x2A7D, 0x30F3, 0x4CE7);
const PAL1D: [u32; 4] = pal_entry(0x57FF, 0x2618, 0x001F, 0x006A);
const PAL1E: [u32; 4] = pal_entry(0x5B7F, 0x3F0F, 0x222D, 0x10EB);
const PAL1F: [u32; 4] = pal_entry(0x7FBB, 0x2A3C, 0x0015, 0x0900);
const PAL1G: [u32; 4] = pal_entry(0x2800, 0x7680, 0x01EF, 0x2FFF);
const PAL1H: [u32; 4] = pal_entry(0x73BF, 0x46FF, 0x0110, 0x0066);
const PAL2A: [u32; 4] = pal_entry(0x533E, 0x2638, 0x01E5, 0x0000);
const PAL2B: [u32; 4] = pal_entry(0x7FFF, 0x2BBF, 0x00DF, 0x2C0A);
const PAL2C: [u32; 4] = pal_entry(0x7F1F, 0x463D, 0x74CF, 0x4CA5);
const PAL2D: [u32; 4] = pal_entry(0x53FF, 0x03E0, 0x00DF, 0x2800);
const PAL2E: [u32; 4] = pal_entry(0x433F, 0x72D2, 0x3045, 0x0822);
const PAL2F: [u32; 4] = pal_entry(0x7FFA, 0x2A5F, 0x0014, 0x0003);
const PAL2G: [u32; 4] = pal_entry(0x1EED, 0x215C, 0x42FC, 0x0060);
const PAL2H: [u32; 4] = pal_entry(0x7FFF, 0x5EF7, 0x39CE, 0x0000);
const PAL3A: [u32; 4] = pal_entry(0x4F5F, 0x630E, 0x159F, 0x3126);
const PAL3B: [u32; 4] = pal_entry(0x637B, 0x121C, 0x0140, 0x0840);
const PAL3C: [u32; 4] = pal_entry(0x66BC, 0x3FFF, 0x7EE0, 0x2C84);
const PAL3D: [u32; 4] = pal_entry(0x5FFE, 0x3EBC, 0x0321, 0x0000);
const PAL3E: [u32; 4] = pal_entry(0x63FF, 0x36DC, 0x11F6, 0x392A);
const PAL3F: [u32; 4] = pal_entry(0x65EF, 0x7DBF, 0x035F, 0x2108);
const PAL3G: [u32; 4] = pal_entry(0x2B6C, 0x7FFF, 0x1CD9, 0x0007);
const PAL3H: [u32; 4] = pal_entry(0x53FC, 0x1F2F, 0x0E29, 0x0061);
const PAL4A: [u32; 4] = pal_entry(0x36BE, 0x7EAF, 0x681A, 0x3C00);
const PAL4B: [u32; 4] = pal_entry(0x7BBE, 0x329D, 0x1DE8, 0x0423);
const PAL4C: [u32; 4] = pal_entry(0x739F, 0x6A9B, 0x7293, 0x0001);
const PAL4D: [u32; 4] = pal_entry(0x5FFF, 0x6732, 0x3DA9, 0x2481);
const PAL4E: [u32; 4] = pal_entry(0x577F, 0x3EBC, 0x456F, 0x1880);
const PAL4F: [u32; 4] = pal_entry(0x6B57, 0x6E1B, 0x5010, 0x0007);
const PAL4G: [u32; 4] = pal_entry(0x0F96, 0x2C97, 0x0045, 0x3200);
const PAL4H: [u32; 4] = pal_entry(0x67FF, 0x2F17, 0x2230, 0x1548);

/// `_gbcOverrides` (gb/overrides.c); 144 entries.
pub static GBC_OVERRIDES: &[GbOverride] = &[
    // Adventures of Lolo (Europe)
    ov(0xFBE65286, palette(PAL0, PAL28, PAL3)),
    // Alleyway (World)
    ov(0xCBAA161B, palette(PAL9, PAL9, PAL9)),
    // Arcade Classic No. 1 - Asteroids & Missile Command (USA, Europe)
    ov(0x309FDB70, palette(PAL3, PAL4, PAL4)),
    // Arcade Classic No. 3 - Galaga & Galaxian (USA)
    ov(0xE13EF629, palette(PAL27, PAL27, PAL27)),
    // Arcade Classic No. 4 - Defender & Joust (USA, Europe)
    ov(0x5C8B229D, palette(PAL0, PAL28, PAL3)),
    // Balloon Kid (USA, Europe)
    ov(0xEC3438FA, palette(PAL20, PAL20, PAL20)),
    // Baseball (World)
    ov(0xE02904BD, palette(PAL15, PAL31, PAL4)),
    // Battle Arena Toshinden (USA)
    ov(0xA2C3DF62, palette(PAL0, PAL28, PAL3)),
    // Battletoads in Ragnarok's World (Europe)
    ov(0x51B259CF, palette(PAL0, PAL3, PAL3)),
    // Chessmaster, The (DMG-EM) (Europe)
    ov(0x96A68366, palette(PAL0, PAL28, PAL28)),
    // David Crane's The Rescue of Princess Blobette Starring A Boy and His Blob (Europe)
    ov(0x6413F5E2, palette(PAL0, PAL3, PAL28)),
    // Donkey Kong (Japan, USA)
    ov(0xA777EE2F, palette(PAL20, PAL4, PAL4)),
    // Donkey Kong (World) (Rev A)
    ov(0xC8F8ACDA, palette(PAL20, PAL4, PAL4)),
    // Donkey Kong Land (Japan)
    ov(0x2CA7EEF3, palette(PAL2, PAL17, PAL22)),
    // Donkey Kong Land (USA, Europe)
    ov(0x0D3E401D, palette(PAL13, PAL17, PAL4)),
    // Donkey Kong Land 2 (USA, Europe)
    ov(0x07ED9445, palette(PAL2, PAL17, PAL22)),
    // Donkey Kong Land III (USA, Europe)
    ov(0xCA01A31C, palette(PAL2, PAL17, PAL22)),
    // Donkey Kong Land III (USA, Europe) (Rev A)
    ov(0x6805BA1E, palette(PAL2, PAL17, PAL22)),
    // Dr. Mario (World)
    ov(0xA3C2C1E9, palette(PAL28, PAL28, PAL4)),
    // Dr. Mario (World) (Rev A)
    ov(0x69975661, palette(PAL28, PAL28, PAL4)),
    // Dr. Mario (World) (Beta)
    ov(0x22E55535, palette(PAL9, PAL19, PAL30)),
    // Dynablaster (Europe)
    ov(0xD9D0211F, palette(PAL0, PAL28, PAL28)),
    // F-1 Race (World)
    ov(0x8434CB2C, palette(PAL0, PAL0, PAL0)),
    // F-1 Race (World) (Rev A)
    ov(0xBA63383B, palette(PAL0, PAL0, PAL0)),
    // Game & Watch Gallery (Europe)
    ov(0x4A43B8B9, palette(PAL7, PAL4, PAL4)),
    // Game & Watch Gallery (USA)
    ov(0xBD0736D4, palette(PAL7, PAL4, PAL4)),
    // Game & Watch Gallery (USA) (Rev A)
    ov(0xA969B4F0, palette(PAL7, PAL4, PAL4)),
    // Game Boy Camera Gold (USA)
    ov(0x83947EC8, palette(PAL4, PAL3, PAL4)),
    // Game Boy Gallery (Japan)
    ov(0xDC3C3642, palette(PAL7, PAL4, PAL4)),
    // Game Boy Gallery - 5 Games in One (Europe)
    ov(0xD83E3F82, palette(PAL0, PAL0, PAL0)),
    // Game Boy Gallery 2 (Australia)
    ov(0x6C477A30, palette(PAL7, PAL4, PAL4)),
    // Game Boy Gallery 2 (Japan)
    ov(0xC5AAAFDA, palette(PAL7, PAL4, PAL4)),
    // Game Boy Wars (Japan)
    ov(0x03E3ED72, palette(PAL8, PAL16, PAL22)),
    // Golf (World)
    ov(0x885C242D, palette(PAL3, PAL4, PAL4)),
    // Hoshi no Kirby (Japan)
    ov(0x4AA02A13, palette(PAL9, PAL19, PAL30)),
    // Hoshi no Kirby (Japan) (Rev A)
    ov(0x88D03280, palette(PAL9, PAL19, PAL30)),
    // Hoshi no Kirby 2 (Japan)
    ov(0x58B7A321, palette(PAL9, PAL19, PAL30)),
    // James Bond 007 (USA, Europe)
    ov(0x7DDEB68E, palette(PAL29, PAL4, PAL29)),
    // Kaeru no Tame ni Kane wa Naru (Japan)
    ov(0x7F805941, palette(PAL2, PAL4, PAL2)),
    // Kid Icarus - Of Myths and Monsters (USA, Europe)
    ov(0x5D93DB0F, palette(PAL2, PAL4, PAL4)),
    // Killer Instinct (USA, Europe)
    ov(0x117043A9, palette(PAL2, PAL4, PAL0)),
    // King of Fighters '95, The (USA)
    ov(0x0F81CC70, palette(PAL0, PAL28, PAL3)),
    // King of the Zoo (Europe)
    ov(0xB492FB51, palette(PAL0, PAL28, PAL28)),
    // Kirby no Block Ball (Japan)
    ov(0x4203B79F, palette(PAL9, PAL19, PAL30)),
    // Kirby no Kirakira Kids (Japan)
    ov(0x74C3A937, palette(PAL0, PAL0, PAL0)),
    // Kirby no Pinball (Japan)
    ov(0x89239AED, palette(PAL9, PAL19, PAL19)),
    // Kirby's Block Ball (USA, Europe)
    ov(0xCE8B1B18, palette(PAL9, PAL19, PAL30)),
    // Kirby's Dream Land (USA, Europe)
    ov(0x302017CC, palette(PAL9, PAL19, PAL30)),
    // Kirby's Dream Land 2 (USA, Europe)
    ov(0xF6C9E5A8, palette(PAL9, PAL19, PAL30)),
    // Kirby's Pinball Land (USA, Europe)
    ov(0x9C4AA9D8, palette(PAL9, PAL19, PAL19)),
    // Kirby's Star Stacker (USA, Europe)
    ov(0xC1B481CA, palette(PAL0, PAL0, PAL0)),
    // Legend of Zelda, The - Link's Awakening (Canada)
    ov(0x9F54D47B, palette(PAL4, PAL21, PAL28)),
    // Legend of Zelda, The - Link's Awakening (France)
    ov(0x441D7FAD, palette(PAL4, PAL21, PAL28)),
    // Legend of Zelda, The - Link's Awakening (Germany)
    ov(0x838D65D6, palette(PAL4, PAL21, PAL28)),
    // Legend of Zelda, The - Link's Awakening (USA, Europe) (Rev A)
    ov(0x24CAAB4D, palette(PAL4, PAL21, PAL28)),
    // Legend of Zelda, The - Link's Awakening (USA, Europe) (Rev B)
    ov(0xBCBB6BDB, palette(PAL4, PAL21, PAL28)),
    // Legend of Zelda, The - Link's Awakening (USA, Europe)
    ov(0x9A193109, palette(PAL4, PAL21, PAL28)),
    // Magnetic Soccer (Europe)
    ov(0x6735A1F5, palette(PAL3, PAL4, PAL28)),
    // Mario & Yoshi (Europe)
    ov(0xEC14B007, palette(PAL18, PAL4, PAL4)),
    // Mario no Picross (Japan)
    ov(0x602C2371, palette(PAL0, PAL0, PAL0)),
    // Mario's Picross (USA, Europe)
    ov(0x725BBFF6, palette(PAL0, PAL0, PAL0)),
    // Mega Man - Dr. Wily's Revenge (Europe)
    ov(0xB2FE1EDB, palette(PAL0, PAL28, PAL3)),
    // Mega Man II (Europe)
    ov(0xC5EE1580, palette(PAL0, PAL28, PAL3)),
    // Mega Man III (Europe)
    ov(0x88249B90, palette(PAL0, PAL28, PAL3)),
    // Metroid II - Return of Samus (World)
    ov(0xBDCCC648, palette(PAL28, PAL25, PAL3)),
    // Moguranya (Japan)
    ov(0x41C1D13C, palette(PAL8, PAL16, PAL16)),
    // Mole Mania (USA, Europe)
    ov(0x32E8EEA3, palette(PAL8, PAL16, PAL16)),
    // Mystic Quest (Europe)
    ov(0x8DC57012, palette(PAL3, PAL4, PAL28)),
    // Mystic Quest (France)
    ov(0x09728780, palette(PAL3, PAL4, PAL28)),
    // Mystic Quest (Germany)
    ov(0x6F8568A8, palette(PAL3, PAL4, PAL28)),
    // Nigel Mansell's World Championship Racing (Europe)
    ov(0xAC2D636D, palette(PAL0, PAL0, PAL0)),
    // Nintendo World Cup (USA, Europe)
    ov(0xB43E44C1, palette(PAL3, PAL4, PAL4)),
    // Othello (Europe)
    ov(0x45F34317, palette(PAL3, PAL4, PAL28)),
    // Pac-In-Time (Europe)
    ov(0x8C608574, palette(PAL29, PAL4, PAL28)),
    // Picross 2 (Japan)
    ov(0xBA91DDD8, palette(PAL0, PAL0, PAL0)),
    // Pinocchio (Europe)
    ov(0x849C74C0, palette(PAL2, PAL2, PAL17)),
    // Play Action Football (USA)
    ov(0x2B703514, palette(PAL3, PAL4, PAL4)),
    // Pocket Bomberman (Europe)
    ov(0x9C5E0D5E, palette(PAL2, PAL17, PAL17)),
    // Pocket Camera (Japan) (Rev A)
    ov(0x211A85AC, palette(PAL26, PAL26, PAL26)),
    // Pocket Monsters - Aka (Japan)
    ov(0x29D07340, palette(PAL4, PAL3, PAL4)),
    // Pocket Monsters - Aka (Japan) (Rev A)
    ov(0x6BB566EC, palette(PAL4, PAL3, PAL4)),
    // Pocket Monsters - Ao (Japan)
    ov(0x65EF364B, palette(PAL28, PAL4, PAL28)),
    // Pocket Monsters - Midori (Japan)
    ov(0x923D46DD, palette(PAL29, PAL4, PAL29)),
    // Pocket Monsters - Midori (Japan) (Rev A)
    ov(0x6C926BFF, palette(PAL29, PAL4, PAL29)),
    // Pocket Monsters - Pikachu (Japan)
    ov(0xF52AD7C1, palette(PAL24, PAL24, PAL24)),
    // Pocket Monsters - Pikachu (Japan) (Rev A)
    ov(0x0B54FAEB, palette(PAL24, PAL24, PAL24)),
    // Pocket Monsters - Pikachu (Japan) (Rev B)
    ov(0x9A161366, palette(PAL24, PAL24, PAL24)),
    // Pocket Monsters - Pikachu (Japan) (Rev C)
    ov(0x8E1C14E4, palette(PAL24, PAL24, PAL24)),
    // Pokemon - Blaue Edition (Germany)
    ov(0x6C3587F2, palette(PAL28, PAL4, PAL28)),
    // Pokemon - Blue Version (USA, Europe)
    ov(0x28323CE0, palette(PAL28, PAL4, PAL28)),
    // Pokemon - Edicion Azul (Spain)
    ov(0x93FCE15B, palette(PAL28, PAL4, PAL28)),
    // Pokemon - Edicion Roja (Spain)
    ov(0xFD20BB1C, palette(PAL4, PAL3, PAL4)),
    // Pokemon - Red Version (USA, Europe)
    ov(0xCC25454F, palette(PAL4, PAL3, PAL4)),
    // Pokemon - Rote Edition (Germany)
    ov(0xE5DD23CE, palette(PAL4, PAL3, PAL4)),
    // Pokemon - Version Bleue (France)
    ov(0x98BFEC5A, palette(PAL28, PAL4, PAL28)),
    // Pokemon - Version Rouge (France)
    ov(0x1D6D8022, palette(PAL4, PAL3, PAL4)),
    // Pokemon - Versione Blu (Italy)
    ov(0x7864DECC, palette(PAL28, PAL4, PAL28)),
    // Pokemon - Versione Rossa (Italy)
    ov(0xFE2A3F93, palette(PAL4, PAL3, PAL4)),
    // QIX (World)
    ov(0x5EECB346, palette(PAL24, PAL24, PAL22)),
    // Radar Mission (Japan)
    ov(0xD03B1A15, palette(PAL8, PAL16, PAL8)),
    // Radar Mission (USA, Europe)
    ov(0xCEDD9FEB, palette(PAL8, PAL16, PAL8)),
    // Soccer (Europe)
    ov(0xB0274CDA, palette(PAL14, PAL31, PAL0)),
    // SolarStriker (World)
    ov(0x981620E7, palette(PAL27, PAL27, PAL27)),
    // Space Invaders (Europe)
    ov(0x3B032784, palette(PAL27, PAL27, PAL27)),
    // Space Invaders (USA)
    ov(0x63A767E2, palette(PAL27, PAL27, PAL27)),
    // Star Wars (USA, Europe) (Rev A)
    ov(0x44CE17EE, palette(PAL0, PAL3, PAL28)),
    // Street Fighter II (USA)
    ov(0xC512D0B1, palette(PAL0, PAL28, PAL3)),
    // Street Fighter II (USA, Europe) (Rev A)
    ov(0x79E16545, palette(PAL0, PAL28, PAL3)),
    // Super Donkey Kong GB (Japan)
    ov(0x940D4974, palette(PAL13, PAL17, PAL4)),
    // Super Mario Land (World)
    ov(0x6C0ACA9F, palette(PAL11, PAL32, PAL32)),
    // Super Mario Land (World) (Rev A)
    ov(0xCA117ACC, palette(PAL11, PAL32, PAL32)),
    // Super Mario Land 2 - 6 Golden Coins (USA, Europe) (Rev A)
    ov(0x423E09E6, palette(PAL10, PAL16, PAL28)),
    // Super Mario Land 2 - 6 Golden Coins (USA, Europe) (Rev B)
    ov(0x445A0358, palette(PAL10, PAL16, PAL28)),
    // Super Mario Land 2 - 6 Golden Coins (USA, Europe)
    ov(0xDE2960A1, palette(PAL10, PAL16, PAL28)),
    // Super Mario Land 2 - 6-tsu no Kinka (Japan)
    ov(0xD47CED78, palette(PAL10, PAL16, PAL28)),
    // Super Mario Land 2 - 6-tsu no Kinka (Japan) (Rev A)
    ov(0xA4B4F9F9, palette(PAL10, PAL16, PAL28)),
    // Super Mario Land 2 - 6-tsu no Kinka (Japan) (Rev B)
    ov(0x5842F25D, palette(PAL10, PAL16, PAL28)),
    // Super R.C. Pro-Am (USA, Europe)
    ov(0x8C39B1C8, palette(PAL0, PAL28, PAL3)),
    // Tennis (World)
    ov(0xD2BEBF08, palette(PAL14, PAL31, PAL0)),
    // Tetris (World)
    ov(0xE906C6A6, palette(PAL24, PAL24, PAL24)),
    // Tetris (World) (Rev A)
    ov(0x4674B43F, palette(PAL24, PAL24, PAL24)),
    // Tetris 2 (USA)
    ov(0x687505F1, palette(PAL24, PAL24, PAL22)),
    // Tetris 2 (USA, Europe)
    ov(0x6761459F, palette(PAL24, PAL24, PAL22)),
    // Tetris Attack (USA)
    ov(0x00E9474B, palette(PAL18, PAL18, PAL22)),
    // Tetris Blast (USA, Europe)
    ov(0xDDDEEEDE, palette(PAL20, PAL20, PAL20)),
    // Tetris Attack (USA, Europe) (Rev A)
    ov(0x6628C535, palette(PAL18, PAL18, PAL22)),
    // Tetris Flash (Japan)
    ov(0xED669A78, palette(PAL24, PAL24, PAL22)),
    // Top Rank Tennis (USA)
    ov(0xA6497CC0, palette(PAL14, PAL31, PAL0)),
    // Top Ranking Tennis (Europe)
    ov(0x62C12E05, palette(PAL14, PAL31, PAL0)),
    // Toy Story (Europe)
    ov(0x67066E28, palette(PAL3, PAL4, PAL4)),
    // Vegas Stakes (USA, Europe)
    ov(0x80CB217F, palette(PAL3, PAL4, PAL28)),
    // Wario Land - Super Mario Land 3 (World)
    ov(0xF1EA10E9, palette(PAL8, PAL16, PAL22)),
    // Wario Land II (USA, Europe)
    ov(0xD56A50A1, palette(PAL8, PAL0, PAL28)),
    // Wave Race (USA, Europe)
    ov(0x52A6E4CC, palette(PAL28, PAL4, PAL23)),
    // X (Japan)
    ov(0xFED4C47F, palette(PAL5, PAL5, PAL5)),
    // Yakuman (Japan)
    ov(0x40604F17, palette(PAL0, PAL0, PAL0)),
    // Yakuman (Japan) (Rev A)
    ov(0x2959ACFC, palette(PAL0, PAL0, PAL0)),
    // Yoshi (USA)
    ov(0xAB1605B9, palette(PAL18, PAL4, PAL4)),
    // Yoshi no Cookie (Japan)
    ov(0x841753DA, palette(PAL20, PAL20, PAL22)),
    // Yoshi no Panepon (Japan)
    ov(0xAA1AD903, palette(PAL18, PAL18, PAL22)),
    // Yoshi no Tamago (Japan)
    ov(0xD4098A6B, palette(PAL18, PAL4, PAL4)),
    // Yoshi's Cookie (USA, Europe)
    ov(0x940EDD87, palette(PAL20, PAL20, PAL22)),
    // Zelda no Densetsu - Yume o Miru Shima (Japan)
    ov(0x259C9A82, palette(PAL4, PAL21, PAL28)),
    // Zelda no Densetsu - Yume o Miru Shima (Japan) (Rev A)
    ov(0x61F269CD, palette(PAL4, PAL21, PAL28)),
];

/// `_sgbOverrides` (gb/overrides.c); 46 entries.
pub static SGB_OVERRIDES: &[GbOverride] = &[
    // Alleyway (World)
    ov(0xCBAA161B, sgb_pal(PAL3F)),
    // Baseball (World)
    ov(0xE02904BD, sgb_pal(PAL2G)),
    // Dr. Mario (World)
    ov(0xA3C2C1E9, sgb_pal(PAL3B)),
    // Dr. Mario (World) (Rev A)
    ov(0x69975661, sgb_pal(PAL3B)),
    // F-1 Race (World)
    ov(0x8434CB2C, sgb_pal(PAL4F)),
    // F-1 Race (World) (Rev A)
    ov(0xBA63383B, sgb_pal(PAL4F)),
    // Game Boy Wars (Japan)
    ov(0x03E3ED72, sgb_pal(PAL3E)),
    // Golf (World)
    ov(0x885C242D, sgb_pal(PAL3H)),
    // Hoshi no Kirby (Japan)
    ov(0x4AA02A13, sgb_pal(PAL2C)),
    // Hoshi no Kirby (Japan) (Rev A)
    ov(0x88D03280, sgb_pal(PAL2C)),
    // Kaeru no Tame ni Kane wa Naru (Japan)
    ov(0x7F805941, sgb_pal(PAL2A)),
    // Kid Icarus - Of Myths and Monsters (USA, Europe)
    ov(0x5D93DB0F, sgb_pal(PAL2F)),
    // Kirby no Pinball (Japan)
    ov(0x89239AED, sgb_pal(PAL1C)),
    // Kirby's Dream Land (USA, Europe)
    ov(0x302017CC, sgb_pal(PAL2C)),
    // Kirby's Pinball Land (USA, Europe)
    ov(0x9C4AA9D8, sgb_pal(PAL1C)),
    // Legend of Zelda, The - Link's Awakening (Canada)
    ov(0x9F54D47B, sgb_pal(PAL1E)),
    // Legend of Zelda, The - Link's Awakening (France)
    ov(0x441D7FAD, sgb_pal(PAL1E)),
    // Legend of Zelda, The - Link's Awakening (Germany)
    ov(0x838D65D6, sgb_pal(PAL1E)),
    // Legend of Zelda, The - Link's Awakening (USA, Europe) (Rev A)
    ov(0x24CAAB4D, sgb_pal(PAL1E)),
    // Legend of Zelda, The - Link's Awakening (USA, Europe) (Rev B)
    ov(0xBCBB6BDB, sgb_pal(PAL1E)),
    // Legend of Zelda, The - Link's Awakening (USA, Europe)
    ov(0x9A193109, sgb_pal(PAL1E)),
    // Mario & Yoshi (Europe)
    ov(0xEC14B007, sgb_pal(PAL2D)),
    // Metroid II - Return of Samus (World)
    ov(0xBDCCC648, sgb_pal(PAL4G)),
    // QIX (World)
    ov(0x5EECB346, sgb_pal(PAL4A)),
    // SolarStriker (World)
    ov(0x981620E7, sgb_pal(PAL1G)),
    // Super Mario Land (World)
    ov(0x6C0ACA9F, sgb_pal(PAL1F)),
    // Super Mario Land (World) (Rev A)
    ov(0xCA117ACC, sgb_pal(PAL1F)),
    // Super Mario Land 2 - 6 Golden Coins (USA, Europe) (Rev A)
    ov(0x423E09E6, sgb_pal(PAL3D)),
    // Super Mario Land 2 - 6 Golden Coins (USA, Europe) (Rev B)
    ov(0x445A0358, sgb_pal(PAL3D)),
    // Super Mario Land 2 - 6 Golden Coins (USA, Europe)
    ov(0xDE2960A1, sgb_pal(PAL3D)),
    // Super Mario Land 2 - 6-tsu no Kinka (Japan)
    ov(0xD47CED78, sgb_pal(PAL3D)),
    // Super Mario Land 2 - 6-tsu no Kinka (Japan) (Rev A)
    ov(0xA4B4F9F9, sgb_pal(PAL3D)),
    // Super Mario Land 2 - 6-tsu no Kinka (Japan) (Rev B)
    ov(0x5842F25D, sgb_pal(PAL3D)),
    // Tennis (World)
    ov(0xD2BEBF08, sgb_pal(PAL3G)),
    // Tetris (World)
    ov(0xE906C6A6, sgb_pal(PAL3A)),
    // Tetris (World) (Rev A)
    ov(0x4674B43F, sgb_pal(PAL3A)),
    // Wario Land - Super Mario Land 3 (World)
    ov(0xF1EA10E9, sgb_pal(PAL1B)),
    // X (Japan)
    ov(0xFED4C47F, sgb_pal(PAL4D)),
    // Yakuman (Japan)
    ov(0x40604F17, sgb_pal(PAL3C)),
    // Yakuman (Japan) (Rev A)
    ov(0x2959ACFC, sgb_pal(PAL3C)),
    // Yoshi (USA)
    ov(0xAB1605B9, sgb_pal(PAL2D)),
    // Yoshi no Cookie (Japan)
    ov(0x841753DA, sgb_pal(PAL1D)),
    // Yoshi no Tamago (Japan)
    ov(0xD4098A6B, sgb_pal(PAL2D)),
    // Yoshi's Cookie (USA, Europe)
    ov(0x940EDD87, sgb_pal(PAL1D)),
    // Zelda no Densetsu - Yume o Miru Shima (Japan)
    ov(0x259C9A82, sgb_pal(PAL1E)),
    // Zelda no Densetsu - Yume o Miru Shima (Japan) (Rev A)
    ov(0x61F269CD, sgb_pal(PAL1E)),
];

/// `_overrides` (gb/overrides.c); 28 entries.
pub static OVERRIDES: &[GbOverride] = &[
    ovm(0xA61F3EE1, MbcType::M161),
    // Pokemon Spaceworld 1997 demo
    ovm(0x232A067D, MbcType::Mbc3Rtc),
    ovm(0x630ED957, MbcType::Mbc3Rtc),
    ovm(0x5AFF0038, MbcType::Mbc3Rtc),
    ovm(0xA61856BD, MbcType::Mbc3Rtc),
    // Unlicensed bootlegs
    ovm(0x30F8F86C, MbcType::UnlPkjd),
    ovm(0xE1147E75, MbcType::UnlNtOld1),
    ovm(0xEFF88FAA, MbcType::UnlNtOld1),
    ovm(0x811925D9, MbcType::UnlNtOld2),
    ovm(0x62A8016A, MbcType::UnlNtOld2),
    ovm(0x5758D6D9, MbcType::UnlNtOld2),
    ovm(0x62A8016A, MbcType::UnlNtOld2),
    ovm(0x80265A64, MbcType::UnlNtOld2),
    ovm(0x805459DE, MbcType::UnlNtOld2),
    ovm(0x0B1B808A, MbcType::UnlNtOld2),
    ovm(0x0B1B808A, MbcType::UnlNtOld2),
    ovm(0x4650EB9A, MbcType::UnlNtOld2),
    ovm(0xB289D95A, MbcType::UnlNtNew),
    ovm(0x688D6713, MbcType::UnlNtNew),
    ovm(0x8931A272, MbcType::UnlNtNew),
    ovm(0x79083C6B, MbcType::UnlNtNew),
    ovm(0x0C5047EE, MbcType::UnlNtNew),
    ovm(0x8AC634B7, MbcType::UnlNtNew),
    ovm(0x8628A287, MbcType::UnlNtNew),
    ovm(0xBC75D7B8, MbcType::UnlNtNew),
    ovm(0xFF0B60CC, MbcType::UnlNtNew),
    ovm(0x14A992A6, MbcType::UnlNtNew),
    ovm(0x3EF5AFB2, MbcType::UnlLiCheng),
];

/// `_colorPresets` (gb/overrides.c); 48 presets.
pub static COLOR_PRESETS: &[GbColorPreset] = &[
    GbColorPreset { name: "Grayscale", colors: uniform_pal(0x7FFF, 0x56B5, 0x294A, 0x0000) },
    GbColorPreset { name: "DMG Green", colors: uniform_pal(0x2691, 0x19A9, 0x1105, 0x04A3) },
    GbColorPreset { name: "GB Pocket", colors: uniform_pal(0x52D4, 0x4270, 0x2989, 0x10A3) },
    GbColorPreset { name: "GB Light", colors: uniform_pal(0x7FCF, 0x738B, 0x56C3, 0x39E0) },
    GbColorPreset { name: "GBC Brown ↑", colors: palette(PAL0, PAL0, PAL0) },
    GbColorPreset { name: "GBC Red ↑A", colors: palette(PAL4, PAL3, PAL28) },
    GbColorPreset { name: "GBC Dark Brown ↑B", colors: palette(PAL1, PAL0, PAL0) },
    GbColorPreset { name: "GBC Pale Yellow ↓", colors: palette(PAL12, PAL12, PAL12) },
    GbColorPreset { name: "GBC Orange ↓A", colors: palette(PAL24, PAL24, PAL24) },
    GbColorPreset { name: "GBC Yellow ↓B", colors: palette(PAL6, PAL28, PAL3) },
    GbColorPreset { name: "GBC Blue ←", colors: palette(PAL28, PAL4, PAL3) },
    GbColorPreset { name: "GBC Dark Blue ←A", colors: palette(PAL2, PAL4, PAL0) },
    GbColorPreset { name: "GBC Gray ←B", colors: palette(PAL5, PAL5, PAL5) },
    GbColorPreset { name: "GBC Green →", colors: palette(PAL18, PAL18, PAL18) },
    GbColorPreset { name: "GBC Dark Green →A", colors: palette(PAL29, PAL4, PAL4) },
    GbColorPreset { name: "GBC Reverse →B", colors: palette(PAL27, PAL27, PAL27) },
    GbColorPreset { name: "SGB 1-A", colors: sgb_pal(PAL1A) },
    GbColorPreset { name: "SGB 1-B", colors: sgb_pal(PAL1B) },
    GbColorPreset { name: "SGB 1-C", colors: sgb_pal(PAL1C) },
    GbColorPreset { name: "SGB 1-D", colors: sgb_pal(PAL1D) },
    GbColorPreset { name: "SGB 1-E", colors: sgb_pal(PAL1E) },
    GbColorPreset { name: "SGB 1-F", colors: sgb_pal(PAL1F) },
    GbColorPreset { name: "SGB 1-G", colors: sgb_pal(PAL1G) },
    GbColorPreset { name: "SGB 1-H", colors: sgb_pal(PAL1H) },
    GbColorPreset { name: "SGB 2-A", colors: sgb_pal(PAL2A) },
    GbColorPreset { name: "SGB 2-B", colors: sgb_pal(PAL2B) },
    GbColorPreset { name: "SGB 2-C", colors: sgb_pal(PAL2C) },
    GbColorPreset { name: "SGB 2-D", colors: sgb_pal(PAL2D) },
    GbColorPreset { name: "SGB 2-E", colors: sgb_pal(PAL2E) },
    GbColorPreset { name: "SGB 2-F", colors: sgb_pal(PAL2F) },
    GbColorPreset { name: "SGB 2-G", colors: sgb_pal(PAL2G) },
    GbColorPreset { name: "SGB 2-H", colors: sgb_pal(PAL2H) },
    GbColorPreset { name: "SGB 3-A", colors: sgb_pal(PAL3A) },
    GbColorPreset { name: "SGB 3-B", colors: sgb_pal(PAL3B) },
    GbColorPreset { name: "SGB 3-C", colors: sgb_pal(PAL3C) },
    GbColorPreset { name: "SGB 3-D", colors: sgb_pal(PAL3D) },
    GbColorPreset { name: "SGB 3-E", colors: sgb_pal(PAL3E) },
    GbColorPreset { name: "SGB 3-F", colors: sgb_pal(PAL3F) },
    GbColorPreset { name: "SGB 3-G", colors: sgb_pal(PAL3G) },
    GbColorPreset { name: "SGB 3-H", colors: sgb_pal(PAL3H) },
    GbColorPreset { name: "SGB 4-A", colors: sgb_pal(PAL4A) },
    GbColorPreset { name: "SGB 4-B", colors: sgb_pal(PAL4B) },
    GbColorPreset { name: "SGB 4-C", colors: sgb_pal(PAL4C) },
    GbColorPreset { name: "SGB 4-D", colors: sgb_pal(PAL4D) },
    GbColorPreset { name: "SGB 4-E", colors: sgb_pal(PAL4E) },
    GbColorPreset { name: "SGB 4-F", colors: sgb_pal(PAL4F) },
    GbColorPreset { name: "SGB 4-G", colors: sgb_pal(PAL4G) },
    GbColorPreset { name: "SGB 4-H", colors: sgb_pal(PAL4H) },
];

/// GBOverrideColorFind
pub fn override_color_find(override_: &mut GbOverride, order: u32) -> bool {
    if order & GB_COLORS_SGB != 0 {
        for o in SGB_OVERRIDES {
            if override_.header_crc32 == o.header_crc32 {
                override_.gb_colors = o.gb_colors;
                return true;
            }
        }
    }
    if order & GB_COLORS_CGB != 0 {
        for o in GBC_OVERRIDES {
            if override_.header_crc32 == o.header_crc32 {
                override_.gb_colors = o.gb_colors;
                return true;
            }
        }
    }
    false
}

/// GBOverrideFind (static-table part; the config/ini lookup is not ported)
pub fn override_find(override_: &mut GbOverride) -> bool {
    override_.model = GbModel::Autodetect;
    override_.mbc = MbcType::Autodetect;
    override_.gb_colors = [0; 12];
    for o in OVERRIDES {
        if override_.header_crc32 == o.header_crc32 {
            *override_ = *o;
            return true;
        }
    }
    false
}

/// GBColorPresetList
pub fn color_preset_list() -> &'static [GbColorPreset] {
    COLOR_PRESETS
}

/// GBOverrideApply
pub fn override_apply(gb: &mut Gb, override_: &GbOverride) {
    if override_.model != GbModel::Autodetect {
        gb.model = override_.model;
        gb.video.renderer_init(gb.model, gb.video.sgb_borders);
    }

    if override_.mbc != MbcType::Autodetect {
        gb.memory.mbc_type = override_.mbc;
        gb.mbc_init();
    }

    for i in 0..12u32 {
        if override_.gb_colors[i as usize] & 0xFF000000 == 0 {
            continue;
        }
        gb.video_set_palette(i, override_.gb_colors[i as usize]);
        if i < 8 {
            gb.video_set_palette(i + 4, override_.gb_colors[i as usize]);
        }
        if i < 4 {
            gb.video_set_palette(i + 8, override_.gb_colors[i as usize]);
        }
    }
}

/// GBOverrideApplyDefaults
pub fn override_apply_defaults(gb: &mut Gb) {
    if gb.memory.rom.len() < 0x150 {
        return;
    }
    // doCrc32(&gb->memory.rom[0x100], sizeof(struct GBCartridge) == 0x50)
    let mut override_ = GbOverride::new(crate::crc32(&gb.memory.rom[0x100..0x150]));
    if override_find(&mut override_) {
        override_apply(gb, &override_);
    }
}
