// Tests for the access-logger core (regions, recording, mAL\1 format)
// ported from mgba/src/debugger/access-logger.c.
use rgba_debugger::access_logger::*;
use rgba_debugger::debugger::{access_log_flags, access_log_flags_ex, watchpoint_type, DebuggerInstructionInfo};

fn block(id: i32, name: &'static str, start: u32, size: u32) -> CoreMemoryBlock {
    CoreMemoryBlock::flat(id, name, start, start + size, size, core_memory_flags::RW | core_memory_flags::MAPPED)
}

#[test]
fn record_access_marks_flags() {
    let mut core = AccessLoggerCore::new(MAL_PLATFORM_GB);
    assert_eq!(core.watch_memory_block(&block(0xC, "wram", 0xC000, 0x2000), 0), 0);
    // Unmapped (VIRTUAL) blocks are rejected.
    let virt = CoreMemoryBlock::flat(-1, "mem", 0, 0x10000, 0x10000, core_memory_flags::VIRTUAL);
    assert_eq!(core.watch_memory_block(&virt, 0), -1);
    // Same block again returns the same id.
    assert_eq!(core.watch_memory_block(&block(0xC, "wram", 0xC000, 0x2000), 0), 0);

    core.record_access(0xC000, 0, 1, watchpoint_type::WRITE, 0);
    core.record_access(0xC004, 0, 2, watchpoint_type::READ, 0);
    core.record_access(0xC008, 0, 4, watchpoint_type::READ | watchpoint_type::WRITE, 0);
    core.record_access(0xD123, 0, 1, watchpoint_type::READ, 0);
    core.record_access(0x0000, 0, 1, watchpoint_type::WRITE, 0); // not logged
    // Unaligned access: offset &= -width.
    core.record_access(0xC009, 0, 4, watchpoint_type::READ, 0);
    // exec + illegal-op marking
    core.record_instruction(&DebuggerInstructionInfo {
        address: 0xC100,
        segment: 0,
        width: 3,
        flags: [access_log_flags::ACCESS8; 4],
        flags_ex: [0; 4],
    });
    core.record_illegal_op(0xC200, 0);

    let r = &core.regions[0];
    assert_eq!(r.block[0], access_log_flags::WRITE | access_log_flags::ACCESS8);
    // Width fans out over every byte (C: width switch in _mDebuggerAccessLoggerEntered).
    assert_eq!(r.block[4], access_log_flags::READ | access_log_flags::ACCESS16);
    assert_eq!(r.block[5], access_log_flags::READ | access_log_flags::ACCESS16);
    assert_eq!(r.block[8], access_log_flags::READ | access_log_flags::WRITE | access_log_flags::ACCESS32);
    assert_eq!(r.block[0xB], access_log_flags::READ | access_log_flags::WRITE | access_log_flags::ACCESS32);
    assert_eq!(r.block[0x1123], access_log_flags::READ | access_log_flags::ACCESS8);
    // Unaligned 4-wide record rewrote 0xC008 (same bits) and not past it.
    assert_eq!(r.block[0xC], 0);
    // Out-of-region address marked nothing new.
    assert_eq!(r.block[1], 0);
    assert_eq!(
        r.block[0x100],
        access_log_flags::EXECUTE | access_log_flags::ACCESS8
    );
    assert_eq!(r.block[0x102], access_log_flags::EXECUTE | access_log_flags::ACCESS8);
    assert_eq!(r.block[0x103], 0);
    assert_eq!(r.block[0x200], access_log_flags::EXECUTE);
    assert!(r.block_ex.is_none());
}

#[test]
fn ex_block_and_segment_fold() {
    let mut core = AccessLoggerCore::new(MAL_PLATFORM_GB);
    // Upgrades an existing region to have an ex block (C: remap + fill).
    assert_eq!(core.watch_memory_block(&block(0x8, "vram", 0x8000, 0x4000), 0), 0);
    assert_eq!(
        core.watch_memory_block(&block(0x8, "vram", 0x8000, 0x4000), access_log_region_flags::HAS_EX_BLOCK),
        0
    );
    core.record_access(0x8000, 0, 1, watchpoint_type::WRITE, access_log_flags_ex::ACCESS_PROGRAM);
    let r = &core.regions[0];
    assert_eq!(r.block[0], access_log_flags::WRITE | access_log_flags::ACCESS8);
    assert_eq!(r.block_ex.as_ref().unwrap()[0], access_log_flags_ex::ACCESS_PROGRAM);

    // Segmented region (GB cart0 shape): banked addresses fold over
    // [segmentStart, end).
    let mut core = AccessLoggerCore::new(MAL_PLATFORM_GB);
    let cart0 = CoreMemoryBlock {
        id: 0,
        internal_name: "cart0",
        start: 0,
        end: 0x8000,
        size: 0x10000,
        flags: core_memory_flags::READ | core_memory_flags::WORM | core_memory_flags::MAPPED,
        max_segments: 3,
        segment_start: 0x4000,
    };
    assert_eq!(core.watch_memory_block(&cart0, 0), 0);
    core.record_access(0x0100, 0, 1, watchpoint_type::READ, 0);
    core.record_access(0x4200, 1, 1, watchpoint_type::READ, 0);
    core.record_access(0x4200, 2, 1, watchpoint_type::WRITE, 0);
    // Bank index past the region: nothing.
    core.record_access(0x4200, 4, 1, watchpoint_type::READ, 0);
    let r = &core.regions[0];
    assert_eq!(r.block[0x100], access_log_flags::READ | access_log_flags::ACCESS8);
    assert_eq!(r.block[0x4000 + 0x200], access_log_flags::READ | access_log_flags::ACCESS8);
    assert_eq!(r.block[0x8000 + 0x200], access_log_flags::WRITE | access_log_flags::ACCESS8);
    assert_eq!(r.block[0xC000 + 0x200], 0);
}

#[test]
fn serialize_roundtrip_matches_file_layout() {
    let mut core = AccessLoggerCore::new(MAL_PLATFORM_GB);
    core.watch_memory_block(&block(0xC, "wram", 0xC000, 0x2000), 0);
    core.watch_memory_block(
        &block(0x8, "vram", 0x8000, 0x4000),
        access_log_region_flags::HAS_EX_BLOCK,
    );
    core.record_access(0xC000, 0, 1, watchpoint_type::WRITE, 0);
    core.record_access(0x8010, 0, 2, watchpoint_type::READ, access_log_flags_ex::ACCESS_PROGRAM);

    let data = core.serialize();
    // Header (struct mDebuggerAccessLogHeader).
    assert_eq!(&data[0..4], b"mAL\x01");
    assert_eq!(u32::from_le_bytes(data[4..8].try_into().unwrap()), 1);
    assert_eq!(data[0x10], 2); // nRegions
    assert_eq!(data[0x11], DEFAULT_MAX_REGIONS); // regionCapacity
    // Region info[0]: wram
    let info0 = HEADER_SIZE;
    assert_eq!(u32::from_le_bytes(data[info0..info0 + 4].try_into().unwrap()), 0xC000);
    assert_eq!(u32::from_le_bytes(data[info0 + 4..info0 + 8].try_into().unwrap()), 0xE000);
    assert_eq!(u32::from_le_bytes(data[info0 + 8..info0 + 12].try_into().unwrap()), 0x2000);
    let off0 = u64::from_le_bytes(data[info0 + 0x10..info0 + 0x18].try_into().unwrap()) as usize;
    assert_eq!(off0, HEADER_SIZE + DEFAULT_MAX_REGIONS as usize * REGION_INFO_SIZE);
    assert_eq!(data[off0], access_log_flags::WRITE | access_log_flags::ACCESS8);
    // Region info[1]: vram, ex block after the plain block.
    let info1 = HEADER_SIZE + REGION_INFO_SIZE;
    let off1 = u64::from_le_bytes(data[info1 + 0x10..info1 + 0x18].try_into().unwrap()) as usize;
    let off1ex = u64::from_le_bytes(data[info1 + 0x18..info1 + 0x20].try_into().unwrap()) as usize;
    assert_eq!(off1ex, off1 + 0x4000);
    assert_eq!(u64::from_le_bytes(data[info1 + 0x20..info1 + 0x28].try_into().unwrap()), access_log_region_flags::HAS_EX_BLOCK);
    assert_eq!(data[off1 + 0x10], access_log_flags::READ | access_log_flags::ACCESS16);
    assert_eq!(u16::from_le_bytes([data[off1ex + 0x20], data[off1ex + 0x21]]), access_log_flags_ex::ACCESS_PROGRAM);

    // Round trip into a fresh core.
    let mut core2 = AccessLoggerCore::new(MAL_PLATFORM_GB);
    assert!(core2.deserialize(&data, Some(MAL_PLATFORM_GB)));
    assert_eq!(core2.regions.len(), 2);
    assert_eq!(core2.regions[0].block[0], access_log_flags::WRITE | access_log_flags::ACCESS8);
    assert_eq!(core2.regions[1].block[0x10], access_log_flags::READ | access_log_flags::ACCESS16);
    assert_eq!(core2.regions[1].block_ex.as_ref().unwrap()[0x10], access_log_flags_ex::ACCESS_PROGRAM);

    // Platform mismatch and bad magic are rejected.
    let mut core3 = AccessLoggerCore::new(MAL_PLATFORM_GBA);
    assert!(!core3.deserialize(&data, Some(MAL_PLATFORM_GBA)));
    let mut bad = data.clone();
    bad[0] = b'X';
    assert!(!core3.deserialize(&bad, None));
}

#[test]
fn shadow_file_fill_and_dump() {
    let mut core = AccessLoggerCore::new(MAL_PLATFORM_GB);
    core.watch_memory_block(&block(0xC, "wram", 0xC000, 0x2000), 0);
    core.record_access(0xC000, 0, 4, watchpoint_type::WRITE, 0);
    let shadow = core
        .create_shadow_file(0, 0xEE, |addr, seg| {
            assert_eq!(seg, 0);
            (addr & 0xFF) as u8
        })
        .unwrap();
    assert_eq!(shadow.len(), 0x2000);
    // First four bytes were accessed → console bytes; rest is fill.
    assert_eq!(&shadow[0..4], &[0x00, 0x01, 0x02, 0x03]);
    assert_eq!(shadow[4], 0xEE);
    assert!(core.create_shadow_file(1, 0, |_, _| 0).is_none());
    assert!(core.create_shadow_file(-1, 0, |_, _| 0).is_none());
}

#[test]
fn open_bytes_semantics() {
    use rgba_debugger::access_logger::AccessLogger;
    let mut logger = AccessLogger::new(MAL_PLATFORM_GB);
    // No existing image, no create → fail (C: !O_CREAT with tiny file).
    assert!(!logger.open_bytes(None, false, false));
    let mut logger = AccessLogger::new(MAL_PLATFORM_GB);
    assert!(logger.open_bytes(None, true, true));
    assert_eq!(logger.access_log().unwrap().regions.len(), 0);
    // Garbage image with O_CREAT (no O_TRUNC): C's open resets the file
    // to a fresh log too (`(mode & O_CREAT) && ((mode & O_TRUNC) || !loaded)`).
    assert!(logger.open_bytes(Some(&vec![0xAA; 0x100]), true, false));
    assert_eq!(logger.access_log().unwrap().regions.len(), 0);
}
