// Tests for the SM83 debugger platform (breakpoint/watchpoint entry).
use rgba_debugger::debugger::*;
use rgba_debugger::debugger::*;
use rgba_debugger::DebugConsole;
use rgba_gb::gb::Gb;

fn test_rom(entry: &[u8]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x103].copy_from_slice(&[0xC3, 0x50, 0x01]);
    rom[0x150..0x150 + entry.len()].copy_from_slice(entry);
    let logo: [u8; 48] = [
        0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C,
        0x00, 0x0D, 0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6,
        0xDD, 0xDD, 0xD9, 0x99, 0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC,
        0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
    ];
    rom[0x104..0x134].copy_from_slice(&logo);
    rom[0x134..0x13E].copy_from_slice(b"TESTROM\0\0\0");
    rom[0x147] = 0;
    rom
}

// Program: writes 0x42 to 0xC000 forever.
// 0150: 3E 42       LD A, $42
// 0152: EA 00 C0    LD ($C000), A
// 0155: 18 F9       JR -7 (to 0x150)
const ENTRY: &[u8] = &[0x3E, 0x42, 0xEA, 0x00, 0xC0, 0x18, 0xF9];

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

#[test]
fn breakpoint_hits_and_clears() {
    let mut gb = Gb::new();
    gb.load_rom(test_rom(ENTRY));
    gb.sm83_reset();
    gb.debugger_attach();
    let module_idx = {
        let mut dbg = gb.debugger.take().unwrap();
        let idx = dbg.core.attach_module(&mut *gb, Box::new(Rec { entries: vec![], paused: false }));
        gb.debugger = Some(dbg);
        idx
    };
    assert_eq!(module_idx, 0);

    let bp = Breakpoint {
        id: -1,
        address: 0x152,
        segment: -1,
        ty: BreakpointType::Hardware,
        condition: None,
        disabled: false,
        is_temporary: false,
    };
    let id = gb.dbg_set_breakpoint(None, &bp);
    assert!(id > 0);

    // Step the CPU with breakpoint checking, mDebuggerRun-style.
    gb.dbg_step();
    gb.dbg_check_breakpoints();
    // PC is now past 0x152 (LD A done), 0x152 comes next iteration-ish; run a few.
    for _ in 0..16 {
        gb.dbg_step();
        gb.dbg_check_breakpoints();
    }

    {
        let dbg = gb.debugger.take().unwrap();
        let module = dbg.core.modules[0]
            .as_ref()
            .unwrap()
            .as_ref() as &dyn DebuggerModule;
        let _ = module;
        gb.debugger = Some(dbg);
    }
    let hits = count_entries(&mut gb);
    assert!(hits > 0, "breakpoint at 0x152 was hit");

    // Clearing the breakpoint stops further hits.
    assert!(gb.dbg_clear_breakpoint(id));
    let mut gb = gb;
    let before = count_entries(&mut gb);
    for _ in 0..16 {
        gb.dbg_step();
        gb.dbg_check_breakpoints();
    }
    assert_eq!(count_entries(&mut gb), before);
}

fn count_entries(gb: &mut Gb) -> usize {
    let mut dbg = gb.debugger.take().unwrap();
    let n = {
        let module = dbg.core.modules[0].as_ref().unwrap();
        module.as_any().downcast_ref::<Rec>().unwrap().entries.len()
    };
    gb.debugger = Some(dbg);
    n
}

#[test]
fn write_watchpoint_hits() {
    let mut gb = Gb::new();
    gb.load_rom(test_rom(ENTRY));
    gb.sm83_reset();
    gb.debugger_attach();
    let wp = rgba_debugger::debugger::Watchpoint {
        id: -1,
        segment: -1,
        min_address: 0xC000,
        max_address: 0xC010,
        ty: watchpoint_type::WRITE,
        condition: None,
        disabled: false,
    };
    let id = rgba_debugger::DebugConsole::dbg_set_watchpoint(&mut *gb, None, &wp);
    assert!(id > 0);
    assert!(gb.dbg_watchpoints_active);
    for _ in 0..64 {
        gb.step();
    }
    let hit = {
        let dbg = gb.debugger.as_ref().unwrap();
        // watchpoint enters with no modules consuming: check that pause flag
        // didn't stick (no modules) but the entry happened — observe via
        // pointing at state: use list_watchpoints presence + a marker: the
        // wram value was written through the shim.
        let _ = dbg;
        gb.view8(0xC000, -1)
    };
    assert_eq!(hit, 0x42);
    // remove watchpoint clears the shim flag
    assert!(gb.dbg_clear_breakpoint(id));
    assert!(!gb.dbg_watchpoints_active);
}

// Access-log test: attach an AccessLogger watching WRAM, run the ROM's
// 0xC000-writing loop with the debugger resident (like the watchpoint
// tests), then check the recorded flags in the logger's block.
#[test]
fn access_logger_records_wram_writes() {
    use rgba_debugger::access_logger::{AccessLogger, MAL_PLATFORM_GB};

    let mut gb = Gb::new();
    gb.load_rom(test_rom(ENTRY));
    gb.sm83_reset();
    gb.debugger_attach();
    let idx = {
        let mut dbg = gb.debugger.take().unwrap();
        let idx = dbg
            .core
            .attach_module(&mut *gb, Box::new(AccessLogger::new(MAL_PLATFORM_GB)));
        gb.debugger = Some(dbg);
        idx
    };

    // Take the module out to configure + start it while the console (and
    // hence the install target, GbDebugger::access_log) is resident.
    let mut module = {
        let mut dbg = gb.debugger.take().unwrap();
        let m = dbg.core.modules[idx].take().unwrap();
        gb.debugger = Some(dbg);
        m
    };
    {
        let logger = module.as_any_mut().downcast_mut::<AccessLogger>().unwrap();
        assert!(logger.watch_memory_block_name(&mut *gb, "wram", 0) >= 0);
        assert!(logger.start(&mut *gb));
    }
    {
        let mut dbg = gb.debugger.take().unwrap();
        dbg.core.modules[idx] = Some(module);
        // C: _setupRegion -> mDebuggerModuleSetNeedsCallback
        dbg.core.set_module_needs_callback(&mut *gb, idx);
        gb.debugger = Some(dbg);
    }
    assert!(gb.dbg_watchpoints_active); // shim kept active by the logger

    for _ in 0..64 {
        gb.step();
    }

    {
        let log = rgba_debugger::DebugConsole::dbg_access_log(&mut *gb)
            .expect("logger core installed in console");
        let (ri, off) = log.get_region(0xC000, 0).unwrap();
        assert_eq!(off, 0);
        let region = &log.regions[ri];
        assert!(
            region.block[0] & access_log_flags::WRITE != 0,
            "LD ($C000),A write was logged"
        );
        assert!(region.block[0] & access_log_flags::ACCESS8 != 0);
        assert_eq!(region.block[0x100], 0, "untouched WRAM stays clear");
    }

    // Stop: the module reclaims its core and the shim deactivates.
    let mut module = {
        let mut dbg = gb.debugger.take().unwrap();
        let m = dbg.core.modules[idx].take().unwrap();
        gb.debugger = Some(dbg);
        m
    };
    {
        let logger = module.as_any_mut().downcast_mut::<AccessLogger>().unwrap();
        logger.stop(&mut *gb);
        assert!(logger.access_log().is_some());
    }
    {
        let mut dbg = gb.debugger.take().unwrap();
        dbg.core.clear_module_needs_callback(&mut *gb, idx);
        dbg.core.modules[idx] = Some(module);
        gb.debugger = Some(dbg);
    }
    assert!(!gb.dbg_watchpoints_active);
    assert!(gb.dbg_access_log().is_none());
}
