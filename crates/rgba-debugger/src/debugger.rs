// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Ported from mgba/src/debugger/debugger.c / include/mgba/debugger/debugger.h
//
// The C structure is: `mDebugger` (shared state) + `mDebuggerPlatform`
// (per-CPU-family vtable living inside the console) + `mDebuggerModule`
// (CLI/GDB/access-logger frontends). In Rust the platform vtable becomes
// inherent methods on the console (Gb/Gba), and the generic interface the
// modules use to talk to the console is the `DebugConsole` trait below.

use crate::parser::ParseTree;
use crate::stack_trace::{StackTrace, StackTraceMode};
use crate::symbols::SymbolTable;
use std::collections::HashMap;

pub const DEBUGGER_ID: u32 = 0xDEADBEEF;

pub const INSN_LENGTH_MAX: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DebuggerType {
    None = 0,
    Custom,
    Cli,
    Gdb,
    AccessLogger,
    Max,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum DebuggerState {
    Created = 0,
    Paused,
    Running,
    Callback,
    Shutdown,
}

// mWatchpointType (bitfield)
pub mod watchpoint_type {
    pub const WRITE: u8 = 1;
    pub const READ: u8 = 2;
    pub const RW: u8 = 3;
    pub const CHANGE: u8 = 4;
    pub const WRITE_CHANGE: u8 = 5;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BreakpointType {
    Hardware,
    Software,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DebuggerEntryReason {
    Manual,
    Attached,
    Breakpoint,
    Watchpoint,
    IllegalOp,
    Stack,
}

// mMemoryAccessSource (mgba/core/memory.h); only Program/DMA matter to the logger.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemoryAccessSource {
    Unknown = 0,
    Segment,
    Program,
    Dma,
}

#[derive(Clone, Copy, Debug)]
pub struct WatchpointEntryInfo {
    pub old_value: u32,
    pub new_value: u32,
    pub watch_type: u8,
    pub access_type: u8,
    pub access_source: MemoryAccessSource,
}

#[derive(Clone, Copy, Debug)]
pub struct BreakpointEntryInfo {
    pub opcode: u32,
    pub break_type: BreakpointType,
}

#[derive(Clone, Copy, Debug)]
pub struct StackEntryInfo {
    pub trace_type: StackTraceMode,
}

#[derive(Clone, Copy, Debug)]
pub enum EntryTypeInfo {
    Wp(WatchpointEntryInfo),
    Bp(BreakpointEntryInfo),
    St(StackEntryInfo),
}

#[derive(Clone, Debug)]
pub struct DebuggerEntryInfo {
    pub address: u32,
    pub segment: i32,
    pub width: i32,
    pub type_info: Option<EntryTypeInfo>,
    pub point_id: i64,
    // Index of the module that owns the point that was hit (C: `target`).
    pub target: Option<usize>,
}

impl Default for DebuggerEntryInfo {
    fn default() -> Self {
        DebuggerEntryInfo {
            address: 0,
            segment: -1,
            width: 0,
            type_info: None,
            point_id: 0,
            target: None,
        }
    }
}

#[derive(Clone)]
pub struct Breakpoint {
    pub id: i64,
    pub address: u32,
    pub segment: i32,
    pub ty: BreakpointType,
    pub condition: Option<ParseTree>,
    pub disabled: bool,
    pub is_temporary: bool,
}

#[derive(Clone)]
pub struct Watchpoint {
    pub id: i64,
    pub segment: i32,
    pub min_address: u32,
    pub max_address: u32,
    pub ty: u8,
    pub condition: Option<ParseTree>,
    pub disabled: bool,
}

// mDebuggerAccessLogFlags
pub mod access_log_flags {
    pub const READ: u8 = 1 << 0;
    pub const WRITE: u8 = 1 << 1;
    pub const EXECUTE: u8 = 1 << 2;
    pub const ABORT: u8 = 1 << 3;
    pub const ACCESS8: u8 = 1 << 4;
    pub const ACCESS16: u8 = 1 << 5;
    pub const ACCESS32: u8 = 1 << 6;
    pub const ACCESS64: u8 = 1 << 7;
}

// mDebuggerAccessLogFlagsEx
pub mod access_log_flags_ex {
    pub const ACCESS_PROGRAM: u16 = 1 << 0;
    pub const ACCESS_DMA: u16 = 1 << 1;
    pub const ACCESS_SYSTEM: u16 = 1 << 2;
    pub const ACCESS_DECOMPRESS: u16 = 1 << 3;
    pub const ACCESS_COPY: u16 = 1 << 4;

    pub const ERROR_ILLEGAL_OPCODE: u16 = 1 << 8;
    pub const ERROR_ACCESS_READ: u16 = 1 << 9;
    pub const ERROR_ACCESS_WRITE: u16 = 1 << 10;
    pub const ERROR_ACCESS_EXECUTE: u16 = 1 << 11;
    pub const PRIVATE0: u16 = 1 << 12;
    pub const PRIVATE1: u16 = 1 << 13;
    pub const PRIVATE2: u16 = 1 << 14;
    pub const PRIVATE3: u16 = 1 << 15;

    pub const EXECUTE_ARM: u16 = 1 << 14;
    pub const EXECUTE_THUMB: u16 = 1 << 15;

    pub const EXECUTE_OPCODE: u16 = 1 << 14;
    pub const EXECUTE_OPERAND: u16 = 1 << 15;
}

#[derive(Clone, Copy, Default)]
pub struct DebuggerInstructionInfo {
    pub address: u32,
    pub segment: i32,
    pub width: u32,
    pub flags: [u8; INSN_LENGTH_MAX],
    pub flags_ex: [u16; INSN_LENGTH_MAX],
}

/// `mCoreMemoryBlock` subset consumed by the GDB stub's memory-map XML
/// (only `mCORE_MEMORY_MAPPED` blocks; `writable` = C's
/// `flags & (mCORE_MEMORY_WRITE | mCORE_MEMORY_WORM)` test).
#[derive(Clone, Copy, Debug)]
pub struct MemoryBlockInfo {
    pub start: u32,
    pub size: u32,
    pub writable: bool,
}

/// The subset of the `mCore` vtable the debugger uses, plus the
/// `mDebuggerPlatform` vtable (which is implemented as inherent console
/// methods in the C-shaped ports). Implemented by `Gb` and `Gba`.
pub trait DebugConsole {
    // --- mCore subset ---
    /// Run the emulation full-throttle (C: `core->runLoop`). Must return
    /// promptly when the debugger pauses execution.
    fn dbg_run_loop(&mut self);
    /// Execute a single instruction (C: `core->step`).
    fn dbg_step(&mut self);
    fn dbg_frame_counter(&self) -> u32;
    fn dbg_read_register(&self, name: &str) -> Option<i32>;
    fn dbg_write_register(&mut self, name: &str, value: i32) -> bool;
    /// Raw (no-side-effect) memory read. Width is 1, 2 or 4.
    fn dbg_raw_read(&mut self, address: u32, segment: i32, width: u32) -> u32;
    fn dbg_raw_write(&mut self, address: u32, segment: i32, width: u32, value: u32);
    fn dbg_lookup_identifier(&mut self, name: &str) -> Option<(i32, i32)>;
    fn dbg_current_segment(&self, address: u32) -> i32 {
        let _ = address;
        -1
    }

    // --- mDebuggerPlatform subset ---
    fn dbg_platform_init(&mut self) {}
    fn dbg_platform_deinit(&mut self) {}
    /// Per-console platform enter hook (C: platform->entered). Called by the
    /// console's own debugger_enter before the shared module dispatch.
    fn dbg_platform_entered(
        &mut self,
        _reason: DebuggerEntryReason,
        _info: Option<&mut DebuggerEntryInfo>,
    ) {
    }
    fn dbg_has_breakpoints(&mut self) -> bool {
        false
    }
    fn dbg_check_breakpoints(&mut self) {}
    fn dbg_set_breakpoint(&mut self, owner: Option<usize>, bp: &Breakpoint) -> i64;
    fn dbg_list_breakpoints(&self, owner: Option<usize>) -> Vec<Breakpoint>;
    fn dbg_clear_breakpoint(&mut self, id: i64) -> bool;
    fn dbg_toggle_breakpoint(&mut self, id: i64, status: bool) -> bool;
    fn dbg_set_watchpoint(&mut self, owner: Option<usize>, wp: &Watchpoint) -> i64;
    fn dbg_list_watchpoints(&self, owner: Option<usize>) -> Vec<Watchpoint>;
    /// Reset the emulated system (C: `core->reset`, used by the CLI `reset`
    /// command). Default no-op.
    fn dbg_reset(&mut self) {}
    /// The CLI `events` command (C: `_events` walking `core->timing`):
    /// preformatted "name in N cycles" lines (each ending in \n). Empty
    /// when the console does not expose its timing queue.
    fn dbg_events_string(&mut self) -> String {
        String::new()
    }
    /// Default start address for the CLI `disassemble` command when no
    /// address is given (per-console `system->disassemble`). SM83: pc.
    /// GBA overrides with `pc - instruction size`.
    fn dbg_disassemble_default_address(&self) -> u32 {
        self.dbg_read_register("pc").unwrap_or(0) as u32
    }
    /// Whether platform->getStackTraceMode exists in C terms (GBA only; on
    /// the GB stack tracing is unsupported).
    fn dbg_supports_stack_trace(&self) -> bool {
        false
    }
    /// CLIDebuggerSystem platformCommands registration: `Some("ARM")` on
    /// the GBA (arm/debugger/cli-debugger.c `_armCommands`), `None` on the
    /// GB (SM83 registers no platform commands).
    fn dbg_cli_platform(&self) -> Option<&'static str> {
        None
    }
    /// One-line register + disassembly dump (C: `platform->trace`).
    fn dbg_trace(&mut self) -> String;
    /// Disassemble a single instruction for the CLI (C: `_printLine` in the
    /// per-console cli-debugger.c). Returns (text-without-newline, next
    /// address). `thumb`: force GBA mode when Some; GB ignores it.
    fn dbg_disassemble_line(
        &mut self,
        address: u32,
        _segment: i32,
        _thumb: Option<bool>,
    ) -> (String, u32) {
        (String::new(), address + 1)
    }
    /// Full status register dump (C: `CLIDebuggerSystem->printStatus`).
    fn dbg_print_status(&mut self) -> String {
        String::new()
    }
    /// Global timing counter for the status line (mTimingGlobalTime).
    fn dbg_global_time(&self) -> u64 {
        0
    }
    /// GBA: currently executing Thumb (default false for GB).
    fn dbg_is_thumb(&self) -> bool {
        false
    }
    /// GBA break/a, break/t (ARMDebuggerSetSoftwareBreakpoint).
    fn dbg_set_software_breakpoint(
        &mut self,
        _owner: Option<usize>,
        _address: u32,
        _thumb: bool,
    ) -> i64 {
        -1
    }
    fn dbg_get_stack_trace_mode(&self) -> StackTraceMode {
        StackTraceMode::Disabled
    }
    fn dbg_set_stack_trace_mode(&mut self, _mode: StackTraceMode) {}
    fn dbg_update_stack_trace(&mut self) -> bool {
        false
    }
    fn dbg_next_instruction_info(&mut self) -> DebuggerInstructionInfo {
        DebuggerInstructionInfo {
            segment: -1,
            ..Default::default()
        }
    }
    /// Mapped memory blocks for the GDB `qXfer:memory-map:read` reply
    /// (C: `mCore::listMemoryBlocks`, filtered to `mCORE_MEMORY_MAPPED`).
    fn dbg_list_memory_blocks(&self) -> Vec<MemoryBlockInfo> {
        Vec::new()
    }
    /// The full C-shaped block list including ids, internal names, flags
    /// and segment info (C: `mCore::listMemoryBlocks` as consumed by
    /// `mDebuggerAccessLoggerWatchMemoryBlockId/Name`).
    fn dbg_list_memory_blocks_full(&self) -> Vec<crate::access_logger::CoreMemoryBlock> {
        Vec::new()
    }
    /// Install/remove the active access-logger core (C: the logger's
    /// `_setupRegion` watchpoint installs; here the console's memory-shim
    /// hooks record directly into the held core). Returns the previously
    /// installed core. Consoles without support return their input.
    fn dbg_set_access_log(
        &mut self,
        core: Option<Box<crate::access_logger::AccessLoggerCore>>,
    ) -> Option<Box<crate::access_logger::AccessLoggerCore>> {
        core
    }
    /// Borrow the installed access-logger core (recording target for the
    /// shim hooks and the module's exec/illegal-op logging).
    fn dbg_access_log(&mut self) -> Option<&mut crate::access_logger::AccessLoggerCore> {
        None
    }
    /// While the debugger core is detached from the console for driving
    /// (`run_timeout`), console-side breakpoint/watchpoint hits cannot be
    /// dispatched to modules directly (the module list lives in the core);
    /// the platform queues them here and the running `Debugger` drains and
    /// dispatches them (`mDebuggerEnter`) after each step/check iteration.
    fn dbg_take_pending_entries(&mut self) -> Vec<(DebuggerEntryReason, DebuggerEntryInfo)> {
        Vec::new()
    }
}

/// mDebuggerModule vtable. Methods receive the owning `Debugger` (C:
/// `module->p`) and the console via `DebugConsole`. Slot-index identity
/// replaces the C pointers (`module` pointer, `info->target`).
pub trait DebuggerModule {
    fn module_type(&self) -> DebuggerType;
    /// Downcasting support for embedders/tests.
    fn as_any(&self) -> &dyn std::any::Any;
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
    fn is_paused(&self) -> bool {
        false
    }
    fn set_paused(&mut self, _paused: bool) {}
    fn needs_callback(&self) -> bool {
        false
    }
    fn set_needs_callback(&mut self, _needs: bool) {}
    fn debugger_state(&self) -> DebuggerState {
        DebuggerState::Created
    }

    /// Called by `Debugger::attach_module` with the module's slot index
    /// (replaces the C `module` pointer used as breakpoint/watchpoint owner).
    fn set_module_index(&mut self, _idx: usize) {}

    fn init(&mut self, _debugger: &mut Debugger, _console: &mut dyn DebugConsole) {}
    fn deinit(&mut self, _debugger: &mut Debugger, _console: &mut dyn DebugConsole) {}
    fn paused(
        &mut self,
        _debugger: &mut Debugger,
        _console: &mut dyn DebugConsole,
        _timeout_ms: i32,
    ) {
    }
    fn update(&mut self, _debugger: &mut Debugger, _console: &mut dyn DebugConsole) {}
    fn entered(
        &mut self,
        _debugger: &mut Debugger,
        _console: &mut dyn DebugConsole,
        _reason: DebuggerEntryReason,
        _info: Option<&DebuggerEntryInfo>,
    ) {
    }
    fn custom(&mut self, _debugger: &mut Debugger, _console: &mut dyn DebugConsole) {}
    fn interrupt(&mut self, _debugger: &mut Debugger, _console: &mut dyn DebugConsole) {}
}

pub struct Debugger {
    pub state: DebuggerState,
    // Vec of slots; None after detach (keeps module indices stable, since
    // `point_owner` and `EntryInfo.target` refer to them).
    pub modules: Vec<Option<Box<dyn DebuggerModule>>>,
    // point id -> module index (C: Table pointOwner)
    pub point_owner: HashMap<i64, usize>,
    pub stack_trace: StackTrace,
    pub symbols: Option<SymbolTable>,
    // Stand-in installed in the platform debugger structs while the real
    // core is detached for driving (see `DebugConsole::dbg_take_pending_entries`).
    placeholder: bool,
}

impl Debugger {
    pub fn new() -> Self {
        Debugger {
            state: DebuggerState::Created,
            modules: Vec::with_capacity(4),
            point_owner: HashMap::new(),
            stack_trace: StackTrace::new(),
            symbols: None,
            placeholder: false,
        }
    }

    /// Stand-in core the consoles install while the real one is detached
    /// for driving; console hit paths queue entries instead of dispatching
    /// into the (empty) module list.
    pub fn placeholder() -> Self {
        let mut d = Debugger::new();
        d.placeholder = true;
        d
    }

    pub fn is_placeholder(&self) -> bool {
        self.placeholder
    }

    /// Absorb point-ownership registrations made against the placeholder
    /// while detached (module hooks setting breakpoints mid-run).
    pub fn merge_placeholder(&mut self, mut placeholder: Debugger) {
        for (id, owner) in placeholder.point_owner.drain() {
            self.point_owner.insert(id, owner);
        }
    }

    /// mDebuggerAttachModule. Returns the module's slot index.
    pub fn attach_module(
        &mut self,
        console: &mut dyn DebugConsole,
        mut module: Box<dyn DebuggerModule>,
    ) -> usize {
        let idx = self.modules.len();
        module.set_module_index(idx);
        if self.state > DebuggerState::Created && self.state < DebuggerState::Shutdown {
            module.init(self, console);
        }
        self.modules.push(Some(module));
        idx
    }

    pub fn detach_module(&mut self, console: &mut dyn DebugConsole, idx: usize) {
        if idx >= self.modules.len() {
            return;
        }
        if let Some(mut module) = self.modules[idx].take() {
            if self.state > DebuggerState::Created && self.state < DebuggerState::Shutdown {
                module.deinit(self, console);
            }
        }
        self.update_paused(console);
    }

    fn for_each_module(
        &mut self,
        console: &mut dyn DebugConsole,
        mut f: impl FnMut(&mut dyn DebuggerModule, usize, &mut Debugger, &mut dyn DebugConsole),
    ) {
        for i in 0..self.modules.len() {
            if let Some(mut module) = self.modules[i].take() {
                f(&mut *module, i, self, console);
                if self.modules[i].is_none() {
                    self.modules[i] = Some(module);
                }
            }
        }
    }

    pub fn point_owner(&self, id: i64) -> Option<usize> {
        self.point_owner.get(&id).copied()
    }

    /// Dispatch entries the console's platform queued while the core was
    /// detached for driving (C dispatches these synchronously from the load/
    /// store/execute paths via mDebuggerEnter).
    fn drain_pending_entries(&mut self, console: &mut dyn DebugConsole) {
        for (reason, mut info) in console.dbg_take_pending_entries() {
            // Resolve the owning module against the real core (C: the shared
            // pointOwner table at enter time).
            if info.target.is_none() {
                info.target = self.point_owner(info.point_id);
            }
            self.enter(console, reason, Some(&mut info));
        }
    }

    /// mDebuggerRunTimeout
    pub fn run_timeout(&mut self, console: &mut dyn DebugConsole, timeout_ms: i32) {
        match self.state {
            DebuggerState::Running => {
                if !console.dbg_has_breakpoints() {
                    console.dbg_run_loop();
                    self.drain_pending_entries(console);
                } else {
                    console.dbg_step();
                    console.dbg_check_breakpoints();
                    self.drain_pending_entries(console);
                }
            }
            DebuggerState::Callback => {
                console.dbg_step();
                console.dbg_check_breakpoints();
                self.drain_pending_entries(console);
                let mut any = false;
                self.for_each_module(console, |m, _, d, c| {
                    if m.needs_callback() {
                        m.custom(d, c);
                        any = true;
                    }
                });
                let _ = any;
                // custom() may flip paused/needs_callback from any thread of
                // control the C reaches via mDebuggerEnter /
                // mDebuggerModuleClearNeedsCallback (e.g. the GDB stub's
                // Ctrl-C poll); recompute the state wholesale.
                self.update_paused(console);
            }
            DebuggerState::Paused => {
                let mut any_paused = false;
                self.for_each_module(console, |m, _, d, c| {
                    if m.is_paused() {
                        m.paused(d, c, timeout_ms);
                        if m.is_paused() {
                            any_paused = true;
                        }
                    } else if m.needs_callback() {
                        m.custom(d, c);
                    }
                });
                // Module hooks may have stepped the core directly (GDB `s`),
                // queuing platform hits; dispatch them before recomputing.
                self.drain_pending_entries(console);
                // C reaches mDebuggerUpdatePaused via several side channels
                // (e.g. mDebuggerModuleSetNeedsCallback from `trace`); recompute
                // the state wholesale here at the end of the poll iteration.
                self.update_paused(console);
                let _ = any_paused;
            }
            DebuggerState::Created => {
                // mLOG(DEBUGGER, ERROR, ...): attempted to run before init
            }
            DebuggerState::Shutdown => {}
        }
    }

    /// mDebuggerRun
    pub fn run(&mut self, console: &mut dyn DebugConsole) {
        self.run_timeout(console, 50);
    }

    /// mDebuggerRunFrame
    /// mDebuggerRunFrame. Unlike the C, this also returns while the debugger
    /// is paused: mGBA's frontends run the debugger on its own thread and
    /// block in readline, but rgba's GUI consoles are non-blocking and must
    /// get the UI thread back to collect input. Blocking backends (the stdio
    /// CLI) never return from their prompt, so they behave as before.
    pub fn run_frame(&mut self, console: &mut dyn DebugConsole) {
        let frame = console.dbg_frame_counter();
        loop {
            self.run(console);
            if console.dbg_frame_counter() != frame || self.state == DebuggerState::Paused {
                break;
            }
        }
    }

    /// mDebuggerEnter
    pub fn enter(
        &mut self,
        console: &mut dyn DebugConsole,
        reason: DebuggerEntryReason,
        info: Option<&mut DebuggerEntryInfo>,
    ) {
        // NB: the per-console "platform entered" hook runs in each console's
        // debugger_enter before calling this, so the platform can adjust
        // `info` (C: platform->entered invoked here).
        // C iterates the module list, entering only the target module when
        // info->target is set (checked inside the loop).
        for i in 0..self.modules.len() {
            if let Some(info) = info.as_deref() {
                if let Some(target) = info.target {
                    if target != i {
                        continue;
                    }
                }
            }
            if let Some(mut module) = self.modules[i].take() {
                module.set_paused(true);
                module.entered(self, console, reason, info.as_deref());
                if self.modules[i].is_none() {
                    self.modules[i] = Some(module);
                }
            }
            if let Some(info) = info.as_deref() {
                if info.target.is_some() {
                    // Make this the last loop so we don't hit this one twice
                    // (C sets i = size - 1; we just break).
                    break;
                }
            }
        }

        self.update_paused(console);
    }

    /// mDebuggerInterrupt
    pub fn interrupt(&mut self, console: &mut dyn DebugConsole) {
        self.for_each_module(console, |m, _, d, c| m.interrupt(d, c));
    }

    /// mDebuggerUpdatePaused
    pub fn update_paused(&mut self, console: &mut dyn DebugConsole) {
        if self.state == DebuggerState::Shutdown {
            return;
        }
        let mut any_paused = false;
        let mut any_callback = false;
        self.for_each_module(console, |m, _, _, _| {
            if m.is_paused() {
                any_paused = true;
            }
            if m.needs_callback() {
                any_callback = true;
            }
        });
        if any_paused {
            self.state = DebuggerState::Paused;
        } else if any_callback {
            self.state = DebuggerState::Callback;
        } else {
            self.state = DebuggerState::Running;
        }
    }

    /// mDebuggerShutdown
    pub fn shutdown(&mut self) {
        self.state = DebuggerState::Shutdown;
    }

    pub fn is_shutdown(&self) -> bool {
        self.state == DebuggerState::Shutdown
    }

    /// mDebuggerUpdate
    pub fn update(&mut self, console: &mut dyn DebugConsole) {
        self.for_each_module(console, |m, _, d, c| m.update(d, c));
        // A module's update hook may flip paused/needs_callback (the C
        // recomputes inline via mDebuggerEnter/mDebuggerUpdatePaused inside
        // the hook; here the central recompute happens after the fact).
        self.update_paused(console);
    }

    /// _mDebuggerInit (CPU component init hook in C): start running.
    pub fn debugger_attach_init(&mut self, console: &mut dyn DebugConsole) {
        self.state = DebuggerState::Running;
        console.dbg_platform_init();
        self.for_each_module(console, |m, _, d, c| m.init(d, c));
    }

    /// _mDebuggerDeinit
    pub fn debugger_attach_deinit(&mut self, console: &mut dyn DebugConsole) {
        self.state = DebuggerState::Shutdown;
        self.for_each_module(console, |m, _, d, c| m.deinit(d, c));
        console.dbg_platform_deinit();
    }

    /// mDebuggerLookupIdentifier: symbols, then console identifiers, then
    /// registers.
    pub fn lookup_identifier(
        &mut self,
        console: &mut dyn DebugConsole,
        name: &str,
    ) -> Option<(i32, i32)> {
        if let Some(symbols) = &self.symbols {
            if let Some((value, segment)) = symbols.lookup(name) {
                return Some((value, segment));
            }
        }
        if let Some(v) = console.dbg_lookup_identifier(name) {
            return Some(v);
        }
        if let Some(value) = console.dbg_read_register(name) {
            return Some((value, -1));
        }
        None
    }

    /// mDebuggerModuleSetNeedsCallback helper for modules: set flag on the
    /// module at `idx` and recompute state.
    pub fn set_module_needs_callback(&mut self, console: &mut dyn DebugConsole, idx: usize) {
        if let Some(Some(m)) = self.modules.get_mut(idx) {
            m.set_needs_callback(true);
        }
        self.update_paused(console);
    }

    pub fn clear_module_needs_callback(&mut self, console: &mut dyn DebugConsole, idx: usize) {
        if let Some(Some(m)) = self.modules.get_mut(idx) {
            m.set_needs_callback(false);
        }
        self.update_paused(console);
    }
}
