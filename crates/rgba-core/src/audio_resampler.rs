// Copyright (c) 2013-2024 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/util/audio-resampler.c and
// include/mgba-util/audio-resampler.h.
//
// mAudioResampler: stream resampler that pulls interleaved i16 frames from a
// source audio buffer through an interpolator into a destination buffer,
// tracking a fractional source timestamp across calls. This is what mGBA's
// SDL frontend (src/platform/sdl/sdl-audio.c) runs in its audio callback,
// always with mINTERPOLATOR_SINC.
//
// Rust differences:
// - The C resampler stores raw source/destination mAudioBuffer pointers plus
//   their rates (mAudioResamplerSetSource/SetDestination); here source and
//   destination are `&mut RingI16` parameters of `process` (RingI16 is our
//   channels == 2 mAudioBuffer equivalent — cores hand out `&mut RingI16`,
//   so the resampler cannot hold references to them long-term). The rates
//   and the `consume` flag are per-call parameters for the same reason.
// - The C supports up to MAX_CHANNELS = 2 channels; RingI16 is stereo by
//   construction, so the channel loop is over MAX_CHANNELS = 2.

use crate::interpolator::{InterpolationData, Interpolator, InterpolatorCosine, InterpolatorSinc};
use crate::ring::RingI16;

// audio-resampler.h includes interpolator.h, so mInterpolatorType comes in
// with it; mirror that here.
pub use crate::interpolator::InterpolatorType;

/// MAX_CHANNELS
pub const MAX_CHANNELS: usize = 2;

/// struct mAudioResampler
pub struct AudioResampler {
    /// Fractional position (in source frames) of the next output sample.
    timestamp: f64,
    /// Source frames of history kept after consuming (interp kernel radius).
    low_water_mark: f64,
    /// Source frames of lookahead required before producing one output frame.
    high_water_mark: f64,
    interp: Interpolator,
}

impl AudioResampler {
    /// mAudioResamplerInit.
    pub fn new(interp_type: InterpolatorType) -> Self {
        match interp_type {
            InterpolatorType::Sinc => {
                let sinc = InterpolatorSinc::new(0, 0);
                AudioResampler {
                    timestamp: 0.0,
                    low_water_mark: sinc.width() as f64,
                    high_water_mark: sinc.width() as f64,
                    interp: Interpolator::Sinc(sinc),
                }
            }
            InterpolatorType::Cosine => AudioResampler {
                timestamp: 0.0,
                low_water_mark: 0.0,
                high_water_mark: 1.0,
                interp: Interpolator::Cosine(InterpolatorCosine::new(0)),
            },
        }
    }

    pub fn low_water_mark(&self) -> f64 {
        self.low_water_mark
    }

    pub fn high_water_mark(&self) -> f64 {
        self.high_water_mark
    }

    /// mAudioResamplerProcess: resample from `source` (at `source_rate` Hz)
    /// into `dest` (at `dest_rate` Hz). With `consume`, source frames older
    /// than the low water mark are dropped. Returns the number of frames
    /// written to `dest`.
    pub fn process(
        &mut self,
        source: &mut RingI16,
        source_rate: f64,
        consume: bool,
        dest: &mut RingI16,
        dest_rate: f64,
    ) -> usize {
        let mut sample_buffer = [0i16; MAX_CHANNELS];
        let timestep = source_rate / dest_rate;
        let mut timestamp = self.timestamp;

        let mut read = 0usize;
        loop {
            if timestamp + self.high_water_mark >= source.available_frames() as f64 {
                break;
            }
            if dest.full_frames() {
                break;
            }

            for (channel, slot) in sample_buffer.iter_mut().enumerate() {
                // _sampleAt/mAudioBufferPeek via the ring.
                let mut at = |index: i32| -> i16 {
                    if index < 0 {
                        return 0;
                    }
                    source.peek_frame(channel, index as usize)
                };
                let mut data = InterpolationData { at: &mut at };
                *slot = self.interp.interpolate(&mut data, timestamp, timestep);
            }
            // mAudioBufferWrite(..., 1): the full check above guarantees space
            // for one frame; write_stereo would silently drop otherwise, so
            // there is no second break path here.
            dest.write_stereo(sample_buffer[0], sample_buffer[1]);
            timestamp += timestep;
            read += 1;
        }

        if consume && timestamp > self.low_water_mark {
            // C: size_t drop = timestamp - lowWaterMark (truncation).
            let drop = (timestamp - self.low_water_mark) as usize;
            let dropped = source.drop_frames(drop);
            timestamp -= dropped as f64;
        }
        self.timestamp = timestamp;
        read
    }
}
