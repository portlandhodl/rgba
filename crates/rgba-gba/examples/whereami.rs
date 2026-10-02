use rgba_gba::gba::Gba;
fn main() {
    let rom = std::fs::read(std::env::args().nth(1).expect("rom")).unwrap();
    let mut g = Gba::new();
    g.load_rom(rom);
    g.arm_reset();
    for _ in 0..2_000 { g.step(); }
    eprintln!("t={} pc={:08X}", g.timing.current_time(), g.cpu.gprs[15]);
    let mut last = 0i64;
    let mut stuck = 0u64;
    let mut path: Vec<i32> = Vec::new();
    for n in 0..50_000_000u64 {
        let pc = g.cpu.gprs[15];
        g.step();
        if g.cpu.gprs[15] == pc { continue; }
        if path.len() < 64 { path.push(pc); } else { path[n as usize % 64] = pc; }
        if pc as i64 == last { stuck += 1; } else { stuck = 0; }
        last = pc as i64;
        if stuck > 200_000 {
            eprintln!("stuck at step {} pc={:08X} IF={:04X} IE={:04X} IME={:02X} halt={} t={}",
                n, pc, g.memory.io[(0x202u32>>1) as usize], g.memory.io[(0x208u32>>1) as usize],
                g.memory.io[(0x200u32>>1) as usize], g.cpu.halted, g.timing.current_time());
            break;
        }
    }
}
