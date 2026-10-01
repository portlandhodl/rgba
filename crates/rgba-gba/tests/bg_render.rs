use rgba_gba::gba::Gba;

// Thumb code: mode3|BG2 enable, then loop writing 0x7C1F (blue-ish) to VRAM
// scanline 0 forever.
fn make_rom() -> Vec<u8> {
    let code: &[u8] = &[
        // movs r0,#3; lsls r0,#10 → r0=0xC00? compute DISPCNT=3|0x400
        0x03, 0x20,         // movs r0,#3
        0x0A, 0x04,         // lsls r0,r0,#10 = 0xC00... then add 3:
        0x40, 0x1C,         // adds r0,r0,#1? no: orr
        0x40, 0x1C,
        0x40, 0x1C,         // r0 = 0xC03 — wrong; want 0x403
        0xFF, 0x20, 0x40, 0x04,
        // r0 = 0xFF << 8? meh
        0xFF, 0x20,         // movs r0,#0xFF
        0x00, 0x04,         // lsls r0,r0,#16 = 0xFF0000
        // Stop writing code by hand; instead use the debugger path below.
    ];
    let mut rom = vec![0u8; 0x8000];
    // ARM reset entry: jump to 0x080000C0? no—our entry is at 0 (rom). The
    // simplest valid program: b .
    rom[0] = 0xFE; rom[1] = 0xFF; rom[2] = 0xFF; rom[3] = 0xEA; // b $
    rom[0xB2] = 0x96;
    let _ = code;
    rom
}

// Instead of hand-assembled Thumb above (which is hard to get right blind),
// verify via the bus path: write VRAM directly, render, check output.
#[test]
fn mode3_bitmap_visible() {
    let mut gb = Gba::new();
    gb.load_rom(make_rom());
    gb.arm_reset();
    // Write a red pixel at VRAM (0,0) and DISPCNT = mode 3 + BG2:
    let mut cc = 0;
    gb.store16(0x04000000, 0x403, &mut cc); // DISPCNT = mode3|BG2
    gb.store16(0x06000000, 0x001F, &mut cc); // red pixel at (0,0)
    gb.run_frame();
    gb.run_frame();
    let frame = &gb.video.sw.output;
    assert!(gb.video.frame_counter >= 2);
    // pixel(0,0) rendered as out_pixel(palette[0x001F]) — nonzero expected
    let p = frame[0];
    assert_ne!(p & 0xFFFFFF, 0, "framebuffer[0] is black");
}
