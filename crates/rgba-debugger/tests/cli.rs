// Tests for the CLI debugger module (cli.rs, ported from cli-debugger.c):
// command parsing/dispatch round-trips against a mock console, driven
// through the scriptable CallbackBackend.
use rgba_debugger::cli::{CallbackBackend, CliDebugger};
use rgba_debugger::debugger::{
    Breakpoint, DebugConsole, Debugger, DebuggerModule, Watchpoint,
};
use rgba_debugger::symbols::SymbolTable;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

type Out = Rc<RefCell<String>>;

/// Mock console with 64KiB of flat memory, registers r0..r3, a frame/step
/// counter, and breakpoint/watchpoint lists owned like the real consoles'.
struct MockConsole {
    mem: Vec<u8>,
    regs: HashMap<String, i32>,
    frame: u32,
    steps: usize,
    resets: usize,
    breakpoints: Vec<(Option<usize>, Breakpoint)>,
    watchpoints: Vec<(Option<usize>, Watchpoint)>,
    next_id: i64,
}

impl MockConsole {
    fn new() -> Self {
        let mut regs = HashMap::new();
        for r in ["r0", "r1", "r2", "r3"] {
            regs.insert(r.to_string(), 0);
        }
        MockConsole {
            mem: vec![0; 0x10000],
            regs,
            frame: 0,
            steps: 0,
            resets: 0,
            breakpoints: Vec::new(),
            watchpoints: Vec::new(),
            next_id: 0,
        }
    }
}

impl DebugConsole for MockConsole {
    fn dbg_run_loop(&mut self) {
        self.frame += 1;
    }

    fn dbg_step(&mut self) {
        self.frame += 1;
        self.steps += 1;
    }

    fn dbg_frame_counter(&self) -> u32 {
        self.frame
    }

    fn dbg_read_register(&self, name: &str) -> Option<i32> {
        self.regs.get(&name.to_ascii_lowercase()).copied()
    }

    fn dbg_write_register(&mut self, name: &str, value: i32) -> bool {
        if let Some(r) = self.regs.get_mut(&name.to_ascii_lowercase()) {
            *r = value;
            true
        } else {
            false
        }
    }

    fn dbg_raw_read(&mut self, address: u32, _segment: i32, width: u32) -> u32 {
        let mut v = 0u32;
        for i in 0..width {
            let b = *self.mem.get(address.wrapping_add(i) as usize).unwrap_or(&0);
            v |= (b as u32) << (8 * i);
        }
        v
    }

    fn dbg_raw_write(&mut self, address: u32, _segment: i32, width: u32, value: u32) {
        for i in 0..width {
            if let Some(b) = self.mem.get_mut(address.wrapping_add(i) as usize) {
                *b = (value >> (8 * i)) as u8;
            }
        }
    }

    fn dbg_lookup_identifier(&mut self, _name: &str) -> Option<(i32, i32)> {
        None
    }

    fn dbg_reset(&mut self) {
        self.resets += 1;
    }

    fn dbg_set_breakpoint(&mut self, owner: Option<usize>, bp: &Breakpoint) -> i64 {
        self.next_id += 1;
        let mut bp = bp.clone();
        bp.id = self.next_id;
        self.breakpoints.push((owner, bp));
        self.next_id
    }

    fn dbg_list_breakpoints(&self, owner: Option<usize>) -> Vec<Breakpoint> {
        match owner {
            Some(o) => self
                .breakpoints
                .iter()
                .filter(|(ow, _)| *ow == Some(o))
                .map(|(_, b)| b.clone())
                .collect(),
            None => self.breakpoints.iter().map(|(_, b)| b.clone()).collect(),
        }
    }

    fn dbg_clear_breakpoint(&mut self, id: i64) -> bool {
        // C clearBreakpoint clears either list (or both by id).
        let n = self.breakpoints.len() + self.watchpoints.len();
        self.breakpoints.retain(|(_, b)| b.id != id);
        self.watchpoints.retain(|(_, w)| w.id != id);
        self.breakpoints.len() + self.watchpoints.len() != n
    }

    fn dbg_toggle_breakpoint(&mut self, id: i64, status: bool) -> bool {
        for (_, b) in self.breakpoints.iter_mut() {
            if b.id == id {
                b.disabled = !status;
                return true;
            }
        }
        for (_, w) in self.watchpoints.iter_mut() {
            if w.id == id {
                w.disabled = !status;
                return true;
            }
        }
        false
    }

    fn dbg_set_watchpoint(&mut self, owner: Option<usize>, wp: &Watchpoint) -> i64 {
        self.next_id += 1;
        let mut wp = wp.clone();
        wp.id = self.next_id;
        self.watchpoints.push((owner, wp));
        self.next_id
    }

    fn dbg_list_watchpoints(&self, owner: Option<usize>) -> Vec<Watchpoint> {
        match owner {
            Some(o) => self
                .watchpoints
                .iter()
                .filter(|(ow, _)| *ow == Some(o))
                .map(|(_, w)| w.clone())
                .collect(),
            None => self.watchpoints.iter().map(|(_, w)| w.clone()).collect(),
        }
    }

    fn dbg_trace(&mut self) -> String {
        format!("TRACE pc={:08X}", self.frame)
    }

    fn dbg_disassemble_line(
        &mut self,
        address: u32,
        _segment: i32,
        _thumb: Option<bool>,
    ) -> (String, u32) {
        (format!("{:08X}:  NOP", address), address.wrapping_add(4))
    }

    fn dbg_print_status(&mut self) -> String {
        format!("::status frame={}::\n", self.frame)
    }
}

/// (cli, debugger, console, captured output)
struct Harness {
    cli: CliDebugger,
    dbg: Debugger,
    console: MockConsole,
    out: Out,
}

fn harness() -> Harness {
    let out: Out = Rc::new(RefCell::new(String::new()));
    let out2 = out.clone();
    let be = CallbackBackend::new(Box::new(move |s: &str| out2.borrow_mut().push_str(s)));
    Harness {
        cli: CliDebugger::new(be),
        dbg: Debugger::new(),
        console: MockConsole::new(),
        out,
    }
}

impl Harness {
    fn run(&mut self, line: &str) {
        self.cli
            .run_command(&mut self.dbg, &mut self.console, line);
    }

    fn take_out(&mut self) -> String {
        std::mem::take(&mut *self.out.borrow_mut())
    }

    fn backend(&mut self) -> &mut CallbackBackend {
        self.cli
            .backend_mut()
            .as_any_mut()
            .unwrap()
            .downcast_mut::<CallbackBackend>()
            .unwrap()
    }
}

#[test]
fn breakpoint_lifecycle() {
    let mut h = harness();
    h.run("break 0x100");
    assert_eq!(h.take_out(), "Added breakpoint #1\n");
    h.run("listb");
    assert_eq!(h.take_out(), "E 1: 0x100\n");
    h.run("disable 1");
    h.run("listb");
    assert_eq!(h.take_out(), "D 1: 0x100\n");
    h.run("enable 1");
    h.run("listb");
    assert_eq!(h.take_out(), "E 1: 0x100\n");
    h.run("delete 1");
    h.run("listb");
    assert_eq!(h.take_out(), "");
    assert!(h.console.breakpoints.is_empty());
}

#[test]
fn breakpoint_alias_and_conditions() {
    let mut h = harness();
    h.run("b 0x200");
    assert_eq!(h.take_out(), "Added breakpoint #1\n");
    // A condition parses (but is not evaluated at set time).
    h.run("b 0x300 r0==0");
    assert_eq!(h.take_out(), "Added breakpoint #2\n");
    h.run("lb");
    assert_eq!(h.take_out(), "E 1: 0x200\nE 2: 0x300\n");
    assert!(h.console.breakpoints[1].1.condition.is_some());
    // Bad condition -> Invalid arguments
    h.run("b 0x400 (1");
    assert_eq!(h.take_out(), "Invalid arguments\n");
}

#[test]
fn watchpoint_commands() {
    let mut h = harness();
    h.run("watch 0x200");
    assert_eq!(h.take_out(), "Added watchpoint #1\n");
    h.run("watch/r 0x204");
    assert_eq!(h.take_out(), "Added watchpoint #2\n");
    h.run("watch-range 0x300 0x310");
    assert_eq!(h.take_out(), "Added watchpoint #3\n");
    h.run("listw");
    assert_eq!(
        h.take_out(),
        "E 1: 0x200\nE 2: 0x204\nE 3: 0x300-0x310\n"
    );
    // Reversed range is an error.
    h.run("watch-range 0x310 0x300");
    assert_eq!(
        h.take_out(),
        "Range watchpoint end is before start. Note that the end of the range is not included.\n"
    );
    // watch alias `w`
    h.run("w 0x400");
    assert_eq!(h.take_out(), "Added watchpoint #4\n");
    h.run("d 4");
    h.run("lw");
    let out = h.take_out();
    assert!(!out.contains("0x400"));
    assert_eq!(h.console.watchpoints[0].1.max_address, 0x201);
    assert_eq!(h.console.watchpoints[2].1.max_address, 0x310);
}

#[test]
fn memory_write_and_examine_roundtrip() {
    let mut h = harness();
    h.run("w/4 0x10 0xdeadbeef");
    h.run("x/4 0x10 1");
    assert_eq!(h.take_out(), "0x00000010: DEADBEEF\n");
    h.run("r/4 0x10");
    assert_eq!(h.take_out(), " 0xDEADBEEF\n");
    h.run("w/1 0x12 0xa5");
    h.run("r/1 0x12");
    assert_eq!(h.take_out(), " 0xA5\n");
    // Word value was clobbered at byte 0x12
    h.run("r/4 0x10");
    assert_eq!(h.take_out(), " 0xDEA5BEEF\n");
    // Overflow is rejected
    h.run("w/1 0x10 0x100");
    assert_eq!(h.take_out(), "Arguments overflow\n");
    h.run("w/2 0x10 0x10000");
    assert_eq!(h.take_out(), "Arguments overflow\n");
    // r/2 and r/4 mask the address when no segment is given (C: & ~1 / & ~3)
    h.run("w/2 0x20 0x1234");
    h.run("r/2 0x21");
    assert_eq!(h.take_out(), " 0x1234\n");
    // Byte examine of 20 bytes wraps onto a second line of 4.
    h.run("x/1 0x10 20");
    assert_eq!(
        h.take_out(),
        "0x00000010: EF BE A5 DE 00 00 00 00 00 00 00 00 00 00 00 00\n0x00000020: 34 12 00 00\n"
    );
}

#[test]
fn print_expression_formats() {
    let mut h = harness();
    h.run("print 0x20 + 8");
    assert_eq!(h.take_out(), " 40\n");
    h.run("print/x 0x20 + 8");
    assert_eq!(h.take_out(), " 0x00000028\n");
    h.run("print/t 5");
    assert_eq!(h.take_out(), " 0b00000000000000000000000000000101\n");
    // alias
    h.run("p 1 * 2 + 3");
    assert_eq!(h.take_out(), " 5\n");
    // Parse error / missing args
    h.run("print");
    assert_eq!(h.take_out(), "Parse error\n");
    h.run("p/x zz");
    assert_eq!(h.take_out(), "Parse error\n");
}

#[test]
fn register_write_and_read() {
    let mut h = harness();
    h.run("w/r r0 123");
    h.run("print r0");
    assert_eq!(h.take_out(), " 123\n");
    assert_eq!(h.console.regs["r0"], 123);
    // Unknown register -> Invalid arguments
    h.run("w/r r9 1");
    assert_eq!(h.take_out(), "Invalid arguments\n");
}

#[test]
fn help_lists_commands() {
    let mut h = harness();
    h.run("help");
    let out = h.take_out();
    assert!(out.starts_with("Generic commands:\n"));
    assert!(out.contains("break            Set a breakpoint\n"));
    assert!(out.contains("                 Aliases: b\n"));
    assert!(out.contains("watch-range/c    Set a change range watchpoint\n"));
    // Per-command summary
    h.run("help break");
    assert_eq!(h.take_out(), " Set a breakpoint\n Aliases: b\n");
}

#[test]
fn disassemble_uses_console_line() {
    let mut h = harness();
    h.run("disassemble 0x20 3");
    assert_eq!(
        h.take_out(),
        "00000020:  NOP\n00000024:  NOP\n00000028:  NOP\n"
    );
    h.run("dis 0x30");
    assert_eq!(h.take_out(), "00000030:  NOP\n");
}

#[test]
fn trace_command_flow() {
    let mut h = harness();
    h.cli.set_paused(true);
    h.backend().push_line("trace 2");
    // Drive the module paused() hook like Debugger::run_timeout would.
    h.cli.paused(&mut h.dbg, &mut h.console, 0);
    let out = h.take_out();
    // One status banner, then the first trace line; the command keeps the
    // callback alive for the remaining line.
    assert!(out.starts_with("::status"));
    assert!(out.contains("TRACE pc="));
    assert_eq!(out.matches("TRACE").count(), 1, "{out}");
    assert!(!h.cli.is_paused());
    assert!(h.cli.needs_callback());

    // The Callback state steps once per iteration, then custom().
    h.console.dbg_step();
    h.cli.custom(&mut h.dbg, &mut h.console);
    let out = h.take_out();
    assert_eq!(out.matches("TRACE").count(), 1, "{out}");
    assert!(h.cli.is_paused());
    assert!(!h.cli.needs_callback());

    // No more tracing
    h.console.dbg_step();
    h.cli.custom(&mut h.dbg, &mut h.console);
    assert_eq!(h.take_out().matches("TRACE").count(), 0);
}

#[test]
fn trace_invalid_and_zero() {
    let mut h = harness();
    h.run("trace");
    assert_eq!(h.take_out(), "Arguments missing\n");
    h.run("trace 0");
    assert_eq!(h.take_out(), "");
    assert!(!h.cli.needs_callback());
}

#[test]
fn quit_sets_flag_and_shuts_down() {
    let mut h = harness();
    h.run("quit");
    assert!(h.cli.should_quit());
    assert!(h.dbg.is_shutdown());
}

#[test]
fn unknown_and_malformed_commands() {
    let mut h = harness();
    h.run("bogus");
    assert_eq!(h.take_out(), "Command not found\n");
    h.run("break");
    assert_eq!(h.take_out(), "Arguments missing\n");
    h.run("continue foo");
    assert_eq!(h.take_out(), "Wrong number of arguments\n");
    // Case-insensitive command names
    h.run("BREAK 0x100");
    assert_eq!(h.take_out(), "Added breakpoint #1\n");
    // Prefixes are NOT abbreviations ("dele" does not match "delete")
    h.run("dele 1");
    assert_eq!(h.take_out(), "Command not found\n");
}

#[test]
fn next_steps_and_prints_status() {
    let mut h = harness();
    let steps = h.console.steps;
    h.run("next");
    assert_eq!(h.console.steps, steps + 1);
    let out = h.take_out();
    assert!(out.starts_with("::status"), "{out}");
}

#[test]
fn reset_resets_console_and_prints_status() {
    let mut h = harness();
    h.run("reset");
    assert_eq!(h.console.resets, 1);
    assert!(h.take_out().starts_with("::status"));
}

#[test]
fn symbols_set_and_lookup() {
    let mut h = harness();
    // No symbol table yet
    h.run("set foo 0x1234");
    assert_eq!(h.take_out(), "No symbol table available.\n");
    h.dbg.symbols = Some(SymbolTable::new());
    h.run("set foo 0x1234");
    assert_eq!(h.take_out(), "");
    h.run("print foo");
    assert_eq!(h.take_out(), " 4660\n");
    h.run("symbol 0x1234");
    assert_eq!(h.take_out(), " 0x00001234 = foo\n");
    h.run("symbol 0x9999");
    assert_eq!(h.take_out(), "Not found.\n");
}

#[test]
fn source_runs_file_commands() {
    let mut h = harness();
    let dir = std::env::temp_dir();
    let path = dir.join(format!("rgba-cli-test-{}.txt", std::process::id()));
    std::fs::write(&path, "break 0x100\n# comment\nprint 0x20 + 8\n").unwrap();
    h.run(&format!("source {}", path.display()));
    assert_eq!(h.take_out(), "Added breakpoint #1\n 40\n");
    let _ = std::fs::remove_file(&path);
    h.run("source /nonexistent/rgba-cli");
    assert_eq!(h.take_out(), "Failed to load script\n");
}

#[test]
fn tab_completion() {
    let mut h = harness();
    // "pri" completes to "print" (longest common prefix of print/print/t/print/x)
    assert!(h.cli.tab_complete("pri", true));
    assert_eq!(h.backend().take_completion(), "nt");
    // Unambiguous prefix completes the rest + a space
    assert!(h.cli.tab_complete("backt", true));
    assert_eq!(h.backend().take_completion(), "race ");
    // Exact full name appends just a space (C behavior)
    assert!(h.cli.tab_complete("backtrace", true));
    assert_eq!(h.backend().take_completion(), " ");
    // Unknown prefix
    assert!(!h.cli.tab_complete("zz", true));
}

#[test]
fn history_repeat_via_paused_loop() {
    let mut h = harness();
    h.cli.set_paused(true);
    h.backend().push_line("break 0x100");
    h.backend().push_line("\n"); // bare Enter repeats history
    h.backend().push_line("# a comment");
    h.backend().push_line("continue");
    h.cli.paused(&mut h.dbg, &mut h.console, 0);
    let out = h.take_out();
    assert_eq!(out.matches("Added breakpoint #").count(), 2, "{out}");
    // History got the break/comment/continue lines (again-Enter repeats but
    // does not re-append).
    assert_eq!(
        h.backend().history(),
        &["break 0x100".to_string(), "# a comment".to_string(), "continue".to_string()]
    );
    assert!(!h.cli.is_paused());
}

#[test]
fn enter_reports_and_module_flow() {
    let mut h = harness();
    // Attach the CLI as a real module so owner bookkeeping is exercised.
    h.dbg.debugger_attach_init(&mut h.console);
    let out: Out = Rc::new(RefCell::new(String::new()));
    let out2 = out.clone();
    let be = CallbackBackend::new(Box::new(move |s: &str| out2.borrow_mut().push_str(s)));
    let mut cli = CliDebugger::new(be);
    cli.backend_mut()
        .as_any_mut()
        .unwrap()
        .downcast_mut::<CallbackBackend>()
        .unwrap()
        .push_line("break 0x100");
    // Note: pushing more input before entering so paused() drains it.
    cli.backend_mut()
        .as_any_mut()
        .unwrap()
        .downcast_mut::<CallbackBackend>()
        .unwrap()
        .push_line("listb");
    let idx = h.dbg.attach_module(&mut h.console, Box::new(cli));
    assert_eq!(idx, 0);

    // Breakpoint hit report with point id 7
    let mut info = rgba_debugger::debugger::DebuggerEntryInfo {
        address: 0x08000100,
        point_id: 7,
        ..Default::default()
    };
    h.dbg.enter(
        &mut h.console,
        rgba_debugger::debugger::DebuggerEntryReason::Breakpoint,
        Some(&mut info),
    );
    {
        let d = out.borrow();
        assert!(d.contains("Hit breakpoint 7 at 0x08000100\n"), "{d}");
    }
    assert_eq!(h.dbg.state, rgba_debugger::debugger::DebuggerState::Paused);

    // paused() drains the queued commands.
    h.dbg.run_timeout(&mut h.console, 0);
    {
        let d = out.borrow();
        assert!(d.contains("Added breakpoint #1\n"), "{d}");
        assert!(d.contains("E 1: 0x100\n"), "{d}");
    }
    // Owner is the CLI module slot.
    assert_eq!(h.console.breakpoints[0].0, Some(0));
    // Still paused: run again with no input -> poll timeout, no output.
    let before = out.borrow().len();
    h.dbg.run_timeout(&mut h.console, 0);
    assert_eq!(out.borrow().len(), before);
    assert_eq!(h.dbg.state, rgba_debugger::debugger::DebuggerState::Paused);
}
