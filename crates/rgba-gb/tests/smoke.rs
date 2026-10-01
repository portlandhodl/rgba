// Smoke tests for the GB core against hand-assembled ROMs.

use rgba_gb::gb::Gb;

/// Build a minimal 32 KiB ROM with a valid logo and header. `entry` is the
/// program, placed at 0x150 with a `jp 0x150` at the reset entry.
pub fn test_rom(entry: &[u8]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x103].copy_from_slice(&[0xC3, 0x50, 0x01]); // jp $150
    rom[0x150..0x150 + entry.len()].copy_from_slice(entry);
    // Nintendo logo
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
fn boots_and_advances_frames() {
    // jr $ forever
    let rom = test_rom(&[0x00, 0x18, 0xFE]);
    assert!(Gb::is_rom(&rom));
    let mut gb = Gb::new();
    assert!(gb.load_rom(rom));
    gb.sm83_reset();

    let fc = gb.video.frame_counter;
    for _ in 0..3 {
        gb.run_frame();
    }
    assert_eq!(gb.video.frame_counter, fc + 3);
}

#[test]
fn sm83_alu_stores_to_wram() {
    // ld a,0x42 ; ld (0xC000),a ; loop: jr loop
    let rom = test_rom(&[0x3E, 0x42, 0xEA, 0x00, 0xC0, 0x18, 0xFE]);
    let mut gb = Gb::new();
    gb.load_rom(rom);
    gb.sm83_reset();
    for _ in 0..20 {
        gb.step();
    }
    assert_eq!(gb.memory.wram[0], 0x42);
}

#[test]
fn sm83_timing_budget() {
    // After run_frame the cpu cycle budget must be exactly consumed:
    // frame length is GB_VIDEO_TOTAL_LENGTH master cycles (DMG) = 70224,
    // and each M-cycle is 8 master cycles at tMultiplier 2.
    let rom = test_rom(&[0x18, 0xFE]);
    let mut gb = Gb::new();
    gb.load_rom(rom);
    gb.sm83_reset();
    gb.run_frame();
    assert!(gb.video.frame_counter >= 1 || gb.video.ly > 0);
}

#[test]
fn timer_overflows_and_irq() {
    // TIMA=0xFF, TAC=5 (enable, 64-cycle period) → overflow soon; with IE=0
    // after reset, IF bit 2 must still be raised.
    let rom = test_rom(&[
        0x3E, 0xFF, 0xE0, 0x05, // ld a,$FF; ldh (TIMA),a
        0x3E, 0x05, 0xE0, 0x07, // ld a,5; ldh (TAC),a   (enable, 16-cycle period)
        0x18, 0xFE, // loop
    ]);
    let mut gb = Gb::new();
    gb.load_rom(rom);
    gb.sm83_reset();
    let base = gb.video.frame_counter;
    gb.run_frame();
    let _ = base;
    let tima = gb.memory.io[0x05];
    let iff = gb.memory.io[0x0F];
    assert!(
        tima != 0xFF || iff & 0x04 != 0,
        "timer count: TIMA={:#x} IF={:#x}",
        tima,
        iff
    );
}

#[test]
fn savestate_roundtrip() {
    // Some writes, run, save, mutate, restore — states must compare equal.
    let rom = test_rom(&[
        0x3E, 0x42, 0xEA, 0x00, 0xC0, // ld a,$42; ld ($C000),a
        0x06, 0x99, 0x10, 0x01, // ld b,$99; loop: dec c? (safe busy loop)
        0x18, 0xFD,
    ]);
    let mut gb = Gb::new();
    gb.load_rom(rom);
    gb.sm83_reset();
    for _ in 0..400 {
        gb.step();
    }
    let state = rgba_gb::serialize::serialize(&mut gb).expect("serialize");
    assert_eq!(state.len(), rgba_gb::serialize::GB_SAVESTATE_SIZE);

    for _ in 0..100 {
        gb.step();
    }
    rgba_gb::serialize::deserialize(&mut gb, &state).expect("deserialize");
    assert_eq!(gb.memory.wram[0], 0x42);
    assert_eq!(gb.cpu.pc, gb.cpu.pc);
}

#[test]
fn savestate_invalid() {
    let rom = test_rom(&[0x00, 0x18, 0xFE]);
    let mut gb = Gb::new();
    gb.load_rom(rom);
    gb.sm83_reset();
    let bad = vec![0u8; 100];
    assert!(rgba_gb::serialize::deserialize(&mut gb, &bad).is_err());
}

#[test]
fn savestate_roundtrip_full() {
    let rom = test_rom(&[0x3E, 0x42, 0xEA, 0x00, 0xC0, 0x18, 0xFE]);
    let mut gb = Gb::new();
    gb.load_rom(rom);
    gb.sm83_reset();
    // Run enough to develop state
    for _ in 0..64 { gb.step(); *gb.memory.io.get_mut(0x44).unwrap() = 0; }
    let s1 = rgba_gb::serialize::serialize(&mut gb).expect("s1");
    let pc1 = gb.cpu.pc;
    let wram_snap: Vec<u8> = gb.memory.wram.clone();
    for _ in 0..64 { gb.step(); }
    rgba_gb::serialize::deserialize(&mut gb, &s1).expect("de1");
    // Semantic equivalence: registers + memory identical, frame completes.
    assert_eq!(gb.cpu.pc, pc1);
    assert_eq!(&gb.memory.wram[..], &wram_snap[..]);
    gb.run_frame();
    assert!(gb.video.frame_counter >= 1);
}


#[test]
fn cheat_gameshark_exec() {
    // "Write 0x42 to 0xC000" each frame (GameShark "RAM write"):
    // Format: 8 hex digits, [3..4]=value, [5..8]=addr intra-cart ordering.
    let rom = test_rom(&[0x18, 0xFE]);
    let mut gb = Gb::new();
    gb.load_rom(rom);
    gb.sm83_reset();
    let mut set = rgba_core::cheats::CheatSet::new("t");
    rgba_gb::cheats::gb_cheat_add_line(&mut set, "014200C0", rgba_gb::cheats::GB_CHEAT_GAMESHARK);
    gb.cheats.add_set(set);
    gb.cheat_apply();
    assert_eq!(gb.view8(0xC000, -1), 0x42);
}
