// Copyright (c) 2013-2026 Jeffrey Pfau (mGBA), MPL-2.0.
// Tests for the mVL video-log format port (mgba/src/feature/video-logger.c,
// crates/rgba-core/src/video_logger.rs).
//
// No console is involved (the core hookup is integration-pending): a fake
// "viewer" scripts renderer-dirty events into a write-side logger, the mVL
// file is built in memory, and a read-side logger replays it into a
// verification sink whose event list must match the script exactly.

use std::cell::RefCell;
use std::io::Cursor;
use std::rc::Rc;

use rgba_core::video_logger::{
    video_log_is_compatible, video_log_player_find, DirtyInfo, DirtyType, InjectionPoint,
    PacketSink, VideoLogContext, VideoLogPlayer, VideoLogger, VramBlockSource,
};
use rgba_core::Platform;

type TestF = Cursor<Vec<u8>>;
type Ctx = VideoLogContext<TestF>;
type Logger = VideoLogger<TestF>;

fn patterned(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| (i as u8) ^ seed).collect()
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

/// VRAM stand-in for the record-side logger.
struct FakeVram {
    mem: Vec<u8>,
}

impl FakeVram {
    fn new(size: usize) -> FakeVram {
        FakeVram {
            mem: vec![0; size],
        }
    }
    fn fill_block(&mut self, addr: usize, seed: u8) {
        for i in 0..0x1000 {
            self.mem[addr + i] = (i as u8) ^ seed;
        }
    }
    fn block(&self, addr: u32) -> Vec<u8> {
        let addr = addr as usize;
        self.mem[addr..addr + 0x1000].to_vec()
    }
}

impl VramBlockSource for FakeVram {
    fn vram_block(&mut self, address: u32, out: &mut [u8]) {
        out.copy_from_slice(&self.mem[address as usize..address as usize + out.len()]);
    }
}

/// For loggers with vram_size == 0 the dirty bitmap is empty and this is
/// never invoked.
struct NullVram;

impl VramBlockSource for NullVram {
    fn vram_block(&mut self, _address: u32, _out: &mut [u8]) {
        unreachable!("no VRAM configured on this logger");
    }
}

fn new_writer(initial_state: Vec<u8>, compression: bool) -> Rc<RefCell<Ctx>> {
    let ctx = Rc::new(RefCell::new(Ctx::create_writer(initial_state)));
    ctx.borrow_mut().set_compression(compression);
    ctx.borrow_mut().set_output(Cursor::new(Vec::new())).unwrap();
    ctx
}

/// Close the writer (flush + footer) and extract the mVL bytes.
fn finish(ctx: &Rc<RefCell<Ctx>>) -> Vec<u8> {
    ctx.borrow_mut().close().unwrap();
    ctx.borrow_mut().take_backing().unwrap().into_inner()
}

fn new_reader(bytes: Vec<u8>) -> Rc<RefCell<Ctx>> {
    let ctx = Rc::new(RefCell::new(Ctx::create_reader()));
    ctx.borrow_mut().load(Cursor::new(bytes)).unwrap();
    ctx
}

/// Replay one channel end-to-end, resuming past flushes.
fn run_all(ctx: &Rc<RefCell<Ctx>>, channel: usize, sink: &mut Sink) {
    let mut logger = Logger::attach(ctx.clone(), channel, true).unwrap();
    while logger.run(sink, true) {}
}

/// The main scripted record session: register/palette/OAM writes, VRAM
/// dirty-block dumps (incl. dedup and re-dirty after a scanline), scanline
/// and range boundaries, an aux buffer, one oversized single write, a
/// mid-frame flush with traffic after it, and frame boundaries.
fn record(compression: bool) -> (Vec<u8>, Vec<u8>, Vec<Ev>) {
    let state = patterned(0x123, 0x5A);
    let ctx = new_writer(state.clone(), compression);
    let ch0 = ctx.borrow_mut().add_channel().unwrap();
    assert_eq!(ch0, 0);
    ctx.borrow_mut().write_header(Platform::Gba).unwrap();

    let mut logger = Logger::attach(ctx.clone(), 0, false).unwrap();
    logger.set_sizes(0x18000, 0x400, 0x400);
    logger.renderer_init();

    let mut vram = FakeVram::new(0x18000);
    let mut expected = Vec::new();

    for f in 0u8..2 {
        logger.write_video_register(0x100 + f as u32, 0x1F00 + f as u16);
        expected.push(Ev::Register(0x100 + f as u32, 0x1F00 + f as u16));

        logger.write_palette(0x22, 0x7FFF - f as u16);
        expected.push(Ev::Palette(0x22, 0x7FFF - f as u16));

        logger.write_oam(3 + f as u32, 0xBEEF);
        expected.push(Ev::Oam(3 + f as u32, 0xBEEF));

        vram.fill_block(0x2000, 0x30 + f);
        logger.write_vram(0x2000);
        logger.write_vram(0x2000); // dedup: marking twice logs once

        logger.draw_scanline(&mut vram, 0);
        expected.push(Ev::Vram(0x2000, vram.block(0x2000)));
        expected.push(Ev::Scanline(0));

        vram.fill_block(0x2000, 0x70 + f);
        vram.fill_block(0x8000, 0x90 + f);
        logger.write_vram(0x2000);
        logger.write_vram(0x8000);

        let payload = patterned(140, f);
        logger.write_buffer(7, 0x10, &payload);
        expected.push(Ev::Buffer(7, 0x10, payload));

        logger.draw_range(&mut vram, 0, 240, 77);
        expected.push(Ev::Vram(0x2000, vram.block(0x2000)));
        expected.push(Ev::Vram(0x8000, vram.block(0x8000)));
        expected.push(Ev::Range(77, 0, 240));

        logger.write_video_register(0x200 + f as u32, 0x3333);
        expected.push(Ev::Register(0x200 + f as u32, 0x3333));

        if f == 0 {
            // A single write larger than BUFFER_BASE_SIZE: the C grows the
            // channel's circle buffer; we emit one big data block instead.
            let big = patterned(300_000, 0xAA);
            logger.write_buffer(9, 0x20, &big);
            expected.push(Ev::Buffer(9, 0x20, big));
        }

        logger.flush();
        expected.push(Ev::Flush);

        // Traffic after a flush must survive the pause/resume on replay.
        logger.write_palette(0x24, 0x1111 + f as u16);
        expected.push(Ev::Palette(0x24, 0x1111 + f as u16));

        logger.draw_scanline(&mut vram, 160);
        expected.push(Ev::Scanline(160));

        logger.finish_frame();
        expected.push(Ev::Frame);
    }

    logger.renderer_deinit();
    let bytes = finish(&ctx);
    (state, bytes, expected)
}

fn verify_roundtrip(state: &[u8], bytes: &[u8], expected: &[Ev]) -> Rc<RefCell<Ctx>> {
    assert_eq!(
        video_log_is_compatible(&mut Cursor::new(bytes.to_vec())),
        Platform::Gba
    );
    assert_eq!(
        video_log_player_find(&mut Cursor::new(bytes.to_vec())),
        Some(VideoLogPlayer::Gba)
    );

    let ctx = new_reader(bytes.to_vec());
    assert_eq!(ctx.borrow().initial_state(), Some(state));
    assert_eq!(ctx.borrow().n_channels(), 1);
    assert!(Logger::attach(ctx.clone(), 1, true).is_none());

    let mut sink = Sink::default();
    run_all(&ctx, 0, &mut sink);
    assert_eq!(&sink.events, expected);
    ctx
}

#[test]
fn roundtrip_uncompressed() {
    let (state, bytes, expected) = record(false);
    verify_roundtrip(&state, &bytes, &expected);
}

#[test]
fn roundtrip_compressed() {
    let (state, bytes, expected) = record(true);
    verify_roundtrip(&state, &bytes, &expected);
}

/// Two channels interleaved in one file: each reader sees only its own
/// stream, scanning past the other channel's data blocks.
#[test]
fn roundtrip_two_channels() {
    // No initial state in this one: exercises the no-initial-state header.
    let ctx = new_writer(Vec::new(), false);
    assert_eq!(ctx.borrow_mut().add_channel(), Some(0));
    assert_eq!(ctx.borrow_mut().add_channel(), Some(1));
    ctx.borrow_mut().write_header(Platform::Gb).unwrap();

    let mut l0 = Logger::attach(ctx.clone(), 0, false).unwrap();
    let mut l1 = Logger::attach(ctx.clone(), 1, false).unwrap();
    let mut expected0 = Vec::new();
    let mut expected1 = Vec::new();

    l0.write_video_register(0xFF40, 0x91);
    expected0.push(Ev::Register(0xFF40, 0x91));
    l1.write_palette(1, 0x1234);
    expected1.push(Ev::Palette(1, 0x1234));
    l1.write_video_register(5, 6);
    expected1.push(Ev::Register(5, 6));
    l0.write_oam(0, 0xAA);
    expected0.push(Ev::Oam(0, 0xAA));
    l0.finish_frame();
    expected0.push(Ev::Frame);
    l1.finish_frame();
    expected1.push(Ev::Frame);
    l0.write_video_register(0xFF41, 0x00);
    expected0.push(Ev::Register(0xFF41, 0x00));

    let bytes = finish(&ctx);

    assert_eq!(
        video_log_is_compatible(&mut Cursor::new(bytes.clone())),
        Platform::Gb
    );
    assert_eq!(
        video_log_player_find(&mut Cursor::new(bytes.clone())),
        Some(VideoLogPlayer::Gb)
    );

    let ctx = new_reader(bytes);
    assert_eq!(ctx.borrow().n_channels(), 2);
    assert_eq!(ctx.borrow().initial_state(), None);
    let mut sink0 = Sink::default();
    run_all(&ctx, 0, &mut sink0);
    let mut sink1 = Sink::default();
    run_all(&ctx, 1, &mut sink1);
    assert_eq!(sink0.events, expected0);
    assert_eq!(sink1.events, expected1);
}

/// After a full replay, mVideoLogContextRewind resets the channels so the
/// log replays identically a second time.
#[test]
fn rewind_replays_identically() {
    let (state, bytes, expected) = record(true);
    let ctx = new_reader(bytes);
    {
        let mut sink = Sink::default();
        run_all(&ctx, 0, &mut sink);
        assert_eq!(&sink.events, &expected);
    }
    ctx.borrow_mut().rewind().unwrap();
    assert_eq!(ctx.borrow().initial_state(), Some(state.as_slice()));
    let mut sink = Sink::default();
    run_all(&ctx, 0, &mut sink);
    assert_eq!(&sink.events, &expected);
}

#[test]
fn sniff_rejects_non_mvl() {
    let mut junk = Cursor::new(b"not an mVL file".to_vec());
    assert_eq!(video_log_is_compatible(&mut junk), Platform::None);
    assert_eq!(video_log_player_find(&mut junk), None);
    let mut empty = Cursor::new(Vec::new());
    assert_eq!(video_log_is_compatible(&mut empty), Platform::None);

    let ctx = Rc::new(RefCell::new(Ctx::create_reader()));
    assert!(ctx.borrow_mut().load(Cursor::new(b"garbage".to_vec())).is_err());

    // Truncated header after a valid magic and platform: sniff succeeds but
    // load must fail, not mispars.
    let mut hdr = Vec::new();
    hdr.extend_from_slice(b"mVL\0");
    hdr.extend_from_slice(&0u32.to_le_bytes());
    hdr.extend_from_slice(&(Platform::Gba as u32).to_le_bytes());
    assert!(ctx
        .borrow_mut()
        .load(Cursor::new(hdr))
        .map(|_| ())
        .is_err());
}

/// Script shared by the injection tests: traffic, scanline 0 (injection
/// point for the first-scanline mode), frame boundaries.
fn record_injection_script() -> Vec<u8> {
    let ctx = new_writer(Vec::new(), false);
    ctx.borrow_mut().add_channel().unwrap();
    ctx.borrow_mut().write_header(Platform::Gba).unwrap();
    let mut w = Logger::attach(ctx.clone(), 0, false).unwrap();
    w.write_video_register(0x40, 0x1111);
    w.draw_scanline(&mut NullVram, 0);
    w.draw_scanline(&mut NullVram, 1);
    w.finish_frame();
    w.write_video_register(0x41, 0x2222);
    w.draw_scanline(&mut NullVram, 0);
    w.finish_frame();
    finish(&ctx)
}

#[test]
fn injection_immediate() {
    let bytes = record_injection_script();
    let ctx = new_reader(bytes);
    let mut logger = Logger::attach(ctx.clone(), 0, true).unwrap();
    logger.inject_video_register(0x99, 0xAAAA);
    logger.inject_oam(7, 0xBBBB);
    logger.ignore_after_injection(1 << DirtyType::Frame as u32);

    let mut sink = Sink::default();
    while logger.run(&mut sink, true) {}

    // Injected packets replay first (immediate point), then the stream
    // minus the ignored frame boundaries.
    assert_eq!(
        sink.events,
        vec![
            Ev::Register(0x99, 0xAAAA),
            Ev::Oam(7, 0xBBBB),
            Ev::Register(0x40, 0x1111),
            Ev::Scanline(0),
            Ev::Scanline(1),
            Ev::Register(0x41, 0x2222),
            Ev::Scanline(0),
        ]
    );
}

#[test]
fn injection_first_scanline() {
    let bytes = record_injection_script();
    let ctx = new_reader(bytes);
    let mut logger = Logger::attach(ctx.clone(), 0, true).unwrap();
    logger.set_injection_point(InjectionPoint::FirstScanline);
    logger.inject_video_register(0x99, 0xAAAA);
    logger.inject_oam(7, 0xBBBB);
    logger.ignore_after_injection(1 << DirtyType::Frame as u32);

    let mut sink = Sink::default();
    while logger.run(&mut sink, true) {}

    // Injected packets replay right before the first scanline-0 packet;
    // the second frame's scanline-0 re-runs the injection but the buffer
    // is drained by then, so nothing duplicates.
    assert_eq!(
        sink.events,
        vec![
            Ev::Register(0x40, 0x1111),
            Ev::Register(0x99, 0xAAAA),
            Ev::Oam(7, 0xBBBB),
            Ev::Scanline(0),
            Ev::Scanline(1),
            Ev::Register(0x41, 0x2222),
            Ev::Scanline(0),
        ]
    );
}
