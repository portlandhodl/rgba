// Copyright (c) 2013-2017 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from the record half of mgba/src/gba/extra/proxy.c (the
// GBAVideoProxyRenderer shim's mVideoLogger endpoints) and the
// _GBACoreStartVideoLog/_GBACoreEndVideoLog glue in mgba/src/gba/core.c,
// plus the mVideoLogContextCreate/SetOutput/WriteHeader sequence wrapped
// around them by the frontend (mgba/src/platform/qt/CoreController.cpp).
//
// Same approach as the GB port (crates/rgba-gb/src/video_log.rs): no
// renderer vtable to shim, so the concrete renderer entry points in
// video/renderers.rs tee into `Gba.video_logger` while recording.
//
// Not ported: the replay side (GBAVideoLogPlayerCreate), the proxy's shadow
// VRAM/OAM copies (they exist so the blocking player core can own VRAM),
// and the thread wake/wait hooks.

use std::cell::RefCell;
use std::fs::File;
use std::io;
use std::path::Path;
use std::rc::Rc;

use rgba_core::core::Platform;
use rgba_core::video_logger::{VideoLogContext, VideoLogger};

use crate::gba::Gba;
use crate::memory::{GBA_SIZE_OAM, GBA_SIZE_PALETTE_RAM, GBA_SIZE_VRAM};

/// The record-side video-log session held on `Gba` while logging (the C's
/// GBACore pair of proxyRenderer + logContext).
pub struct GbaVideoLog {
    pub(crate) context: Rc<RefCell<VideoLogContext<File>>>,
    pub(crate) logger: VideoLogger<File>,
}

impl GbaVideoLog {
    /// Logger access for the renderer tee points (`self.video_logger` is
    /// `Option<Box<GbaVideoLog>>`).
    pub(crate) fn logger(&mut self) -> &mut VideoLogger<File> {
        &mut self.logger
    }
}

impl Gba {
    /// mVideoLogContextCreate + _GBACoreStartVideoLog: capture the savestate
    /// as the mVL initial state, open + truncate the file, attach one
    /// record channel, write the header, install the logger. The renderer
    /// entry points in video/renderers.rs already tee into `video_logger`.
    pub fn start_video_log(&mut self, path: &Path) -> io::Result<()> {
        let state = crate::serialize::serialize(self)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let mut context = VideoLogContext::<File>::create_writer(state);
        let channel = context.add_channel().expect("fresh mVL writer");
        context.set_output(File::create(path)?)?;
        context.write_header(Platform::Gba)?;
        let context = Rc::new(RefCell::new(context));
        let mut logger = VideoLogger::attach(context.clone(), channel, false)
            .expect("channel just added");
        // GBAVideoProxyRendererCreate's sizes: GBA palette/VRAM/OAM.
        logger.set_sizes(GBA_SIZE_VRAM as usize, GBA_SIZE_OAM as usize, GBA_SIZE_PALETTE_RAM as usize);
        logger.renderer_init();
        self.video_logger = Some(Box::new(GbaVideoLog { context, logger }));
        Ok(())
    }

    /// _GBACoreEndVideoLog: flush whatever's in flight, deinit, write the
    /// footer (via `close`).
    pub fn end_video_log(&mut self) {
        if let Some(video_logger) = self.video_logger.take() {
            let mut logger = video_logger.logger;
            logger.renderer_deinit();
            let _ = logger.flush();
            let _ = video_logger.context.borrow_mut().close();
        }
    }
}
