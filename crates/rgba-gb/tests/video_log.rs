// Copyright (c) 2013-2017 Jeffrey Pfau (mGBA), MPL-2.0.
// End-to-end test of the GB record-side video-log glue (mgba
// src/gb/extra/proxy.c record hooks + _GBCoreStartVideoLog/_GBCoreEndVideoLog
// in src/gb/core.c): a hand-assembled ROM pokes video registers, VRAM, OAM
// and the BGP palette while the logger is attached, one frame is run, and
// the mVL is read back through the rgba-core reader to check the packets.

use std::cell::RefCell;
use std::io::Cursor;
use std::rc::Rc;

use rgba_core::video_logger::{
    video_log_is_compatible, video_log_player_find, DirtyInfo, DirtyType, PacketSink,
    VideoLogContext, VideoLogPlayer, VideoLogger,
};
use rgba_core::Platform;
use rgba_gb::gb::Gb;

/// Same helper shape as tests/smoke.rs: minimal 32 KiB ROM, entry at 0x150.
fn test_rom(entry: &[u8]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x103].copy_from_slice(&[0xC3, 0x50, 0x01]); // jp $150
    rom[0x150..0x150 + entry.len()].copy_from_slice(entry);
    let logo: [u8; 48] = [
        0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00,
        0x0D, 0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD,
        0xD9, 0x99, 0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB,
        0xB9, 0x33, 0x3E,
    ];
    rom[0x104..0x134].copy_from_slice(&logo);
    rom[0x134..0x13E].copy_from_slice(b"TESTROM\0\0\0");
    rom[0x147] = 0; // ROM ONLY
    rom[0x148] = 0; // 32KB
    rom[0x149] = 0; // no RAM
    rom
}

/// The replayed event stream, as the verification sink sees it.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Ev {
    Register(u32, u16),
    Palette(u32, u16),
    Oam(u32, u16),
    Vram(u32, Vec<u8>),
    Scanline(u32),
    Range(u32, u32, u32),
    Flush,
    Frame,
    Buffer(u32, u32, Vec<u8>),
}

#[derive(Default)]
struct Sink {
    events: Vec<Ev>,
}

impl<F: rgba_core::video_logger::VFile> PacketSink<F> for Sink {
    fn parse_packet(&mut self, logger: &mut VideoLogger<F>, p: &DirtyInfo) -> bool {
        match p.kind {
            DirtyType::Register => self.events.push(Ev::Register(p.address, p.value as u16)),
            DirtyType::Palette => self.events.push(Ev::Palette(p.address, p.value as u16)),
            DirtyType::Oam => self.events.push(Ev::Oam(p.address, p.value as u16)),
            DirtyType::Vram => {
                let mut data = vec![0u8; p.value as usize];
                assert!(logger.read_data(&mut data));
                self.events.push(Ev::Vram(p.address, data));
            }
            DirtyType::Scanline => self.events.push(Ev::Scanline(p.address)),
            DirtyType::Range => self.events.push(Ev::Range(p.address, p.value, p.value2)),
            // Pause at flushes like the C proxy renderers do; the driver
            // resumes by calling run() again.
            DirtyType::Flush => {
                self.events.push(Ev::Flush);
                return false;
            }
            DirtyType::Frame => self.events.push(Ev::Frame),
            DirtyType::Buffer => {
                let mut data = vec![0u8; p.value2 as usize];
                assert!(logger.read_data(&mut data));
                self.events.push(Ev::Buffer(p.address, p.value, data));
            }
            DirtyType::Dummy => return false,
        }
        true
    }
}

#[test]
fn video_log_records_one_frame() {
    // LCDC off (so VRAM/OAM writes are never blocked by mode 3/2), poke
    // VRAM[0], OAM[0], BGP, SCX, SCY, then LCDC back on and loop.
    let rom = test_rom(&[
        0x3E, 0x00, 0xE0, 0x40, // ld a,0        ; ldh (0x40),a — LCDC off
        0x3E, 0x5A, 0xEA, 0x00, 0x80, // ld a,0x5A ; ld (0x8000),a — VRAM[0]
        0xEA, 0x00, 0xFE, //               ld (0xFE00),a — OAM[0]
        0x3E, 0xE4, 0xE0, 0x47, // ld a,0xE4   ; ldh (0x47),a — BGP
        0x3E, 0x05, 0xE0, 0x43, // ld a,5      ; ldh (0x43),a — SCX
        0x3E, 0x07, 0xE0, 0x42, // ld a,7      ; ldh (0x42),a — SCY
        0x3E, 0x91, 0xE0, 0x40, // ld a,0x91   ; ldh (0x40),a — LCDC on
        0x18, 0xFE, // jr $
    ]);
    let mut gb = Gb::new();
    assert!(gb.load_rom(rom));
    gb.sm83_reset();

    let path = std::env::temp_dir().join(format!(
        "rgba-gb-video-log-test-{}.mvl",
        std::process::id()
    ));
    gb.start_video_log(&path).expect("start_video_log");
    gb.run_frame();
    gb.end_video_log();

    let bytes = std::fs::read(&path).expect("read log back");
    let _ = std::fs::remove_file(&path);

    // It's a GB mVL and it carries the savestate as its initial state.
    assert_eq!(
        video_log_is_compatible(&mut Cursor::new(bytes.clone())),
        Platform::Gb
    );
    assert_eq!(
        video_log_player_find(&mut Cursor::new(bytes.clone())),
        Some(VideoLogPlayer::Gb)
    );

    let ctx = Rc::new(RefCell::new(VideoLogContext::<Cursor<Vec<u8>>>::create_reader()));
    ctx.borrow_mut().load(Cursor::new(bytes)).unwrap();
    assert_eq!(
        ctx.borrow().initial_state().map(|s| s.len()),
        Some(rgba_gb::serialize::GB_SAVESTATE_SIZE)
    );

    // Replay channel 0, pausing/resuming at flushes.
    let mut sink = Sink::default();
    let mut logger = VideoLogger::attach(ctx.clone(), 0, true).unwrap();
    while logger.run(&mut sink, true) {}

    // DIRTY_REGISTER packets for the IO registers the ROM wrote.
    let regs = [
        (0x40u32, 0x00u16), // LCDC off
        (0x40, 0x91),       // LCDC on
        (0x43, 0x05),       // SCX
        (0x42, 0x07),       // SCY
    ];
    for (addr, value) in regs {
        assert!(
            sink.events.contains(&Ev::Register(addr, value)),
            "missing DIRTY_REGISTER {addr:#04X}={value:#04X}\nevents: {:?}",
            sink.events
        );
    }

    // DIRTY_PALETTE packets from the BGP write: 0xE4 expands to the four
    // DMG shades 0/1/2/3 (identity mapping).
    for (index, value) in [(0u32, 0x7FFFu16), (1, 0x56B5), (2, 0x294A), (3, 0x0000)] {
        assert!(
            sink.events.contains(&Ev::Palette(index, value)),
            "missing DIRTY_PALETTE {index}={value:#06X}"
        );
    }

    // DIRTY_OAM for the OAM[0] write.
    assert!(sink.events.contains(&Ev::Oam(0, 0x5A)));

    // DIRTY_VRAM block-0 dump carrying the written byte.
    assert!(
        sink.events
            .iter()
            .any(|e| matches!(e, Ev::Vram(0, data) if data.len() == 0x1000 && data[0] == 0x5A)),
        "missing DIRTY_VRAM block 0 with the written byte"
    );

    // Scanline packets once the LCD is on, and exactly one frame boundary,
    // followed by the proxy's flush.
    assert!(
        sink.events.iter().any(|e| matches!(e, Ev::Scanline(0))),
        "missing DIRTY_SCANLINE 0"
    );
    assert_eq!(
        sink.events.iter().filter(|e| **e == Ev::Frame).count(),
        1,
        "expected exactly one DIRTY_FRAME"
    );
    assert_eq!(
        sink.events.iter().filter(|e| **e == Ev::Flush).count(),
        1,
        "expected the DIRTY_FLUSH that follows DIRTY_FRAME"
    );
}

#[test]
fn video_log_end_is_idempotent() {
    let rom = test_rom(&[0x18, 0xFE]);
    let mut gb = Gb::new();
    gb.load_rom(rom);
    gb.sm83_reset();
    // end without start is a no-op (_GBCoreEndVideoLog's NULL check).
    gb.end_video_log();
    assert!(gb.video_logger.is_none());
}
