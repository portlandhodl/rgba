// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Ported from mgba/src/debugger/access-logger.c and
// mgba/include/mgba/internal/debugger/access-logger.h
// (`mDebuggerAccessLogger`, `mDebuggerAccessLogRegion`, the mAL\1 file
// format).
//
// Structural deviations from the C, all noted inline where they apply:
//
// - The C mmaps a backing VFile and keeps the per-address flag blocks at
//   recorded file offsets inside the mapping; `region->block` points into
//   the file image. Here the flag blocks live in memory (`Vec`s) and the
//   on-disk format is materialized by `serialize` / parsed by
//   `deserialize` (the public entry points stay open/close/start/stop
//   shaped). The byte layout produced/consumed matches the C file.
// - The C installs a real WATCHPOINT_RW watchpoint per logged region
//   (`_setupRegion` via `platform->setWatchpoint`) so the memory shim
//   routes hits into `_mDebuggerAccessLoggerEntered`. The Rust consoles
//   instead record accesses directly from their existing flag-checked
//   memory-shim hooks (`dbg_watch_load*`/`dbg_watch_store*`, i.e. the
//   memory-debugger.c call sites, which this port routes through
//   `AccessLoggerCore::record_access`) into the core they hold while
//   logging is started. Coverage is the same set of bus accesses
//   (loads/stores of width 1/2/4 and load/store-multiple); the C
//   `setActiveRegion` shim is a plain pass-through and stays a no-op.
//   `region.watchpoint` is kept for struct parity but is always -1.
// - Execution and illegal-opcode logging are unchanged in mechanism:
//   `needs_callback` drives `custom()` (= `_mDebuggerAccessLoggerCallback`,
//   via `dbg_next_instruction_info`) and `entered` handles
//   `DebuggerEntryReason::IllegalOp` (GBA routes it; see gba.rs
//   `hit_illegal`). On GB, driving through `Gb::debugger_run_frame`
//   detaches the whole `GbDebugger` (no placeholder queue, unlike GBA),
//   so neither shim recording nor `custom()` fire mid-frame there — the
//   same pre-existing gap GB watchpoints have; step with the debugger
//   resident (`dbg_step`/`step`) to record on GB.
//
// Usage order (mirrors fuzz-main.c: init -> attach -> open -> start):
// configure regions with `watch_memory_block_*`, `attach_module`, then
// `start(console)`, and finally refresh the run state with
// `Debugger::set_module_needs_callback(console, idx)` (C's
// `mDebuggerModuleSetNeedsCallback` from `_setupRegion`). The two-step is
// needed because `start` borrows the module while the state refresh
// borrows the module list.

use crate::debugger::{
    access_log_flags, access_log_flags_ex, watchpoint_type, Debugger, DebuggerEntryInfo,
    DebuggerEntryReason, DebuggerInstructionInfo, DebuggerModule, DebuggerType, EntryTypeInfo,
    MemoryAccessSource, INSN_LENGTH_MAX,
};
use crate::DebugConsole;

/// mAL_MAGIC ("mAL\1")
pub const MAL_MAGIC: [u8; 4] = *b"mAL\x01";
/// Header version stored at offset 4 (C checks == 1).
pub const MAL_VERSION: u32 = 1;
/// sizeof(struct mDebuggerAccessLogHeader)
pub const HEADER_SIZE: usize = 0x40;
/// sizeof(struct mDebuggerAccessLogRegionInfo)
pub const REGION_INFO_SIZE: usize = 0x30;
/// DEFAULT_MAX_REGIONS
pub const DEFAULT_MAX_REGIONS: u8 = 20;

// Header field offsets (struct mDebuggerAccessLogHeader).
const HDR_N_REGIONS: usize = 0x10;
const HDR_REGION_CAPACITY: usize = 0x11;
const HDR_PLATFORM: usize = 0x14;

// Region-info field offsets (struct mDebuggerAccessLogRegionInfo).
const RI_START: usize = 0x00;
const RI_END: usize = 0x04;
const RI_SIZE: usize = 0x08;
const RI_SEGMENT_START: usize = 0x0C;
const RI_FILE_OFFSET: usize = 0x10;
const RI_FILE_OFFSET_EX: usize = 0x18;
const RI_FLAGS: usize = 0x20;
const RI_RESERVED: usize = 0x28;

/// mDebuggerAccessLogRegionFlags (bitfield over u64)
pub mod access_log_region_flags {
    pub const HAS_EX_BLOCK: u64 = 1 << 0;
}

/// mPlatform values stored in the header (mgba/core/core.h).
pub const MAL_PLATFORM_GBA: u32 = 0;
pub const MAL_PLATFORM_GB: u32 = 1;

/// mCoreMemoryBlockFlags (mgba/core/interface.h)
pub mod core_memory_flags {
    pub const READ: u32 = 0x01;
    pub const WRITE: u32 = 0x02;
    pub const RW: u32 = 0x03;
    pub const WORM: u32 = 0x04;
    pub const MAPPED: u32 = 0x10;
    pub const VIRTUAL: u32 = 0x20;
}

/// mCoreMemoryBlock, as consumed by
/// mDebuggerAccessLoggerWatchMemoryBlockId/Name (the GDB memory-map's
/// `MemoryBlockInfo` is a smaller subset of the same list). Produced by
/// `DebugConsole::dbg_list_memory_blocks_full`.
#[derive(Clone, Copy, Debug)]
pub struct CoreMemoryBlock {
    pub id: i32,
    pub internal_name: &'static str,
    pub start: u32,
    pub end: u32,
    pub size: u32,
    pub flags: u32,
    /// mCoreMemoryBlock::maxSegments (0 when unsegmented)
    pub max_segments: u32,
    /// mCoreMemoryBlock::segmentStart (only meaningful when segmented)
    pub segment_start: u32,
}

impl CoreMemoryBlock {
    /// Unsegmented block shorthand (most table entries).
    pub const fn flat(
        id: i32,
        internal_name: &'static str,
        start: u32,
        end: u32,
        size: u32,
        flags: u32,
    ) -> Self {
        CoreMemoryBlock {
            id,
            internal_name,
            start,
            end,
            size,
            flags,
            max_segments: 0,
            segment_start: 0,
        }
    }
}

/// mDebuggerAccessLogRegion
pub struct AccessLogRegion {
    pub start: u32,
    pub end: u32,
    pub size: u32,
    pub segment_start: u32,
    /// mDebuggerAccessLogFlags per address (access_log_flags::*).
    pub block: Vec<u8>,
    /// mDebuggerAccessLogFlagsEx per address, when the region has an ex block.
    pub block_ex: Option<Vec<u16>>,
    /// C: watchpoint id installed for this region. This port records from
    /// the console memory-shim hooks instead, so it stays -1.
    pub watchpoint: i64,
}

/// The logger state minus the `mDebuggerModule` vtable: regions, header
/// bookkeeping and the recording primitives. The module (`entered`/
/// `custom`) and the console memory-shim hooks both record through this.
pub struct AccessLoggerCore {
    /// mDebuggerAccessLogRegionList regions
    pub regions: Vec<AccessLogRegion>,
    /// header.regionCapacity (max regions the file can hold)
    pub region_capacity: u8,
    /// header.flags (mDebuggerAccessLogHeaderFlags; no bits defined in C)
    pub header_flags: u64,
    /// header.platform of the console this log belongs to.
    pub platform: u32,
}

impl AccessLoggerCore {
    pub fn new(platform: u32) -> Self {
        AccessLoggerCore {
            regions: Vec::new(),
            region_capacity: DEFAULT_MAX_REGIONS,
            header_flags: 0,
            platform,
        }
    }

    fn has_ex_block(flags: u64) -> bool {
        flags & access_log_region_flags::HAS_EX_BLOCK != 0
    }

    /// mDebuggerAccessLoggerGetRegion: find the region containing
    /// `address` (folding banked `segment`s over the region when set) and
    /// return (region index, offset into the block).
    pub fn get_region(&self, address: u32, segment: i32) -> Option<(usize, usize)> {
        for (i, region) in self.regions.iter().enumerate() {
            if address < region.start || address >= region.end {
                continue;
            }
            let mut offset = (address - region.start) as usize;
            if segment > 0 {
                let segment_size = (region.end.wrapping_sub(region.segment_start)) as usize;
                if segment_size == 0 {
                    continue;
                }
                offset %= segment_size;
                offset += segment_size * segment as usize;
            }
            if offset >= region.size as usize {
                continue;
            }
            return Some((i, offset));
        }
        None
    }

    /// Record a bus access of `width` bytes at `address`. `rw` carries
    /// watchpoint_type::READ/WRITE bits; `flags_ex` carries the
    /// access_log_flags_ex::ACCESS_* source bits (0 when unknown).
    ///
    /// This is the body of the WATCHPOINT case of
    /// `_mDebuggerAccessLoggerEntered`, factored out so the console shim
    /// hooks (this port's recording path) share it.
    pub fn record_access(&mut self, address: u32, segment: i32, width: u32, rw: u8, flags_ex: u16) {
        let fill = match width {
            1 => access_log_flags::ACCESS8,
            2 => access_log_flags::ACCESS16,
            4 => access_log_flags::ACCESS32,
            8 => access_log_flags::ACCESS64,
            _ => return,
        };
        let Some((region_idx, offset)) = self.get_region(address, segment) else {
            return;
        };
        // offset &= -info->width
        let offset = offset & !(width as usize - 1);
        let mut flags = 0u8;
        if rw & watchpoint_type::WRITE != 0 {
            flags |= access_log_flags::WRITE;
        }
        if rw & watchpoint_type::READ != 0 {
            flags |= access_log_flags::READ;
        }
        let region = &mut self.regions[region_idx];
        // NB: C writes width bytes unconditionally; clamp at the block end
        // so a tail access can't wander out of the region.
        let end = (offset + width as usize).min(region.size as usize);
        for i in offset..end {
            // region->block[offset] = flags | FillAccessN(region->block[offset])
            region.block[i] |= flags | fill;
            if let Some(block_ex) = region.block_ex.as_mut() {
                block_ex[i] |= flags_ex;
            }
        }
    }

    /// `_mDebuggerAccessLoggerCallback`: mark the just-executed
    /// instruction bytes (EXECUTE + per-byte flags from the platform's
    /// next-instruction info).
    pub fn record_instruction(&mut self, info: &DebuggerInstructionInfo) {
        let Some((region_idx, offset)) = self.get_region(info.address, info.segment) else {
            return;
        };
        let region = &mut self.regions[region_idx];
        let width = (info.width as usize).min(INSN_LENGTH_MAX);
        for i in 0..width {
            if offset + i >= region.size as usize {
                break;
            }
            region.block[offset + i] |= access_log_flags::EXECUTE | info.flags[i];
            if let Some(block_ex) = region.block_ex.as_mut() {
                block_ex[offset + i] |= info.flags_ex[i];
            }
        }
    }

    /// The DEBUGGER_ENTER_ILLEGAL_OP case of `_mDebuggerAccessLoggerEntered`.
    pub fn record_illegal_op(&mut self, address: u32, segment: i32) {
        let Some((region_idx, offset)) = self.get_region(address, segment) else {
            return;
        };
        let region = &mut self.regions[region_idx];
        region.block[offset] |= access_log_flags::EXECUTE;
        if let Some(block_ex) = region.block_ex.as_mut() {
            block_ex[offset] |= access_log_flags_ex::ERROR_ILLEGAL_OPCODE;
        }
    }

    /// `_mDebuggerAccessLoggerWatchMemoryBlock` (minus the file remap: the
    /// blocks are in memory here). Returns the region id or -1.
    pub fn watch_memory_block(&mut self, block: &CoreMemoryBlock, flags: u64) -> i32 {
        if self.regions.len() >= self.region_capacity as usize {
            return -1;
        }
        if block.flags & core_memory_flags::MAPPED == 0 {
            return -1;
        }
        for (i, region) in self.regions.iter_mut().enumerate() {
            if region.start != block.start
                || region.end != block.end
                || region.size != block.size
            {
                continue;
            }
            // Upgrade in place to an ex-block region (C: extends the file
            // and remaps).
            if region.block_ex.is_none() && Self::has_ex_block(flags) {
                region.block_ex = Some(vec![0u16; region.size as usize]);
            }
            return i as i32;
        }
        let id = self.regions.len();
        self.regions.push(AccessLogRegion {
            start: block.start,
            end: block.end,
            size: block.size,
            segment_start: block.segment_start,
            block: vec![0u8; block.size as usize],
            block_ex: if Self::has_ex_block(flags) {
                Some(vec![0u16; block.size as usize])
            } else {
                None
            },
            watchpoint: -1,
        });
        id as i32
    }

    /// mDebuggerAccessLoggerCreateShadowFile: a `region.size` image where
    /// bytes never accessed read as `fill` and the rest come from the
    /// console (C: `core->rawRead8`, supplied here as a closure).
    pub fn create_shadow_file(
        &self,
        region_id: i32,
        fill: u8,
        mut raw_read8: impl FnMut(u32, i32) -> u8,
    ) -> Option<Vec<u8>> {
        if region_id < 0 {
            return None;
        }
        let region = self.regions.get(region_id as usize)?;
        let mut out = Vec::with_capacity(region.size as usize);
        let mut segment: i32 = 0;
        let mut segment_address = region.start;
        let segment_end = region.end;
        for i in 0..region.size as usize {
            // NB: mirrors the C test/increment order exactly, including the
            // extra segment bump when segmentAddress wraps back onto
            // segmentStart.
            if segment_address == region.segment_start && segment_address != region.start {
                segment += 1;
            }
            if segment_address == segment_end {
                segment_address = region.segment_start;
                segment += 1;
            }
            if region.block[i] != 0 {
                out.push(raw_read8(segment_address, segment));
            } else {
                out.push(fill);
            }
            segment_address = segment_address.wrapping_add(1);
        }
        Some(out)
    }

    /// mDebuggerAccessLoggerLoad: parse the mAL\1 image. `expect_platform`
    /// checks the header platform like the C (`core->platform`).
    pub fn deserialize(&mut self, data: &[u8], expect_platform: Option<u32>) -> bool {
        if data.len() < HEADER_SIZE || data[0..4] != MAL_MAGIC {
            return false;
        }
        let Some(version) = rd_u32(data, 0x04) else {
            return false;
        };
        if version != MAL_VERSION {
            return false;
        }
        if let Some(platform) = expect_platform {
            if rd_u32(data, HDR_PLATFORM) != Some(platform) {
                return false;
            }
        }
        self.header_flags = rd_u64(data, 0x08).unwrap_or(0);
        let n_regions = data[HDR_N_REGIONS];
        let region_capacity = data[HDR_REGION_CAPACITY];
        self.platform = rd_u32(data, HDR_PLATFORM).unwrap_or(self.platform);
        self.region_capacity = region_capacity.max(1);
        self.regions.clear();
        for i in 0..n_regions as usize {
            let info = HEADER_SIZE + i * REGION_INFO_SIZE;
            let (Some(start), Some(end), Some(size), Some(segment_start), Some(file_offset), Some(rflags)) = (
                rd_u32(data, info + RI_START),
                rd_u32(data, info + RI_END),
                rd_u32(data, info + RI_SIZE),
                rd_u32(data, info + RI_SEGMENT_START),
                rd_u64(data, info + RI_FILE_OFFSET),
                rd_u64(data, info + RI_FLAGS),
            ) else {
                self.regions.clear();
                return false;
            };
            // _mapRegion bounds checks.
            let block_off = file_offset as usize;
            if block_off < HEADER_SIZE
                || data.len() <= block_off
                || data.len() < block_off + size as usize
            {
                self.regions.clear();
                return false;
            }
            let block = data[block_off..block_off + size as usize].to_vec();
            let mut block_ex = None;
            if Self::has_ex_block(rflags) {
                if let Some(ex_off) = rd_u64(data, info + RI_FILE_OFFSET_EX) {
                    if ex_off != 0 {
                        let ex_off = ex_off as usize;
                        if ex_off < HEADER_SIZE || data.len() < ex_off + size as usize * 2 {
                            self.regions.clear();
                            return false;
                        }
                        let mut v = Vec::with_capacity(size as usize);
                        for b in 0..size as usize {
                            v.push(u16::from_le_bytes([
                                data[ex_off + b * 2],
                                data[ex_off + b * 2 + 1],
                            ]));
                        }
                        block_ex = Some(v);
                    }
                }
            }
            self.regions.push(AccessLogRegion {
                start,
                end,
                size,
                segment_start,
                block,
                block_ex,
                watchpoint: -1,
            });
        }
        true
    }

    /// Serialize to the C on-disk layout: header, capacity-padded region
    /// info array, then each region's block (followed by its ex block when
    /// present), matching the C append-at-EOF allocation order.
    pub fn serialize(&self) -> Vec<u8> {
        let capacity = self
            .region_capacity
            .max(self.regions.len() as u8)
            .max(1);
        let mut out = vec![0u8; HEADER_SIZE + capacity as usize * REGION_INFO_SIZE];
        out[0..4].copy_from_slice(&MAL_MAGIC);
        out[4..8].copy_from_slice(&MAL_VERSION.to_le_bytes());
        out[8..16].copy_from_slice(&self.header_flags.to_le_bytes());
        out[HDR_N_REGIONS] = self.regions.len() as u8;
        out[HDR_REGION_CAPACITY] = capacity;
        out[HDR_PLATFORM..HDR_PLATFORM + 4].copy_from_slice(&self.platform.to_le_bytes());
        let mut cursor = out.len() as u64;
        for (i, region) in self.regions.iter().enumerate() {
            let info = HEADER_SIZE + i * REGION_INFO_SIZE;
            out[info + RI_START..info + RI_START + 4].copy_from_slice(&region.start.to_le_bytes());
            out[info + RI_END..info + RI_END + 4].copy_from_slice(&region.end.to_le_bytes());
            out[info + RI_SIZE..info + RI_SIZE + 4].copy_from_slice(&region.size.to_le_bytes());
            out[info + RI_SEGMENT_START..info + RI_SEGMENT_START + 4]
                .copy_from_slice(&region.segment_start.to_le_bytes());
            out[info + RI_FILE_OFFSET..info + RI_FILE_OFFSET + 8]
                .copy_from_slice(&cursor.to_le_bytes());
            let mut rflags = 0u64;
            if region.block_ex.is_some() {
                rflags |= access_log_region_flags::HAS_EX_BLOCK;
                out[info + RI_FILE_OFFSET_EX..info + RI_FILE_OFFSET_EX + 8]
                    .copy_from_slice(&(cursor + region.size as u64).to_le_bytes());
            }
            out[info + RI_FLAGS..info + RI_FLAGS + 8].copy_from_slice(&rflags.to_le_bytes());
            out[info + RI_RESERVED..info + RI_RESERVED + 8].copy_from_slice(&0u64.to_le_bytes());
            out.extend_from_slice(&region.block);
            cursor += region.size as u64;
            if let Some(block_ex) = &region.block_ex {
                for v in block_ex {
                    out.extend_from_slice(&v.to_le_bytes());
                }
                cursor += region.size as u64 * 2;
            }
        }
        out
    }
}

fn rd_u32(data: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(off..off + 4)?.try_into().ok()?))
}

fn rd_u64(data: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(data.get(off..off + 8)?.try_into().ok()?))
}

/// mDebuggerAccessLogger: the module. Owns the core until `start` hands it
/// to the console (which then records into it from its memory-shim hooks);
/// `stop`/`close` take it back.
pub struct AccessLogger {
    core: Option<AccessLoggerCore>,
    platform: u32,
    is_paused: bool,
    needs_callback: bool,
    module_index: usize,
}

impl AccessLogger {
    /// mDebuggerAccessLoggerInit.
    pub fn new(platform: u32) -> Self {
        AccessLogger {
            core: Some(AccessLoggerCore::new(platform)),
            platform,
            is_paused: false,
            needs_callback: false,
            module_index: 0,
        }
    }

    /// The owned core. `None` while it is installed in a console (between
    /// `start` and `stop`); query `DebugConsole::dbg_access_log` then.
    pub fn access_log(&self) -> Option<&AccessLoggerCore> {
        self.core.as_ref()
    }

    pub fn access_log_mut(&mut self) -> Option<&mut AccessLoggerCore> {
        self.core.as_mut()
    }

    /// mDebuggerAccessLoggerOpen. `data` is the existing file image, if
    /// any; `create`/`truncate` mirror O_CREAT/O_TRUNC.
    pub fn open_bytes(&mut self, data: Option<&[u8]>, create: bool, truncate: bool) -> bool {
        // C closes any previous backing first; a started logger's core
        // lives in the console, so only allow opening while module-owned.
        let Some(_) = self.core.as_ref() else {
            return false;
        };
        let mut core = AccessLoggerCore::new(self.platform);
        let mut loaded = false;
        if !truncate {
            if let Some(data) = data {
                if data.len() >= HEADER_SIZE {
                    loaded = core.deserialize(data, Some(self.platform));
                }
            }
        }
        if create && (truncate || !loaded) {
            core = AccessLoggerCore::new(self.platform);
            loaded = true;
        }
        if loaded {
            self.core = Some(core);
        }
        loaded
    }

    /// Materialize the mAL\1 image (the C keeps it live in the mmap).
    pub fn save_bytes(&self) -> Option<Vec<u8>> {
        self.core.as_ref().map(|c| c.serialize())
    }

    /// mDebuggerAccessLoggerClose.
    pub fn close(&mut self, console: &mut dyn DebugConsole) -> bool {
        self.stop(console);
        self.core = None;
        true
    }

    /// mDebuggerAccessLoggerStart: install the core into the console so
    /// its memory-shim hooks record into it (C: `_setupRegion` per
    /// region), and mark the module for the per-step exec callback.
    ///
    /// After re-stowing the module, the caller must refresh the debugger
    /// state with `Debugger::set_module_needs_callback(console, idx)`
    /// (C's `mDebuggerModuleSetNeedsCallback` from `_setupRegion`).
    pub fn start(&mut self, console: &mut dyn DebugConsole) -> bool {
        let Some(core) = self.core.take() else {
            return false;
        };
        match console.dbg_set_access_log(Some(Box::new(core))) {
            // Consoles hold a single logger core; `Some` comes back either
            // from a console without logger support (it returns its input)
            // or from replacing a previous install — treat both as failure.
            Some(returned) => {
                self.core = Some(*returned);
                false
            }
            None => {
                self.needs_callback = true;
                true
            }
        }
    }

    /// mDebuggerAccessLoggerStop (C: clears the region watchpoints and the
    /// callback flag; here: reclaim the core from the console). Callers
    /// should follow with `Debugger::clear_module_needs_callback`.
    pub fn stop(&mut self, console: &mut dyn DebugConsole) {
        if let Some(core) = console.dbg_set_access_log(None) {
            self.core = Some(*core);
        }
        self.needs_callback = false;
    }

    /// mDebuggerAccessLoggerGetRegion (query helper for embedders).
    pub fn get_region(&self, address: u32, segment: i32) -> Option<(usize, usize)> {
        self.core.as_ref()?.get_region(address, segment)
    }

    /// mDebuggerAccessLoggerCreateShadowFile.
    pub fn create_shadow_file(
        &self,
        region_id: i32,
        fill: u8,
        raw_read8: impl FnMut(u32, i32) -> u8,
    ) -> Option<Vec<u8>> {
        self.core.as_ref()?.create_shadow_file(region_id, fill, raw_read8)
    }

    /// mDebuggerAccessLoggerWatchMemoryBlockId. Only valid before `start`
    /// (or after `stop`), while the core is module-owned.
    pub fn watch_memory_block_id(
        &mut self,
        console: &mut dyn DebugConsole,
        id: i32,
        flags: u64,
    ) -> i32 {
        let Some(core) = self.core.as_mut() else {
            return -1;
        };
        for block in console.dbg_list_memory_blocks_full() {
            if block.id == id {
                return core.watch_memory_block(&block, flags);
            }
        }
        -1
    }

    /// mDebuggerAccessLoggerWatchMemoryBlockName.
    pub fn watch_memory_block_name(
        &mut self,
        console: &mut dyn DebugConsole,
        internal_name: &str,
        flags: u64,
    ) -> i32 {
        let Some(core) = self.core.as_mut() else {
            return -1;
        };
        for block in console.dbg_list_memory_blocks_full() {
            if block.internal_name == internal_name {
                return core.watch_memory_block(&block, flags);
            }
        }
        -1
    }
}

impl DebuggerModule for AccessLogger {
    fn module_type(&self) -> DebuggerType {
        DebuggerType::AccessLogger
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn is_paused(&self) -> bool {
        self.is_paused
    }
    fn set_paused(&mut self, paused: bool) {
        self.is_paused = paused;
    }
    fn needs_callback(&self) -> bool {
        self.needs_callback
    }
    fn set_needs_callback(&mut self, needs: bool) {
        self.needs_callback = needs;
    }
    fn set_module_index(&mut self, idx: usize) {
        self.module_index = idx;
    }

    /// C's module deinit is NULL (the embedder calls
    /// mDebuggerAccessLoggerDeinit by hand); we stop on deinit so a
    /// started logger reclaims its core instead of dropping it with the
    /// console debugger state.
    fn deinit(&mut self, _debugger: &mut Debugger, console: &mut dyn DebugConsole) {
        self.stop(console);
    }

    /// `_mDebuggerAccessLoggerEntered`.
    fn entered(
        &mut self,
        _debugger: &mut Debugger,
        console: &mut dyn DebugConsole,
        reason: DebuggerEntryReason,
        info: Option<&DebuggerEntryInfo>,
    ) {
        // mDebuggerEnter set us paused just before this; the logger never
        // pauses (C: logger->d.isPaused = false).
        self.is_paused = false;
        match reason {
            DebuggerEntryReason::Manual | DebuggerEntryReason::Attached => return,
            DebuggerEntryReason::Breakpoint | DebuggerEntryReason::Stack => {
                // mLOG(DEBUGGER, WARN, "Hit unexpected access logger entry type")
                return;
            }
            DebuggerEntryReason::Watchpoint | DebuggerEntryReason::IllegalOp => {}
        }
        let Some(info) = info else { return };
        let Some(log) = console.dbg_access_log() else {
            return;
        };
        match reason {
            DebuggerEntryReason::Watchpoint => {
                let Some(EntryTypeInfo::Wp(wp)) = info.type_info else {
                    return;
                };
                let flags_ex = match wp.access_source {
                    MemoryAccessSource::Program => access_log_flags_ex::ACCESS_PROGRAM,
                    MemoryAccessSource::Dma => access_log_flags_ex::ACCESS_DMA,
                    // Our MemoryAccessSource subset has no SYSTEM/DECOMPRESS/
                    // COPY variants; map what exists (C switch).
                    MemoryAccessSource::Segment => access_log_flags_ex::ACCESS_SYSTEM,
                    MemoryAccessSource::Unknown => 0,
                };
                log.record_access(
                    info.address,
                    info.segment,
                    info.width.max(1) as u32,
                    wp.access_type,
                    flags_ex,
                );
            }
            DebuggerEntryReason::IllegalOp => {
                log.record_illegal_op(info.address, info.segment);
            }
            _ => {}
        }
    }

    /// `_mDebuggerAccessLoggerCallback` (per-step exec logging).
    fn custom(&mut self, _debugger: &mut Debugger, console: &mut dyn DebugConsole) {
        let info = console.dbg_next_instruction_info();
        if let Some(log) = console.dbg_access_log() {
            log.record_instruction(&info);
        }
    }
}
