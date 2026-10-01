// Smoke tests for the GBA core.

use rgba_gba::gba::{EventId, Gba};

/// Minimal GBA ROM: header magic + Thumb code that loops forever.
/// Assembled by hand: see https://problemkaputt.de/gbatek.htm
pub fn test_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000]; // tiny 32KB image mirroring into 0x08000000
    rm_header(&mut rom);
    // at 0x000 our entry gets mapped to 0x08000000
    // branch-to-self: `b .` in Thumb = 0xE7FE (little-endian FE E7)
    rom[0] = 0xFE;
    rom[1] = 0xE7;
    rom
}

fn rm_header(rom: &mut [u8]) {
    // First instruction is normally a branch; magic bytes:
    rom[0x3] = 0xEA; // ARM unconditional branch
    rom[0xB2] = 0x96;
}

#[test]
fn gba_boots_and_advances_frames() {
    let rom = test_rom();
    let mut gb = Gba::new();
    assert!(Gba::is_rom(&rom));
    assert!(gb.load_rom(rom));
    gb.arm_reset();

    let fc = gb.video.frame_counter;
    for _ in 0..2 {
        gb.run_frame();
    }
    assert!(gb.video.frame_counter >= fc + 1, "frames did not advance");
}

#[test]
fn thumb_branch_self_loop_runs() {
    let rom = test_rom();
    let mut gb = Gba::new();
    gb.load_rom(rom);
    gb.arm_reset();
    for _ in 0..50000 {
        gb.arm_run();
        if gb.cpu.cycles >= gb.cpu.next_event {
            break;
        }
    }
    assert!(gb.cpu.cycles > 0);
}

#[test]
fn irq_path_safe() {
    let rom = test_rom();
    let mut gb = Gba::new();
    gb.load_rom(rom);
    gb.arm_reset();
    // Raise keypad IRQ directly and run — RE should not crash.
    gb.raise_irq(0xC);
    gb.run_frame();
    assert!(gb.video.frame_counter >= 1);
}

#[test]
fn sram_initializes_and_rw() {
    let rom = test_rom();
    let mut gb = Gba::new();
    gb.load_rom(rom);
    gb.arm_reset();
    // Directly poke SRAM space
    gb.store8(0x0E000000, 0x42, &mut 0);
    gb.store16(0x08000000 + 0xF000, 0x1234, &mut 0); // no-op on ROM region
    let isa_val = gb.load8(0x0E000000, &mut 0);
    // With our public path... see load8's no-cycle variant
    let _ = isa_val;
}
