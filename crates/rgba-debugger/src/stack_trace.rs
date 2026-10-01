// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Ported from mgba/src/debugger/stack-trace.c.

use crate::symbols::SymbolTable;

// mStackTraceMode (bitfield)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StackTraceMode {
    Disabled = 0,
    Enabled = 1,
    BreakOnReturn = 2,
    BreakOnCall = 4,
    BreakOnBoth = 6,
}

impl StackTraceMode {
    pub fn has_return(self) -> bool {
        (self as u8) & (StackTraceMode::BreakOnReturn as u8) != 0
    }
    pub fn has_call(self) -> bool {
        (self as u8) & (StackTraceMode::BreakOnCall as u8) != 0
    }
    pub fn is_disabled(self) -> bool {
        self == StackTraceMode::Disabled
    }
}

pub struct StackFrame {
    pub call_segment: i32,
    pub call_address: u32,
    pub entry_segment: i32,
    pub entry_address: u32,
    pub frame_base_segment: i32,
    pub frame_base_address: u32,
    /// Platform register-file snapshot (ARMRegisterFile on GBA).
    pub regs: Vec<u32>,
    pub finished: bool,
    pub break_when_finished: bool,
    pub interrupt: bool,
}

#[derive(Default)]
pub struct StackTrace {
    pub stack: Vec<StackFrame>,
}

impl StackTrace {
    pub fn new() -> Self {
        StackTrace::default()
    }

    pub fn clear(&mut self) {
        self.stack.clear();
    }

    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// mStackTracePush (regs = flat register file snapshot)
    pub fn push(&mut self, pc: u32, dest_address: u32, sp: u32, regs: &[u32]) -> &mut StackFrame {
        self.stack.push(StackFrame {
            call_segment: -1,
            call_address: pc,
            entry_segment: -1,
            entry_address: dest_address,
            frame_base_segment: -1,
            frame_base_address: sp,
            regs: regs.to_vec(),
            finished: false,
            break_when_finished: false,
            interrupt: false,
        });
        self.stack.last_mut().unwrap()
    }

    pub fn push_segmented(
        &mut self,
        pc_segment: i32,
        pc: u32,
        dest_segment: i32,
        dest_address: u32,
        sp_segment: i32,
        sp: u32,
        regs: &[u32],
    ) -> &mut StackFrame {
        let frame = self.push(pc, dest_address, sp, regs);
        frame.call_segment = pc_segment;
        frame.entry_segment = dest_segment;
        frame.frame_base_segment = sp_segment;
        frame
    }

    /// mStackTraceGetFrame: frame 0 is the top of the stack.
    pub fn get_frame(&self, frame: usize) -> Option<&StackFrame> {
        let depth = self.depth();
        if frame >= depth {
            return None;
        }
        Some(&self.stack[depth - frame - 1])
    }

    pub fn get_frame_mut(&mut self, frame: usize) -> Option<&mut StackFrame> {
        let depth = self.depth();
        if frame >= depth {
            return None;
        }
        let idx = depth - frame - 1;
        Some(&mut self.stack[idx])
    }

    /// mStackTraceFormatFrame: `format_registers` renders the snapshot.
    pub fn format_frame(
        &self,
        symbols: Option<&SymbolTable>,
        frame: usize,
        format_registers: Option<&dyn Fn(&StackFrame) -> String>,
    ) -> String {
        let mut out = format!("#{}  ", frame);
        let stack_frame = self.get_frame(frame);
        let prev_frame = self.get_frame(frame + 1);
        let Some(stack_frame) = stack_frame else {
            out.push_str("(no stack frame available)\n");
            return out;
        };
        let reverse = |value: i32, segment: i32| -> Option<String> {
            symbols.and_then(|st| st.reverse_lookup(value, segment).map(String::from))
        };
        match reverse(stack_frame.entry_address as i32, stack_frame.entry_segment) {
            Some(name) => {
                out.push_str(&name);
                out.push(' ');
            }
            None if stack_frame.entry_segment >= 0 => {
                out.push_str(&format!(
                    "0x{:02X}:{:08X} ",
                    stack_frame.entry_segment, stack_frame.entry_address
                ));
            }
            None => {
                out.push_str(&format!("0x{:08X} ", stack_frame.entry_address));
            }
        }
        if let Some(fmt) = format_registers {
            out.push('(');
            out.push_str(&fmt(stack_frame));
            out.push_str(")\n    ");
        }
        if stack_frame.call_segment >= 0 {
            out.push_str(&format!(
                "at 0x{:02X}:{:08X}",
                stack_frame.call_segment, stack_frame.call_address
            ));
        } else {
            out.push_str(&format!("at 0x{:08X}", stack_frame.call_address));
        }
        if let Some(prev_frame) = prev_frame {
            let offset = stack_frame.call_address as i32 - prev_frame.entry_address as i32;
            if offset >= 0 {
                if let Some(name) = reverse(prev_frame.entry_address as i32, prev_frame.entry_segment) {
                    out.push_str(&format!(" [{}+{}]", name, offset));
                } else if prev_frame.entry_segment >= 0 {
                    out.push_str(&format!(
                        " [0x{:02X}:{:08X}+{}]",
                        prev_frame.entry_segment, prev_frame.entry_address, offset
                    ));
                } else {
                    out.push_str(&format!(" [0x{:08X}+{}]", prev_frame.entry_address, offset));
                }
            }
        }
        out.push('\n');
        out
    }

    /// mStackTracePop
    pub fn pop(&mut self) {
        self.stack.pop();
    }
}
