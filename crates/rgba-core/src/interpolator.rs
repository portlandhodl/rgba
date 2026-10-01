// Copyright (c) 2013-2024 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/util/interpolator.c and include/mgba-util/interpolator.h.
//
// Interpolators for the audio resampler: windowed-sinc (default, used by the
// SDL frontend) and cosine. The C vtable (`struct mInterpolator` with an
// `interpolate` function pointer) becomes the `Interpolator` enum; the
// `mInterpolationData` callback becomes `InterpolationData<'a>` holding a
// closure. Quirks of the C are preserved bit-for-bit (see notes at each site).

use std::f64::consts::PI;

const SINC_RESOLUTION: u32 = 8192;
const SINC_WIDTH: u32 = 8;

const COSINE_RESOLUTION: u32 = 8192;

/// enum mInterpolatorType
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InterpolatorType {
    Sinc,
    Cosine,
}

/// struct mInterpolationData: a sample-fetch callback. `at(index)` must return
/// the sample held `index` frames into the source buffer (0 for out of range,
/// as mAudioBufferPeek does); negative indices must yield 0 (_sampleAt).
pub struct InterpolationData<'a> {
    pub at: &'a mut dyn FnMut(i32) -> i16,
}

/// struct mInterpolator vtable → enum dispatch (same pattern as SioDriver).
pub enum Interpolator {
    Sinc(InterpolatorSinc),
    Cosine(InterpolatorCosine),
}

impl Interpolator {
    /// struct mInterpolator.interpolate dispatch.
    pub fn interpolate(&self, data: &mut InterpolationData, time: f64, sample_step: f64) -> i16 {
        match self {
            Interpolator::Sinc(s) => s.interpolate(data, time, sample_step),
            Interpolator::Cosine(c) => c.interpolate(data, time, sample_step),
        }
    }
}

/// windowedSinc(): sinc with a 4th-order flat-top window (coefficients as
/// used in Matlab).
fn windowed_sinc(width: f64, t: f64) -> f64 {
    // sinc has a removable singularity at x=0
    let sinc_val = if t == 0.0 { 1.0 } else { t.sin() / t };
    let y = t / width;
    let window_val = 0.21557895
        + 0.41663158 * y.cos()
        + 0.27723158 * (y * 2.0).cos()
        + 0.083578947 * (y * 3.0).cos()
        + 0.006947368 * (y * 4.0).cos();
    window_val * sinc_val
}

/// struct mInterpolatorSinc
pub struct InterpolatorSinc {
    resolution: u32,
    width: u32,
    sinc_lut: Vec<f64>,
}

impl InterpolatorSinc {
    /// mInterpolatorSincInit; 0 resolution/width selects the C defaults
    /// (mSINC_RESOLUTION/mSINC_WIDTH).
    pub fn new(resolution: u32, width: u32) -> Self {
        let resolution = if resolution == 0 { SINC_RESOLUTION } else { resolution };
        let width = if width == 0 { SINC_WIDTH } else { width };
        let samples = (resolution * width) as usize;
        let dx = PI / resolution as f64;
        let mut sinc_lut = vec![0.0; samples];
        for (i, slot) in sinc_lut.iter_mut().enumerate() {
            *slot = windowed_sinc(width as f64, i as f64 * dx);
        }
        InterpolatorSinc { resolution, width, sinc_lut }
    }

    /// Sinc kernel width in samples; the resampler uses it as both watermarks.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// fastWindowedSinc(): LUT lookup; x is in units of π.
    fn fast_windowed_sinc(&self, mut x: f64) -> f64 {
        // Both sinc and the flat-top window are symmetric
        if x < 0.0 {
            x = -x;
        }
        // sinc asymptotically approaches 0
        if x >= self.width as f64 {
            return 0.0;
        }
        let index = (x * self.resolution as f64) as usize;
        self.sinc_lut[index]
    }

    /// mInterpolatorSincInterpolate (16 taps around floor(time)).
    pub fn interpolate(&self, data: &mut InterpolationData, time: f64, _sample_step: f64) -> i16 {
        let x = time as u32; // C: unsigned x = time (truncation)
        let offset = (time - x as f64) / PI;

        let mut sum = 0.0f64;
        for i in -7i32..=8 {
            let weight = self.fast_windowed_sinc(i as f64 - offset);
            let sample = (data.at)(x as i32 + i);
            sum += sample as f64 * weight;
        }

        // C: implicit double→int16 truncation; Rust `as` also saturates, which
        // only differs for out-of-range sums (UB in C).
        sum as i16
    }
}

/// struct mInterpolatorCosine
pub struct InterpolatorCosine {
    resolution: u32,
    lut: Vec<f64>,
}

impl InterpolatorCosine {
    /// mInterpolatorCosineInit; 0 resolution selects the C default.
    pub fn new(resolution: u32) -> Self {
        let resolution = if resolution == 0 { COSINE_RESOLUTION } else { resolution };
        // C: calloc(resolution + 1) but fills only [0..resolution); the last
        // entry stays 0.
        let mut lut = vec![0.0; resolution as usize + 1];
        for i in 0..resolution as usize {
            // Quirk preserved from the C: the cosine is multiplied by π
            // (presumably meant to be part of the cos argument).
            lut[i] = (1.0 - (PI * i as f64 / resolution as f64).cos() * PI) * 0.5;
        }
        InterpolatorCosine { resolution, lut }
    }

    /// mInterpolatorCosineInterpolate.
    pub fn interpolate(&self, data: &mut InterpolationData, time: f64, _sample_step: f64) -> i16 {
        let left = (data.at)(time as i32);
        let right = (data.at)(time as i32 + 1);
        let weight = time - time.floor();
        let factor = self.lut[(weight * self.resolution as f64) as usize];
        (left as f64 * factor + right as f64 * (1.0 - factor)) as i16
    }
}
