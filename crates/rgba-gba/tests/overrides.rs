// Tests for the ported per-game override table (mgba/src/gba/overrides.c).

use rgba_gba::gba::{Gba, GBA_IDLE_LOOP_NONE, HW_GPIO, HW_NO_OVERRIDE, HW_RTC, HW_RUMBLE};
use rgba_gba::overrides::OVERRIDES;
use rgba_gba::savedata::SavedataType;

/// Minimal 32 KiB ROM image carrying just a game code in the header
/// (`struct GBACartridge.id` at 0xAC).
fn fake_rom(code: &[u8; 4]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0xAC..0xB0].copy_from_slice(code);
    rom
}

#[test]
fn table_entry_count() {
    assert_eq!(OVERRIDES.len(), 104);
}

#[test]
fn pokemon_ruby_flash1m_rtc() {
    // "AXVE" (Pokemon Ruby) → FLASH1M + RTC
    let mut gba = Gba::new();
    gba.load_rom(fake_rom(b"AXVE"));
    assert_eq!(gba.savedata.savedata_type, SavedataType::Flash1M);
    assert_ne!(gba.hw_devices() & HW_RTC, 0);
}

#[test]
fn drill_dozer_sram_rumble() {
    // "V49E" (Drill Dozer) → SRAM + rumble, no RTC
    let mut gba = Gba::new();
    gba.load_rom(fake_rom(b"V49E"));
    assert_eq!(gba.savedata.savedata_type, SavedataType::Sram);
    assert_ne!(gba.hw_devices() & HW_RUMBLE, 0);
    assert_eq!(gba.hw_devices() & HW_RTC, 0);
}

#[test]
fn super_mario_advance_2_idle_loop() {
    // "AA2E" (Super Mario Advance 2) → EEPROM, idle loop 0x800052E, no hardware
    let mut gba = Gba::new();
    gba.load_rom(fake_rom(b"AA2E"));
    assert_eq!(gba.savedata.savedata_type, SavedataType::Eeprom);
    assert_eq!(gba.idle_loop, 0x800052E);
    assert_eq!(gba.hw_devices() & HW_GPIO, 0);
}

#[test]
fn unknown_game_keeps_autodetect() {
    let mut gba = Gba::new();
    gba.load_rom(fake_rom(b"ZZZZ"));
    assert_eq!(gba.savedata.savedata_type, SavedataType::Autodetect);
    assert_eq!(gba.hw_devices(), HW_NO_OVERRIDE);
    assert_eq!(gba.idle_loop, GBA_IDLE_LOOP_NONE);
}

#[test]
fn classic_nes_series_eeprom() {
    // GBAOverrideFind: game codes starting with 'F' force EEPROM
    let mut gba = Gba::new();
    gba.load_rom(fake_rom(b"FANE"));
    assert_eq!(gba.savedata.savedata_type, SavedataType::Eeprom);
}

#[test]
fn iridion_force_none() {
    // "AI2E" (Iridion II) → explicitly no savedata
    let mut gba = Gba::new();
    gba.load_rom(fake_rom(b"AI2E"));
    assert_eq!(gba.savedata.savedata_type, SavedataType::ForceNone);
}
