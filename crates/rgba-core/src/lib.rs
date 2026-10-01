//! rgba-core: shared infrastructure for the rgba emulator cores.
//!
//! This crate is a Rust port of the shared infrastructure of mGBA
//! (`src/core/timing.c`, parts of `src/util/*`, `src/core/serialize.c`...).
//! mGBA is Copyright (c) 2013-2026 Jeffrey Pfau and contributors, MPL-2.0.
//! This port is a derived work, also MPL-2.0.


pub mod audio_resampler;
pub mod cheats;
pub mod convolve;
pub mod core;
pub mod interpolator;
pub mod log;
pub mod mem_search;
pub mod patch;
pub mod patch_fast;
pub mod rewind;
pub mod ring;
pub mod serialize;
pub mod timing;
pub mod video_logger;

pub use core::{Core, Platform};
pub use timing::Timing;
pub use log::Level;
