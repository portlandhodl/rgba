// Resampler tests: sample-rate conversion through the sinc interpolator
// (what the SDL frontend uses): 32768→48000 (GBA core rate) and
// 131072→48000 (GB core rate), chunked into per-frame calls like the
// frontend does.

use rgba_core::audio_resampler::{AudioResampler, InterpolatorType};
use rgba_core::ring::RingI16;

/// Push 1 s of a 1 kHz sine at `src_rate`, resampled to 48000 in 60
/// frame-sized chunks. Returns (dest samples produced, source frames left).
fn resample_sine(src_rate: usize, amplitude: f64) -> (Vec<i16>, usize) {
    const DST_RATE: usize = 48000;
    let total_src = src_rate; // 1 s
    let mut source = RingI16::new(16384);
    let mut dest = RingI16::new(2048 * 2); // staging buffer, like sdl-audio.c
    let mut resampler = AudioResampler::new(InterpolatorType::Sinc);

    let mut out = Vec::new();
    let mut pushed = 0usize;
    for chunk in 0..60 {
        // chunk sizes alternate so they sum to exactly total_src
        let target = total_src * (chunk + 1) / 60;
        while pushed < target {
            let t = pushed as f64 / src_rate as f64;
            let v = (amplitude * (2.0 * std::f64::consts::PI * 1000.0 * t).sin()) as i16;
            source.write_stereo(v, v);
            pushed += 1;
        }
        resampler.process(&mut source, src_rate as f64, true, &mut dest, DST_RATE as f64);
        let avail = dest.len();
        let mut buf = vec![0i16; avail];
        let got = dest.read_into(&mut buf);
        out.extend_from_slice(&buf[..got]);
        // Consume keeps at most ~lowWater + highWater source frames around.
        assert!(source.available_frames() <= 18, "source not consumed: {}", source.available_frames());
    }
    (out, source.available_frames())
}

fn rms(samples: &[i16]) -> f64 {
    let sum: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum / samples.len() as f64).sqrt()
}

fn zero_crossings(samples: &[i16]) -> usize {
    let mut count = 0;
    let mut prev = 0i16;
    for &s in samples {
        if s == 0 {
            continue;
        }
        if prev != 0 && (s > 0) != (prev > 0) {
            count += 1;
        }
        prev = s;
    }
    count
}

fn check_upsampled_sine(src_rate: usize) {
    let (out, left) = resample_sine(src_rate, 10000.0);
    assert_eq!(out.len() % 2, 0);
    let frames = out.len() / 2;

    // ~1 s at 48000 Hz; a handful of frames can stay buffered
    // (sinc low/high watermarks), and a few source frames are left behind.
    assert!((frames as i64 - 48000).abs() <= 16, "frames = {frames}");
    assert!(left <= 18, "leftover source frames = {left}");

    // Amplitude preserved: RMS of a sine is A/√2 ≈ 7071 (skip the first
    // 64 frames of warmup while the sinc kernel fills).
    let body: Vec<i16> = out[64 * 2..].to_vec();
    let r = rms(&body);
    assert!((r - 7071.0).abs() / 7071.0 < 0.05, "rms = {r}");

    // Frequency preserved: 1 s of 1 kHz has 2000 zero crossings.
    let zc = zero_crossings(&body);
    let expected = 2 * 1000 * (body.len() / 2) / 48000;
    assert!((zc as i64 - expected as i64).abs() <= expected as i64 / 50, "zero crossings = {zc}, expected ~{expected}");
}

#[test]
fn sinc_32768_to_48000() {
    check_upsampled_sine(32768);
}

#[test]
fn sinc_131072_to_48000() {
    check_upsampled_sine(131072);
}

#[test]
fn sinc_dc_passthrough() {
    // A constant signal is its own Nyquist-limited version.
    let mut source = RingI16::new(4096);
    let mut dest = RingI16::new(4096);
    let mut r = AudioResampler::new(InterpolatorType::Sinc);
    for _ in 0..3000 {
        source.write_stereo(1234, 1234);
    }
    let n = r.process(&mut source, 32768.0, true, &mut dest, 48000.0);
    assert!(n > 0);
    let mut buf = vec![0i16; dest.len()];
    let got = dest.read_into(&mut buf);
    // Skip kernel warmup at both ends.
    for &s in &buf[16..got - 16] {
        assert!((s - 1234i16).abs() <= 2, "dc sample = {s}");
    }
}

#[test]
fn cosine_dc_passthrough() {
    let mut source = RingI16::new(4096);
    let mut dest = RingI16::new(4096);
    let mut r = AudioResampler::new(InterpolatorType::Cosine);
    for _ in 0..1000 {
        source.write_stereo(-2345, -2345);
    }
    let n = r.process(&mut source, 32768.0, true, &mut dest, 48000.0);
    assert!(n > 0);
    let mut buf = vec![0i16; dest.len()];
    let got = dest.read_into(&mut buf);
    // Identical neighbours would interpolate exactly in real math, but the
    // C's (float64) `left*f + right*(1-f)` plus double→int16 truncation can
    // land one ulp off; allow ±1 (matches C behavior bit-for-bit).
    for &s in &buf[4..got - 4] {
        assert!((s + 2345i16).abs() <= 1, "dc sample = {s}");
    }
}

#[test]
fn undersampled_source_is_untouched() {
    // Fewer available frames than the sinc high watermark -> no output, no
    // consumption (mAudioResamplerProcess's loop-exit condition).
    let mut source = RingI16::new(64);
    let mut dest = RingI16::new(64);
    let mut r = AudioResampler::new(InterpolatorType::Sinc);
    for i in 0..4 {
        source.write_stereo(i, i);
    }
    assert_eq!(r.process(&mut source, 32768.0, true, &mut dest, 48000.0), 0);
    assert_eq!(source.len(), 8);
    assert_eq!(dest.len(), 0);
}
