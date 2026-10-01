// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Ported from mgba/src/core/rewind.c (mCoreRewindContext). The C's VFile
// mem-chunks become owned `Vec<u8>` buffers; the threaded-difference mode
// (`onThread`) is not ported — our diffing is cheap enough inline.

use crate::core::Core;
use crate::patch_fast::PatchFast;

pub struct RewindContext {
    patch_memory: Vec<PatchFast>,
    previous_state: Vec<u8>,
    current_state: Vec<u8>,
    size: usize,
    current: usize,
    pub rewind_frame_counter: usize,
}

impl RewindContext {
    /// mCoreRewindContextInit
    pub fn new(entries: usize) -> Self {
        let mut patch_memory = Vec::with_capacity(entries);
        for _ in 0..entries {
            patch_memory.push(PatchFast::new());
        }
        RewindContext {
            patch_memory,
            previous_state: Vec::new(),
            current_state: Vec::new(),
            size: 0,
            current: 0,
            rewind_frame_counter: 0,
        }
    }

    /// mCoreRewindAppend
    pub fn append(&mut self, core: &mut dyn Core) {
        let mut next_state = std::mem::take(&mut self.previous_state);
        // SAVESTATE_SAVEDATA | SAVESTATE_RTC — save everything but omit the
        // big static buffers? The C passes those flags to omit screenshotted
        // framebuffers; our mCoreSaveState is the full state; size check
        // below mirrors the C (both states same layout).
        next_state.clear();
        if core.save_state(&mut next_state).is_err() {
            self.previous_state = next_state;
            return;
        }
        self.previous_state = std::mem::take(&mut self.current_state);
        self.current_state = next_state;
        self.rewind_diff();
    }

    fn rewind_diff(&mut self) {
        let mut size = self.previous_state.len();
        if size == 0 {
            // We are appending the first state
            return;
        }
        self.current += 1;
        if self.size < self.patch_memory.len() {
            self.size += 1;
        }
        if self.current >= self.patch_memory.len() {
            self.current = 0;
        }
        let size2 = self.current_state.len();
        if size2 > size {
            self.previous_state.resize(size2, 0);
            size = size2;
        } else if size > size2 {
            self.current_state.resize(size, 0);
        }
        let size = self.previous_state.len();
        self.patch_memory[self.current].diff(&self.previous_state[..size], &self.current_state[..size]);
    }

    /// mCoreRewindRestore
    pub fn restore(&mut self, core: &mut dyn Core, mut count: usize) -> bool {
        if self.size == 0 {
            return false;
        }
        while count > 0 && self.size > 0 {
            if self.current == 0 {
                self.current = self.patch_memory.len();
            }
            self.current -= 1;

            if self.size > 1 {
                let size2 = self.previous_state.len();
                let mut size = self.current_state.len();
                if size2 < size {
                    size = size2;
                }
                // applyPatch(patch, previous(map WRITE), size, current(map READ),
                // size) in the C; the XOR extents are their own inverse, so
                // applying the diff to the newer state reproduces the older.
                let cur = self.current_state[..size].to_vec();
                let prev = &mut self.previous_state;
                self.patch_memory[self.current].apply(&cur, &mut prev[..size]);
            }
            std::mem::swap(&mut self.previous_state, &mut self.current_state);
            self.size -= 1;
            count -= 1;
        }

        let state = std::mem::take(&mut self.previous_state);
        let r = core.load_state(&state);
        self.previous_state = state;
        let _ = r;
        true
    }
}
