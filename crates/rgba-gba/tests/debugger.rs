// Tests for the ARM debugger platform (hardware/sw breakpoints, watchpoints).
use rgba_debugger::debugger::*;
use rgba_debugger::DebugConsole;
use rgba_gba::gba::Gba;

// mov r0, #1 / mov r1, #2 / b .
fn rom() -> Vec<u8> {
    let mut r = vec![0u8; 0x8000];
    r[0..4].copy_from_slice(&[0x01, 0x00, 0xA0, 0xE3]); // mov r0, #1
    r[4..8].copy_from_slice(&[0x02, 0x10, 0xA0, 0xE3]); // mov r1, #2
    r[8..12].copy_from_slice(&[0xFE, 0xFF, 0xFF, 0xEA]); // b .
    r[0xB2] = 0x96; // complement check-ish header byte like other tests
    r
}

struct Rec {
    entries: Vec<(DebuggerEntryReason, u32, i64)>,
    paused: bool,
}

impl DebuggerModule for Rec {
    fn module_type(&self) -> DebuggerType {
        DebuggerType::Custom
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn is_paused(&self) -> bool {
        self.paused
    }
    fn set_paused(&mut self, p: bool) {
        self.paused = p;
    }
    fn entered(
        &mut self,
        _debugger: &mut Debugger,
        _console: &mut dyn DebugConsole,
        reason: DebuggerEntryReason,
        info: Option<&DebuggerEntryInfo>,
    ) {
        let info = info.cloned().unwrap_or_default();
        self.entries.push((reason, info.address, info.point_id));
    }
}

fn attach(gba: &mut Gba) -> usize {
    gba.debugger_attach();
    let mut dbg = gba.debugger.take().unwrap();
    let idx = dbg.core.attach_module(
        &mut *gba,
        Box::new(Rec {
            entries: vec![],
            paused: false,
        }),
    );
    gba.debugger = Some(dbg);
    idx
}

fn entry_count(gba: &mut Gba) -> usize {
    let mut dbg = gba.debugger.take().unwrap();
    let n = dbg.core.modules[0]
        .as_ref()
        .unwrap()
        .as_any()
        .downcast_ref::<Rec>()
        .unwrap()
        .entries
        .len();
    gba.debugger = Some(dbg);
    n
}

fn last_entry(gba: &mut Gba) -> Option<(DebuggerEntryReason, u32, i64)> {
    let mut dbg = gba.debugger.take().unwrap();
    let e = dbg.core.modules[0]
        .as_ref()
        .unwrap()
        .as_any()
        .downcast_ref::<Rec>()
        .unwrap()
        .entries
        .last()
        .copied();
    gba.debugger = Some(dbg);
    e
}

fn unpause(gba: &mut Gba) {
    let mut dbg = gba.debugger.take().unwrap();
    if let Some(m) = dbg.core.modules[0].as_mut() {
        m.as_any_mut().downcast_mut::<Rec>().unwrap().paused = false;
        m.set_paused(false);
    }
    dbg.core.update_paused(&mut *gba);
    gba.debugger = Some(dbg);
}

#[test]
fn arm_hardware_breakpoint() {
    let mut gba = Gba::new();
    gba.load_rom(rom());
    gba.arm_reset();
    attach(&mut gba);

    assert!(gba.dbg_set_breakpoint(
        None,
        &Breakpoint {
            id: -1,
            address: 0x08000004,
            segment: -1,
            ty: BreakpointType::Hardware,
            condition: None,
            disabled: false,
            is_temporary: false,
        }
    ) > 0);

    gba.dbg_step(); // mov r0, #1
    assert_eq!(gba.cpu.gprs[0], 1);
    gba.dbg_check_breakpoints(); // about to execute breakpoint at ..04
    assert_eq!(entry_count(&mut gba), 1);
    let (reason, addr, _id) = last_entry(&mut gba).unwrap();
    assert_eq!(reason, DebuggerEntryReason::Breakpoint);
    assert_eq!(addr, 0x08000004);
    unpause(&mut gba);
    // Continue: execute mov r1, #2
    gba.dbg_step();
    assert_eq!(gba.cpu.gprs[1], 2);
}

#[test]
fn arm_software_breakpoint() {
    let mut gba = Gba::new();
    gba.load_rom(rom());
    gba.arm_reset();
    attach(&mut gba);

    // Breakpoint goes on the `b .` — it is re-fetched every iteration
    // (the C likewise relies on the instruction being fetched post-patch).
    let id = gba.dbg_set_software_breakpoint(None, 0x08000008, false);
    assert!(id > 0);
    // The ROM word is now a BKPT encoding (0xE1200070 | component id)
    assert_eq!(gba.view32(0x08000008), 0xE1200070);

    gba.dbg_step(); // mov r0, #1
    gba.dbg_step(); // mov r1, #2
    gba.dbg_step(); // b . (fetch loop starts; BP at 0x8 pending)
    gba.dbg_step(); // executes BKPT patch -> enters debugger
    assert_eq!(entry_count(&mut gba), 1);
    let (reason, addr, point_id) = last_entry(&mut gba).unwrap();
    assert_eq!(reason, DebuggerEntryReason::Breakpoint);
    assert_eq!(addr, 0x08000008);
    assert_eq!(point_id, id);
    unpause(&mut gba);
    // Breakpoint was re-patched after ARMRunFake staged the original
    assert_eq!(gba.view32(0x08000008), 0xE1200070);
    // Clear it: original opcode back
    assert!(gba.dbg_clear_breakpoint(id));
    assert_eq!(gba.view32(0x08000008), 0xEAFFFFFE);
    assert_eq!(gba.cpu.gprs[0], 1);
    assert_eq!(gba.cpu.gprs[1], 2);
}

#[test]
fn arm_write_watchpoint() {
    let mut gba = Gba::new();
    gba.load_rom(rom());
    gba.arm_reset();
    attach(&mut gba);

    let id = gba.dbg_set_watchpoint(
        None,
        &Watchpoint {
            id: -1,
            segment: -1,
            min_address: 0x02000100,
            max_address: 0x02000104,
            ty: watchpoint_type::READ | watchpoint_type::WRITE,
            condition: None,
            disabled: false,
        },
    );
    assert!(id > 0);
    assert!(gba.dbg_watchpoints_active);

    gba.store32(0x02000100, 0x1234, &mut 0);
    assert_eq!(entry_count(&mut gba), 1);
    let (reason, addr, point_id) = last_entry(&mut gba).unwrap();
    assert_eq!(reason, DebuggerEntryReason::Watchpoint);
    assert_eq!(addr, 0x02000100);
    assert_eq!(point_id, id);
    // The write still went through
    assert_eq!(gba.view32(0x02000100), 0x1234);

    unpause(&mut gba);
    assert!(gba.dbg_clear_breakpoint(id));
    assert!(!gba.dbg_watchpoints_active);
    gba.store32(0x02000100, 0x5678, &mut 0);
    assert_eq!(entry_count(&mut gba), 1);
}

// Access-log test: logger watches EWRAM (writes) and ROM (execution);
// the console is driven through the debugger run path so both the
// memory-shim recorders and the per-step exec callback (`custom`) run.
#[test]
fn access_logger_records_writes_and_execution() {
    use rgba_debugger::access_logger::{access_log_region_flags, AccessLogger, MAL_PLATFORM_GBA};

    let mut r = vec![0u8; 0x8000];
    // mov r0, #0x02000000     ; E3A00402
    r[0..4].copy_from_slice(&[0x02, 0x04, 0xA0, 0xE3]);
    // mov r1, #0xAB           ; E3A010AB
    r[4..8].copy_from_slice(&[0xAB, 0x10, 0xA0, 0xE3]);
    // str r1, [r0]            ; E5801000
    r[8..12].copy_from_slice(&[0x00, 0x10, 0x80, 0xE5]);
    // b .                     ; EAFFFFFE
    r[12..16].copy_from_slice(&[0xFE, 0xFF, 0xFF, 0xEA]);

    let mut gba = Gba::new();
    gba.load_rom(r);
    gba.arm_reset();
    gba.debugger_attach();
    let idx = {
        let mut dbg = gba.debugger.take().unwrap();
        let idx = dbg
            .core
            .attach_module(&mut *gba, Box::new(AccessLogger::new(MAL_PLATFORM_GBA)));
        gba.debugger = Some(dbg);
        idx
    };

    let mut module = {
        let mut dbg = gba.debugger.take().unwrap();
        let m = dbg.core.modules[idx].take().unwrap();
        gba.debugger = Some(dbg);
        m
    };
    {
        let logger = module.as_any_mut().downcast_mut::<AccessLogger>().unwrap();
        assert!(logger.watch_memory_block_name(&mut *gba, "wram", access_log_region_flags::HAS_EX_BLOCK) >= 0);
        assert!(logger.watch_memory_block_name(&mut *gba, "cart0", access_log_region_flags::HAS_EX_BLOCK) >= 0);
        assert!(logger.start(&mut *gba));
    }
    {
        let mut dbg = gba.debugger.take().unwrap();
        dbg.core.modules[idx] = Some(module);
        dbg.core.set_module_needs_callback(&mut *gba, idx);
        gba.debugger = Some(dbg);
    }
    assert!(gba.dbg_watchpoints_active); // shim active due to the logger

    // Direct store through the bus while resident (watchpoint-test style).
    gba.store32(0x02000100, 0x12345678, &mut 0);
    {
        let log = rgba_debugger::DebugConsole::dbg_access_log(&mut *gba)
            .expect("logger core installed in console");
        let (ri, off) = log.get_region(0x02000100, 0).unwrap();
        assert_eq!(off, 0x100);
        let r = &log.regions[ri];
        assert_eq!(
            r.block[0x100],
            access_log_flags::WRITE | access_log_flags::ACCESS32
        );
        assert_eq!(r.block[0x103], access_log_flags::WRITE | access_log_flags::ACCESS32);
        assert_eq!(r.block[0x104], 0);
    }

    // Drive a few instructions through mDebuggerRunTimeout: each step runs
    // the shim record hooks and then the exec-info callback.
    for _ in 0..8 {
        gba.debugger_run_timeout(50);
    }
    {
        let log = rgba_debugger::DebugConsole::dbg_access_log(&mut *gba).unwrap();
        // str r1, [r0] -> EWRAM[0]
        let (ri, _) = log.get_region(0x02000000, 0).unwrap();
        assert_eq!(
            log.regions[ri].block[0],
            access_log_flags::WRITE | access_log_flags::ACCESS32
        );
        // Instructions at 0x08000004.. were executed (EXECUTE + ACCESS32,
        // ex flags EXECUTE_ARM).
        // NB: like the C (which logs gprs[PC]-width AFTER the step), the
        // marker runs one instruction ahead of execution: offset 0 is
        // never hit, the `b .` at 0xC is marked on every loop iteration.
        let (ri, _) = log.get_region(0x08000000, 0).unwrap();
        let r = &log.regions[ri];
        assert_eq!(r.block[0], 0);
        for pc in [4usize, 8, 12] {
            assert_eq!(
                r.block[pc] & (access_log_flags::EXECUTE | access_log_flags::ACCESS32),
                access_log_flags::EXECUTE | access_log_flags::ACCESS32,
                "instruction at {pc:#x} marked executed"
            );
            assert_eq!(
                r.block_ex.as_ref().unwrap()[pc] & access_log_flags_ex::EXECUTE_ARM,
                access_log_flags_ex::EXECUTE_ARM,
                "instruction at {pc:#x} marked as ARM-mode"
            );
        }
    }

    // Stop takes the core back into the module; serialization round-trips.
    let mut module = {
        let mut dbg = gba.debugger.take().unwrap();
        let m = dbg.core.modules[idx].take().unwrap();
        gba.debugger = Some(dbg);
        m
    };
    {
        let logger = module.as_any_mut().downcast_mut::<AccessLogger>().unwrap();
        logger.stop(&mut *gba);
        let data = logger.save_bytes().unwrap();
        assert_eq!(&data[0..4], b"mAL\x01");
        assert_eq!(data[0x10], 2); // two regions in the header
        let mut logger2 = AccessLogger::new(MAL_PLATFORM_GBA);
        assert!(logger2.open_bytes(Some(&data), false, false));
        let core2 = logger2.access_log().unwrap();
        let (ri, _) = core2.get_region(0x02000000, 0).unwrap();
        assert_eq!(
            core2.regions[ri].block[0],
            access_log_flags::WRITE | access_log_flags::ACCESS32
        );
    }
    {
        let mut dbg = gba.debugger.take().unwrap();
        dbg.core.clear_module_needs_callback(&mut *gba, idx);
        dbg.core.modules[idx] = Some(module);
        gba.debugger = Some(dbg);
    }
    assert!(!gba.dbg_watchpoints_active);
}



// GBA GameShark hook cheat lifecycle: cheat_add_set patches a Thumb BKPT
// (0xBE00|1) into the hooked address, cheat_remove_set restores the old
// bytes. (Firing the hook runs through the same BKPT dispatch path that the
// software breakpoint tests exercise; hooked cheat sets typically hook
// Thumb-compiled ROM symbols.)
#[test]
fn gsa_hook_patch_and_restore() {
    use rgba_gba::cheats::gba_cheat_set_create_with_hook;
    use rgba_gba::gba::Gba;
    let mut rom_img = vec![0u8; 0x8000];
    // Thumb nop sled at 0x8000000: movs r0, #0 (0x2000)
    for off in (0..16).step_by(2) {
        rom_img[off..off + 2].copy_from_slice(&[0x00, 0x20]);
    }
    rom_img[0xB2] = 0x96;
    let mut gba = Gba::new();
    gba.load_rom(rom_img);
    gba.arm_reset();
    let set = rgba_core::cheats::CheatSet::new("hook test");
    let state = gba_cheat_set_create_with_hook(0x08000004);
    gba.cheat_add_set(set, state);
    assert_eq!(gba.view16(0x08000004), 0xBE01);
    gba.cheat_remove_set(0);
    assert_eq!(gba.view16(0x08000004), 0x2000);
}
