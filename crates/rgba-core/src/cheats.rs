// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/core/cheats.c and include/mgba/core/cheats.h —
// the shared cheat-list machinery: mCheatList/mCheatSet/mCheatDevice and the
// mCheatRefresh state machine (ROM patching + the idle assignment loop).
// File parsing/saving (mCheatParseFile/mCheatSaveFile and friends) is not
// ported: it needs the VFile abstraction, which is a frontend concern.

use std::collections::HashMap;

/// enum mCheatType
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CheatType {
    Assign,
    AssignIndirect,
    And,
    Add,
    Sub,
    Or,
    IfEq,
    IfNe,
    IfLt,
    IfGt,
    IfLe,
    IfGe,
    IfUlt,
    IfUgt,
    IfAnd,
    IfLand,
    IfNand,
    IfButton,
    Never,
}

/// struct mCheat. The C appends these to a vector with all fields always
/// initialized by the decoders (this port zero-initializes the offsets the C
/// leaves uninitialized).
#[derive(Clone, Copy, Debug)]
pub struct Cheat {
    pub typ: CheatType,
    pub width: i32,
    pub address: u32,
    pub operand: u32,
    pub repeat: u32,
    pub negative_repeat: u32,
    pub address_offset: i32,
    pub operand_offset: i32,
}

/// struct mCheatPatch
#[derive(Clone, Copy, Debug)]
pub struct CheatPatch {
    pub address: u32,
    pub segment: i32,
    pub value: u32,
    pub width: i32,
    pub applied: bool,
    pub check_value: u32,
    pub check: bool,
}

/// struct mCheatPatchedMem (bookkeeping for restoring patched ROM)
struct CheatPatchedMem {
    original_value: u32,
    refs: i32,
    dirty: bool,
}

/// struct mCheatSet, minus the C function-pointer hooks: the per-console
/// `addLine`/`copyProperties`/`parseDirectives` variants become batch of free
/// functions in the console crates, and GB's `refresh` hook is NULL in the C.
pub struct CheatSet {
    /// mCheatList list: the idle-refreshed (per-frame) cheats.
    pub list: Vec<Cheat>,
    /// char* name ("" in place of NULL).
    pub name: String,
    pub enabled: bool,
    /// mCheatPatchList romPatches: the static ROM patches.
    pub rom_patches: Vec<CheatPatch>,
    /// StringList lines: raw code lines, kept verbatim for the save format.
    pub lines: Vec<String>,
}

impl CheatSet {
    /// mCheatSetInit
    pub fn new(name: &str) -> Self {
        Self {
            list: Vec::new(),
            name: name.to_owned(),
            enabled: true,
            rom_patches: Vec::new(),
            lines: Vec::new(),
        }
    }

    /// mCheatSetRename
    pub fn rename(&mut self, name: &str) {
        self.name = name.to_owned();
    }

    /// mCheatAddLine's core-side bookkeeping: runs the per-console `addLine`
    /// hook (`parse`); on success the raw line is kept for save/dump.
    pub fn add_line(&mut self, line: &str, parse: impl FnOnce(&mut Self) -> bool) -> bool {
        if !parse(self) {
            return false;
        }
        self.lines.push(line.to_owned());
        true
    }
}

impl Default for CheatSet {
    /// Default state matches mCheatSetInit (enabled, unnamed).
    fn default() -> Self {
        Self::new("")
    }
}

/// The pieces of the `mCore` vtable that mCheatRefresh drives. Each console
/// implements it over its own bus/raw accessors (GB: load8/store8 and
/// view8/patch8).
pub trait CheatBus {
    /// _readMem: core->busRead8/16/32 selected by `width` (bytes).
    fn read_mem(&mut self, address: u32, width: i32) -> i32;
    /// _writeMem: core->busWrite8/16/32 selected by `width`.
    fn write_mem(&mut self, address: u32, width: i32, value: i32);
    /// _readMemSegment: core->rawRead8/16/32 selected by `width`.
    fn read_mem_segment(&mut self, address: u32, segment: i32, width: i32) -> i32;
    /// _patchMem: core->rawWrite8/16/32 selected by `width`.
    fn patch_mem(&mut self, address: u32, segment: i32, width: i32, value: i32);
    /// mCoreGetMemoryBlockInfo(address)->maxSegment: how many segments the
    /// check-value scan must cover, or None if no block exists.
    fn memory_block_max_segment(&self, address: u32) -> Option<i32>;
}

/// struct mCheatDevice. The `mCore* p` back-pointer is replaced by passing
/// `&mut impl CheatBus` to `cheat_refresh`, per the port's ownership model.
#[derive(Default)]
pub struct CheatDevice {
    /// mCheatSets cheats
    pub cheats: Vec<CheatSet>,
    /// Table unpatchedMemory, keyed by _patchMakeKey.
    unpatched_memory: HashMap<u32, CheatPatchedMem>,
    pub button_down: bool,
}

impl CheatDevice {
    /// mCheatDeviceCreate
    pub fn new() -> Self {
        Self::default()
    }

    /// mCheatDeviceClear
    pub fn clear(&mut self) {
        self.cheats.clear();
    }

    /// mCheatAddSet (the set is moved in; GB sets have no add/remove hooks).
    pub fn add_set(&mut self, cheats: CheatSet) {
        self.cheats.push(cheats);
    }

    /// mCheatRemoveSet (by index instead of pointer identity).
    pub fn remove_set(&mut self, index: usize) {
        if index < self.cheats.len() {
            self.cheats.remove(index);
        }
    }

    /// mCheatPressButton
    pub fn press_button(&mut self, down: bool) {
        self.button_down = down;
    }

    /// mCheatRefresh for the set at `set_index`; the console calls this once
    /// per set at frame end (see GBFrameEnded). The set is temporarily moved
    /// out of the device so the device and the set can be borrowed apart.
    pub fn cheat_refresh(&mut self, core: &mut impl CheatBus, set_index: usize) {
        if set_index >= self.cheats.len() {
            return;
        }
        let mut cheats = std::mem::take(&mut self.cheats[set_index]);

        if cheats.enabled {
            self.patch_rom(core, &mut cheats);
        }
        // GB has no per-set refresh hook (set->refresh == NULL in the C).
        if !cheats.enabled {
            self.unpatch_rom(core, &mut cheats);
            self.cheats[set_index] = cheats;
            return;
        }

        let mut else_loc = 0usize;
        let mut end_loc = 0usize;
        let n_codes = cheats.list.len();
        let mut i = 0usize;
        while i < n_codes {
            let cheat = cheats.list[i];
            let mut value = 0i32;
            let mut operand = cheat.operand as i32;
            let mut operations_remaining = cheat.repeat;
            let mut address = cheat.address;
            let mut perform_assignment = false;
            let mut condition = true;
            let mut condition_remaining = 0i32;
            let mut negative_condition_remaining = 0i32;

            // C: `for (; operationsRemaining; --operationsRemaining)` — the
            // decrement runs AFTER the body, so the IF cases setting
            // `operations_remaining = 1` exit after one iteration.
            while operations_remaining != 0 {
                match cheat.typ {
                    CheatType::Assign => {
                        value = operand;
                        perform_assignment = true;
                    }
                    CheatType::AssignIndirect => {
                        value = operand;
                        address =
                            core.read_mem(address, 4).wrapping_add(cheat.address_offset) as u32;
                        perform_assignment = true;
                    }
                    CheatType::And => {
                        value = core.read_mem(address, cheat.width) & operand;
                        perform_assignment = true;
                    }
                    CheatType::Add => {
                        value = core.read_mem(address, cheat.width).wrapping_add(operand);
                        perform_assignment = true;
                    }
                    CheatType::Sub => {
                        value = core.read_mem(address, cheat.width).wrapping_sub(operand);
                        perform_assignment = true;
                    }
                    CheatType::Or => {
                        value = core.read_mem(address, cheat.width) | operand;
                        perform_assignment = true;
                    }
                    CheatType::IfEq => {
                        condition = core.read_mem(address, cheat.width) == operand;
                        condition_remaining = cheat.repeat as i32;
                        negative_condition_remaining = cheat.negative_repeat as i32;
                        operations_remaining = 1;
                    }
                    CheatType::IfNe => {
                        condition = core.read_mem(address, cheat.width) != operand;
                        condition_remaining = cheat.repeat as i32;
                        negative_condition_remaining = cheat.negative_repeat as i32;
                        operations_remaining = 1;
                    }
                    CheatType::IfLt => {
                        condition = core.read_mem(address, cheat.width) < operand;
                        condition_remaining = cheat.repeat as i32;
                        negative_condition_remaining = cheat.negative_repeat as i32;
                        operations_remaining = 1;
                    }
                    CheatType::IfGt => {
                        condition = core.read_mem(address, cheat.width) > operand;
                        condition_remaining = cheat.repeat as i32;
                        negative_condition_remaining = cheat.negative_repeat as i32;
                        operations_remaining = 1;
                    }
                    CheatType::IfLe => {
                        condition = core.read_mem(address, cheat.width) <= operand;
                        condition_remaining = cheat.repeat as i32;
                        negative_condition_remaining = cheat.negative_repeat as i32;
                        operations_remaining = 1;
                    }
                    CheatType::IfGe => {
                        condition = core.read_mem(address, cheat.width) >= operand;
                        condition_remaining = cheat.repeat as i32;
                        negative_condition_remaining = cheat.negative_repeat as i32;
                        operations_remaining = 1;
                    }
                    CheatType::IfUlt => {
                        condition = (core.read_mem(address, cheat.width) as u32) < operand as u32;
                        condition_remaining = cheat.repeat as i32;
                        negative_condition_remaining = cheat.negative_repeat as i32;
                        operations_remaining = 1;
                    }
                    CheatType::IfUgt => {
                        condition = (core.read_mem(address, cheat.width) as u32) > operand as u32;
                        condition_remaining = cheat.repeat as i32;
                        negative_condition_remaining = cheat.negative_repeat as i32;
                        operations_remaining = 1;
                    }
                    CheatType::IfAnd => {
                        condition = core.read_mem(address, cheat.width) & operand != 0;
                        condition_remaining = cheat.repeat as i32;
                        negative_condition_remaining = cheat.negative_repeat as i32;
                        operations_remaining = 1;
                    }
                    CheatType::IfLand => {
                        condition = core.read_mem(address, cheat.width) != 0 && operand != 0;
                        condition_remaining = cheat.repeat as i32;
                        negative_condition_remaining = cheat.negative_repeat as i32;
                        operations_remaining = 1;
                    }
                    CheatType::IfNand => {
                        condition = core.read_mem(address, cheat.width) & operand == 0;
                        condition_remaining = cheat.repeat as i32;
                        negative_condition_remaining = cheat.negative_repeat as i32;
                        operations_remaining = 1;
                    }
                    CheatType::IfButton => {
                        condition = self.button_down;
                        condition_remaining = cheat.repeat as i32;
                        negative_condition_remaining = cheat.negative_repeat as i32;
                        operations_remaining = 1;
                    }
                    CheatType::Never => {
                        condition = false;
                        condition_remaining = cheat.repeat as i32;
                        negative_condition_remaining = cheat.negative_repeat as i32;
                        operations_remaining = 1;
                    }
                }

                if perform_assignment {
                    core.write_mem(address, cheat.width, value);
                }

                address = address.wrapping_add(cheat.address_offset as u32);
                operand = operand.wrapping_add(cheat.operand_offset);
                operations_remaining -= 1;
            }

            if else_loc != 0 && i == else_loc {
                i = end_loc;
                end_loc = 0;
            }
            if condition_remaining > 0 && !condition {
                i = i.wrapping_add(condition_remaining as usize);
            } else if negative_condition_remaining > 0 {
                else_loc = i.wrapping_add(condition_remaining as usize);
                end_loc = else_loc.wrapping_add(negative_condition_remaining as usize);
            }
            i = i.wrapping_add(1);
        }

        self.cheats[set_index] = cheats;
    }

    /// _patchROM: apply a set's static ROM patches, saving originals so the
    /// patch can be reverted.
    fn patch_rom(&mut self, core: &mut impl CheatBus, cheats: &mut CheatSet) {
        for patch in cheats.rom_patches.iter_mut() {
            let mut segment = -1;
            if patch.check && patch.segment < 0 {
                let Some(max_segment) = core.memory_block_max_segment(patch.address) else {
                    continue;
                };
                let mut found = -1;
                for s in 0..max_segment {
                    let value = core.read_mem_segment(patch.address, s, patch.width) as u32;
                    if value == patch.check_value {
                        found = s;
                        break;
                    }
                }
                if found < 0 {
                    continue;
                }
                segment = found;
            }
            patch.segment = segment;

            let patch_key = patch_make_key(patch);
            match self.unpatched_memory.entry(patch_key) {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(CheatPatchedMem {
                        original_value: core.read_mem_segment(patch.address, segment, patch.width)
                            as u32,
                        refs: 1,
                        dirty: false,
                    });
                }
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    let patch_data = e.get_mut();
                    if !patch.applied {
                        patch_data.refs += 1;
                        patch_data.dirty = true;
                    } else if !patch_data.dirty {
                        continue;
                    }
                }
            }
            core.patch_mem(patch.address, segment, patch.width, patch.value as i32);
            patch.applied = true;
        }
    }

    /// _unpatchROM: revert a disabled set's applied patches.
    fn unpatch_rom(&mut self, core: &mut impl CheatBus, cheats: &mut CheatSet) {
        for patch in cheats.rom_patches.iter_mut() {
            if !patch.applied {
                continue;
            }
            let patch_key = patch_make_key(patch);
            let mut remove = false;
            if let Some(patch_data) = self.unpatched_memory.get_mut(&patch_key) {
                patch_data.refs -= 1;
                patch_data.dirty = true;
                if patch_data.refs <= 0 {
                    core.patch_mem(
                        patch.address,
                        patch.segment,
                        patch.width,
                        patch_data.original_value as i32,
                    );
                    remove = true;
                }
            }
            if remove {
                self.unpatched_memory.remove(&patch_key);
            }
            patch.applied = false;
        }
    }
}

/// _patchMakeKey
fn patch_make_key(patch: &CheatPatch) -> u32 {
    // NB: This assumes patches have only one valid size per platform
    let mut patch_key = patch.address;
    match patch.width {
        2 => patch_key >>= 1,
        4 => patch_key >>= 2,
        _ => {}
    }
    // TODO: More than one segment
    if patch.segment > 0 {
        patch_key |= (patch.segment as u32) << 16;
    }
    patch_key
}
