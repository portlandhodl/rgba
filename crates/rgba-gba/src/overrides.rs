// Copyright (c) 2013-2015 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gba/overrides.c and include/mgba/internal/gba/overrides.h
// (the static per-game override table; config/ini overrides are not ported).

use crate::gba::{
    Gba, GBA_IDLE_LOOP_NONE, HW_EREADER, HW_GB_PLAYER_DETECTION, HW_GYRO, HW_LIGHT_SENSOR,
    HW_NONE, HW_NO_OVERRIDE, HW_RTC, HW_RUMBLE, HW_TILT,
};
use crate::memory::{IDLE_LOOP_DETECT, IDLE_LOOP_REMOVE};
use crate::savedata::SavedataType;

/// GBACartridgeOverride (include/mgba/gba/interface.h)
#[derive(Clone, Copy, Debug)]
pub struct GbaOverride {
    /// Game code (`struct GBACartridge.id`, rom[0xAC..0xB0]).
    pub id: [u8; 4],
    pub savetype: SavedataType,
    pub hardware: u32,
    pub idle_loop: u32,
    pub vba_bug_compat: bool,
}

/// Table-entry constructor; the C entries leave `vbaBugCompat` false.
const fn mk(id: &[u8; 4], savetype: SavedataType, hardware: u32, idle_loop: u32) -> GbaOverride {
    GbaOverride {
        id: *id,
        savetype,
        hardware,
        idle_loop,
        vba_bug_compat: false,
    }
}

/// `_overrides` (gba/overrides.c), 104 entries. The C array is terminated by a
/// zeroed sentinel; a Rust slice carries its own length.
pub static OVERRIDES: &[GbaOverride] = &[
    // Advance Wars
    mk(b"AWRE", SavedataType::Flash512, HW_NONE, 0x8038810),
    mk(b"AWRP", SavedataType::Flash512, HW_NONE, 0x8038810),
    // Advance Wars 2: Black Hole Rising
    mk(b"AW2E", SavedataType::Flash512, HW_NONE, 0x8036E08),
    mk(b"AW2P", SavedataType::Flash512, HW_NONE, 0x803719C),
    // Boktai: The Sun is in Your Hand
    mk(b"U3IJ", SavedataType::Eeprom, HW_RTC | HW_LIGHT_SENSOR, GBA_IDLE_LOOP_NONE),
    mk(b"U3IE", SavedataType::Eeprom, HW_RTC | HW_LIGHT_SENSOR, GBA_IDLE_LOOP_NONE),
    mk(b"U3IP", SavedataType::Eeprom, HW_RTC | HW_LIGHT_SENSOR, GBA_IDLE_LOOP_NONE),
    // Boktai 2: Solar Boy Django
    mk(b"U32J", SavedataType::Eeprom, HW_RTC | HW_LIGHT_SENSOR, GBA_IDLE_LOOP_NONE),
    mk(b"U32E", SavedataType::Eeprom, HW_RTC | HW_LIGHT_SENSOR, GBA_IDLE_LOOP_NONE),
    mk(b"U32P", SavedataType::Eeprom, HW_RTC | HW_LIGHT_SENSOR, GBA_IDLE_LOOP_NONE),
    // Crash Bandicoot 2 - N-Tranced
    mk(b"AC8J", SavedataType::Eeprom, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"AC8E", SavedataType::Eeprom, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"AC8P", SavedataType::Eeprom, HW_NONE, GBA_IDLE_LOOP_NONE),
    // DigiCommunication Nyo - Datou! Black Gemagema Dan
    mk(b"BDKJ", SavedataType::Eeprom, HW_NONE, GBA_IDLE_LOOP_NONE),
    // Dragon Ball Z - The Legacy of Goku
    mk(b"ALGP", SavedataType::Eeprom, HW_NONE, GBA_IDLE_LOOP_NONE),
    // Dragon Ball Z - The Legacy of Goku II
    mk(b"ALFJ", SavedataType::Eeprom, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"ALFE", SavedataType::Eeprom, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"ALFP", SavedataType::Eeprom, HW_NONE, GBA_IDLE_LOOP_NONE),
    // Dragon Ball Z - Taiketsu
    mk(b"BDBE", SavedataType::Eeprom, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"BDBP", SavedataType::Eeprom, HW_NONE, GBA_IDLE_LOOP_NONE),
    // Drill Dozer
    mk(b"V49J", SavedataType::Sram, HW_RUMBLE, GBA_IDLE_LOOP_NONE),
    mk(b"V49E", SavedataType::Sram, HW_RUMBLE, GBA_IDLE_LOOP_NONE),
    mk(b"V49P", SavedataType::Sram, HW_RUMBLE, GBA_IDLE_LOOP_NONE),
    // e-Reader
    mk(b"PEAJ", SavedataType::Flash1M, HW_EREADER, GBA_IDLE_LOOP_NONE),
    mk(b"PSAJ", SavedataType::Flash1M, HW_EREADER, GBA_IDLE_LOOP_NONE),
    mk(b"PSAE", SavedataType::Flash1M, HW_EREADER, GBA_IDLE_LOOP_NONE),
    // Final Fantasy Tactics Advance
    mk(b"AFXE", SavedataType::Flash512, HW_NONE, 0x8000428),
    // F-Zero - Climax
    mk(b"BFTJ", SavedataType::Flash1M, HW_NONE, GBA_IDLE_LOOP_NONE),
    // Goodboy Galaxy
    mk(b"2GBP", SavedataType::Sram, HW_RUMBLE, GBA_IDLE_LOOP_NONE),
    // Iridion II
    mk(b"AI2E", SavedataType::ForceNone, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"AI2P", SavedataType::ForceNone, HW_NONE, GBA_IDLE_LOOP_NONE),
    // Game Boy Wars Advance 1+2
    mk(b"BGWJ", SavedataType::Flash1M, HW_NONE, GBA_IDLE_LOOP_NONE),
    // Golden Sun: The Lost Age
    mk(b"AGFE", SavedataType::Flash512, HW_NONE, 0x801353A),
    // Koro Koro Puzzle - Happy Panechu!
    mk(b"KHPJ", SavedataType::Eeprom, HW_TILT, GBA_IDLE_LOOP_NONE),
    // Legendz - Yomigaeru Shiren no Shima
    mk(b"BLJJ", SavedataType::Flash512, HW_RTC, GBA_IDLE_LOOP_NONE),
    mk(b"BLJK", SavedataType::Flash512, HW_RTC, GBA_IDLE_LOOP_NONE),
    // Legendz - Sign of Nekuromu
    mk(b"BLVJ", SavedataType::Flash512, HW_RTC, GBA_IDLE_LOOP_NONE),
    // Mega Man Battle Network
    mk(b"AREE", SavedataType::Sram, HW_NONE, 0x800032E),
    // Mega Man Zero
    mk(b"AZCE", SavedataType::Sram, HW_NONE, 0x80004E8),
    // Metal Slug Advance
    mk(b"BSME", SavedataType::Eeprom, HW_NONE, 0x8000290),
    // Pokemon Ruby
    mk(b"AXVJ", SavedataType::Flash1M, HW_RTC, GBA_IDLE_LOOP_NONE),
    mk(b"AXVE", SavedataType::Flash1M, HW_RTC, GBA_IDLE_LOOP_NONE),
    mk(b"AXVP", SavedataType::Flash1M, HW_RTC, GBA_IDLE_LOOP_NONE),
    mk(b"AXVI", SavedataType::Flash1M, HW_RTC, GBA_IDLE_LOOP_NONE),
    mk(b"AXVS", SavedataType::Flash1M, HW_RTC, GBA_IDLE_LOOP_NONE),
    mk(b"AXVD", SavedataType::Flash1M, HW_RTC, GBA_IDLE_LOOP_NONE),
    mk(b"AXVF", SavedataType::Flash1M, HW_RTC, GBA_IDLE_LOOP_NONE),
    // Pokemon Sapphire
    mk(b"AXPJ", SavedataType::Flash1M, HW_RTC, GBA_IDLE_LOOP_NONE),
    mk(b"AXPE", SavedataType::Flash1M, HW_RTC, GBA_IDLE_LOOP_NONE),
    mk(b"AXPP", SavedataType::Flash1M, HW_RTC, GBA_IDLE_LOOP_NONE),
    mk(b"AXPI", SavedataType::Flash1M, HW_RTC, GBA_IDLE_LOOP_NONE),
    mk(b"AXPS", SavedataType::Flash1M, HW_RTC, GBA_IDLE_LOOP_NONE),
    mk(b"AXPD", SavedataType::Flash1M, HW_RTC, GBA_IDLE_LOOP_NONE),
    mk(b"AXPF", SavedataType::Flash1M, HW_RTC, GBA_IDLE_LOOP_NONE),
    // Pokemon Emerald
    mk(b"BPEJ", SavedataType::Flash1M, HW_RTC, 0x80008C6),
    mk(b"BPEE", SavedataType::Flash1M, HW_RTC, 0x80008C6),
    mk(b"BPEP", SavedataType::Flash1M, HW_RTC, 0x80008C6),
    mk(b"BPEI", SavedataType::Flash1M, HW_RTC, 0x80008C6),
    mk(b"BPES", SavedataType::Flash1M, HW_RTC, 0x80008C6),
    mk(b"BPED", SavedataType::Flash1M, HW_RTC, 0x80008C6),
    mk(b"BPEF", SavedataType::Flash1M, HW_RTC, 0x80008C6),
    // Pokemon Mystery Dungeon
    mk(b"B24E", SavedataType::Flash1M, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"B24P", SavedataType::Flash1M, HW_NONE, GBA_IDLE_LOOP_NONE),
    // Pokemon FireRed
    mk(b"BPRJ", SavedataType::Flash1M, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"BPRE", SavedataType::Flash1M, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"BPRP", SavedataType::Flash1M, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"BPRI", SavedataType::Flash1M, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"BPRS", SavedataType::Flash1M, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"BPRD", SavedataType::Flash1M, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"BPRF", SavedataType::Flash1M, HW_NONE, GBA_IDLE_LOOP_NONE),
    // Pokemon LeafGreen
    mk(b"BPGJ", SavedataType::Flash1M, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"BPGE", SavedataType::Flash1M, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"BPGP", SavedataType::Flash1M, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"BPGI", SavedataType::Flash1M, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"BPGS", SavedataType::Flash1M, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"BPGD", SavedataType::Flash1M, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"BPGF", SavedataType::Flash1M, HW_NONE, GBA_IDLE_LOOP_NONE),
    // RockMan EXE 4.5 - Real Operation
    mk(b"BR4J", SavedataType::Flash512, HW_RTC, GBA_IDLE_LOOP_NONE),
    // Rocky
    mk(b"AR8E", SavedataType::Eeprom, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"AROP", SavedataType::Eeprom, HW_NONE, GBA_IDLE_LOOP_NONE),
    // Sennen Kazoku
    mk(b"BKAJ", SavedataType::Flash1M, HW_RTC, GBA_IDLE_LOOP_NONE),
    // Shin Bokura no Taiyou: Gyakushuu no Sabata
    mk(b"U33J", SavedataType::Eeprom, HW_RTC | HW_LIGHT_SENSOR, GBA_IDLE_LOOP_NONE),
    // Stuart Little 2
    mk(b"ASLE", SavedataType::ForceNone, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"ASLF", SavedataType::ForceNone, HW_NONE, GBA_IDLE_LOOP_NONE),
    // Super Mario Advance 2
    mk(b"AA2J", SavedataType::Eeprom, HW_NONE, 0x800052E),
    mk(b"AA2E", SavedataType::Eeprom, HW_NONE, 0x800052E),
    mk(b"AA2P", SavedataType::Autodetect, HW_NONE, 0x800052E),
    // Super Mario Advance 3
    mk(b"A3AJ", SavedataType::Eeprom, HW_NONE, 0x8002B9C),
    mk(b"A3AE", SavedataType::Eeprom, HW_NONE, 0x8002B9C),
    mk(b"A3AP", SavedataType::Eeprom, HW_NONE, 0x8002B9C),
    // Super Mario Advance 4
    mk(b"AX4J", SavedataType::Flash1M, HW_NONE, 0x800072A),
    mk(b"AX4E", SavedataType::Flash1M, HW_NONE, 0x800072A),
    mk(b"AX4P", SavedataType::Flash1M, HW_NONE, 0x800072A),
    // Super Monkey Ball Jr.
    mk(b"ALUE", SavedataType::Eeprom, HW_NONE, GBA_IDLE_LOOP_NONE),
    mk(b"ALUP", SavedataType::Eeprom, HW_NONE, GBA_IDLE_LOOP_NONE),
    // Top Gun - Combat Zones
    mk(b"A2YE", SavedataType::ForceNone, HW_NONE, GBA_IDLE_LOOP_NONE),
    // Ueki no Housoku - Jingi Sakuretsu! Nouryokusha Battle
    mk(b"BUHJ", SavedataType::Eeprom, HW_NONE, GBA_IDLE_LOOP_NONE),
    // Wario Ware Twisted
    mk(b"RZWJ", SavedataType::Sram, HW_RUMBLE | HW_GYRO, GBA_IDLE_LOOP_NONE),
    mk(b"RZWE", SavedataType::Sram, HW_RUMBLE | HW_GYRO, GBA_IDLE_LOOP_NONE),
    mk(b"RZWP", SavedataType::Sram, HW_RUMBLE | HW_GYRO, GBA_IDLE_LOOP_NONE),
    // Yoshi's Universal Gravitation
    mk(b"KYGJ", SavedataType::Eeprom, HW_TILT, GBA_IDLE_LOOP_NONE),
    mk(b"KYGE", SavedataType::Eeprom, HW_TILT, GBA_IDLE_LOOP_NONE),
    mk(b"KYGP", SavedataType::Eeprom, HW_TILT, GBA_IDLE_LOOP_NONE),
    // Aging cartridge
    mk(b"TCHK", SavedataType::Eeprom, HW_NONE, GBA_IDLE_LOOP_NONE),
];

/// GBAOverrideFind (static-table part; the config/ini lookup is not ported)
pub fn override_find(override_: &mut GbaOverride) -> bool {
    override_.savetype = SavedataType::Autodetect;
    override_.hardware = HW_NO_OVERRIDE;
    override_.idle_loop = GBA_IDLE_LOOP_NONE;
    override_.vba_bug_compat = false;
    let mut found = false;

    for o in OVERRIDES {
        if o.id == override_.id {
            *override_ = *o;
            found = true;
            break;
        }
    }
    if !found && override_.id[0] == b'F' {
        // Classic NES Series
        override_.savetype = SavedataType::Eeprom;
        found = true;
    }
    found
}

/// GBAOverrideApply
pub fn override_apply(gba: &mut Gba, override_: &GbaOverride) {
    if override_.savetype != SavedataType::Autodetect {
        gba.savedata_force_type(override_.savetype);
    }

    gba.vba_bug_compat = override_.vba_bug_compat;

    if override_.hardware != HW_NO_OVERRIDE {
        gba.hw_clear();
        gba.hw.devices &= !HW_NO_OVERRIDE;

        if override_.hardware & HW_RTC != 0 {
            gba.hw_init_rtc();
            // C also does GBASavedataRTCRead (RTC persisted in the save file);
            // this port leaves save persistence to the frontend.
        }

        if override_.hardware & HW_GYRO != 0 {
            gba.hw_init_gyro();
        }

        if override_.hardware & HW_RUMBLE != 0 {
            gba.hw_init_rumble();
        }

        if override_.hardware & HW_LIGHT_SENSOR != 0 {
            gba.hw_init_light();
        }

        if override_.hardware & HW_TILT != 0 {
            gba.hw_init_tilt();
        }

        if override_.hardware & HW_EREADER != 0 {
            gba.ereader_init();
        }

        if override_.hardware & HW_GB_PLAYER_DETECTION != 0 {
            gba.hw.devices |= HW_GB_PLAYER_DETECTION;
        } else {
            gba.hw.devices &= !HW_GB_PLAYER_DETECTION;
        }
    }

    if override_.idle_loop != GBA_IDLE_LOOP_NONE {
        gba.idle_loop = override_.idle_loop;
        if gba.idle_optimization == IDLE_LOOP_DETECT {
            gba.idle_optimization = IDLE_LOOP_REMOVE;
        }
    }
}

/// GBAOverrideApplyDefaults (config-file overrides not supported)
pub fn override_apply_defaults(gba: &mut Gba) {
    let mut override_ = GbaOverride {
        id: [0; 4],
        savetype: SavedataType::ForceNone, // C: zero-initialized GBACartridgeOverride
        hardware: HW_NONE,
        idle_loop: GBA_IDLE_LOOP_NONE,
        vba_bug_compat: false,
    };
    if gba.memory.rom.len() < 0xC0 {
        // Too small to hold a GBACartridge header.
        return;
    }
    if gba.unl.cart_type == crate::cart::unlicensed::UnlCartType::Multicart {
        override_.savetype = SavedataType::Sram;
        override_apply(gba, &override_);
        return;
    }

    override_.id.copy_from_slice(&gba.memory.rom[0xAC..0xB0]);

    // CRC32s of known Pokémon dumps (pokemonTable in the C)
    const POKEMON_TABLE: [u32; 17] = [
        // Emerald
        0x4881F3F8, // BPEJ
        0x8C4D3108, // BPES
        0x1F1C08FB, // BPEE
        0x34C9DF89, // BPED
        0xA3FDCCB1, // BPEF
        0xA0AEC80A, // BPEI
        // FireRed
        0x1A81EEDF, // BPRD
        0x3B2056E9, // BPRJ
        0x5DC668F6, // BPRF
        0x73A72167, // BPRI
        0x84EE4776, // BPRE rev 1
        0x9F08064E, // BPRS
        0xBB640DF7, // BPRJ rev 1
        0xDD88761C, // BPRE
        // Ruby
        0x61641576, // AXVE rev 1
        0xAEAC73E6, // AXVE rev 2
        0xF0815EE7, // AXVE
    ];

    let is_pokemon = {
        let rom = &gba.memory.rom;
        rom.get(0x108..0x108 + 20) == Some(b"pokemon red version\0" as &[u8])
            || rom.get(0x108..0x108 + 24) == Some(b"pokemon emerald version\0" as &[u8])
            || &rom[0xAC..0xB0] == b"AXVE"
    };
    let is_known_pokemon = is_pokemon && POKEMON_TABLE.contains(&gba.rom_crc32);

    if is_pokemon && !is_known_pokemon {
        // Enable FLASH1M and RTC on Pokémon ROM hacks
        override_.savetype = SavedataType::Flash1M;
        override_.hardware = HW_RTC;
        override_.vba_bug_compat = true;
        override_apply(gba, &override_);
    } else if override_find(&mut override_) {
        override_apply(gba, &override_);
    }
}
