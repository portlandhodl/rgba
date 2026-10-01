// Copyright (c) 2013-2017 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/feature/video-logger.c and
// mgba/include/mgba/feature/video-logger.h — the "mVL" video-log format: a
// write-side context that serializes renderer "dirty" packets (video
// register/palette/OAM writes, VRAM dirty-block dumps, scanline and frame
// boundaries, raw aux buffers) into an optionally zlib-compressed stream of
// per-channel blocks, and a read-side context plus run loop that replays
// those packets into a PacketSink (the C player replays them into a proxied
// renderer of a headless core for verification/TAS).
//
// Divergences from the C:
// - C's `struct VFile` becomes the generic `VFile` trait below: std::io
//   Read+Write+Seek plus the truncate hook mVideoLogContextSetOutput needs.
// - zlib is provided by flate2 (deflate level 9, matching the C's
//   deflateInit(&zstr, 9)); compression defaults on, as under USE_ZLIB.
// - The C shares mVideoLogContext/mVideoLogChannel between the core and the
//   renderer through raw pointers; here the logger endpoint holds an
//   Rc<RefCell<VideoLogContext<F>>> (same pattern as the SIO lockstep port).
// - C streams inflate output through a fixed circle buffer with a live
//   z_stream per channel; we inflate each compressed data block whole into
//   the channel buffer. The replayed byte stream and the file offsets are
//   identical (the compressed block length in the header is consumed
//   exactly); only peak buffering differs.
// - INTEGRATION PENDING: the console glue is not wired up. In C the hook
//   points are `mCore::startVideoLog`/`endVideoLog` (src/gba/core.c
//   _GBACoreStartVideoLog, src/gb/core.c _GBCoreStartVideoLog), which shim a
//   proxy renderer (src/{gba,gb}/extra/proxy.c) between the video core and
//   the real renderer, and GBAVideoLogPlayerCreate/GBVideoLogPlayerCreate
//   build the headless replay core. rgba's Core trait has no start_video_log
//   entry and the proxy renderers are unported, so this module covers the
//   file format plus the endpoint/run-loop machinery; per-console wiring
//   (and actual construction of Gb/Gba player cores behind the
//   VideoLogPlayer enum) is future work. The shading vram/oam/palette copies
//   mVideoLoggerRendererInit mmaps also belong to that glue and are skipped;
//   only the dirty bitmaps that drive packet emission are kept.
// - The initial-state capture is passed in by the caller as raw savestate
//   bytes (C calls core->stateSize/mCoreSaveStateNamed internally and pokes
//   GBASerializedState fields); `rewind` likewise only re-reads the file and
//   resets channels, it does not push the state into a core.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::rc::Rc;

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;

use crate::core::Platform;

/// mVL_MAX_CHANNELS.
pub const MVL_MAX_CHANNELS: usize = 32;
/// BUFFER_BASE_SIZE: channel buffers are flushed around this size.
const BUFFER_BASE_SIZE: usize = 0x20000;
/// MAX_BLOCK_SIZE: reject blocks larger than this on read (corruption guard).
const MAX_BLOCK_SIZE: u32 = 0x800000;
/// mVL_MAGIC.
pub const MVL_MAGIC: [u8; 4] = *b"mVL\0";

// enum mVLBlockType.
const BLOCK_INITIAL_STATE: u32 = 1;
const BLOCK_CHANNEL_HEADER: u32 = 2;
const BLOCK_DATA: u32 = 3;
/// mVL_BLOCK_FOOTER — "mVLx" when read as bytes.
const BLOCK_FOOTER: u32 = 0x784C_566D;

// enum mVLHeaderFlag.
const FLAG_HAS_INITIAL_STATE: u32 = 1;
// enum mVLBlockFlag.
const FLAG_BLOCK_COMPRESSED: u32 = 1;

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

fn round_up(value: usize, shift: usize) -> usize {
    // _roundUp in the C.
    (value + (1 << shift) - 1) >> shift
}

/// The slice of C's `struct VFile` the logger needs: a seekable read/write
/// stream that can be truncated (`vf->truncate` in mVideoLogContextSetOutput).
pub trait VFile: Read + Write + Seek {
    fn truncate(&mut self, size: u64) -> io::Result<()>;
}

impl VFile for std::fs::File {
    fn truncate(&mut self, size: u64) -> io::Result<()> {
        self.set_len(size)
    }
}

impl VFile for io::Cursor<Vec<u8>> {
    fn truncate(&mut self, size: u64) -> io::Result<()> {
        self.get_mut().truncate(size as usize);
        if self.position() > size {
            self.set_position(size);
        }
        Ok(())
    }
}

/// enum mVideoLoggerDirtyType: the packet kinds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum DirtyType {
    Dummy = 0,
    Flush = 1,
    Scanline = 2,
    Register = 3,
    Oam = 4,
    Palette = 5,
    Vram = 6,
    Frame = 7,
    Range = 8,
    Buffer = 9,
}

impl DirtyType {
    fn from_u32(v: u32) -> Option<DirtyType> {
        Some(match v {
            0 => DirtyType::Dummy,
            1 => DirtyType::Flush,
            2 => DirtyType::Scanline,
            3 => DirtyType::Register,
            4 => DirtyType::Oam,
            5 => DirtyType::Palette,
            6 => DirtyType::Vram,
            7 => DirtyType::Frame,
            8 => DirtyType::Range,
            9 => DirtyType::Buffer,
            _ => return None,
        })
    }
}

/// struct mVideoLoggerDirtyInfo: one packet, laid out as four little-endian
/// u32s on disk (the C writes the struct raw, and loads it field-by-field with
/// LOAD_32LE on replay, so the effective format is LE).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DirtyInfo {
    pub kind: DirtyType,
    pub address: u32,
    pub value: u32,
    pub value2: u32,
}

impl DirtyInfo {
    pub const SIZE: usize = 16;

    pub fn new(kind: DirtyType, address: u32, value: u32, value2: u32) -> DirtyInfo {
        DirtyInfo {
            kind,
            address,
            value,
            value2,
        }
    }

    fn encode(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&(self.kind as u32).to_le_bytes());
        out[4..8].copy_from_slice(&self.address.to_le_bytes());
        out[8..12].copy_from_slice(&self.value.to_le_bytes());
        out[12..16].copy_from_slice(&self.value2.to_le_bytes());
        out
    }

    /// None on an out-of-range type (the C switch's `default: return false`).
    fn decode(buf: &[u8; 16]) -> Option<DirtyInfo> {
        let kind = DirtyType::from_u32(u32::from_le_bytes(buf[0..4].try_into().unwrap()))?;
        Some(DirtyInfo {
            kind,
            address: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            value: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            value2: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
        })
    }
}

/// enum mVideoLoggerInjectionPoint.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InjectionPoint {
    /// LOGGER_INJECTION_IMMEDIATE: injected packets replay before anything
    /// else on the next run.
    Immediate,
    /// LOGGER_INJECTION_FIRST_SCANLINE: injected packets replay when the
    /// first scanline packet (y == 0) is hit.
    FirstScanline,
}

/// enum mVideoLoggerEvent. Carried by the C logger's postEvent/handleEvent
/// hooks, which are part of the proxy-renderer/threading glue (integration
/// pending); kept here so that glue has the same vocabulary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoggerEvent {
    None,
    Init,
    Deinit,
    Reset,
    GetPixels,
    LoadState,
    SaveState,
}

/// struct mVLBlockHeader: four LE u32s (blockType, length, channelId, flags).
#[derive(Clone, Copy)]
struct BlockHeader {
    block_type: u32,
    length: u32,
    channel_id: u32,
    flags: u32,
}

impl BlockHeader {
    fn encode(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&self.block_type.to_le_bytes());
        out[4..8].copy_from_slice(&self.length.to_le_bytes());
        out[8..12].copy_from_slice(&self.channel_id.to_le_bytes());
        out[12..16].copy_from_slice(&self.flags.to_le_bytes());
        out
    }

    fn decode(buf: &[u8; 16]) -> BlockHeader {
        BlockHeader {
            block_type: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            length: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            channel_id: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            flags: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
        }
    }
}

/// _readBlockHeader: Ok(None) on a truncated stream or an oversized block
/// (the C returns false for both).
fn read_block_header(r: &mut impl Read) -> io::Result<Option<BlockHeader>> {
    let mut buf = [0u8; 16];
    match r.read_exact(&mut buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let header = BlockHeader::decode(&buf);
    if header.length > MAX_BLOCK_SIZE {
        // Pre-emptively reject blocks that are too big.
        // If we encounter one, the file is probably corrupted.
        return Ok(None);
    }
    Ok(Some(header))
}

/// _compress: deflate one blob (zlib wrapper, level 9).
fn zlib_compress(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::new(9));
    enc.write_all(data)?;
    enc.finish()
}

/// _decompress, operating on an in-memory blob (see the divergence note in
/// the file header). None on a corrupt stream.
fn zlib_decompress(blob: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    ZlibDecoder::new(blob).read_to_end(&mut out).ok()?;
    Some(out)
}

/// struct mVideoLogChannel.
struct VideoLogChannel {
    /// Decoded packet bytes waiting to be consumed (mCircleBuffer buffer).
    buffer: VecDeque<u8>,
    /// Frontend-injected packets, consumed first during replay
    /// (mCircleBuffer injectedBuffer). Never written to the file.
    injected_buffer: VecDeque<u8>,
    /// File offset of the next unread byte of this channel's stream.
    current_pointer: u64,
    /// Bytes left unread in the current data block (compressed size when the
    /// block is compressed).
    buffer_remaining: u64,
    /// Whether the current data block is compressed (C: `inflating`; we
    /// decode whole blocks rather than holding a live z_stream).
    block_compressed: bool,
    injecting: bool,
    injection_point: InjectionPoint,
    ignore_packets: u32,
}

impl VideoLogChannel {
    fn new() -> VideoLogChannel {
        VideoLogChannel {
            buffer: VecDeque::new(),
            injected_buffer: VecDeque::new(),
            current_pointer: 0,
            buffer_remaining: 0,
            block_compressed: false,
            injecting: false,
            injection_point: InjectionPoint::Immediate,
            ignore_packets: 0,
        }
    }
}

/// struct mVideoLogContext: the file-level mVL reader/writer.
///
/// The C is constructed from an mCore and captures the core's savestate as
/// the log's initial state; here the caller supplies those bytes (record
/// path) and reads them back (replay path) with `initial_state`.
pub struct VideoLogContext<F: VFile> {
    initial_state: Vec<u8>,
    channels: Vec<VideoLogChannel>,
    write: bool,
    compression: bool,
    active_channel: usize,
    backing: Option<F>,
    closed: bool,
}

impl<F: VFile> VideoLogContext<F> {
    /// mVideoLogContextCreate(core) minus the core hook: a write-side
    /// context. `initial_state` is the captured savestate (empty for none).
    pub fn create_writer(initial_state: Vec<u8>) -> VideoLogContext<F> {
        VideoLogContext {
            initial_state,
            channels: Vec::new(),
            write: true,
            compression: true, // as under USE_ZLIB; set_compression to change
            active_channel: 0,
            backing: None,
            closed: false,
        }
    }

    /// mVideoLogContextCreate(NULL): a read-side context, to be given a file
    /// with `load`.
    pub fn create_reader() -> VideoLogContext<F> {
        VideoLogContext {
            initial_state: Vec::new(),
            channels: Vec::new(),
            write: false,
            compression: true,
            active_channel: 0,
            backing: None,
            closed: false,
        }
    }

    /// mVideoLogContextSetCompression.
    pub fn set_compression(&mut self, compression: bool) {
        self.compression = compression;
    }

    /// mVideoLogContextSetOutput: take the output file, truncate it.
    pub fn set_output(&mut self, mut f: F) -> io::Result<()> {
        f.truncate(0)?;
        f.seek(SeekFrom::Start(0))?;
        self.backing = Some(f);
        Ok(())
    }

    /// mVideoLoggerAddChannel. Channels must all be added before
    /// `write_header`, as in the C flow.
    pub fn add_channel(&mut self) -> Option<usize> {
        if self.channels.len() >= MVL_MAX_CHANNELS {
            return None;
        }
        self.channels.push(VideoLogChannel::new());
        Some(self.channels.len() - 1)
    }

    pub fn n_channels(&self) -> usize {
        self.channels.len()
    }

    /// mVideoLogContextInitialState.
    pub fn initial_state(&self) -> Option<&[u8]> {
        if self.initial_state.is_empty() {
            None
        } else {
            Some(&self.initial_state)
        }
    }

    /// Take the backing file back out (e.g. after `close`).
    pub fn take_backing(&mut self) -> Option<F> {
        self.backing.take()
    }

    /// mVideoLogContextWriteHeader (platform is what the C reads from the
    /// core): file magic, flags, platform, channel count; then the initial
    /// state block (optionally compressed), then one channel header block
    /// per channel.
    pub fn write_header(&mut self, platform: Platform) -> io::Result<()> {
        let mut hdr = [0u8; 16];
        hdr[0..4].copy_from_slice(&MVL_MAGIC);
        let flags = if self.initial_state.is_empty() {
            0
        } else {
            FLAG_HAS_INITIAL_STATE
        };
        hdr[4..8].copy_from_slice(&flags.to_le_bytes());
        hdr[8..12].copy_from_slice(&(platform as u32).to_le_bytes());
        hdr[12..16].copy_from_slice(&(self.channels.len() as u32).to_le_bytes());
        let backing = match self.backing.as_mut() {
            Some(b) => b,
            None => return Err(invalid("mVL: no output file")),
        };
        backing.write_all(&hdr)?;

        if !self.initial_state.is_empty() {
            let (blob, bflags) = if self.compression {
                (zlib_compress(&self.initial_state)?, FLAG_BLOCK_COMPRESSED)
            } else {
                (self.initial_state.clone(), 0)
            };
            backing.write_all(
                &BlockHeader {
                    block_type: BLOCK_INITIAL_STATE,
                    length: blob.len() as u32,
                    channel_id: 0,
                    flags: bflags,
                }
                .encode(),
            )?;
            backing.write_all(&blob)?;
        }

        for i in 0..self.channels.len() {
            backing.write_all(
                &BlockHeader {
                    block_type: BLOCK_CHANNEL_HEADER,
                    length: 0,
                    channel_id: i as u32,
                    flags: 0,
                }
                .encode(),
            )?;
        }
        Ok(())
    }

    /// _readHeader: magic, flags, channel count, initial-state block.
    /// Returns the channel count.
    fn read_header(&mut self) -> io::Result<usize> {
        let backing = match self.backing.as_mut() {
            Some(b) => b,
            None => return Err(invalid("mVL: no input file")),
        };
        backing.seek(SeekFrom::Start(0))?;
        let mut hdr = [0u8; 16];
        if backing.read_exact(&mut hdr).is_err() {
            return Err(invalid("mVL: truncated header"));
        }
        if hdr[0..4] != MVL_MAGIC {
            return Err(invalid("mVL: bad magic"));
        }
        let flags = u32::from_le_bytes(hdr[4..8].try_into().unwrap());
        // hdr[8..12] is the platform; as in the C _readHeader it is not
        // validated here (video_log_is_compatible/video_log_player_find do).
        let n_channels = u32::from_le_bytes(hdr[12..16].try_into().unwrap()) as usize;
        if n_channels > MVL_MAX_CHANNELS {
            return Err(invalid("mVL: too many channels"));
        }
        if flags & FLAG_HAS_INITIAL_STATE != 0 {
            let header = match read_block_header(backing)? {
                Some(h) => h,
                None => return Err(invalid("mVL: truncated initial-state block")),
            };
            if header.block_type != BLOCK_INITIAL_STATE || header.length == 0 {
                return Err(invalid("mVL: bad initial-state block"));
            }
            let mut blob = vec![0u8; header.length as usize];
            if backing.read_exact(&mut blob).is_err() {
                return Err(invalid("mVL: truncated initial state"));
            }
            self.initial_state = if header.flags & FLAG_BLOCK_COMPRESSED != 0 {
                match zlib_decompress(&blob) {
                    Some(state) => state,
                    None => return Err(invalid("mVL: corrupt initial state")),
                }
            } else {
                blob
            };
        }
        Ok(n_channels)
    }

    /// mVideoLogContextLoad.
    pub fn load(&mut self, f: F) -> io::Result<()> {
        self.backing = Some(f);
        let n_channels = self.read_header()?;
        let pointer = self.backing.as_mut().unwrap().stream_position()?;
        self.channels = (0..n_channels)
            .map(|_| {
                let mut ch = VideoLogChannel::new();
                ch.current_pointer = pointer;
                ch
            })
            .collect();
        Ok(())
    }

    /// mVideoLogContextRewind, minus the core (the C also reloads the initial
    /// state into the core; the caller does that with `initial_state` here).
    /// Re-reads the header and resets every channel to the start of its
    /// stream.
    pub fn rewind(&mut self) -> io::Result<()> {
        self.read_header()?;
        let pointer = self.backing.as_mut().unwrap().stream_position()?;
        for ch in &mut self.channels {
            ch.buffer.clear();
            ch.injected_buffer.clear();
            ch.buffer_remaining = 0;
            ch.block_compressed = false;
            ch.current_pointer = pointer;
        }
        Ok(())
    }

    /// mVideoLogContextDestroy's on-disk half: flush the pending buffer and
    /// write the footer (write mode only). Idempotent; splitting the flush
    /// from Drop lets errors surface at the call site.
    pub fn close(&mut self) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        if self.write && self.backing.is_some() {
            self.flush_buffer()?;
            let backing = self.backing.as_mut().unwrap();
            backing.write_all(
                &BlockHeader {
                    block_type: BLOCK_FOOTER,
                    length: 0,
                    channel_id: 0,
                    flags: 0,
                }
                .encode(),
            )?;
        }
        Ok(())
    }

    /// _flushBuffer (and _flushBufferCompressed): emit the active channel's
    /// buffered bytes as one data block. Like the C, write errors from the
    /// logger path are swallowed by the caller.
    fn flush_buffer(&mut self) -> io::Result<()> {
        let active = self.active_channel;
        if self.channels.get(active).map_or(true, |c| c.buffer.is_empty()) {
            return Ok(());
        }
        if self.backing.is_none() {
            return Ok(());
        }
        if self.compression {
            let mut enc = ZlibEncoder::new(Vec::new(), Compression::new(9));
            io::copy(&mut self.channels[active].buffer, &mut enc)?;
            let blob = enc.finish()?;
            let backing = self.backing.as_mut().unwrap();
            backing.write_all(
                &BlockHeader {
                    block_type: BLOCK_DATA,
                    length: blob.len() as u32,
                    channel_id: active as u32,
                    flags: FLAG_BLOCK_COMPRESSED,
                }
                .encode(),
            )?;
            backing.write_all(&blob)?;
        } else {
            let length = self.channels[active].buffer.len() as u32;
            let backing = self.backing.as_mut().unwrap();
            backing.write_all(
                &BlockHeader {
                    block_type: BLOCK_DATA,
                    length,
                    channel_id: active as u32,
                    flags: 0,
                }
                .encode(),
            )?;
            io::copy(&mut self.channels[active].buffer, backing)?;
        }
        Ok(())
    }

    /// mVideoLoggerWriteChannel.
    fn write_channel(&mut self, channel_id: usize, data: &[u8]) -> usize {
        if channel_id >= self.channels.len() {
            return 0;
        }
        if channel_id != self.active_channel {
            let _ = self.flush_buffer();
            self.active_channel = channel_id;
        }
        let injecting = self.channels[channel_id].injecting;
        if !injecting
            && !self.channels[channel_id].buffer.is_empty()
            && self.channels[channel_id].buffer.len() + data.len() > BUFFER_BASE_SIZE
        {
            let _ = self.flush_buffer();
        }
        {
            let buffer = if injecting {
                &mut self.channels[channel_id].injected_buffer
            } else {
                &mut self.channels[channel_id].buffer
            };
            buffer.extend(data.iter().copied());
        }
        if !injecting && self.channels[channel_id].buffer.len() >= BUFFER_BASE_SIZE {
            let _ = self.flush_buffer();
        }
        data.len()
    }

    /// mVideoLoggerReadChannel.
    fn read_channel(&mut self, channel_id: usize, out: &mut [u8]) -> usize {
        if channel_id >= self.channels.len() {
            return 0;
        }
        let injecting = self.channels[channel_id].injecting;
        let mut n = {
            let buffer = if injecting {
                &mut self.channels[channel_id].injected_buffer
            } else {
                &mut self.channels[channel_id].buffer
            };
            buffer.read(out).unwrap_or(0)
        };
        if n == out.len() || injecting {
            return n;
        }
        // Keep filling until the request is satisfied or the stream stops
        // producing bytes (footer / truncation). The C fills once and relies
        // on replay reads never exceeding its circle-buffer capacity
        // (0x20000); a single logged buffer can be larger than that, so we
        // loop instead.
        while n < out.len() {
            match self.fill_buffer(channel_id, BUFFER_BASE_SIZE) {
                Ok(true) => {
                    let m = self.channels[channel_id]
                        .buffer
                        .read(&mut out[n..])
                        .unwrap_or(0);
                    if m == 0 {
                        break;
                    }
                    n += m;
                }
                _ => break,
            }
        }
        n
    }

    /// _fillBuffer: scan forward from the channel's file position, skipping
    /// other channels' blocks, decoding data into the channel buffer until
    /// `length` bytes are wanted no more, the footer is hit, or the stream
    /// breaks.
    fn fill_buffer(&mut self, channel_id: usize, mut length: usize) -> io::Result<bool> {
        {
            let backing = match self.backing.as_mut() {
                Some(b) => b,
                None => return Ok(false),
            };
            backing.seek(SeekFrom::Start(self.channels[channel_id].current_pointer))?;
        }
        while length > 0 {
            let remaining = self.channels[channel_id].buffer_remaining;
            if remaining > 0 {
                if self.channels[channel_id].block_compressed {
                    // Divergence from C: inflate the block whole instead of
                    // streaming a live z_stream into a circle buffer. Exactly
                    // `remaining` compressed bytes are consumed, and the
                    // writer sized the block so that this is exactly the end
                    // of the zlib stream, so file offsets line up.
                    let blob = {
                        let backing = self.backing.as_mut().unwrap();
                        let mut blob = vec![0u8; remaining as usize];
                        match backing.read_exact(&mut blob) {
                            Ok(()) => blob,
                            Err(_) => return Ok(false),
                        }
                    };
                    let out = match zlib_decompress(&blob) {
                        Some(out) => out,
                        None => return Ok(false),
                    };
                    let produced = out.len();
                    let ch = &mut self.channels[channel_id];
                    ch.buffer.extend(out);
                    ch.current_pointer += remaining;
                    ch.buffer_remaining = 0;
                    ch.block_compressed = false;
                    length = length.saturating_sub(produced);
                    continue;
                }
                let want = remaining.min(length as u64);
                let copied = {
                    let backing = self.backing.as_mut().unwrap();
                    io::copy(
                        &mut (&mut *backing).take(want),
                        &mut self.channels[channel_id].buffer,
                    )?
                };
                self.channels[channel_id].current_pointer += copied;
                self.channels[channel_id].buffer_remaining -= copied;
                length -= copied as usize;
                if copied < want {
                    return Ok(false);
                }
                continue;
            }
            let header = {
                let backing = self.backing.as_mut().unwrap();
                match read_block_header(backing)? {
                    Some(h) => h,
                    None => return Ok(false),
                }
            };
            if header.block_type == BLOCK_FOOTER {
                return Ok(true);
            }
            if header.channel_id as usize != channel_id || header.block_type != BLOCK_DATA {
                let backing = self.backing.as_mut().unwrap();
                backing.seek(SeekFrom::Current(header.length as i64))?;
                continue;
            }
            {
                let backing = self.backing.as_mut().unwrap();
                self.channels[channel_id].current_pointer = backing.stream_position()?;
            }
            if header.length == 0 {
                continue;
            }
            self.channels[channel_id].buffer_remaining = header.length as u64;
            self.channels[channel_id].block_compressed =
                header.flags & FLAG_BLOCK_COMPRESSED != 0;
        }
        Ok(true)
    }
}

impl<F: VFile> Drop for VideoLogContext<F> {
    fn drop(&mut self) {
        // Best-effort footer/flush (mVideoLogContextDestroy); io errors
        // cannot be reported from Drop.
        let _ = self.close();
    }
}

/// C: mVideoLogger.vramBlock callback — implemented by the shimming proxy
/// renderer (or a test) to supply the raw bytes backing one logged
/// 0x1000-byte VRAM block. `out` is always 0x1000 bytes.
pub trait VramBlockSource {
    fn vram_block(&mut self, address: u32, out: &mut [u8]);
}

/// C: mVideoLogger.parsePacket callback — implemented by the replay-side
/// proxy renderer (or a test) to consume replayed packets. Return false to
/// pause the run loop the way the C does on DIRTY_FLUSH (`run` then returns
/// true so the caller can resume later).
pub trait PacketSink<F: VFile> {
    fn parse_packet(&mut self, logger: &mut VideoLogger<F>, packet: &DirtyInfo) -> bool;
}

/// struct mVideoLogger: the endpoint the (proxy) renderer talks to. On the
/// record path (`readonly == false`) its emitters append packets to the
/// context's active channel; on the replay path (`readonly == true`, the C's
/// "readonly" logger with `writeData == _writeNull`) `run` pulls packets out
/// and dispatches them to a PacketSink, and the inject_* methods queue
/// frontend packets through the injected buffer.
pub struct VideoLogger<F: VFile> {
    context: Rc<RefCell<VideoLogContext<F>>>,
    channel_id: usize,
    /// C mVideoLogger.block (`readonly` in mVideoLoggerRendererCreate).
    pub block: bool,
    /// C mVideoLogger.waitOnFlush.
    pub wait_on_flush: bool,
    readonly: bool,
    vram_size: usize,
    oam_size: usize,
    vram_dirty_bitmap: Vec<u32>,
    oam_dirty_bitmap: Vec<u32>,
}

impl<F: VFile> VideoLogger<F> {
    /// mVideoLoggerRendererCreate + mVideoLoggerAttachChannel. `readonly`
    /// picks the replay role (writeData becomes the C's _writeNull, `block`
    /// true, `wait_on_flush` false); false picks the record role.
    pub fn attach(
        context: Rc<RefCell<VideoLogContext<F>>>,
        channel_id: usize,
        readonly: bool,
    ) -> Option<VideoLogger<F>> {
        if channel_id >= context.borrow().channels.len() {
            return None;
        }
        Some(VideoLogger {
            context,
            channel_id,
            block: readonly,
            wait_on_flush: !readonly,
            readonly,
            vram_size: 0,
            oam_size: 0,
            vram_dirty_bitmap: Vec::new(),
            oam_dirty_bitmap: Vec::new(),
        })
    }

    /// The C proxy renderers set logger->vramSize/oamSize/paletteSize
    /// directly; palette is not needed here (no shadow memory, see the file
    /// header) and is accepted only for call-site parity.
    pub fn set_sizes(&mut self, vram_size: usize, oam_size: usize, _palette_size: usize) {
        self.vram_size = vram_size;
        self.oam_size = oam_size;
    }

    /// mVideoLoggerRendererInit: allocate the dirty bitmaps.
    pub fn renderer_init(&mut self) {
        self.vram_dirty_bitmap = vec![0; round_up(self.vram_size, 17)];
        self.oam_dirty_bitmap = vec![0; round_up(self.oam_size, 6)];
    }

    /// mVideoLoggerRendererDeinit.
    pub fn renderer_deinit(&mut self) {
        self.vram_dirty_bitmap = Vec::new();
        self.oam_dirty_bitmap = Vec::new();
    }

    /// mVideoLoggerRendererReset.
    pub fn renderer_reset(&mut self) {
        for word in &mut self.vram_dirty_bitmap {
            *word = 0;
        }
        for word in &mut self.oam_dirty_bitmap {
            *word = 0;
        }
    }

    /// _writeData/_writeNull: append to the channel stream. A readonly
    /// (replay) logger only writes while injecting (into the injected
    /// buffer).
    fn write_data(&mut self, data: &[u8]) -> bool {
        let mut ctx = self.context.borrow_mut();
        if self.readonly && !ctx.channels[self.channel_id].injecting {
            return false;
        }
        ctx.write_channel(self.channel_id, data) == data.len()
    }

    /// _readData: pull bytes from the channel stream; sinks use it to fetch
    /// packet payloads (VRAM blocks, DIRTY_BUFFER data).
    pub fn read_data(&mut self, out: &mut [u8]) -> bool {
        self.context.borrow_mut().read_channel(self.channel_id, out) == out.len()
    }

    /// mVideoLoggerRendererWriteVideoRegister.
    pub fn write_video_register(&mut self, address: u32, value: u16) {
        let dirty = DirtyInfo::new(DirtyType::Register, address, value as u32, 0xDEADBEEF);
        self.write_data(&dirty.encode());
    }

    /// mVideoLoggerRendererWriteVRAM: just marks the 0x1000-byte block dirty;
    /// the data is dumped on the next scanline/range packet.
    pub fn write_vram(&mut self, address: u32) {
        let word = (address >> 17) as usize;
        if word >= self.vram_dirty_bitmap.len() {
            return;
        }
        let bit = 1u32 << ((address >> 12) & 31);
        if self.vram_dirty_bitmap[word] & bit != 0 {
            return;
        }
        self.vram_dirty_bitmap[word] |= bit;
    }

    /// mVideoLoggerRendererWritePalette.
    pub fn write_palette(&mut self, address: u32, value: u16) {
        let dirty = DirtyInfo::new(DirtyType::Palette, address, value as u32, 0xDEADBEEF);
        self.write_data(&dirty.encode());
    }

    /// mVideoLoggerRendererWriteOAM.
    pub fn write_oam(&mut self, address: u32, value: u16) {
        let dirty = DirtyInfo::new(DirtyType::Oam, address, value as u32, 0xDEADBEEF);
        self.write_data(&dirty.encode());
    }

    /// _flushVRAM: dump every dirty 0x1000-byte VRAM block (packet + raw
    /// bytes sourced from the renderer), in ascending block order.
    fn flush_vram(&mut self, vram: &mut dyn VramBlockSource) {
        for i in 0..self.vram_dirty_bitmap.len() {
            let bitmap = self.vram_dirty_bitmap[i];
            if bitmap == 0 {
                continue;
            }
            self.vram_dirty_bitmap[i] = 0;
            for j in 0..MVL_MAX_CHANNELS {
                if bitmap & (1 << j) == 0 {
                    continue;
                }
                let address = (j * 0x1000) as u32;
                let dirty = DirtyInfo::new(DirtyType::Vram, address, 0x1000, 0xDEADBEEF);
                self.write_data(&dirty.encode());
                let mut block = [0u8; 0x1000];
                vram.vram_block(address, &mut block);
                self.write_data(&block);
            }
        }
    }

    /// mVideoLoggerRendererDrawScanline.
    pub fn draw_scanline(&mut self, vram: &mut dyn VramBlockSource, y: i32) {
        self.flush_vram(vram);
        let dirty = DirtyInfo::new(DirtyType::Scanline, y as u32, 0, 0xDEADBEEF);
        self.write_data(&dirty.encode());
    }

    /// mVideoLoggerRendererDrawRange.
    pub fn draw_range(&mut self, vram: &mut dyn VramBlockSource, start_x: i32, end_x: i32, y: i32) {
        self.flush_vram(vram);
        let dirty = DirtyInfo::new(DirtyType::Range, y as u32, start_x as u32, end_x as u32);
        self.write_data(&dirty.encode());
    }

    /// mVideoLoggerRendererFlush. The C's waitOnFlush thread-sync hook is a
    /// frontend concern and is not ported.
    pub fn flush(&mut self) {
        let dirty = DirtyInfo::new(DirtyType::Flush, 0, 0, 0xDEADBEEF);
        self.write_data(&dirty.encode());
    }

    /// mVideoLoggerRendererFinishFrame: the frame boundary packet.
    pub fn finish_frame(&mut self) {
        let dirty = DirtyInfo::new(DirtyType::Frame, 0, 0, 0xDEADBEEF);
        self.write_data(&dirty.encode());
    }

    /// mVideoLoggerWriteBuffer (GB SGB packets and friends): header packet
    /// followed by the raw payload.
    pub fn write_buffer(&mut self, buffer_id: u32, offset: u32, data: &[u8]) {
        let dirty = DirtyInfo::new(DirtyType::Buffer, buffer_id, offset, data.len() as u32);
        self.write_data(&dirty.encode());
        self.write_data(data);
    }

    /// mVideoLoggerRendererRun: consume packets from the channel and
    /// dispatch to `sink`. Returns true when processing paused (sink stopped
    /// it, e.g. on DIRTY_FLUSH) and the stream can be resumed; false when the
    /// stream ended (or broke) with `block` set. Injected packets run first
    /// according to the channel's injection point.
    pub fn run<S: PacketSink<F>>(&mut self, sink: &mut S, block: bool) -> bool {
        let mut ignore_packets = 0u32;
        let needs_injection = {
            let ctx = self.context.borrow();
            let ch = &ctx.channels[self.channel_id];
            ch.injection_point == InjectionPoint::Immediate && !ch.injecting
        };
        if needs_injection {
            self.run_injected(sink);
            ignore_packets = self.context.borrow().channels[self.channel_id].ignore_packets;
        }
        loop {
            let mut buf = [0u8; DirtyInfo::SIZE];
            if !self.read_data(&mut buf) {
                break;
            }
            let item = match DirtyInfo::decode(&buf) {
                Some(item) => item,
                None => return false,
            };
            if ignore_packets & (1 << item.kind as u32) != 0 {
                continue;
            }
            match item.kind {
                DirtyType::Scanline => {
                    let first_scanline = {
                        let ctx = self.context.borrow();
                        let ch = &ctx.channels[self.channel_id];
                        ch.injection_point == InjectionPoint::FirstScanline
                            && !ch.injecting
                            && item.address == 0
                    };
                    if first_scanline {
                        self.run_injected(sink);
                        ignore_packets =
                            self.context.borrow().channels[self.channel_id].ignore_packets;
                    }
                    // Fall through, like the C switch.
                    if !sink.parse_packet(self, &item) {
                        return true;
                    }
                }
                DirtyType::Register
                | DirtyType::Palette
                | DirtyType::Oam
                | DirtyType::Vram
                | DirtyType::Flush
                | DirtyType::Frame
                | DirtyType::Range
                | DirtyType::Buffer => {
                    if !sink.parse_packet(self, &item) {
                        return true;
                    }
                }
                DirtyType::Dummy => return false,
            }
        }
        !block
    }

    /// mVideoLoggerRendererRunInjected.
    pub fn run_injected<S: PacketSink<F>>(&mut self, sink: &mut S) -> bool {
        self.context.borrow_mut().channels[self.channel_id].injecting = true;
        let res = self.run(sink, false);
        self.context.borrow_mut().channels[self.channel_id].injecting = false;
        res
    }

    /// mVideoLoggerInjectionPoint.
    pub fn set_injection_point(&mut self, point: InjectionPoint) {
        self.context.borrow_mut().channels[self.channel_id].injection_point = point;
    }

    /// mVideoLoggerIgnoreAfterInjection.
    pub fn ignore_after_injection(&mut self, mask: u32) {
        self.context.borrow_mut().channels[self.channel_id].ignore_packets = mask;
    }

    /// mVideoLoggerInjectVideoRegister.
    pub fn inject_video_register(&mut self, address: u32, value: u16) {
        self.context.borrow_mut().channels[self.channel_id].injecting = true;
        self.write_video_register(address, value);
        self.context.borrow_mut().channels[self.channel_id].injecting = false;
    }

    /// mVideoLoggerInjectPalette.
    pub fn inject_palette(&mut self, address: u32, value: u16) {
        self.context.borrow_mut().channels[self.channel_id].injecting = true;
        self.write_palette(address, value);
        self.context.borrow_mut().channels[self.channel_id].injecting = false;
    }

    /// mVideoLoggerInjectOAM.
    pub fn inject_oam(&mut self, address: u32, value: u16) {
        self.context.borrow_mut().channels[self.channel_id].injecting = true;
        self.write_oam(address, value);
        self.context.borrow_mut().channels[self.channel_id].injecting = false;
    }
}

/// The dispatch half of mVideoLogCoreFind: which platform's video-log player
/// core (C: GBVideoLogPlayerCreate/GBAVideoLogPlayerCreate) can replay a
/// file. Instantiating the core itself stays in the console crates
/// (integration pending — see the file header).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VideoLogPlayer {
    Gb,
    Gba,
}

/// _mVideoLogDescriptor: sniff the header and map the platform u32, leaving
/// the stream positioned after the header like the C.
fn sniff_platform<F: Read + Seek>(vf: &mut F) -> Option<Platform> {
    vf.seek(SeekFrom::Start(0)).ok()?;
    let mut hdr = [0u8; 16];
    vf.read_exact(&mut hdr).ok()?;
    if hdr[0..4] != MVL_MAGIC {
        return None;
    }
    match u32::from_le_bytes(hdr[8..12].try_into().unwrap()) {
        x if x == Platform::Gba as u32 => Some(Platform::Gba),
        x if x == Platform::Gb as u32 => Some(Platform::Gb),
        _ => None,
    }
}

/// mVideoLogIsCompatible.
pub fn video_log_is_compatible<F: Read + Seek>(vf: &mut F) -> Platform {
    sniff_platform(vf).unwrap_or(Platform::None)
}

/// The descriptor-lookup half of mVideoLogCoreFind.
pub fn video_log_player_find<F: Read + Seek>(vf: &mut F) -> Option<VideoLogPlayer> {
    match sniff_platform(vf)? {
        Platform::Gba => Some(VideoLogPlayer::Gba),
        Platform::Gb => Some(VideoLogPlayer::Gb),
        Platform::None => None,
    }
}
