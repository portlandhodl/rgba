// Copyright (c) 2013-2017 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/core/mem-search.c and
// mgba/include/mgba/core/mem-search.h — the "search memory for value" cheat
// engine (mCoreMemorySearch / mCoreMemorySearchRepeat).
//
// The C drives the scan off the mCore vtable: `listMemoryBlocks` +
// `getMemoryBlock` for the initial sweep (direct pointer into console RAM),
// `rawRead8/16/32` for repeat refinements. Here a console (or test) instead
// implements the small `MemSearchOps` trait; the engine itself is stateless —
// the persistent state between scans is the result list itself (per-result
// `old_value` snapshot plus guess scaling), held by the caller as a
// `Vec<SearchResult>`, exactly like the C's mCoreMemorySearchResults vector.
//
// Notes carried over from / about the C:
// - Segments are a TODO upstream; `segment` is always -1.
// - Little-endian only ("TODO: Big endian" in the C).
// - String results leave oldValue/guess fields uninitialized in the C; this
//   port zero/neutral-initializes them.
// - mGBA never exports search results as cheat codes (the Qt GUI only
//   displays them and raw-reads current values), so there is no export step
//   here either.

/// mCoreMemoryBlockFlags (mgba/include/mgba/core/interface.h), as consumed by
/// [`SearchParams::memory_flags`].
pub mod core_memory_flags {
    pub const READ: u32 = 0x01;
    pub const WRITE: u32 = 0x02;
    pub const RW: u32 = 0x03;
    pub const WORM: u32 = 0x04;
    pub const MAPPED: u32 = 0x10;
    pub const VIRTUAL: u32 = 0x20;
}

/// enum mCoreMemorySearchType
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchType {
    Int,
    String,
    Guess,
}

/// enum mCoreMemorySearchOp. Declaration order matches the C enum values so
/// `op >= SearchOp::Delta` works like the C's `op >= mCORE_MEMORY_SEARCH_DELTA`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum SearchOp {
    Equal,
    Greater,
    Less,
    Any,
    Delta,
    DeltaPositive,
    DeltaNegative,
    DeltaAny,
}

/// struct mCoreMemorySearchParams. The C unions `valueStr`/`valueInt`; both
/// are fields here and the active one is selected by `typ` (Int →
/// `value_int`, String/Guess → `value_str`).
#[derive(Clone, Debug)]
pub struct SearchParams {
    /// Only blocks whose `flags` intersect this mask are scanned
    /// (e.g. `core_memory_flags::WRITE` for RAM-only searches).
    pub memory_flags: u32,
    pub typ: SearchType,
    pub op: SearchOp,
    /// Required start-address alignment, or -1 for any. Int searches only run
    /// when `align == width || align == -1` (as in the C).
    pub align: i32,
    /// 1, 2 or 4 (bytes) for Int searches, -1 for "guess the width"; for
    /// String searches this is the needle length.
    pub width: i32,
    pub value_int: i32,
    pub value_str: Vec<u8>,
}

/// struct mCoreMemorySearchResult
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SearchResult {
    pub address: u32,
    pub segment: i32,
    /// Guess bookkeeping: memory value × divisor ÷ multiplier = user value.
    pub guess_divisor: u32,
    pub guess_multiplier: u32,
    pub typ: SearchType,
    /// Bytes (1/2/4) for Int, needle length for String.
    pub width: i32,
    /// Value seen when this result was produced — the snapshot DELTA repeats
    /// compare against.
    pub old_value: i32,
}

/// struct mCoreMemoryBlock, subset consumed by the search (id/start/end/flags).
#[derive(Clone, Copy, Debug)]
pub struct SearchBlock {
    pub id: i32,
    pub start: u32,
    pub end: u32,
    pub flags: u32,
}

/// The `mCore` vtable subset the C search engine calls. Consoles can
/// implement this directly (mirroring their `rawRead`/block tables, the same
/// data `DebugConsole::dbg_raw_read`/`dbg_list_memory_blocks_full` expose).
pub trait MemSearchOps {
    /// C: `mCore::listMemoryBlocks` (block id/start/end/flags subset).
    fn memory_blocks(&self) -> Vec<SearchBlock>;
    /// C: `mCore::getMemoryBlock` — direct access to the block's bytes.
    /// `None` mirrors the C returning NULL (segmented/virtual blocks).
    fn memory_block(&mut self, id: i32) -> Option<&[u8]>;
    /// C: `mCore::rawRead8/16/32` — raw (no-side-effect) read, zero-extended.
    /// `width` is 1, 2 or 4 bytes.
    fn raw_read(&mut self, address: u32, segment: i32, width: u32) -> u32;
}

/// C: `_op`.
fn op_match(value: i32, match_value: i32, op: SearchOp) -> bool {
    match op {
        SearchOp::Greater => value > match_value,
        SearchOp::Less => value < match_value,
        SearchOp::Equal | SearchOp::Delta => value == match_value,
        SearchOp::DeltaPositive => value > 0,
        SearchOp::DeltaNegative => value < 0,
        SearchOp::DeltaAny => value != 0,
        SearchOp::Any => true,
    }
}

/// Little-endian read of `width` bytes at byte offset `i`, converted like the
/// C's unsigned loads fed into `_op`'s int32_t parameter (u8/u16 zero-extend,
/// u32 wraps).
fn read_le(mem: &[u8], i: usize, width: usize) -> Option<i32> {
    match width {
        1 => mem.get(i).map(|&b| b as i32),
        2 => mem
            .get(i..i + 2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]) as i32),
        4 => mem
            .get(i..i + 4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as i32),
        _ => None,
    }
}

/// C: `_search8`/`_search16`/`_search32`, unified over `width`.
/// `limit` of 0 means unlimited; returns the number of appended results.
fn search_width(
    mem: &[u8],
    size: usize,
    block: &SearchBlock,
    value: i32,
    width: usize,
    op: SearchOp,
    out: &mut Vec<SearchResult>,
    limit: usize,
) -> usize {
    let mut found = 0;
    let start = block.start;
    let end = size; // TODO: Segments (as in the C)
    let mut i = 0;
    while (limit == 0 || found < limit) && i < end {
        // The C indexes a u8/u16/u32 array directly; real block sizes are
        // width multiples, but guard the tail instead of reading OOB like C.
        let Some(v) = read_le(mem, i, width) else { break };
        if op_match(v, value, op) {
            out.push(SearchResult {
                address: start.wrapping_add(i as u32),
                segment: -1, // TODO (as in the C)
                guess_divisor: 1,
                guess_multiplier: 1,
                typ: SearchType::Int,
                width: width as i32,
                old_value: v,
            });
            found += 1;
        }
        i += width;
    }
    found
}

/// C: `_searchInt` — dispatch on width, honoring the C's truncating casts of
/// the match value (int64 guess values land here too).
fn search_typed(
    mem: &[u8],
    size: usize,
    block: &SearchBlock,
    value: i64,
    width: usize,
    op: SearchOp,
    out: &mut Vec<SearchResult>,
    limit: usize,
) -> usize {
    // int64 → uintN_t → int32_t, exactly the C's conversion chain.
    let value = match width {
        1 => value as u8 as i32,
        2 => value as u16 as i32,
        4 => value as u32 as i32,
        _ => return 0,
    };
    search_width(mem, size, block, value, width, op, out, limit)
}

/// C: `_searchInt`'s align gate. Guess searches bypass it (they call the
/// per-width scanners directly, as in the C).
fn search_int(
    mem: &[u8],
    size: usize,
    block: &SearchBlock,
    params: &SearchParams,
    out: &mut Vec<SearchResult>,
    limit: usize,
) -> usize {
    if params.align == params.width || params.align == -1 {
        search_typed(
            mem,
            size,
            block,
            params.value_int as i64,
            params.width as usize,
            params.op,
            out,
            limit,
        )
    } else {
        0
    }
}

/// C: `_searchStr`. Differences from the C only where the C would be UB:
/// negative/huge `len`, and `end - len` underflow when the needle is longer
/// than the block (C wraps size_t and memcmps out of bounds).
fn search_str(
    mem: &[u8],
    size: usize,
    block: &SearchBlock,
    needle: &[u8],
    out: &mut Vec<SearchResult>,
    limit: usize,
) -> usize {
    let len = needle.len();
    let mut found = 0;
    let start = block.start;
    let end = size; // TODO: Segments (as in the C)
    let mut i = 0;
    while (limit == 0 || found < limit) && i < end.saturating_sub(len) {
        if &mem[i..i + len] == needle {
            out.push(SearchResult {
                address: start.wrapping_add(i as u32),
                segment: -1, // TODO (as in the C)
                guess_divisor: 1,
                guess_multiplier: 1,
                typ: SearchType::String,
                width: len as i32,
                old_value: 0, // uninitialized in the C
            });
            found += 1;
        }
        i += 1;
    }
    found
}

/// The C's per-parse width pick in `_searchGuess`, thresholds applied to the
/// (possibly reduced) int64 value.
fn guess_width(value: i64, width: i32) -> usize {
    if (width == -1 && value > 0x10000) || width == 4 {
        4
    } else if (width == -1 && value > 0x100) || width == 2 {
        2
    } else {
        1
    }
}

/// C: `_searchGuess` — try decimal then hex parses of `value_str`; after each
/// bare-value sweep, repeatedly strip trailing decimal zeros / low hex nibbles
/// and re-search, recording the scale in `guess_divisor`.
fn search_guess(
    mem: &[u8],
    size: usize,
    block: &SearchBlock,
    params: &SearchParams,
    out: &mut Vec<SearchResult>,
    limit: usize,
) -> usize {
    // TODO: As str (as in the C)
    let mut found = 0;
    let mut tmp: Vec<SearchResult> = Vec::new();

    for base in [10u32, 16u32] {
        let (mut value, used) = strtoll(&params.value_str, base);
        // C: strtoll(...) then `if (end && !end[0])` — full consumption.
        if used != params.value_str.len() {
            continue;
        }
        let width = guess_width(value, params.width);
        found += search_typed(
            mem,
            size,
            block,
            value,
            width,
            params.op,
            out,
            if limit != 0 { limit.saturating_sub(found) } else { 0 },
        );

        let mut divisor: u32 = 1;
        // Decimal: while (value && !(value % 10)); hex: !(value & 0xF).
        let (step_ok, step): (fn(i64) -> bool, fn(i64) -> i64) = if base == 10 {
            (|v| v % 10 == 0, |v| v / 10)
        } else {
            (|v| v & 0xF == 0, |v| v >> 4)
        };
        while value != 0 && step_ok(value) {
            tmp.clear();
            value = step(value);
            divisor = if base == 10 {
                divisor.wrapping_mul(10)
            } else {
                divisor.wrapping_shl(4)
            };

            let width = guess_width(value, params.width);
            search_typed(
                mem,
                size,
                block,
                value,
                width,
                params.op,
                &mut tmp,
                if limit != 0 { limit.saturating_sub(found) } else { 0 },
            );
            found += tmp.len();
            for res in tmp.drain(..) {
                let mut res = res;
                res.guess_divisor = divisor;
                out.push(res);
            }
        }
    }

    found
}

/// C: `_search`.
fn search_mem(
    mem: &[u8],
    size: usize,
    block: &SearchBlock,
    params: &SearchParams,
    out: &mut Vec<SearchResult>,
    limit: usize,
) -> usize {
    match params.typ {
        SearchType::Int => search_int(mem, size, block, params, out, limit),
        SearchType::String => {
            // C: _searchStr(mem, size, block, params->valueStr, params->width, ...)
            // with width as the needle length. Clamp instead of reading past
            // the buffer as the C would for a mismatched width.
            let len = (params.width.max(0) as usize).min(params.value_str.len());
            search_str(mem, size, block, &params.value_str[..len], out, limit)
        }
        SearchType::Guess => search_guess(mem, size, block, params, out, limit),
    }
}

/// C: `mCoreMemorySearch`. Appends to `out` (the caller clears it first, as
/// in the C); `limit` of 0 means unlimited.
pub fn search(
    ops: &mut dyn MemSearchOps,
    params: &SearchParams,
    out: &mut Vec<SearchResult>,
    limit: usize,
) {
    let blocks = ops.memory_blocks();
    let mut found = 0usize;

    for block in &blocks {
        if limit != 0 && found >= limit {
            break;
        }
        if block.flags & params.memory_flags == 0 {
            continue;
        }
        let Some(mem) = ops.memory_block(block.id) else {
            continue;
        };
        // C: if (size > block->end - block->start) size = block->end - block->start;
        let size = mem
            .len()
            .min(block.end.wrapping_sub(block.start) as usize);
        found += search_mem(
            &mem[..size],
            size,
            block,
            params,
            out,
            if limit != 0 { limit - found } else { 0 },
        );
    }
}

/// C: `_testSpecificGuess` — test one parsed guess value against the current
/// memory at a result, widening the read when alignment and the result's
/// width allow. Updates `res.old_value` as a side effect, like the C.
fn test_specific_guess(
    ops: &mut dyn MemSearchOps,
    res: &mut SearchResult,
    op_value: i64,
    op: SearchOp,
) -> bool {
    let mut offset: i64 = 0;
    if op >= SearchOp::Delta {
        offset = res.old_value as i64;
    }

    res.old_value = (res.old_value as i64).wrapping_add(op_value) as i32;
    // rawRead8/16/32 zero-extend into the C's int64_t value.
    let mut value = ops.raw_read(res.address, res.segment, 1) as i64;
    if op_match(
        (value
            .wrapping_mul(res.guess_divisor as i64)
            / res.guess_multiplier as i64)
        .wrapping_sub(offset) as i32,
        op_value as i32,
        op,
    ) {
        res.old_value = value as i32;
        return true;
    }
    if res.address & 1 == 0 && (res.width >= 2 || res.width == -1) {
        value = ops.raw_read(res.address, res.segment, 2) as i64;
        if op_match(
            (value
                .wrapping_mul(res.guess_divisor as i64)
                / res.guess_multiplier as i64)
            .wrapping_sub(offset) as i32,
            op_value as i32,
            op,
        ) {
            res.old_value = value as i32;
            return true;
        }
    }
    if res.address & 3 == 0 && (res.width >= 4 || res.width == -1) {
        value = ops.raw_read(res.address, res.segment, 4) as i64;
        if op_match(
            (value
                .wrapping_mul(res.guess_divisor as i64)
                / res.guess_multiplier as i64)
            .wrapping_sub(offset) as i32,
            op_value as i32,
            op,
        ) {
            res.old_value = value as i32;
            return true;
        }
    }
    res.old_value = (res.old_value as i64).wrapping_sub(op_value) as i32;
    false
}

/// C: `_testGuess`. The C's `if (end && ...)` is always true (strtoll always
/// sets end), so both parses are always tested with whatever prefix parsed.
fn test_guess(ops: &mut dyn MemSearchOps, res: &mut SearchResult, params: &SearchParams) -> bool {
    let (value, _) = strtoll(&params.value_str, 10);
    if test_specific_guess(ops, res, value, params.op) {
        return true;
    }

    let (value, _) = strtoll(&params.value_str, 16);
    if test_specific_guess(ops, res, value, params.op) {
        return true;
    }
    false
}

/// C: `mCoreMemorySearchRepeat` — narrow an existing result list in place
/// (failed entries are swapped out with the last element, as in the C).
pub fn search_repeat(
    ops: &mut dyn MemSearchOps,
    params: &SearchParams,
    results: &mut Vec<SearchResult>,
) {
    let mut i = 0;
    while i < results.len() {
        let mut res = results[i];
        let keep = match res.typ {
            SearchType::Int => match params.typ {
                SearchType::Guess => test_guess(ops, &mut res, params),
                SearchType::Int => {
                    let match_value = params.value_int;
                    let mut value: i32 = 0;
                    match params.width {
                        1 => value = ops.raw_read(res.address, res.segment, 1) as i32,
                        2 => value = ops.raw_read(res.address, res.segment, 2) as i32,
                        4 => value = ops.raw_read(res.address, res.segment, 4) as i32,
                        _ => {}
                    }
                    let mut op_value = value;
                    if params.op >= SearchOp::Delta {
                        op_value = op_value.wrapping_sub(res.old_value);
                    }
                    if !op_match(op_value, match_value, params.op) {
                        false
                    } else {
                        res.old_value = value;
                        true
                    }
                }
                // C: Int results are untouched for String params.
                SearchType::String => true,
            },
            // C: TODO for String/Guess results — kept untouched.
            SearchType::String | SearchType::Guess => true,
        };
        if keep {
            results[i] = res;
            i += 1;
        } else {
            results.swap_remove(i);
        }
    }
}

/// strtoll(valueStr, &end, base) reduced to what the guess paths need:
/// C-locale leading whitespace, optional sign, optional 0x prefix for base
/// 16 (only when a hex digit follows), prefix digits, i64 clamping on
/// overflow. Returns (value, bytes consumed); consumed == 0 when no digits
/// parsed (end == nptr), consumed == s.len() means the whole string parsed.
fn strtoll(s: &[u8], base: u32) -> (i64, usize) {
    let mut i = 0;
    while i < s.len() && (s[i] == b' ' || (0x09..=0x0d).contains(&s[i])) {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    if base == 16
        && i + 2 < s.len()
        && s[i] == b'0'
        && (s[i + 1] | 0x20) == b'x'
        && (s[i + 2] as char).to_digit(16).is_some()
    {
        i += 2;
    }
    let digit_start = i;
    // Accumulate clamped at 2^63 so absurdly long strings can't overflow.
    const CAP: u128 = 1 << 63;
    let mut mag: u128 = 0;
    while i < s.len() {
        let Some(d) = (s[i] as char).to_digit(base) else { break };
        mag = (mag * base as u128 + d as u128).min(CAP);
        i += 1;
    }
    if i == digit_start {
        return (0, 0);
    }
    let value = if neg {
        if mag >= CAP {
            i64::MIN
        } else {
            -(mag as i64)
        }
    } else if mag > i64::MAX as u128 {
        i64::MAX
    } else {
        mag as i64
    };
    (value, i)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN_BASE: u32 = 0x1000;
    const MAIN_SIZE: usize = 64;
    const WORM_BASE: u32 = 0x2000;

    /// Mock console: a 64-byte RW block at 0x1000, a 16-byte WORM block at
    /// 0x2000, and a declared block (id 2) with no direct memory (like a
    /// segmented block whose getMemoryBlock returns NULL).
    struct Mock {
        mem: [u8; MAIN_SIZE],
        worm: [u8; 16],
    }

    impl Mock {
        fn new(fill: u8) -> Self {
            Mock {
                mem: [fill; MAIN_SIZE],
                worm: [fill; 16],
            }
        }
    }

    impl MemSearchOps for Mock {
        fn memory_blocks(&self) -> Vec<SearchBlock> {
            vec![
                SearchBlock {
                    id: 0,
                    start: MAIN_BASE,
                    end: MAIN_BASE + MAIN_SIZE as u32,
                    flags: core_memory_flags::RW,
                },
                SearchBlock {
                    id: 1,
                    start: WORM_BASE,
                    end: WORM_BASE + 16,
                    flags: core_memory_flags::WORM,
                },
                SearchBlock {
                    id: 2,
                    start: 0x3000,
                    end: 0x3000 + 16,
                    flags: core_memory_flags::RW,
                },
            ]
        }
        fn memory_block(&mut self, id: i32) -> Option<&[u8]> {
            match id {
                0 => Some(&self.mem),
                1 => Some(&self.worm),
                _ => None,
            }
        }
        fn raw_read(&mut self, address: u32, _segment: i32, width: u32) -> u32 {
            let (mem, base) = if (MAIN_BASE..MAIN_BASE + MAIN_SIZE as u32).contains(&address) {
                (&self.mem[..], MAIN_BASE)
            } else if (WORM_BASE..WORM_BASE + 16).contains(&address) {
                (&self.worm[..], WORM_BASE)
            } else {
                return 0;
            };
            let i = (address - base) as usize;
            match width {
                1 => mem[i] as u32,
                2 => u16::from_le_bytes([mem[i], mem[i + 1]]) as u32,
                4 => u32::from_le_bytes([mem[i], mem[i + 1], mem[i + 2], mem[i + 3]]),
                _ => 0,
            }
        }
    }

    fn int_params(value: i32, width: i32, op: SearchOp) -> SearchParams {
        SearchParams {
            memory_flags: core_memory_flags::RW, // RAM-ish: read&write regions
            typ: SearchType::Int,
            op,
            align: -1,
            width,
            value_int: value,
            value_str: Vec::new(),
        }
    }

    fn guess_params(s: &str, width: i32, op: SearchOp) -> SearchParams {
        SearchParams {
            memory_flags: core_memory_flags::RW,
            typ: SearchType::Guess,
            op,
            align: -1,
            width,
            value_int: 0,
            value_str: s.as_bytes().to_vec(),
        }
    }

    #[test]
    fn search_equal_u8() {
        let mut mock = Mock::new(0);
        mock.mem[5] = 0x42;
        mock.mem[9] = 0x42;
        mock.worm[0] = 0x42; // WORM block: not scanned with RW flags
        let mut out = Vec::new();
        search(&mut mock, &int_params(0x42, 1, SearchOp::Equal), &mut out, 0);
        assert_eq!(
            out,
            vec![
                SearchResult {
                    address: MAIN_BASE + 5,
                    segment: -1,
                    guess_divisor: 1,
                    guess_multiplier: 1,
                    typ: SearchType::Int,
                    width: 1,
                    old_value: 0x42,
                },
                SearchResult {
                    address: MAIN_BASE + 9,
                    segment: -1,
                    guess_divisor: 1,
                    guess_multiplier: 1,
                    typ: SearchType::Int,
                    width: 1,
                    old_value: 0x42,
                },
            ]
        );
    }

    #[test]
    fn search_flags_filter() {
        let mut mock = Mock::new(0x7F);
        // MAPPED bit matches no block → nothing scanned.
        let mut params = int_params(0x7F, 1, SearchOp::Equal);
        params.memory_flags = core_memory_flags::MAPPED;
        let mut out = Vec::new();
        search(&mut mock, &params, &mut out, 0);
        assert!(out.is_empty());

        // WORM-only flag → only the 16-byte WORM block matches.
        params.memory_flags = core_memory_flags::WORM;
        search(&mut mock, &params, &mut out, 0);
        assert_eq!(out.len(), 16);
        assert!(out.iter().all(|r| (WORM_BASE..WORM_BASE + 16).contains(&r.address)));

        // Block id 2 has RW flags but memory_block returns None → skipped (so
        // an RW scan finds the 64 main bytes only).
        params.memory_flags = core_memory_flags::RW;
        out.clear();
        search(&mut mock, &params, &mut out, 0);
        assert_eq!(out.len(), 64);
    }

    #[test]
    fn search_u16_le_and_alignment() {
        let mut mock = Mock::new(0);
        // Aligned u16 at offset 4.
        mock.mem[4] = 0x34;
        mock.mem[5] = 0x12;
        // Same byte pattern across an odd boundary — must NOT match.
        mock.mem[7] = 0x34;
        mock.mem[8] = 0x12;
        let mut out = Vec::new();
        search(&mut mock, &int_params(0x1234, 2, SearchOp::Equal), &mut out, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].address, MAIN_BASE + 4);
        assert_eq!(out[0].width, 2);
        assert_eq!(out[0].old_value, 0x1234);

        // 32-bit LE read.
        let mut out = Vec::new();
        mock.mem[12..16].copy_from_slice(&0xDEADBEEFu32.to_le_bytes());
        let mut p = int_params(0xDEADBEEFu32 as i32, 4, SearchOp::Equal);
        search(&mut mock, &p, &mut out, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].address, MAIN_BASE + 12);
        assert_eq!(out[0].old_value, 0xDEADBEEFu32 as i32);

        // align != width and != -1 → no scan at all (C's _searchInt gate).
        p.align = 2;
        out.clear();
        search(&mut mock, &p, &mut out, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn search_greater_less_any_and_limit() {
        let mut mock = Mock::new(0);
        for (i, b) in mock.mem.iter_mut().enumerate() {
            *b = i as u8; // 0..64
        }
        let mut out = Vec::new();
        search(&mut mock, &int_params(0, 1, SearchOp::Any), &mut out, 0);
        assert_eq!(out.len(), 64);
        assert!(out.iter().all(|r| r.old_value == (r.address - MAIN_BASE) as i32));

        out.clear();
        search(&mut mock, &int_params(60, 1, SearchOp::Greater), &mut out, 0);
        assert_eq!(out.len(), 3); // 61, 62, 63

        out.clear();
        search(&mut mock, &int_params(3, 1, SearchOp::Less), &mut out, 0);
        assert_eq!(out.len(), 3); // 0, 1, 2

        // Limit caps total results.
        out.clear();
        search(&mut mock, &int_params(0, 1, SearchOp::Any), &mut out, 10);
        assert_eq!(out.len(), 10);
    }

    #[test]
    fn search_string() {
        let mut mock = Mock::new(0);
        mock.mem[10..15].copy_from_slice(b"hello");
        mock.mem[20..25].copy_from_slice(b"hello");
        let mut params = SearchParams {
            memory_flags: core_memory_flags::RW,
            typ: SearchType::String,
            op: SearchOp::Equal,
            align: -1,
            width: 5,
            value_int: 0,
            value_str: b"hello".to_vec(),
        };
        let mut out = Vec::new();
        search(&mut mock, &params, &mut out, 0);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].address, MAIN_BASE + 10);
        assert_eq!(out[0].typ, SearchType::String);
        assert_eq!(out[0].width, 5);
        assert_eq!(out[1].address, MAIN_BASE + 20);

        params.value_str = b"world".to_vec();
        out.clear();
        search(&mut mock, &params, &mut out, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn search_guess_decimal_hex_and_divisors() {
        let mut mock = Mock::new(0xFF);
        // 100 stored as its BCD-ish reduced form: memory holds 10.
        mock.mem[4] = 10;
        // A plain 100 elsewhere.
        mock.mem[30] = 100;
        // A byte that matches hex-only parses.
        mock.mem[2] = 0x1F;
        // A u32 big value for width-guessing.
        mock.mem[8..12].copy_from_slice(&0x12345678u32.to_le_bytes());

        // Guess "100": direct 100 at mem[30] (divisor 1) and reduced 10 at
        // mem[4] (divisor 10). Hex parse of "100" (=0x100) must not add
        // anything: truncated u8 0 absent (mem is 0xFF-filled).
        let mut out = Vec::new();
        search(&mut mock, &guess_params("100", -1, SearchOp::Equal), &mut out, 0);
        assert_eq!(out.len(), 2);
        let reduced = out.iter().find(|r| r.address == MAIN_BASE + 4).unwrap();
        assert_eq!(reduced.guess_divisor, 10);
        assert_eq!(reduced.old_value, 10);
        let direct = out.iter().find(|r| r.address == MAIN_BASE + 30).unwrap();
        assert_eq!(direct.guess_divisor, 1);
        assert_eq!(direct.old_value, 100);

        // Guess "1F": decimal parse fails, hex 0x1F hits mem[2].
        out.clear();
        search(&mut mock, &guess_params("1F", -1, SearchOp::Equal), &mut out, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].address, MAIN_BASE + 2);
        assert_eq!(out[0].width, 1);

        // Guess big decimal value picks 32-bit width.
        out.clear();
        search(
            &mut mock,
            &guess_params("305419896", -1, SearchOp::Equal),
            &mut out,
            0,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].address, MAIN_BASE + 8);
        assert_eq!(out[0].width, 4);

        // Forced width (like the UI's 8/16/32 radio) beats guessing.
        out.clear();
        search(&mut mock, &guess_params("100", 4, SearchOp::Equal), &mut out, 0);
        assert!(out.iter().all(|r| r.width == 4));
    }

    fn snapshot_any(mock: &mut Mock) -> Vec<SearchResult> {
        let mut out = Vec::new();
        search(mock, &int_params(0, 1, SearchOp::Any), &mut out, 0);
        out
    }

    #[test]
    fn repeat_equal_narrows_and_tracks() {
        let mut mock = Mock::new(0);
        for (i, b) in mock.mem.iter_mut().enumerate() {
            *b = i as u8;
        }
        let mut results = snapshot_any(&mut mock);
        assert_eq!(results.len(), 64);

        // Repeat equal 30 → one hit.
        search_repeat(&mut mock, &int_params(30, 1, SearchOp::Equal), &mut results);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].address, MAIN_BASE + 30);

        // Value unchanged → still present.
        search_repeat(&mut mock, &int_params(30, 1, SearchOp::Equal), &mut results);
        assert_eq!(results.len(), 1);

        // Change it → gone.
        mock.mem[30] = 31;
        search_repeat(&mut mock, &int_params(30, 1, SearchOp::Equal), &mut results);
        assert!(results.is_empty());
    }

    #[test]
    fn repeat_less_and_greater_against_value() {
        let mut mock = Mock::new(0);
        for (i, b) in mock.mem.iter_mut().enumerate() {
            *b = i as u8;
        }
        let mut results = snapshot_any(&mut mock);

        // "less than 32" transition.
        search_repeat(&mut mock, &int_params(32, 1, SearchOp::Less), &mut results);
        assert_eq!(results.len(), 32);
        assert!(results.iter().all(|r| r.old_value < 32));

        // Mutate one address below and keep narrowing with "greater than 60"
        // from a fresh snapshot.
        let mut results = snapshot_any(&mut mock);
        mock.mem[63] = 0; // only 61, 62 remain > 60
        search_repeat(&mut mock, &int_params(60, 1, SearchOp::Greater), &mut results);
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|r| r.address == MAIN_BASE + 61 || r.address == MAIN_BASE + 62));
    }

    #[test]
    fn repeat_delta_transitions() {
        let mut mock = Mock::new(0);
        for (i, b) in mock.mem.iter_mut().enumerate() {
            *b = i as u8;
        }

        // Exact delta: bump one byte by 5.
        let mut results = snapshot_any(&mut mock);
        mock.mem[20] = mock.mem[20].wrapping_add(5);
        search_repeat(&mut mock, &int_params(5, 1, SearchOp::Delta), &mut results);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].address, MAIN_BASE + 20);
        // old_value updated to the new value for chained repeats.
        assert_eq!(results[0].old_value, 25);

        // No further change → delta of 0 matches everything left, delta-any
        // matches nothing.
        search_repeat(&mut mock, &int_params(0, 1, SearchOp::Delta), &mut results);
        assert_eq!(results.len(), 1);
        search_repeat(&mut mock, &int_params(0, 1, SearchOp::DeltaAny), &mut results);
        assert!(results.is_empty());

        // Positive/negative deltas from a fresh snapshot.
        let mut results = snapshot_any(&mut mock);
        mock.mem[7] -= 3;
        mock.mem[8] += 3;
        search_repeat(&mut mock, &int_params(0, 1, SearchOp::DeltaPositive), &mut results);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].address, MAIN_BASE + 8);

        let mut results = snapshot_any(&mut mock);
        mock.mem[13] -= 3;
        search_repeat(&mut mock, &int_params(0, 1, SearchOp::DeltaNegative), &mut results);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].address, MAIN_BASE + 13);
    }

    #[test]
    fn repeat_guess_parses_value_string() {
        let mut mock = Mock::new(0xFF);
        mock.mem[12] = 50;
        let mut results = snapshot_any(&mut mock);
        assert_eq!(results.len(), 64);

        // Guess-repeat "80" after the value becomes 80 narrows to it.
        mock.mem[12] = 80;
        search_repeat(&mut mock, &guess_params("80", -1, SearchOp::Equal), &mut results);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].address, MAIN_BASE + 12);
        assert_eq!(results[0].old_value, 80);

        // Guess-repeat with a delta: value drops by 40, "−40" as delta-negative.
        mock.mem[12] = 40;
        search_repeat(
            &mut mock,
            &guess_params("0", -1, SearchOp::DeltaNegative),
            &mut results,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].old_value, 40);

        // "50" no longer matches.
        search_repeat(&mut mock, &guess_params("50", -1, SearchOp::Equal), &mut results);
        assert!(results.is_empty());
    }

    #[test]
    fn strtoll_matches_c() {
        assert_eq!(strtoll(b"100", 10), (100, 3));
        assert_eq!(strtoll(b"100", 16), (0x100, 3));
        assert_eq!(strtoll(b"1F", 10), (1, 1)); // prefix parses, not full
        assert_eq!(strtoll(b"1F", 16), (0x1F, 2));
        assert_eq!(strtoll(b"0x10", 16), (0x10, 4));
        assert_eq!(strtoll(b"  -12", 10), (-12, 5));
        assert_eq!(strtoll(b"+7", 10), (7, 2));
        assert_eq!(strtoll(b"", 10), (0, 0));
        assert_eq!(strtoll(b"xyz", 10), (0, 0));
        assert_eq!(strtoll(b"0x", 16), (0, 1)); // '0' then no hex digit
        // Clamping like strtoll's LLONG_MAX/LLONG_MIN.
        assert_eq!(strtoll(b"99999999999999999999999", 10).0, i64::MAX);
        assert_eq!(strtoll(b"-99999999999999999999999", 10).0, i64::MIN);
    }
}
