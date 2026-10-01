// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/include/mgba/core/core.h — the `mCore` vtable becomes the
// `Core` trait. Not every vtable entry has a Rust equivalent (config system,
// VFS, input map, GL textures); the Rust frontend drives cores directly and
// needs far less surface area.

use crate::ring::RingI16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Platform {
    None = -1,
    Gba = 0,
    Gb = 1,
}

/// Buttons, in mGBA's core key order (`struct GBAKey`/GB keys share this map
/// in the frontends).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum Key {
    A = 0,
    B = 1,
    Select = 2,
    Start = 3,
    Right = 4,
    Left = 5,
    Up = 6,
    Down = 7,
    R = 8,
    L = 9,
}

pub const M_CORE_KEY_MAP: [Key; 10] = [
    Key::A,
    Key::B,
    Key::Select,
    Key::Start,
    Key::Right,
    Key::Left,
    Key::Up,
    Key::Down,
    Key::R,
    Key::L,
];

/// The one-time, up-front info a frontend needs before allocating buffers.
#[derive(Clone, Copy, Debug)]
pub struct CoreInfo {
    pub platform: Platform,
    pub width: u32,
    pub height: u32,
    /// Master clock, Hz (GB: 4194304, GBA: 16777216).
    pub frequency: i32,
    /// Cycles per video frame.
    pub frame_cycles: i32,
}

/// Result of running one frame.
pub struct FrameOutput {
    /// 0xFFRRGGBB pixels, width*height.
    pub video_dirty: bool,
}

/// Rust equivalent of `struct mCore`'s vtable: the uniform interface the
/// frontend drives.
pub trait Core {
    fn platform(&self) -> Platform;
    fn info(&self) -> CoreInfo;

    /// Detect whether `rom` looks like this core's ROM format.
    fn is_rom(rom: &[u8]) -> bool
    where
        Self: Sized;
    /// A core must be constructed with the ROM image already in place; use the
    /// per-crate `load_rom` constructor instead of a mid-life load.
    fn reset(&mut self);
    fn unload_rom(&mut self);

    /// Run until the end of the next video frame (or one "iteration" for
    /// cores without video). Fills the internal audio buffer and video
    /// framebuffer.
    fn run_frame(&mut self);
    /// Run one instruction (debugger step).
    fn step(&mut self);

    fn set_keys(&mut self, keys: u32);
    fn add_keys(&mut self, keys: u32) {
        self.set_keys(self.keys() | keys);
    }
    fn clear_keys(&mut self, keys: u32) {
        self.set_keys(self.keys() & !keys);
    }
    fn keys(&self) -> u32;

    /// 0xFFRRGGBB framebuffer, width*height entries.
    fn video_buffer(&self) -> &[u32];
    fn base_video_size(&self) -> (u32, u32);

    /// Native-rate interleaved stereo i16 produced so far this frame.
    fn audio_buffer(&mut self) -> &mut RingI16;
    /// Native sample rate the core emits, in Hz (an integer that divides the
    /// master clock).
    fn audio_sample_rate(&self) -> i32;
    fn set_audio_buffer_size(&mut self, samples: usize);

    fn frame_counter(&self) -> u32;
    fn frame_cycles(&self) -> i32;
    fn frequency(&self) -> i32;

    /// Save-state support.
    fn state_size(&mut self) -> usize;
    fn save_state(&mut self, out: &mut Vec<u8>) -> Result<(), &'static str>;
    fn load_state(&mut self, state: &[u8]) -> Result<(), &'static str>;

    /// Battery save (SRAM/Flash/EEPROM) accessors.
    fn savedata(&self) -> Option<&[u8]>;
    fn savedata_mut(&mut self) -> Option<&mut [u8]>;
    /// Called once per frame so carts with RTC can advance the clock.
    fn synchronize_savedata(&mut self) {}

    // --- Debugger (mGBA: mCore.debugger / attachDebugger) ---
    /// Attach the debugger to this core. Returns false if unsupported.
    fn debugger_attach(&mut self) -> bool {
        false
    }
    /// One frame under debugger control (mDebuggerRunFrame).
    fn debugger_run_frame(&mut self) {
        self.run_frame();
    }
    /// Break into the debugger (mDebuggerEnter with MANUAL reason).
    fn debugger_break(&mut self) {}
    /// Whether the debugger is currently attached.
    fn debugger_attached(&self) -> bool {
        false
    }
}
