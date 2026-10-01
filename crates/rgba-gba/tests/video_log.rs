// Copyright (c) 2013-2017 Jeffrey Pfau (mGBA), MPL-2.0.
// End-to-end test of the GBA record-side video-log glue (mgba
// src/gba/extra/proxy.c record hooks + _GBACoreStartVideoLog/
// _GBACoreEndVideoLog in src/gba/core.c): a frame is run with the logger
// attached while the bus writes DISPCNT/BG0HOFS/palette/VRAM/OAM, and the
// mVL is read back through the rgba-core reader.

use std::cell::RefCell;
use std::io::Cursor;
use std::rc::Rc;

use rgba_core::video_logger::{
    video_log_is_compatible, video_log_player_find, DirtyInfo, DirtyType, PacketSink,
    VideoLogContext, VideoLogPlayer, VideoLogger,
};
use rgba_core::Platform;
use rgba_gba::gba::Gba;

fn rom() -> Vec<u8> {
    let mut r = vec![0u8; 0x8000];
    r[0..4].copy_from_slice(&[0xFE, 0xFF, 0xFF, 0xEA]); // b .
    r[0xB2] = 0x96;
    r
}

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
            DirtyType::Dummy => {}
            DirtyType::Register => self.events.push(Ev::Register(p.address, p.value as u16)),
            DirtyType::Palette => self.events.push(Ev::Palette(p.address, p.value as u16)),
            DirtyType::Oam => self.events.push(Ev::Oam(p.address, p.value as u16)),
            DirtyType::Vram => {
                let mut data = vec![0u8; p.value as usize];
                assert!(logger.read_data(&mut data));
                self.events.push(Ev::Vram(p.address, data));
            }
            DirtyType::Scanline => self.events.push(Ev::Scanline(p.address)),
            DirtyType::Range => {
                self.events.push(Ev::Range(p.address, p.value, p.value2))
            }
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
        }
        true
    }
}

#[test]
fn gba_video_log_records_one_frame() {
    let mut gba = Gba::new();
    gba.load_rom(rom());
    gba.arm_reset();

    let path = std::env::temp_dir().join(format!(
        "rgba_gba_video_log_{}_{}.mvl",
        std::process::id(),
        "one"
    ));
    gba.start_video_log(&path).expect("start_video_log");
    let mut cc = 0;
    gba.store16(0x04000000, 0x403, &mut cc); // DISPCNT = mode3|BG2
    gba.store16(0x04000010, 0x0005, &mut cc); // BG0HOFS = 5
    gba.store16(0x05000000, 0x7C1F, &mut cc); // palette[0] = 0x7C1F
    gba.store16(0x06000000, 0x001F, &mut cc); // VRAM[0..2] = 0x001F
    gba.store16(0x07000000, 0xE001, &mut cc); // OAM
    gba.run_frame();
    gba.end_video_log();

    let bytes = std::fs::read(&path).expect("read back");
    let _ = std::fs::remove_file(&path);

    // Replays as an mVL GBA file
    let mut peek = Cursor::new(bytes.clone());
    assert!(video_log_is_compatible(&mut peek) == Platform::Gba);
    let mut peek = Cursor::new(bytes.clone());
    assert_eq!(video_log_player_find(&mut peek), Some(VideoLogPlayer::Gba));

    // Playback: consume the packet stream.
    let file = Cursor::new(bytes);
    let mut ctx = VideoLogContext::create_reader();
    ctx.load(file).expect("load");
    let ctx = Rc::new(RefCell::new(ctx));
    let channel = 0usize;
    let mut logger = VideoLogger::attach(ctx.clone(), channel, true)
        .expect("channel 0");
    logger.set_sizes(0x18000, 0x400, 0x400);
    logger.renderer_init();
    let mut sink = Sink::default();
    while logger.run(&mut sink, true) {}
    let e = &sink.events;

    // The mVL captures its initial state as a savestate:
    assert!(RefCell::borrow(&ctx).initial_state().is_some());

    // Register writes (DISPCNT is written by run_frame's own flow too, so
    // check the specific ours at least once)
    assert!(
        e.contains(&Ev::Register(0x04000000u32 & 0xFFFF, 0x403)),
        "expected a register packet for DISPCNT=0x403: {:?}",
        e.iter().filter(|x| matches!(x, Ev::Register(..))).take(5).collect::<Vec<_>>()
    );
    assert!(e.contains(&Ev::Register(0x10u32, 0x0005)), "BG0HOFS=5 missing");
    // Palette[0]
    assert!(
        e.contains(&Ev::Palette(0, 0x7C1F)),
        "palette entry 0 missing"
    );
    // VRAM dirty block (at least one), carrying the 0x001F word
    let vram_ok = e
        .iter()
        .filter_map(|ev| match ev {
            Ev::Vram(addr, data) => Some((addr, data)),
            _ => None,
        })
        .any(|(_, data)| data.windows(2).any(|w| w == [0x1F, 0x00]));
    assert!(vram_ok, "no VRAM packet contained 0x001F");
    // OAM entry 0 (halfword idx 0; value read from live OAM)
    assert!(e.contains(&Ev::Oam(0, 0xE001)), "OAM packet missing");
    // Scanline + frame + flush packets are present
    assert!(e.iter().any(|x| matches!(x, Ev::Scanline(_))), "scanline missing");
    assert_eq!(e.iter().filter(|x| **x == Ev::Frame).count(), 1, "frame count");
    assert!(e.iter().any(|x| *x == Ev::Flush), "flush missing");
}

#[test]
fn gba_video_log_end_is_idempotent() {
    let mut gba = Gba::new();
    gba.load_rom(rom());
    gba.arm_reset();
    gba.end_video_log(); // no-op without a start
}
