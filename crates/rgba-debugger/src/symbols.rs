// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Ported from mgba/src/debugger/symbols.c.

use std::collections::HashMap;

pub struct SymbolTable {
    names: HashMap<String, (i32, i32)>,
    reverse: HashMap<(i32, i32), String>,
}

impl SymbolTable {
    pub fn new() -> Self {
        SymbolTable {
            names: HashMap::new(),
            reverse: HashMap::new(),
        }
    }

    /// mDebuggerSymbolLookup: `(value, segment)`
    pub fn lookup(&self, name: &str) -> Option<(i32, i32)> {
        self.names.get(name).copied()
    }

    /// mDebuggerSymbolReverseLookup
    pub fn reverse_lookup(&self, value: i32, segment: i32) -> Option<&str> {
        self.reverse.get(&(value, segment)).map(String::as_str)
    }

    /// mDebuggerSymbolAdd
    pub fn add(&mut self, name: &str, value: i32, segment: i32) {
        self.names.insert(name.to_string(), (value, segment));
        self.reverse.insert((value, segment), name.to_string());
    }

    /// mDebuggerSymbolRemove
    pub fn remove(&mut self, name: &str) {
        if let Some(sym) = self.names.remove(name) {
            self.reverse.remove(&sym);
        }
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// mDebuggerLoadARMIPSSymbols: `<addr8-hex> <name>[,size]` per line.
    pub fn load_armips_symbols(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.trim_end_matches('\n');
            if line.len() < 8 {
                continue;
            }
            let (addr, rest) = line.split_at(8);
            let Ok(address) = u32::from_str_radix(addr, 16) else {
                continue;
            };
            let rest = rest.trim_start();
            if rest.is_empty() || rest.starts_with('.') {
                continue;
            }
            let name = match rest.find(',') {
                Some(i) => &rest[..i],
                None => rest,
            };
            self.add(name, address as i32, -1);
        }
    }
}
