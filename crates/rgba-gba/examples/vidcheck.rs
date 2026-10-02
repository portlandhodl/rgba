use rgba_gba::gba::Gba;

fn main() {
    let path = std::env::args().nth(1).expect("rom path");
    let rom = std::fs::read(path).expect("read");
    let mut g = *Gba::new();
    assert!(g.load_rom(rom));
    g.arm_reset();
    for _ in 0..18u32 {
        rgba_core::core::Core::run_frame(&mut g);
    }
    // single-step until we enter IntrMain (0x03002750..0x030028A5), trace regs
    let t0 = std::time::Instant::now();
    let mut in_intr = false;
    let mut count = 0u32;
    while t0.elapsed().as_secs() < 120 {
        let pc = (g.cpu.gprs[15] as u32).wrapping_sub(if matches!(g.cpu.execution_mode, rgba_gba::arm::ExecutionMode::Arm) { 4 } else { 4 });
        let raw_pc = g.cpu.gprs[15] as u32;
        let base = if raw_pc >= 0x03002750 && raw_pc < 0x030028B0 { raw_pc } else { 0 };
        if base != 0 && !in_intr {
            in_intr = true;
            eprintln!("=== entering IntrMain region, IE={:04X} IF={:04X}", g.memory.io[(0x200>>1) as usize], g.memory.io[(0x202>>1) as usize]);
        }
        if in_intr {
            let r = &g.cpu.gprs;
            eprintln!("pc={:08X} r0={:08X} r1={:08X} r2={:08X} r3={:08X} r12={:08X} IF={:04X} IE={:04X}",
                raw_pc, r[0] as u32, r[1] as u32, r[2] as u32, r[3] as u32, r[12] as u32,
                g.memory.io[(0x202>>1) as usize], g.memory.io[(0x200>>1) as usize]);
            count += 1;
            if raw_pc == 0x03002828 || count > 80 {
                eprintln!("=== leaving trace at pc={:08X}", raw_pc);
                break;
            }
        }
        let _ = pc;
        g.step();
    }
}
