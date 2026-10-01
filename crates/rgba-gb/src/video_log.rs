// Copyright (c) 2013-2017 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from the record half of mgba/src/gb/extra/proxy.c (the
// GBVideoProxyRenderer shim's mVideoLogger endpoints) and the
// _GBCoreStartVideoLog/_GBCoreEndVideoLog glue in mgba/src/gb/core.c, plus
// the mVideoLogContextCreate/SetOutput/WriteHeader sequence wrapped around
// them by the frontend (mgba/src/platform/qt/CoreController.cpp
// CoreController::startVideoLog).
//
// Rust has no renderer vtable to swap, so instead of a proxy renderer
// sitting between GBVideo and the software renderer, the concrete renderer
// write/draw/scanline/frame entry points in video.rs (and their call
// boundaries in memory.rs/io.rs) tee into this logger while
// `Gb.video_logger` is Some. The C proxy hooks' order (log before/after the
// backend call) is preserved at those hook points.
//
// Not ported: the replay side (GBVideoLogPlayerCreate, _parsePacket), the
// proxy's shadow VRAM/OAM mmaps, and the getPixels/wake/wait thread hooks
// (single-threaded; the real renderer always renders). DIRTY_VRAM payloads
// are dumped from live VRAM at scanline/range flush time exactly like the
// C's _vramBlock reading proxyRenderer->d.vram.

use std::cell::RefCell;
use std::fs::File;
use std::io;
use std::path::Path;
use std::rc::Rc;

use rgba_core::core::Platform;
use rgba_core::video_logger::{VideoLogContext, VideoLogger, VramBlockSource};

use crate::gb::Gb;
use crate::video::{GB_SIZE_OAM, GB_SIZE_VRAM};

/// BUFFER_SGB (gb/extra/proxy.c): aux-buffer id for logged SGB packets.
const BUFFER_SGB: u32 = 2;

/// The record-side video-log session held on `Gb` while logging (the C's
/// GBCore::proxyRenderer + logContext pair from _GBCoreStartVideoLog).
pub struct GbVideoLog {
    pub(crate) context: Rc<RefCell<VideoLogContext<File>>>,
    pub(crate) logger: VideoLogger<File>,
}

/// _vramBlock (gb/extra/proxy.c): payload source for DIRTY_VRAM dumps,
/// reading live VRAM like the C's pointer into proxyRenderer->d.vram.
struct GbVramBlock<'a>(&'a [u8]);

impl VramBlockSource for GbVramBlock<'_> {
    fn vram_block(&mut self, address: u32, out: &mut [u8]) {
        out.copy_from_slice(&self.0[address as usize..address as usize + out.len()]);
    }
}

impl Gb {
    /// mVideoLogContextCreate(core) + _GBCoreStartVideoLog +
    /// CoreController::startVideoLog: capture the savestate as the mVL
    /// initial state, open + truncate the output file, attach one record
    /// channel, write the header, and install the logger. From here the
    /// renderer entry points log their traffic until `end_video_log`.
    pub fn start_video_log(&mut self, path: &Path) -> io::Result<()> {
        let state = crate::serialize::serialize(self)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let mut context = VideoLogContext::<File>::create_writer(state);
        let channel = context.add_channel().expect("fresh mVL writer");
        context.set_output(File::create(path)?)?;
        context.write_header(Platform::Gb)?;
        let context = Rc::new(RefCell::new(context));
        let mut logger =
            VideoLogger::attach(context.clone(), channel, false).expect("channel just added");
        // GBVideoProxyRendererCreate: paletteSize 0, vramSize GB_SIZE_VRAM,
        // oamSize GB_SIZE_OAM; GBVideoProxyRendererShim's _init() becomes
        // renderer_init + zeroed dirty bitmaps.
        logger.set_sizes(GB_SIZE_VRAM, GB_SIZE_OAM, 0);
        logger.renderer_init();
        self.video_logger = Some(GbVideoLog { context, logger });
        Ok(())
    }

    /// _GBCoreEndVideoLog + the destroy half of mVideoLogContextDestroy:
    /// unhook the logger, deinit its bitmaps, flush and write the footer.
    /// Write errors are swallowed as in the C.
    pub fn end_video_log(&mut self) {
        if let Some(mut vl) = self.video_logger.take() {
            vl.logger.renderer_deinit();
            let _ = vl.context.borrow_mut().close();
        }
    }

    // ---- GBVideoProxyRenderer* hook halves (block == false: record) ----

    /// GBVideoProxyRendererWriteVideoRegister: log, then the backend (the
    /// backend half is the caller's body in video.rs).
    pub(crate) fn vl_write_video_register(&mut self, address: u16, value: u8) {
        if let Some(vl) = self.video_logger.as_mut() {
            vl.logger.write_video_register(address as u32, value as u16);
        }
    }

    /// GBVideoProxyRendererWriteVRAM: mark the 0x1000-byte block dirty.
    pub(crate) fn vl_write_vram(&mut self, addr: u16) {
        if let Some(vl) = self.video_logger.as_mut() {
            vl.logger.write_vram(addr as u32);
        }
    }

    /// GBVideoProxyRendererWriteOAM: log the just-written OAM byte
    /// (((uint8_t*) proxyRenderer->d.oam->raw)[oam]).
    pub(crate) fn vl_write_oam(&mut self, addr: u16) {
        if let Some(vl) = self.video_logger.as_mut() {
            let value = self.video.oam[addr as usize] as u16;
            vl.logger.write_oam(addr as u32, value);
        }
    }

    /// GBVideoProxyRendererWritePalette.
    pub(crate) fn vl_write_palette(&mut self, index: i32, value: u16) {
        if let Some(vl) = self.video_logger.as_mut() {
            vl.logger.write_palette(index as u32, value);
        }
    }

    /// GBVideoProxyRendererWriteSGBPacket: mVideoLoggerWriteBuffer of the
    /// packet buffer's first 16 bytes.
    pub(crate) fn vl_write_sgb_packet(&mut self) {
        if let Some(vl) = self.video_logger.as_mut() {
            let data: [u8; 16] = self.video.sgb_packet_buffer[..16].try_into().unwrap();
            vl.logger.write_buffer(BUFFER_SGB, 0, &data);
        }
    }

    /// GBVideoProxyRendererDrawRange.
    pub(crate) fn vl_draw_range(&mut self, start_x: i32, end_x: i32, y: i32) {
        if let Some(vl) = self.video_logger.as_mut() {
            vl.logger
                .draw_range(&mut GbVramBlock(&self.video.vram), start_x, end_x, y);
        }
    }

    /// GBVideoProxyRendererFinishScanline.
    pub(crate) fn vl_finish_scanline(&mut self, y: i32) {
        if let Some(vl) = self.video_logger.as_mut() {
            vl.logger.draw_scanline(&mut GbVramBlock(&self.video.vram), y);
        }
    }

    /// GBVideoProxyRendererFinishFrame: frame boundary + flush.
    pub(crate) fn vl_finish_frame(&mut self) {
        if let Some(vl) = self.video_logger.as_mut() {
            vl.logger.finish_frame();
            vl.logger.flush();
        }
    }
}
