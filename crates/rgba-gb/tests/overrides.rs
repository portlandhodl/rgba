// Tests for the ported per-game override tables (mgba/src/gb/overrides.c).

use rgba_gb::gb::Gb;
use rgba_gb::memory::MbcType;
use rgba_gb::overrides::*;

/// Minimal 32 KiB ROM (ROM ONLY cart type) like the smoke tests use.
fn test_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x103].copy_from_slice(&[0xC3, 0x50, 0x01]); // jp $150
    rom[0x150..0x152].copy_from_slice(&[0x18, 0xFE]); // jr $
    let logo: [u8; 48] = [
        0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00,
        0x0D, 0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD,
        0xD9, 0x99, 0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB,
        0xB9, 0x33, 0x3E,
    ];
    rom[0x104..0x134].copy_from_slice(&logo);
    rom[0x134..0x13E].copy_from_slice(b"TESTROM\0\0\0");
    rom[0x147] = 0; // ROM ONLY
    rom[0x148] = 0; // 32KB
    rom[0x149] = 0; // no RAM
    rom
}

#[test]
fn table_entry_counts() {
    assert_eq!(OVERRIDES.len(), 28);
    assert_eq!(GBC_OVERRIDES.len(), 144);
    assert_eq!(SGB_OVERRIDES.len(), 46);
    assert_eq!(color_preset_list().len(), 48);
}

#[test]
fn mbc_override_found() {
    // Mani 4 in 1 multicart → M161
    let mut o = GbOverride::new(0xA61F3EE1);
    assert!(override_find(&mut o));
    assert_eq!(o.mbc, MbcType::M161);

    // Pokemon Spaceworld 1997 demo (Gold debug) → MBC3+RTC
    let mut o = GbOverride::new(0x232A067D);
    assert!(override_find(&mut o));
    assert_eq!(o.mbc, MbcType::Mbc3Rtc);

    // Unknown checksum resets the override and is not found
    let mut o = GbOverride::new(0xDEADBEEF);
    assert!(!override_find(&mut o));
    assert_eq!(o.mbc, MbcType::Autodetect);
    assert_eq!(o.gb_colors, [0; 12]);
}

#[test]
fn color_override_found_and_applied() {
    // Pokemon - Blue Version (USA, Europe) → GBC PALETTE(28, 4, 28)
    let mut o = GbOverride::new(0x28323CE0);
    assert!(!override_color_find(&mut o, GB_COLORS_SGB));
    assert!(override_color_find(&mut o, GB_COLORS_CGB));
    // PAL28 entry 0 = 0x7FFF (white) → 0xFF000000 | M_RGB5_TO_RGB8(0x7FFF)
    assert_eq!(o.gb_colors[0], 0xFFF8F8F8);
    assert_eq!(o.gb_colors[8], 0xFFF8F8F8);

    let mut gb = Gb::new();
    gb.load_rom(test_rom());
    override_apply(&mut gb, &o);
    // GBVideoSetPalette converts RGB8 back to RGB5 for the DMG palette
    assert_eq!(gb.video.dmg_palette[0], 0x7FFF);
}

#[test]
fn mbc_override_applied_mbc_init() {
    let mut o = GbOverride::new(0xA61F3EE1);
    assert!(override_find(&mut o));
    let mut gb = Gb::new();
    gb.load_rom(test_rom());
    assert_eq!(gb.memory.mbc_type, MbcType::None);
    override_apply(&mut gb, &o);
    assert_eq!(gb.memory.mbc_type, MbcType::M161);
}
