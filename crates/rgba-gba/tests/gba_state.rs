use rgba_gba::gba::Gba;

fn rom() -> Vec<u8> {
    let mut r = vec![0u8; 0x8000];
    r[0] = 0xFE;
    r[1] = 0xFF;
    r[2] = 0xFF;
    r[3] = 0xEA;
    r[0xB2] = 0x96;
    r
}

#[test]
fn gba_savestate_roundtrip() {
    let mut gb = Gba::new();
    gb.load_rom(rom());
    gb.arm_reset();
    gb.run_frame();
    gb.store32(0x02000000, 0xDEADBEEFu32 as i32, &mut 0);
    let st = rgba_gba::serialize::serialize(&mut gb).expect("serialize");
    gb.store32(0x02000000, 0x12345678, &mut 0);
    rgba_gba::serialize::deserialize(&mut gb, &st).expect("deserialize");
    assert_eq!(gb.load32(0x02000000, &mut 0) as u32, 0xDEADBEEF);
}

#[test]
fn gba_savestate_invalid() {
    let mut gb = Gba::new();
    gb.load_rom(rom());
    gb.arm_reset();
    let bad = vec![0u8; 16];
    assert!(rgba_gba::serialize::deserialize(&mut gb, &bad).is_err());
}
