// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Ported from mgba/src/debugger/cli-debugger.c and
// include/mgba/internal/debugger/cli-debugger.h. The ARM platform command
// sub-table comes from mgba/src/arm/debugger/cli-debugger.c (_armCommands);
// the per-console `CLIDebuggerSystem` helpers (_printLine/_printStatus) are
// already part of the `DebugConsole` trait and are not re-ported here.
//
// Mapping notes:
// - `struct CLIDebugger` -> `CliDebugger`; `struct CLIDebuggerBackend`
//   vtable -> the `CliBackend` trait.
// - `struct CLIDebugVector` linked list -> `Vec<CliDebugValue>`; a NULL `dv`
//   -> `None`, a CLIDV_ERROR_TYPE parse -> handled at parse time (the
//   handlers never see it, as in C).
// - The C command tables are terminated arrays; here they are slices. C's
//   `_tryCommands` matches commands and aliases by *exact-length*,
//   case-insensitive comparison (aliases provide all the "prefix" shorthands
//   such as `dis`); only tab completion does prefix matching.
// - `debugger->d.p` (the owning mDebugger) arrives as the `debugger`
//   parameter of the `DebuggerModule` vtable methods.
// - Where C calls `platform->setBreakpoint(platform, &debugger->d, ...)`,
//   the module passes `Some(self.module_index)` as the owner.
// - `traceVf` (a VFile opened O_CREAT|O_WRONLY|O_APPEND) is a
//   `std::fs::File` in append mode.
// - `_source` in C feeds the scripting bridge (cli-debugger-scripting.c);
//   per the port task it runs the file as a list of debugger commands
//   instead (GB/GBA have no scripting bridge in this tree).
// - `_breakInto` ("!" command) raises SIGTRAP in debug builds of the C;
//   here it is a no-op stub.
// - The `events` command and `core->reset` are plumbed through new
//   `DebugConsole` hooks (`dbg_events_string`, `dbg_reset`).

use crate::debugger::{
    watchpoint_type, Breakpoint, BreakpointType, DebugConsole, Debugger, DebuggerEntryInfo,
    DebuggerEntryReason, DebuggerModule, DebuggerState, DebuggerType, EntryTypeInfo, Watchpoint,
};
use crate::parser::{evaluate_parse_tree, lex_expression, parse_lexed_expression, ParseTree};
use crate::stack_trace::StackTraceMode;
use std::collections::VecDeque;
use std::io::Write as _;

// cli-debugger.c shared message strings. The C declares the INFO_* as printf
// format strings ("Added breakpoint #%" PRIz "i\n"); here the id is spliced
// with format!.
pub const ERROR_MISSING_ARGS: &str = "Arguments missing";
pub const ERROR_OVERFLOW: &str = "Arguments overflow";
pub const ERROR_INVALID_ARGS: &str = "Invalid arguments";
pub const INFO_BREAKPOINT_ADDED: &str = "Added breakpoint #";
pub const INFO_WATCHPOINT_ADDED: &str = "Added watchpoint #";

/// struct CLIDebugVector payload (enum CLIDVType). `Int` carries
/// (intValue, segmentValue); `Str` carries charValue.
#[derive(Clone, PartialEq, Debug)]
pub enum CliDebugValue {
    Int(i32, i32),
    Str(String),
}

/// CLIDebuggerCommand — a command handler. `dvs` is None where C passes a
/// NULL `struct CLIDebugVector*`.
type CliCommandFn =
    fn(&mut CliDebugger, &mut Debugger, &mut dyn DebugConsole, Option<&[CliDebugValue]>);

/// struct CLIDebuggerCommandSummary
struct CliCommandSummary {
    name: &'static str,
    command: CliCommandFn,
    format: &'static str,
    summary: &'static str,
}

/// struct CLIDebuggerCommandAlias
struct CliCommandAlias {
    name: &'static str,
    original: &'static str,
}

/// struct CLIDebuggerBackend (vtable). Only `printf`, `poll` and `readline`
/// are mandatory.
///
/// `poll(timeout_ms)` returns >0 when input is ready, 0 on timeout and <0 on
/// error/EOF (which shuts the debugger down, like the C).
/// `readline` returns the typed line with no trailing newline; a line that
/// starts with '\n' (a bare Enter in the C libedit backend) repeats the last
/// history entry. `None` or an empty string means EOF and shuts down.
pub trait CliBackend {
    fn init(&mut self) {}
    fn deinit(&mut self) {}
    /// Output sink (C: backend->printf with a format string).
    fn printf(&mut self, text: &str);
    fn poll(&mut self, timeout_ms: i32) -> i32;
    fn readline(&mut self) -> Option<String>;
    /// Append text to the in-progress input line (tab completion).
    fn line_append(&mut self, _line: &str) {}
    fn history_last(&mut self) -> &str {
        ""
    }
    fn history_append(&mut self, _line: &str) {}
    fn interrupt(&mut self) {}
    /// Downcasting support for tests/embedders.
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }
}

/// A backend driven by closures / scripted input queues, for tests and
/// frontends that want to embed the CLI without a TTY.
pub struct CallbackBackend {
    on_line: Box<dyn FnMut(&str)>,
    /// Lines to be returned from `readline` (scripted input).
    pub input: VecDeque<String>,
    history: Vec<String>,
    completed: String,
    interrupts: usize,
    inited: bool,
    deinited: bool,
}

impl CallbackBackend {
    pub fn new(on_line: Box<dyn FnMut(&str)>) -> Self {
        CallbackBackend {
            on_line,
            input: VecDeque::new(),
            history: Vec::new(),
            completed: String::new(),
            interrupts: 0,
            inited: false,
            deinited: false,
        }
    }

    /// Queue a line of input for `readline`.
    pub fn push_line(&mut self, line: &str) {
        self.input.push_back(line.to_string());
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Text appended via `line_append` (tab completion output).
    pub fn completion(&self) -> &str {
        &self.completed
    }

    /// Drain the completion buffer.
    pub fn take_completion(&mut self) -> String {
        std::mem::take(&mut self.completed)
    }

    pub fn interrupt_count(&self) -> usize {
        self.interrupts
    }

    pub fn was_inited(&self) -> bool {
        self.inited
    }

    pub fn was_deinit(&self) -> bool {
        self.deinited
    }
}

impl CliBackend for CallbackBackend {
    fn init(&mut self) {
        self.inited = true;
    }
    fn deinit(&mut self) {
        self.deinited = true;
    }
    fn printf(&mut self, text: &str) {
        (self.on_line)(text);
    }
    fn poll(&mut self, _timeout_ms: i32) -> i32 {
        if self.input.is_empty() {
            0
        } else {
            1
        }
    }
    fn readline(&mut self) -> Option<String> {
        self.input.pop_front()
    }
    fn line_append(&mut self, line: &str) {
        self.completed.push_str(line);
    }
    fn history_last(&mut self) -> &str {
        self.history.last().map(String::as_str).unwrap_or("")
    }
    fn history_append(&mut self, line: &str) {
        self.history.push(line.to_string());
    }
    fn interrupt(&mut self) {
        self.interrupts += 1;
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

/// struct CLIDebugger
pub struct CliDebugger {
    backend: Box<dyn CliBackend>,
    trace_remaining: i32,
    trace_file: Option<std::fs::File>,
    skip_status: bool,
    paused: bool,
    needs_callback: bool,
    /// This module's slot in the owning `Debugger` (C: `&debugger->d` when
    /// registering points). Set by `Debugger::attach_module`.
    module_index: Option<usize>,
    /// Mirrors the mDebuggerShutdown side effects of `quit`/EOF for callers
    /// that drive the CLI without reading `Debugger::state` afterwards.
    quit_requested: bool,
}

/// Result of parsing one argument token: a value, or the C CLIDV_ERROR_TYPE.
enum ParsedArg {
    Value(CliDebugValue),
    Error,
}

/// C `isspace` set.
fn is_c_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r')
}

fn trim_c_space(s: &str) -> &str {
    s.trim_start_matches(is_c_space)
}

/// strncasecmp over `n` bytes (NUL semantics: a shorter string compares low).
fn strncasecmp(a: &str, b: &str, n: usize) -> i32 {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    for i in 0..n {
        let x = ab.get(i).copied().unwrap_or(0).to_ascii_lowercase();
        let y = bb.get(i).copied().unwrap_or(0).to_ascii_lowercase();
        if x != y {
            return if x < y { -1 } else { 1 };
        }
        if x == 0 {
            return 0;
        }
    }
    0
}

impl CliDebugger {
    /// CLIDebuggerCreate + CLIDebuggerAttachBackend.
    pub fn new(backend: impl CliBackend + 'static) -> Self {
        CliDebugger {
            backend: Box::new(backend),
            trace_remaining: 0,
            trace_file: None,
            skip_status: false,
            paused: false,
            needs_callback: false,
            module_index: None,
            quit_requested: false,
        }
    }

    /// Backend access for tests/frontends (e.g. push input lines).
    pub fn backend_mut(&mut self) -> &mut dyn CliBackend {
        &mut *self.backend
    }

    /// Whether `quit` was typed or input reached EOF.
    pub fn should_quit(&self) -> bool {
        self.quit_requested
    }

    fn printf(&mut self, text: &str) {
        self.backend.printf(text);
    }

    /// _printStatus (system->printStatus in C).
    fn print_status(&mut self, console: &mut dyn DebugConsole) {
        let status = console.dbg_print_status();
        self.backend.printf(&status);
    }

    /// CLIDVParse: lex+parse+evaluate a single argument token.
    fn clidv_parse(
        &mut self,
        debugger: &mut Debugger,
        console: &mut dyn DebugConsole,
        token: &str,
    ) -> Option<ParsedArg> {
        if token.is_empty() {
            return None;
        }
        // C: lexExpression(&lv, string, length, " ")
        let (lv, adjusted) = lex_expression(token, token.len(), " ");
        // C: `adjusted > length` marks an error. The ported lexer clamps
        // `adjusted` to the input length, so this is kept for parity.
        if adjusted > token.len() {
            return Some(ParsedArg::Error);
        }
        let Some(tree) = parse_lexed_expression(&lv) else {
            return Some(ParsedArg::Error);
        };
        match evaluate_parse_tree(debugger, console, &tree) {
            Some((value, segment)) => Some(ParsedArg::Value(CliDebugValue::Int(value, segment))),
            None => Some(ParsedArg::Error),
        }
    }

    /// CLIDVStringParse
    fn clidv_string_parse(token: &str) -> Option<ParsedArg> {
        if token.is_empty() {
            return None;
        }
        Some(ParsedArg::Value(CliDebugValue::Str(token.to_string())))
    }

    /// _parseArg
    fn parse_arg(
        &mut self,
        debugger: &mut Debugger,
        console: &mut dyn DebugConsole,
        token: &str,
        ty: char,
    ) -> Option<ParsedArg> {
        match ty {
            'I' | 'i' => self.clidv_parse(debugger, console, token),
            'S' | 's' => Self::clidv_string_parse(token),
            '*' => match self.clidv_parse(debugger, console, token) {
                None => Self::clidv_string_parse(token),
                some => some,
            },
            _ => None,
        }
    }

    /// _tryCommands: alias resolution, exact-length case-insensitive command
    /// match, format-string argument parsing. Returns 1 handled, 0 error
    /// (already reported), -1 no such command.
    fn try_commands(
        &mut self,
        debugger: &mut Debugger,
        console: &mut dyn DebugConsole,
        commands: &[CliCommandSummary],
        aliases: &[CliCommandAlias],
        command: &str,
        args: Option<&str>,
    ) -> i32 {
        let mut command = command;
        for alias in aliases {
            if alias.name.len() == command.len() && alias.name.eq_ignore_ascii_case(command) {
                command = alias.original;
            }
        }
        for cmd in commands {
            if cmd.name.len() != command.len() || !cmd.name.eq_ignore_ascii_case(command) {
                continue;
            }
            let mut dvs: Vec<CliDebugValue> = Vec::new();
            // C: `args = NULL` is also produced when a *non-mandatory* arg
            // fails to parse; None models that pointer state.
            let mut rem: Option<&str> = args;
            if let Some(mut r) = rem {
                let fmt = cmd.format.as_bytes();
                let mut arg = 0usize;
                let mut last_arg: Option<char> = None;
                while arg < fmt.len() && !r.is_empty() {
                    r = trim_c_space(r);
                    if r.is_empty() {
                        self.printf("Wrong number of arguments\n");
                        return 0;
                    }
                    let adjusted = r.find(' ').unwrap_or(r.len());
                    let token = &r[..adjusted];
                    let next_arg_mandatory;
                    let dv_next;
                    if fmt[arg] == b'+' {
                        // C: `--arg` here cancels the loop's `++arg`.
                        next_arg_mandatory = false;
                        dv_next = match last_arg {
                            Some(la) => self.parse_arg(debugger, console, token, la),
                            // C: lastArg '\0' hits no switch case -> NULL
                            None => None,
                        };
                    } else {
                        let fc = fmt[arg] as char;
                        next_arg_mandatory = fc.is_ascii_uppercase() || fc == '*';
                        dv_next = self.parse_arg(debugger, console, token, fc);
                        last_arg = Some(fc);
                    }
                    r = &r[adjusted..];
                    match dv_next {
                        None => {
                            if !next_arg_mandatory {
                                // C: args = NULL (suppress trailing-args error)
                                rem = None;
                            }
                            break;
                        }
                        Some(ParsedArg::Error) => {
                            self.printf("Parse error\n");
                            return 0;
                        }
                        Some(ParsedArg::Value(v)) => dvs.push(v),
                    }
                    if fmt[arg] != b'+' {
                        arg += 1;
                    }
                }
                if rem.is_some() {
                    rem = Some(r);
                }
            }
            if let Some(rest) = rem {
                if !trim_c_space(rest).is_empty() {
                    self.printf("Wrong number of arguments\n");
                    return 0;
                }
            }
            let dvs_opt: Option<&[CliDebugValue]> = if dvs.is_empty() {
                None
            } else {
                Some(dvs.as_slice())
            };
            (cmd.command)(self, debugger, console, dvs_opt);
            return 1;
        }
        -1
    }

    /// CLIDebuggerRunCommand.
    pub fn run_command(
        &mut self,
        debugger: &mut Debugger,
        console: &mut dyn DebugConsole,
        line: &str,
    ) -> bool {
        let (command, args) = match line.find(' ') {
            Some(i) => (&line[..i], Some(&line[i + 1..])),
            None => (line, None),
        };
        let mut result = self.try_commands(
            debugger,
            console,
            DEBUGGER_COMMANDS,
            DEBUGGER_COMMAND_ALIASES,
            command,
            args,
        );
        // C: system->commands (per-emulator-core) tables exist but no console
        // registers one in this tree; then platformCommands (ARM).
        if result < 0 && console.dbg_cli_platform().is_some() {
            result = self.try_commands(
                debugger,
                console,
                ARM_COMMANDS,
                ARM_COMMAND_ALIASES,
                command,
                args,
            );
        }
        if result < 0 {
            self.printf("Command not found\n");
        }
        result > 0
    }

    /// CLIDebuggerContinue. The C also calls mDebuggerUpdatePaused; when the
    /// module is owned by a `Debugger` its next `run_timeout` (or an explicit
    /// `update_paused`) recomputes the global state from these flags.
    pub fn continue_execution(&mut self) {
        self.needs_callback = self.trace_remaining != 0;
        self.paused = false;
    }

    /// _parseTree: parse (not evaluate) the concatenation of `pieces`
    /// (breakpoint/watchpoint conditions).
    fn parse_condition(pieces: &[&str]) -> Option<ParseTree> {
        let mut lv = Vec::new();
        let mut error = false;
        for piece in pieces {
            // C: lexExpression(&lv, string[i], length, NULL) with NULL eol
            // meaning " \r\n".
            let (toks, adjusted) = lex_expression(piece, piece.len(), " \r\n");
            if adjusted == 0 || adjusted > piece.len() {
                error = true;
            }
            lv.extend(toks);
        }
        if error {
            return None;
        }
        parse_lexed_expression(&lv).map(|tree| *tree)
    }

    /// _parseExpression: evaluate the concatenation of all Str debug values.
    fn parse_expression_dvs(
        &mut self,
        debugger: &mut Debugger,
        console: &mut dyn DebugConsole,
        dvs: Option<&[CliDebugValue]>,
    ) -> Option<(i32, i32)> {
        let dvs = dvs?;
        let mut pieces: Vec<&str> = Vec::with_capacity(dvs.len());
        for dv in dvs {
            // C blindly reads dv->charValue; only print/print/t/print/x reach
            // this and their format ("S+") guarantees Str values.
            let CliDebugValue::Str(s) = dv else {
                return None;
            };
            pieces.push(s);
        }
        let mut lv = Vec::new();
        let mut error = false;
        for piece in &pieces {
            let (toks, adjusted) = lex_expression(piece, piece.len(), " \r\n");
            if adjusted == 0 || adjusted > piece.len() {
                error = true;
            }
            lv.extend(toks);
        }
        if error {
            return None;
        }
        let tree = parse_lexed_expression(&lv)?;
        evaluate_parse_tree(debugger, console, &tree)
    }

    /// _doTrace
    fn do_trace(&mut self, console: &mut dyn DebugConsole) -> bool {
        let mut trace = console.dbg_trace();
        // C appends "\n" to the trace line when it fits its 1024-byte buffer;
        // here the string is unbounded.
        trace.push('\n');
        if let Some(file) = &mut self.trace_file {
            let _ = file.write_all(trace.as_bytes());
        } else {
            self.backend.printf(&trace);
        }
        if self.trace_remaining > 0 {
            self.trace_remaining -= 1;
        }
        if self.trace_remaining == 0 {
            self.trace_file = None; // close
            self.needs_callback = false;
            return false;
        }
        true
    }

    /// CLIDebuggerCheckTraceMode
    fn check_trace_mode(&mut self, console: &mut dyn DebugConsole, require_enabled: bool) -> bool {
        if !console.dbg_supports_stack_trace() {
            self.printf("Stack tracing is not supported by this platform.\n");
            false
        } else if require_enabled
            && console.dbg_get_stack_trace_mode() == StackTraceMode::Disabled
        {
            self.printf("Stack tracing is not enabled.\n");
            false
        } else {
            true
        }
    }

    fn set_watchpoint(
        &mut self,
        _debugger: &mut Debugger,
        console: &mut dyn DebugConsole,
        dvs: Option<&[CliDebugValue]>,
        ty: u8,
    ) {
        let Some((addr, segment)) = get_int(dvs, 0) else {
            self.printf(&format!("{}\n", ERROR_MISSING_ARGS));
            return;
        };
        // C: platform->setWatchpoint may be NULL -> "not supported" message.
        // Here the hook is a required trait method (both consoles support it).
        let mut condition = None;
        if let Some(cond) = get_str(dvs, 1) {
            match Self::parse_condition(&[cond]) {
                Some(tree) => condition = Some(tree),
                None => {
                    self.printf(&format!("{}\n", ERROR_INVALID_ARGS));
                    return;
                }
            }
        }
        let wp = Watchpoint {
            id: 0,
            segment,
            min_address: addr as u32,
            max_address: (addr as u32).wrapping_add(1),
            ty,
            condition,
            disabled: false,
        };
        let id = console.dbg_set_watchpoint(self.module_index, &wp);
        if id > 0 {
            self.printf(&format!("{}{}\n", INFO_WATCHPOINT_ADDED, id));
        }
    }

    fn set_range_watchpoint(
        &mut self,
        _debugger: &mut Debugger,
        console: &mut dyn DebugConsole,
        dvs: Option<&[CliDebugValue]>,
        ty: u8,
    ) {
        let Some((start, segment)) = get_int(dvs, 0) else {
            self.printf(&format!("{}\n", ERROR_MISSING_ARGS));
            return;
        };
        let Some((end, end_segment)) = get_int(dvs, 1) else {
            self.printf(&format!("{}\n", ERROR_MISSING_ARGS));
            return;
        };
        if start >= end {
            self.printf("Range watchpoint end is before start. Note that the end of the range is not included.\n");
            return;
        }
        if segment != end_segment {
            self.printf("Range watchpoint does not start and end in the same segment.\n");
            return;
        }
        let mut condition = None;
        if let Some(cond) = get_str(dvs, 2) {
            match Self::parse_condition(&[cond]) {
                Some(tree) => condition = Some(tree),
                None => {
                    self.printf(&format!("{}\n", ERROR_INVALID_ARGS));
                    return;
                }
            }
        }
        let wp = Watchpoint {
            id: 0,
            segment,
            min_address: start as u32,
            max_address: end as u32,
            ty,
            condition,
            disabled: false,
        };
        let id = console.dbg_set_watchpoint(self.module_index, &wp);
        if id > 0 {
            self.printf(&format!("{}{}\n", INFO_WATCHPOINT_ADDED, id));
        }
    }

    /// _disassembleMode (shared generic/ARM/Thumb disassembler driver)
    fn disassemble_mode(
        &mut self,
        console: &mut dyn DebugConsole,
        dvs: Option<&[CliDebugValue]>,
        thumb: Option<bool>,
    ) {
        let (mut address, segment, next_idx) = match get_int(dvs, 0) {
            Some((a, s)) => (a as u32, s, 1),
            // Per-console default: pc (SM83) or pc - instruction size (ARM).
            None => (console.dbg_disassemble_default_address(), -1, 0),
        };
        let size = get_int(dvs, next_idx).map(|(v, _)| v).unwrap_or(1);
        for _ in 0..size.max(0) {
            let (line, next) = console.dbg_disassemble_line(address, segment, thumb);
            self.printf(&line);
            self.printf("\n");
            address = next;
        }
    }

    /// _printCommands
    fn print_commands(&mut self, commands: &[CliCommandSummary], aliases: &[CliCommandAlias]) {
        for cmd in commands {
            self.printf(&format!("{:<15}  {}\n", cmd.name, cmd.summary));
            let mut printed_alias = false;
            for alias in aliases {
                if alias.original == cmd.name {
                    if !printed_alias {
                        self.printf("                 Aliases:");
                        printed_alias = true;
                    }
                    self.printf(&format!(" {}", alias.name));
                }
            }
            if printed_alias {
                self.printf("\n");
            }
        }
    }

    /// _printCommandSummary
    fn print_command_summary(
        &mut self,
        name: &str,
        commands: &[CliCommandSummary],
        aliases: &[CliCommandAlias],
    ) {
        for cmd in commands {
            if cmd.name == name {
                self.printf(&format!(" {}\n", cmd.summary));
                let mut printed_alias = false;
                for alias in aliases {
                    if alias.original == cmd.name {
                        if !printed_alias {
                            self.printf(" Aliases:");
                            printed_alias = true;
                        }
                        self.printf(&format!(" {}", alias.name));
                    }
                }
                if printed_alias {
                    self.printf("\n");
                }
                return;
            }
        }
    }

    /// _backtrace (also called from entered() on stack entries)
    fn backtrace(
        &mut self,
        debugger: &mut Debugger,
        console: &mut dyn DebugConsole,
        dvs: Option<&[CliDebugValue]>,
    ) {
        if !self.check_trace_mode(console, true) {
            return;
        }
        let depth = debugger.stack_trace.depth();
        let mut frames: i64 = depth as i64;
        if let Some((limit, _)) = get_int(dvs, 0) {
            if (limit as i64) < frames {
                frames = limit as i64;
            }
        }
        let mut i: i64 = 0;
        while i < frames {
            // C: mStackTraceFormatFrame with the core's symbol table; the
            // formatRegisters hook (ARMDebuggerFrameFormatRegisters) is not
            // ported, so frames print without the register snapshot.
            let text = debugger.stack_trace.format_frame(
                debugger.symbols.as_ref(),
                i as usize,
                None,
            );
            self.printf(&text);
            i += 1;
        }
    }

    /// _commandLine
    fn command_line(
        &mut self,
        debugger: &mut Debugger,
        console: &mut dyn DebugConsole,
        timeout_ms: i32,
    ) {
        if self.skip_status {
            self.skip_status = false;
        } else {
            self.print_status(console);
        }
        while self.paused && !debugger.is_shutdown() {
            let poll = self.backend.poll(timeout_ms);
            if poll <= 0 {
                if poll < 0 {
                    debugger.shutdown();
                    self.quit_requested = true;
                } else {
                    self.skip_status = true;
                }
                return;
            }
            let Some(line) = self.backend.readline() else {
                debugger.shutdown();
                self.quit_requested = true;
                return;
            };
            if line.is_empty() {
                // C: readline returning a zero-length line shuts down.
                debugger.shutdown();
                self.quit_requested = true;
                return;
            }
            if line.starts_with('\n') {
                let last = self.backend.history_last().to_string();
                if !last.is_empty() {
                    self.run_command(debugger, console, &last);
                }
            } else {
                if line.starts_with('#') {
                    self.skip_status = true;
                } else {
                    self.run_command(debugger, console, &line);
                }
                self.backend.history_append(&line);
            }
        }
    }

    /// _reportEntry
    fn report_entry(
        &mut self,
        debugger: &mut Debugger,
        console: &mut dyn DebugConsole,
        reason: DebuggerEntryReason,
        info: Option<&DebuggerEntryInfo>,
    ) {
        if self.trace_remaining > 0 {
            self.trace_remaining = 0;
        }
        self.skip_status = false;
        match reason {
            DebuggerEntryReason::Manual | DebuggerEntryReason::Attached => {}
            DebuggerEntryReason::Breakpoint => match info {
                Some(info) => {
                    if info.point_id > 0 {
                        self.printf(&format!(
                            "Hit breakpoint {} at 0x{:08X}\n",
                            info.point_id, info.address
                        ));
                    } else {
                        self.printf(&format!("Hit unknown breakpoint at 0x{:08X}\n", info.address));
                    }
                }
                None => {
                    self.printf("Hit breakpoint\n");
                }
            },
            DebuggerEntryReason::Watchpoint => match info {
                Some(info) => {
                    if let Some(EntryTypeInfo::Wp(wp)) = info.type_info {
                        if wp.access_type & watchpoint_type::WRITE != 0 {
                            self.printf(&format!(
                                "Hit watchpoint {} at 0x{:08X}: (new value = 0x{:08X}, old value = 0x{:08X})\n",
                                info.point_id, info.address, wp.new_value, wp.old_value
                            ));
                        } else {
                            self.printf(&format!(
                                "Hit watchpoint {} at 0x{:08X}: (value = 0x{:08X})\n",
                                info.point_id, info.address, wp.old_value
                            ));
                        }
                    } else {
                        // C reads the union member unconditionally; without
                        // Wp info fall back to the no-info message.
                        self.printf("Hit watchpoint\n");
                    }
                }
                None => {
                    self.printf("Hit watchpoint\n");
                }
            },
            DebuggerEntryReason::IllegalOp => match info {
                Some(info) => {
                    let opcode = match info.type_info {
                        Some(EntryTypeInfo::Bp(bp)) => bp.opcode,
                        _ => 0,
                    };
                    self.printf(&format!(
                        "Hit illegal opcode at 0x{:08X}: 0x{:08X}\n",
                        info.address, opcode
                    ));
                }
                None => {
                    self.printf("Hit illegal opcode\n");
                }
            },
            DebuggerEntryReason::Stack => match info {
                Some(info) => {
                    let break_on_call = matches!(
                        info.type_info,
                        Some(EntryTypeInfo::St(st))
                            if st.trace_type == StackTraceMode::BreakOnCall
                    );
                    if break_on_call {
                        // C dereferences frame 0 unconditionally here.
                        let interrupt = debugger
                            .stack_trace
                            .get_frame(0)
                            .map(|f| f.interrupt)
                            .unwrap_or(false);
                        if interrupt {
                            self.printf(&format!("Hit interrupt at at 0x{:08X}\n", info.address));
                        } else {
                            self.printf(&format!("Hit function call at at 0x{:08X}\n", info.address));
                        }
                    } else {
                        self.printf(&format!(
                            "Hit function return at at 0x{:08X}\n",
                            info.address
                        ));
                    }
                }
                None => {
                    self.printf("Hit function call or return\n");
                }
            },
        }
        if reason == DebuggerEntryReason::Stack {
            self.backtrace(debugger, console, None);
        }
    }
}

/// dv[index] as an int (value, segment) or None (C: NULL or non-INT node).
fn get_int(dvs: Option<&[CliDebugValue]>, index: usize) -> Option<(i32, i32)> {
    match dvs?.get(index) {
        Some(CliDebugValue::Int(v, s)) => Some((*v, *s)),
        _ => None,
    }
}

/// dv[index] as a string (C: CHAR node).
fn get_str<'a>(dvs: Option<&'a [CliDebugValue]>, index: usize) -> Option<&'a str> {
    match dvs?.get(index) {
        Some(CliDebugValue::Str(s)) => Some(s.as_str()),
        _ => None,
    }
}

impl DebuggerModule for CliDebugger {
    fn module_type(&self) -> DebuggerType {
        DebuggerType::Cli
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

    fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    fn needs_callback(&self) -> bool {
        self.needs_callback
    }

    fn set_needs_callback(&mut self, needs: bool) {
        self.needs_callback = needs;
    }

    fn debugger_state(&self) -> DebuggerState {
        if self.paused {
            DebuggerState::Paused
        } else if self.needs_callback {
            DebuggerState::Callback
        } else {
            DebuggerState::Running
        }
    }

    fn set_module_index(&mut self, idx: usize) {
        self.module_index = Some(idx);
    }

    /// _cliDebuggerInit
    fn init(&mut self, _debugger: &mut Debugger, _console: &mut dyn DebugConsole) {
        self.trace_remaining = 0;
        self.trace_file = None;
        self.skip_status = false;
        self.backend.init();
        // (no CLIDebuggerSystem init hook: the platform system is on the
        // DebugConsole trait)
    }

    /// _cliDebuggerDeinit
    fn deinit(&mut self, _debugger: &mut Debugger, _console: &mut dyn DebugConsole) {
        self.trace_file = None; // close
        self.backend.deinit();
    }

    /// .paused = _commandLine
    fn paused(
        &mut self,
        debugger: &mut Debugger,
        console: &mut dyn DebugConsole,
        timeout_ms: i32,
    ) {
        self.command_line(debugger, console, timeout_ms);
    }

    /// .entered = _reportEntry
    fn entered(
        &mut self,
        debugger: &mut Debugger,
        console: &mut dyn DebugConsole,
        reason: DebuggerEntryReason,
        info: Option<&DebuggerEntryInfo>,
    ) {
        self.report_entry(debugger, console, reason, info);
    }

    /// _cliDebuggerCustom (trace continuation)
    fn custom(&mut self, _debugger: &mut Debugger, console: &mut dyn DebugConsole) {
        if self.trace_remaining != 0 {
            if !self.do_trace(console) {
                self.paused = true;
                self.needs_callback = false;
            }
        }
        // C: mDebuggerUpdatePaused(debugger->p) runs here; the enclosing
        // Debugger poll recomputes the state from the module flags instead.
    }

    /// _cliDebuggerInterrupt
    fn interrupt(&mut self, _debugger: &mut Debugger, _console: &mut dyn DebugConsole) {
        self.backend.interrupt();
    }
}

// ---------------------------------------------------------------------------
// Commands (static functions named after the C handlers)
// ---------------------------------------------------------------------------

/// _breakInto ("!"): the C raises SIGTRAP; stubbed out here.
fn cmd_break_into(cli: &mut CliDebugger, _d: &mut Debugger, _c: &mut dyn DebugConsole, _dv: Option<&[CliDebugValue]>) {
    // raise(SIGTRAP) requires a native debugger; no-op in the Rust port.
    let _ = cli;
}

/// _continue
fn cmd_continue(cli: &mut CliDebugger, _d: &mut Debugger, _c: &mut dyn DebugConsole, _dv: Option<&[CliDebugValue]>) {
    cli.needs_callback = cli.trace_remaining != 0;
    cli.paused = false;
}

/// _next
fn cmd_next(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, _dv: Option<&[CliDebugValue]>) {
    console.dbg_step();
    if console.dbg_supports_stack_trace()
        && console.dbg_get_stack_trace_mode() != StackTraceMode::Disabled
    {
        console.dbg_update_stack_trace();
    }
    cli.print_status(console);
}

/// _disassemble
fn cmd_disassemble(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    cli.disassemble_mode(console, dv, None);
}

/// _print
fn cmd_print(cli: &mut CliDebugger, debugger: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    match cli.parse_expression_dvs(debugger, console, dv) {
        Some((value, segment)) => {
            if segment >= 0 {
                cli.printf(&format!(" ${:02X}:{:04X}\n", segment, value as u32));
            } else {
                cli.printf(&format!(" {}\n", value as u32));
            }
        }
        None => cli.printf("Parse error\n"),
    }
}

/// _printBin
fn cmd_print_bin(cli: &mut CliDebugger, debugger: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    match cli.parse_expression_dvs(debugger, console, dv) {
        Some((value, _)) => {
            cli.printf(&format!(" 0b{:032b}\n", value as u32));
        }
        None => cli.printf("Parse error\n"),
    }
}

/// _printHex
fn cmd_print_hex(cli: &mut CliDebugger, debugger: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    match cli.parse_expression_dvs(debugger, console, dv) {
        Some((value, _)) => {
            cli.printf(&format!(" 0x{:08X}\n", value as u32));
        }
        None => cli.printf("Parse error\n"),
    }
}

/// _printStatus
fn cmd_print_status(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, _dv: Option<&[CliDebugValue]>) {
    cli.print_status(console);
}

/// _printHelp
fn cmd_help(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    match get_str(dv, 0) {
        None => {
            cli.printf("Generic commands:\n");
            cli.print_commands(DEBUGGER_COMMANDS, DEBUGGER_COMMAND_ALIASES);
            // C: system->commands (emulator-core) table; none is registered
            // on either console in this tree.
            if let Some(platform_name) = console.dbg_cli_platform() {
                cli.printf(&format!("\n{} commands:\n", platform_name));
                cli.print_commands(ARM_COMMANDS, ARM_COMMAND_ALIASES);
            }
        }
        Some(name) => {
            let name = name.to_string();
            cli.print_command_summary(&name, DEBUGGER_COMMANDS, DEBUGGER_COMMAND_ALIASES);
            if console.dbg_cli_platform().is_some() {
                cli.print_command_summary(&name, ARM_COMMANDS, ARM_COMMAND_ALIASES);
            }
        }
    }
}

/// _quit
fn cmd_quit(cli: &mut CliDebugger, debugger: &mut Debugger, _c: &mut dyn DebugConsole, _dv: Option<&[CliDebugValue]>) {
    cli.quit_requested = true;
    debugger.shutdown();
}

/// _reset
fn cmd_reset(cli: &mut CliDebugger, debugger: &mut Debugger, console: &mut dyn DebugConsole, _dv: Option<&[CliDebugValue]>) {
    debugger.stack_trace.clear();
    console.dbg_reset();
    cli.print_status(console);
}

/// _readByte / r/1
fn cmd_read_byte(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    let Some((address, segment)) = get_int(dv, 0) else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    // C uses rawRead8 for segmented addresses and busRead8 otherwise; the
    // port exposes only the raw accessor (see parser.rs Dereference note).
    let value = console.dbg_raw_read(address as u32, segment, 1);
    cli.printf(&format!(" 0x{:02X}\n", value as u8));
}

/// _readHalfword / r/2
fn cmd_read_halfword(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    let Some((address, segment)) = get_int(dv, 0) else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    let address = address as u32;
    let value = if segment >= 0 {
        console.dbg_raw_read(address, segment, 2) // C: address & -1 (unmasked)
    } else {
        console.dbg_raw_read(address & !1, segment, 2)
    };
    cli.printf(&format!(" 0x{:04X}\n", value as u16));
}

/// _readWord / r/4
fn cmd_read_word(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    let Some((address, segment)) = get_int(dv, 0) else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    let address = address as u32;
    let value = if segment >= 0 {
        console.dbg_raw_read(address, segment, 4) // C: address & -3 (unmasked)
    } else {
        console.dbg_raw_read(address & !3, segment, 4)
    };
    cli.printf(&format!(" 0x{:08X}\n", value));
}

/// _writeByte / w/1
fn cmd_write_byte(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    let Some((address, segment)) = get_int(dv, 0) else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    let Some((value, _)) = get_int(dv, 1) else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    let value = value as u32;
    if value > 0xFF {
        cli.printf(&format!("{}\n", ERROR_OVERFLOW));
        return;
    }
    // C: rawWrite8 vs busWrite8 by segment; raw-only here (see cmd_read_byte).
    console.dbg_raw_write(address as u32, segment, 1, value);
}

/// _writeHalfword / w/2
fn cmd_write_halfword(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    let Some((address, segment)) = get_int(dv, 0) else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    let Some((value, _)) = get_int(dv, 1) else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    let value = value as u32;
    if value > 0xFFFF {
        cli.printf(&format!("{}\n", ERROR_OVERFLOW));
        return;
    }
    console.dbg_raw_write(address as u32, segment, 2, value);
}

/// _writeRegister / w/r
fn cmd_write_register(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    let Some(name) = get_str(dv, 0) else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    let Some((value, _)) = get_int(dv, 1) else {
        // C: !dv->next -> MISSING; wrong type (unreachable) -> INVALID
        if dv.map_or(false, |d| d.len() > 1) {
            cli.printf(&format!("{}\n", ERROR_INVALID_ARGS));
        } else {
            cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        }
        return;
    };
    let name = name.to_string();
    if !console.dbg_write_register(&name, value) {
        cli.printf(&format!("{}\n", ERROR_INVALID_ARGS));
    }
}

/// _writeWord / w/4
fn cmd_write_word(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    let Some((address, segment)) = get_int(dv, 0) else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    let Some((value, _)) = get_int(dv, 1) else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    console.dbg_raw_write(address as u32, segment, 4, value as u32);
}

/// _dumpByte/_dumpHalfword/_dumpWord driver (x/1, x/2, x/4).
fn dump_generic(
    cli: &mut CliDebugger,
    console: &mut dyn DebugConsole,
    dvs: Option<&[CliDebugValue]>,
    width: u32,
    per_line: u32,
    default_count: u32,
) {
    let Some((address, segment)) = get_int(dvs, 0) else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    let mut address = address as u32;
    let mut words = default_count;
    if let Some((count, _)) = get_int(dvs, 1) {
        words = count as u32;
    }
    while words > 0 {
        let line = per_line.min(words);
        cli.printf(&format!("0x{:08X}:", address));
        let mut remaining = line;
        while remaining > 0 {
            let value = console.dbg_raw_read(address, segment, width);
            match width {
                1 => cli.printf(&format!(" {:02X}", value as u8)),
                2 => cli.printf(&format!(" {:04X}", value as u16)),
                _ => cli.printf(&format!(" {:08X}", value)),
            }
            address = address.wrapping_add(width);
            words -= 1;
            remaining -= 1;
        }
        cli.printf("\n");
    }
}

fn cmd_dump_byte(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    dump_generic(cli, console, dv, 1, 16, 16);
}

fn cmd_dump_halfword(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    dump_generic(cli, console, dv, 2, 8, 8);
}

fn cmd_dump_word(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    dump_generic(cli, console, dv, 4, 4, 4);
}

/// _source: in C this loads a script into the scripting bridge; the task
/// spec for this port runs the file as debugger commands (one per line).
fn cmd_source(cli: &mut CliDebugger, debugger: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    let Some(path) = get_str(dv, 0) else {
        cli.printf("Needs a filename\n");
        return;
    };
    let path = path.to_string();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            for line in text.lines() {
                let line = line.strip_suffix('\n').unwrap_or(line);
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                cli.run_command(debugger, console, line);
                if cli.quit_requested {
                    return;
                }
            }
        }
        Err(_) => {
            cli.printf("Failed to load script\n");
        }
    }
}

/// _setBreakpoint
fn cmd_set_breakpoint(cli: &mut CliDebugger, _debugger: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    let Some((address, segment)) = get_int(dv, 0) else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    let mut condition = None;
    if let Some(cond) = get_str(dv, 1) {
        match CliDebugger::parse_condition(&[cond]) {
            Some(tree) => condition = Some(tree),
            None => {
                cli.printf(&format!("{}\n", ERROR_INVALID_ARGS));
                return;
            }
        }
    }
    let bp = Breakpoint {
        id: 0,
        address: address as u32,
        segment,
        ty: BreakpointType::Hardware,
        condition,
        disabled: false,
        is_temporary: false,
    };
    let id = console.dbg_set_breakpoint(cli.module_index, &bp);
    if id > 0 {
        cli.printf(&format!("{}{}\n", INFO_BREAKPOINT_ADDED, id));
    }
}

/// _enableBreakpoint
fn cmd_enable_breakpoint(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    let Some(list) = dv else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    // C: `!dv || dv->type != CLIDV_INT_TYPE` -> missing args (formats always
    // produce Int here, so only presence is checked), then toggles each int.
    if !matches!(list.first(), Some(CliDebugValue::Int(..))) {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    }
    for dv2 in list {
        if let CliDebugValue::Int(id, _) = dv2 {
            console.dbg_toggle_breakpoint(*id as i64, true);
        }
    }
}

/// _disableBreakpoint
fn cmd_disable_breakpoint(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    let Some(list) = dv else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    if !matches!(list.first(), Some(CliDebugValue::Int(..))) {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    }
    for dv2 in list {
        if let CliDebugValue::Int(id, _) = dv2 {
            console.dbg_toggle_breakpoint(*id as i64, false);
        }
    }
}

/// _clearBreakpoint
fn cmd_clear_breakpoint(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    let Some((id, _)) = get_int(dv, 0) else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    console.dbg_clear_breakpoint(id as i64);
}

/// _listBreakpoints
fn cmd_list_breakpoints(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, _dv: Option<&[CliDebugValue]>) {
    let list = console.dbg_list_breakpoints(cli.module_index);
    for bp in &list {
        let status = if bp.disabled { 'D' } else { 'E' };
        if bp.segment >= 0 {
            cli.printf(&format!(
                "{} {}: {:02X}:{:X}\n",
                status, bp.id, bp.segment, bp.address
            ));
        } else {
            cli.printf(&format!("{} {}: 0x{:X}\n", status, bp.id, bp.address));
        }
    }
}

/// _listWatchpoints
fn cmd_list_watchpoints(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, _dv: Option<&[CliDebugValue]>) {
    let list = console.dbg_list_watchpoints(cli.module_index);
    for wp in &list {
        let status = if wp.disabled { 'D' } else { 'E' };
        let single = wp.max_address == wp.min_address.wrapping_add(1);
        if wp.segment >= 0 {
            if single {
                cli.printf(&format!(
                    "{} {}: {:02X}:{:X}\n",
                    status, wp.id, wp.segment, wp.min_address
                ));
            } else {
                cli.printf(&format!(
                    "{} {}: {:02X}:{:X}-{:X}\n",
                    status, wp.id, wp.segment, wp.min_address, wp.max_address
                ));
            }
        } else if single {
            cli.printf(&format!("{} {}: 0x{:X}\n", status, wp.id, wp.min_address));
        } else {
            cli.printf(&format!(
                "{} {}: 0x{:X}-0x{:X}\n",
                status, wp.id, wp.min_address, wp.max_address
            ));
        }
    }
}

/// _trace
fn cmd_trace(cli: &mut CliDebugger, _debugger: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    let Some((count, _)) = get_int(dv, 0) else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    if count < 0 {
        cli.printf(&format!("{}\n", ERROR_INVALID_ARGS));
        return;
    }
    cli.trace_remaining = count;
    cli.trace_file = None; // close any previous trace file
    cli.needs_callback = cli.trace_remaining != 0;
    if cli.trace_remaining == 0 {
        return;
    }
    // C: traceVf = VFileOpen(dv->next->charValue, O_CREAT | O_WRONLY | O_APPEND),
    // failure silently falls back to console output in _doTrace.
    if let Some(path) = get_str(dv, 1) {
        cli.trace_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok();
    }
    if cli.do_trace(console) {
        cli.paused = false;
        // C: mDebuggerUpdatePaused(debugger->d.p); the owning Debugger
        // recomputes state at the end of the poll iteration.
    } else {
        cli.print_status(console);
    }
}

/// _events: the C walks core->timing; the console pre-formats the list.
fn cmd_events(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, _dv: Option<&[CliDebugValue]>) {
    let events = console.dbg_events_string();
    cli.backend.printf(&events);
}

/// _backtrace command
fn cmd_backtrace(cli: &mut CliDebugger, debugger: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    cli.backtrace(debugger, console, dv);
}

/// _finish
fn cmd_finish(cli: &mut CliDebugger, debugger: &mut Debugger, console: &mut dyn DebugConsole, _dv: Option<&[CliDebugValue]>) {
    if !cli.check_trace_mode(console, true) {
        return;
    }
    let Some(frame) = debugger.stack_trace.get_frame_mut(0) else {
        cli.printf("No current stack frame.\n");
        return;
    };
    frame.break_when_finished = true;
    cmd_continue(cli, debugger, console, None);
}

/// _setStackTraceMode
fn cmd_set_stack_trace_mode(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    if !cli.check_trace_mode(console, false) {
        return;
    }
    let Some(mode) = get_str(dv, 0) else {
        cli.printf("off           disable stack tracing (default)\n");
        cli.printf("trace-only    enable stack tracing\n");
        cli.printf("break-call    break on function calls\n");
        cli.printf("break-return  break on function returns\n");
        cli.printf("break-all     break on function calls and returns\n");
        return;
    };
    match mode {
        "off" => console.dbg_set_stack_trace_mode(StackTraceMode::Disabled),
        "trace-only" => console.dbg_set_stack_trace_mode(StackTraceMode::Enabled),
        "break-call" => console.dbg_set_stack_trace_mode(StackTraceMode::BreakOnCall),
        "break-return" => console.dbg_set_stack_trace_mode(StackTraceMode::BreakOnReturn),
        "break-all" => console.dbg_set_stack_trace_mode(StackTraceMode::BreakOnBoth),
        _ => {
            cli.printf(&format!("{}\n", ERROR_INVALID_ARGS));
        }
    }
}

/// _loadSymbols (armips format only; the C tries ELF first when USE_ELF).
fn cmd_load_symbols(cli: &mut CliDebugger, debugger: &mut Debugger, _c: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    if debugger.symbols.is_none() {
        cli.printf("No symbol table available.\n");
        return;
    }
    let Some(path) = get_str(dv, 0) else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    if dv.map_or(false, |d| d.len() > 1) {
        // C: `!dv || dv->next` -> ERROR_MISSING_ARGS
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    }
    let path = path.to_string();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            if let Some(symbols) = debugger.symbols.as_mut() {
                symbols.load_armips_symbols(&text);
            }
        }
        Err(_) => {
            cli.printf("Could not open symbol file\n");
        }
    }
}

/// _setSymbol
fn cmd_set_symbol(cli: &mut CliDebugger, debugger: &mut Debugger, _c: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    if debugger.symbols.is_none() {
        cli.printf("No symbol table available.\n");
        return;
    }
    let Some(name) = get_str(dv, 0) else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    let Some((value, segment)) = get_int(dv, 1) else {
        if dv.map_or(false, |d| d.len() > 1) {
            cli.printf(&format!("{}\n", ERROR_INVALID_ARGS));
        } else {
            cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        }
        return;
    };
    if let Some(symbols) = debugger.symbols.as_mut() {
        symbols.add(name, value, segment);
    }
}

/// _findSymbol
fn cmd_find_symbol(cli: &mut CliDebugger, debugger: &mut Debugger, _c: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    if debugger.symbols.is_none() {
        cli.printf("No symbol table available.\n");
        return;
    }
    let Some((value, segment)) = get_int(dv, 0) else {
        cli.printf(&format!("{}\n", ERROR_MISSING_ARGS));
        return;
    };
    let name = debugger
        .symbols
        .as_ref()
        .and_then(|t| t.reverse_lookup(value, segment))
        .map(|s| s.to_string());
    match name {
        Some(name) => {
            if segment >= 0 {
                cli.printf(&format!(" 0x{:02X}:{:08X} = {}\n", segment, value as u32, name));
            } else {
                cli.printf(&format!(" 0x{:08X} = {}\n", value as u32, name));
            }
        }
        None => {
            cli.printf("Not found.\n");
        }
    }
}

// --- ARM platform commands (arm/debugger/cli-debugger.c _armCommands) ---

/// _setBreakpointARM/_setBreakpointThumb
fn cmd_set_breakpoint_sw(cli: &mut CliDebugger, _d: &mut Debugger, console: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>, thumb: bool) {
    let Some((address, _)) = get_int(dv, 0) else {
        // C quirk: no trailing newline on this one.
        cli.printf(ERROR_MISSING_ARGS);
        return;
    };
    let id = console.dbg_set_software_breakpoint(
        cli.module_index,
        address as u32,
        thumb,
    );
    if id > 0 {
        cli.printf(&format!("{}{}\n", INFO_BREAKPOINT_ADDED, id));
    }
}

fn cmd_set_breakpoint_arm(cli: &mut CliDebugger, d: &mut Debugger, c: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    cmd_set_breakpoint_sw(cli, d, c, dv, false);
}

fn cmd_set_breakpoint_thumb(cli: &mut CliDebugger, d: &mut Debugger, c: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    cmd_set_breakpoint_sw(cli, d, c, dv, true);
}

fn cmd_disassemble_arm(cli: &mut CliDebugger, _d: &mut Debugger, c: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    cli.disassemble_mode(c, dv, Some(false));
}

fn cmd_disassemble_thumb(cli: &mut CliDebugger, _d: &mut Debugger, c: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    cli.disassemble_mode(c, dv, Some(true));
}

// Generic watchpoint/range-watchpoint adapters (_setReadWriteWatchpoint etc.)
fn cmd_watch(cli: &mut CliDebugger, d: &mut Debugger, c: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    cli.set_watchpoint(d, c, dv, watchpoint_type::RW);
}
fn cmd_watch_changed(cli: &mut CliDebugger, d: &mut Debugger, c: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    cli.set_watchpoint(d, c, dv, watchpoint_type::WRITE_CHANGE);
}
fn cmd_watch_read(cli: &mut CliDebugger, d: &mut Debugger, c: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    cli.set_watchpoint(d, c, dv, watchpoint_type::READ);
}
fn cmd_watch_write(cli: &mut CliDebugger, d: &mut Debugger, c: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    cli.set_watchpoint(d, c, dv, watchpoint_type::WRITE);
}
fn cmd_watch_range(cli: &mut CliDebugger, d: &mut Debugger, c: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    cli.set_range_watchpoint(d, c, dv, watchpoint_type::RW);
}
fn cmd_watch_range_changed(cli: &mut CliDebugger, d: &mut Debugger, c: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    cli.set_range_watchpoint(d, c, dv, watchpoint_type::WRITE_CHANGE);
}
fn cmd_watch_range_read(cli: &mut CliDebugger, d: &mut Debugger, c: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    cli.set_range_watchpoint(d, c, dv, watchpoint_type::READ);
}
fn cmd_watch_range_write(cli: &mut CliDebugger, d: &mut Debugger, c: &mut dyn DebugConsole, dv: Option<&[CliDebugValue]>) {
    cli.set_range_watchpoint(d, c, dv, watchpoint_type::WRITE);
}

/// _debuggerCommands (generic table; C arrays are sentinel-terminated, order
/// preserved for `help` output and tab completion)
static DEBUGGER_COMMANDS: &[CliCommandSummary] = &[
    CliCommandSummary { name: "backtrace", command: cmd_backtrace, format: "i", summary: "Print backtrace of all or specified frames" },
    CliCommandSummary { name: "break", command: cmd_set_breakpoint, format: "Is", summary: "Set a breakpoint" },
    CliCommandSummary { name: "continue", command: cmd_continue, format: "", summary: "Continue execution" },
    CliCommandSummary { name: "enable", command: cmd_enable_breakpoint, format: "I+", summary: "Enable a breakpoint or watchpoint" },
    CliCommandSummary { name: "disable", command: cmd_disable_breakpoint, format: "I+", summary: "Disable a breakpoint or watchpoint" },
    CliCommandSummary { name: "delete", command: cmd_clear_breakpoint, format: "I", summary: "Delete a breakpoint or watchpoint" },
    CliCommandSummary { name: "disassemble", command: cmd_disassemble, format: "Ii", summary: "Disassemble instructions" },
    CliCommandSummary { name: "events", command: cmd_events, format: "", summary: "Print list of scheduled events" },
    CliCommandSummary { name: "finish", command: cmd_finish, format: "", summary: "Execute until current stack frame returns" },
    CliCommandSummary { name: "help", command: cmd_help, format: "S", summary: "Print help" },
    CliCommandSummary { name: "listb", command: cmd_list_breakpoints, format: "", summary: "List breakpoints" },
    CliCommandSummary { name: "listw", command: cmd_list_watchpoints, format: "", summary: "List watchpoints" },
    CliCommandSummary { name: "load-symbols", command: cmd_load_symbols, format: "S", summary: "Load symbols from an external file" },
    CliCommandSummary { name: "next", command: cmd_next, format: "", summary: "Execute next instruction" },
    CliCommandSummary { name: "print", command: cmd_print, format: "S+", summary: "Print a value" },
    CliCommandSummary { name: "print/t", command: cmd_print_bin, format: "S+", summary: "Print a value as binary" },
    CliCommandSummary { name: "print/x", command: cmd_print_hex, format: "S+", summary: "Print a value as hexadecimal" },
    CliCommandSummary { name: "quit", command: cmd_quit, format: "", summary: "Quit the emulator" },
    CliCommandSummary { name: "reset", command: cmd_reset, format: "", summary: "Reset the emulation" },
    CliCommandSummary { name: "r/1", command: cmd_read_byte, format: "I", summary: "Read a byte from a specified offset" },
    CliCommandSummary { name: "r/2", command: cmd_read_halfword, format: "I", summary: "Read a halfword from a specified offset" },
    CliCommandSummary { name: "r/4", command: cmd_read_word, format: "I", summary: "Read a word from a specified offset" },
    CliCommandSummary { name: "set", command: cmd_set_symbol, format: "SI", summary: "Assign a symbol to an address" },
    CliCommandSummary { name: "source", command: cmd_source, format: "S", summary: "Load a script" },
    CliCommandSummary { name: "stack", command: cmd_set_stack_trace_mode, format: "S", summary: "Change the stack tracing mode" },
    CliCommandSummary { name: "status", command: cmd_print_status, format: "", summary: "Print the current status" },
    CliCommandSummary { name: "symbol", command: cmd_find_symbol, format: "I", summary: "Find the symbol name for an address" },
    CliCommandSummary { name: "trace", command: cmd_trace, format: "Is", summary: "Trace a number of instructions" },
    CliCommandSummary { name: "w/1", command: cmd_write_byte, format: "II", summary: "Write a byte at a specified offset" },
    CliCommandSummary { name: "w/2", command: cmd_write_halfword, format: "II", summary: "Write a halfword at a specified offset" },
    CliCommandSummary { name: "w/r", command: cmd_write_register, format: "SI", summary: "Write a register" },
    CliCommandSummary { name: "w/4", command: cmd_write_word, format: "II", summary: "Write a word at a specified offset" },
    CliCommandSummary { name: "watch", command: cmd_watch, format: "Is", summary: "Set a watchpoint" },
    CliCommandSummary { name: "watch/c", command: cmd_watch_changed, format: "Is", summary: "Set a change watchpoint" },
    CliCommandSummary { name: "watch/r", command: cmd_watch_read, format: "Is", summary: "Set a read watchpoint" },
    CliCommandSummary { name: "watch/w", command: cmd_watch_write, format: "Is", summary: "Set a write watchpoint" },
    CliCommandSummary { name: "watch-range", command: cmd_watch_range, format: "IIs", summary: "Set a range watchpoint" },
    CliCommandSummary { name: "watch-range/c", command: cmd_watch_range_changed, format: "IIs", summary: "Set a change range watchpoint" },
    CliCommandSummary { name: "watch-range/r", command: cmd_watch_range_read, format: "IIs", summary: "Set a read range watchpoint" },
    CliCommandSummary { name: "watch-range/w", command: cmd_watch_range_write, format: "IIs", summary: "Set a write range watchpoint" },
    CliCommandSummary { name: "x/1", command: cmd_dump_byte, format: "Ii", summary: "Examine bytes at a specified offset" },
    CliCommandSummary { name: "x/2", command: cmd_dump_halfword, format: "Ii", summary: "Examine halfwords at a specified offset" },
    CliCommandSummary { name: "x/4", command: cmd_dump_word, format: "Ii", summary: "Examine words at a specified offset" },
    CliCommandSummary { name: "!", command: cmd_break_into, format: "", summary: "Break into attached debugger (for developers)" },
];

/// _debuggerCommandAliases
static DEBUGGER_COMMAND_ALIASES: &[CliCommandAlias] = &[
    CliCommandAlias { name: "b", original: "break" },
    CliCommandAlias { name: "bt", original: "backtrace" },
    CliCommandAlias { name: "c", original: "continue" },
    CliCommandAlias { name: "eb", original: "enable" },
    CliCommandAlias { name: "db", original: "disable" },
    CliCommandAlias { name: "d", original: "delete" },
    CliCommandAlias { name: "dis", original: "disassemble" },
    CliCommandAlias { name: "disasm", original: "disassemble" },
    CliCommandAlias { name: "fin", original: "finish" },
    CliCommandAlias { name: "h", original: "help" },
    CliCommandAlias { name: "i", original: "status" },
    CliCommandAlias { name: "info", original: "status" },
    CliCommandAlias { name: "loadsyms", original: "load-symbols" },
    CliCommandAlias { name: "lb", original: "listb" },
    CliCommandAlias { name: "lw", original: "listw" },
    CliCommandAlias { name: "n", original: "next" },
    CliCommandAlias { name: "p", original: "print" },
    CliCommandAlias { name: "p/t", original: "print/t" },
    CliCommandAlias { name: "p/x", original: "print/x" },
    CliCommandAlias { name: "q", original: "quit" },
    CliCommandAlias { name: "w", original: "watch" },
    CliCommandAlias { name: "watchr", original: "watch-range" },
    CliCommandAlias { name: "wr", original: "watch-range" },
    CliCommandAlias { name: "watchr/c", original: "watch-range/c" },
    CliCommandAlias { name: "wr/c", original: "watch-range/c" },
    CliCommandAlias { name: "watchr/r", original: "watch-range/r" },
    CliCommandAlias { name: "wr/r", original: "watch-range/r" },
    CliCommandAlias { name: "watchr/w", original: "watch-range/w" },
    CliCommandAlias { name: "wr/w", original: "watch-range/w" },
    CliCommandAlias { name: ".", original: "source" },
];

/// _armCommands (arm/debugger/cli-debugger.c), gated on
/// `DebugConsole::dbg_cli_platform() == Some("ARM")`.
static ARM_COMMANDS: &[CliCommandSummary] = &[
    CliCommandSummary { name: "break/a", command: cmd_set_breakpoint_arm, format: "I", summary: "Set a software breakpoint as ARM" },
    CliCommandSummary { name: "break/t", command: cmd_set_breakpoint_thumb, format: "I", summary: "Set a software breakpoint as Thumb" },
    CliCommandSummary { name: "disassemble/a", command: cmd_disassemble_arm, format: "Ii", summary: "Disassemble instructions as ARM" },
    CliCommandSummary { name: "disassemble/t", command: cmd_disassemble_thumb, format: "Ii", summary: "Disassemble instructions as Thumb" },
];

static ARM_COMMAND_ALIASES: &[CliCommandAlias] = &[
    CliCommandAlias { name: "b/a", original: "break/a" },
    CliCommandAlias { name: "b/t", original: "break/t" },
    CliCommandAlias { name: "dis/a", original: "disassemble/a" },
    CliCommandAlias { name: "dis/t", original: "disassemble/t" },
    CliCommandAlias { name: "disasm/a", original: "disassemble/a" },
    CliCommandAlias { name: "disasm/t", original: "disassemble/t" },
];

impl CliDebugger {
    /// CLIDebuggerTabComplete. Operates on the generic command table only
    /// (like the C). `initial` is accepted for parity (unused in the C too).
    pub fn tab_complete(&mut self, token: &str, _initial: bool) -> bool {
        let token_len = token.len();
        let mut cmd = 0usize;
        let mut name: Option<&'static str> = None;
        for len in 1..=token_len {
            loop {
                let Some(n) = DEBUGGER_COMMANDS.get(cmd).map(|c| c.name) else {
                    name = None;
                    break;
                };
                let cmp = strncasecmp(n, token, len);
                if cmp > 0 {
                    return false;
                }
                if cmp == 0 {
                    name = Some(n);
                    break;
                }
                cmd += 1;
            }
            if name.is_none() {
                return false;
            }
        }
        let Some(name) = name else {
            return false;
        };
        // Here the C `len` variable is tokenLen + 1.
        let len = token_len + 1;
        if let Some(next0) = DEBUGGER_COMMANDS.get(cmd + 1).map(|c| c.name) {
            if next0.len() >= len
                && name.len() >= len
                && name.as_bytes()[len - 1] == next0.as_bytes()[len - 1]
            {
                // Multiple matches share the token: extend to the longest
                // common prefix of the first and last matching command.
                let mut len = len - 1; // C: --len
                let mut last_match: Option<&'static str> = None;
                for i in cmd + 1..DEBUGGER_COMMANDS.len() {
                    if strncasecmp(name, DEBUGGER_COMMANDS[i].name, len) != 0 {
                        break;
                    }
                    last_match = Some(DEBUGGER_COMMANDS[i].name);
                }
                let Some(next) = last_match else {
                    return false;
                };
                let nb = name.as_bytes();
                let xb = next.as_bytes();
                let mut out = String::new();
                while len < nb.len() && len < xb.len() {
                    if nb[len] != xb[len] {
                        break;
                    }
                    out.push(nb[len] as char);
                    len += 1;
                }
                if !out.is_empty() {
                    self.backend.line_append(&out);
                }
                return true;
            }
        }
        // Unique match: append the rest of the name plus a space.
        let mut out = name[token_len..].to_string();
        out.push(' ');
        self.backend.line_append(&out);
        true
    }
}
