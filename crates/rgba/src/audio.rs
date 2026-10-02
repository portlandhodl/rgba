// Audio output. Mirrors mGBA's SDL audio path (src/platform/sdl/sdl-audio.c):
// the core's native-rate stereo ring is resampled with the sinc
// mAudioResampler into a staging ring and handed to the host output device.
// Here cpal (Rust-native) is used instead of SDL, with cpal's pull callback
// draining a shared ring — the frontend still owns the latency bound by
// controlling how much it pushes.

use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rgba_core::audio_resampler::{AudioResampler, InterpolatorType};
use rgba_core::ring::RingI16;

/// Target queue depth, in seconds of output (~50 ms): enough to ride out a
/// UI frame that runs zero emulated frames.
const TARGET_SECS: f64 = 0.05;
/// Hard ceiling (~170 ms). Only reached while fast-forwarding, where the
/// excess is discarded (RingI16 drops on full) like mGBA does.
const MAX_SECS: f64 = 0.17;
/// Maximum rate correction (±0.5%): inaudible as pitch, but enough to absorb
/// the drift between the frame timer and the sound card's clock.
const MAX_RATE_ADJUST: f64 = 0.005;

pub struct Audio {
    /// Kept alive for playback; `None` when no output device is available.
    _stream: Option<cpal::Stream>,
    /// Resampled stereo samples shared with the cpal callback.
    queue: Arc<Mutex<RingI16>>,
    resampler: AudioResampler,
    resampled: RingI16,
    scratch: Vec<i16>,
    dst_rate: f64,
}

impl Audio {
    pub fn new(sample_rate: i32) -> Result<Audio, String> {
        let host = cpal::default_host();
        let mut dst_rate = sample_rate.max(1) as f64;
        let mut stream = None;
        let mut queue = None;
        if let Some(device) = host.default_output_device() {
            match pick_config(&device, dst_rate as u32) {
                Some(config) => {
                    dst_rate = config.sample_rate().0 as f64;
                    // Ring sized to the latency ceiling: pushes past it drop,
                    // matching mGBA's WriteTruncate on a full audio buffer.
                    let cap = ((dst_rate * MAX_SECS) as usize * 2).max(4096);
                    let q = Arc::new(Mutex::new(RingI16::new(cap)));
                    stream = Some(build_stream(&device, &config, q.clone())?);
                    queue = Some(q);
                }
                None => eprintln!("audio: no supported output config"),
            }
        } else {
            eprintln!("audio: no output device");
        }
        // No device/config: run silent (pump still drains the core ring).
        Ok(Audio {
            _stream: stream,
            queue: queue.unwrap_or_else(|| Arc::new(Mutex::new(RingI16::new(4096)))),
            resampler: AudioResampler::new(InterpolatorType::Sinc),
            // mAudioBufferInit(&context->buffer, context->samples, 2)
            resampled: RingI16::new(2048 * 2),
            scratch: Vec::new(),
            dst_rate,
        })
    }

    /// Drain the core's audio ring: resample to the device rate, apply
    /// volume, and push it into the shared queue (or discard it when
    /// muted / over the latency bound). Must be called every emulated frame
    /// so the core ring never overflows.
    pub fn pump(&mut self, src: &mut RingI16, src_rate: i32, volume: f32, muted: bool) {
        // Dynamic rate control: resample to slightly more output samples when
        // the queue is running low and slightly fewer when it is filling up,
        // so the queue hovers around the target without ever running dry (the
        // source of audible gaps/clicks — an underrun outputs silence).
        let target = self.dst_rate * TARGET_SECS;
        let queued = self.queue.lock().unwrap().len() as f64 / 2.0;
        let error = ((target - queued) / target).clamp(-1.0, 1.0);
        let dst_rate = self.dst_rate * (1.0 + error * MAX_RATE_ADJUST);
        loop {
            self.resampler.process(
                src,
                src_rate.max(1) as f64,
                true,
                &mut self.resampled,
                dst_rate,
            );
            let avail = self.resampled.len();
            if avail == 0 {
                break;
            }
            self.scratch.resize(avail, 0);
            let got = self.resampled.read_into(&mut self.scratch);
            if !muted && self._stream.is_some() {
                if volume < 0.999 {
                    for s in &mut self.scratch[..got] {
                        *s = (*s as f32 * volume) as i16;
                    }
                }
                let mut q = self.queue.lock().unwrap();
                for &s in &self.scratch[..got] {
                    q.push(s);
                }
            }
            if src.len() == 0 {
                break;
            }
        }
    }

    /// Drop anything queued (pause, reset, state load).
    pub fn clear(&mut self) {
        self.queue.lock().unwrap().clear();
        self.resampled.clear();
    }
}

/// Prefer a 2-channel config at the requested rate, i16 over f32; fall back
/// to the device's default config.
fn pick_config(device: &cpal::Device, desired_rate: u32) -> Option<cpal::SupportedStreamConfig> {
    let rank = |f: cpal::SampleFormat| match f {
        cpal::SampleFormat::I16 => 0,
        cpal::SampleFormat::F32 => 1,
        _ => 2,
    };
    let mut best: Option<cpal::SupportedStreamConfig> = None;
    for range in device
        .supported_output_configs()
        .ok()?
        .filter(|r| r.channels() == 2)
    {
        let rate = desired_rate.clamp(range.min_sample_rate().0, range.max_sample_rate().0);
        let cand = range.with_sample_rate(cpal::SampleRate(rate));
        let better = match &best {
            None => true,
            Some(b) => {
                (rank(cand.sample_format()), -(rate as i64 - desired_rate as i64).abs())
                    < (rank(b.sample_format()), -(b.sample_rate().0 as i64 - desired_rate as i64).abs())
            }
        };
        if better {
            best = Some(cand);
        }
    }
    best.or_else(|| device.default_output_config().ok())
}

fn build_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    queue: Arc<Mutex<RingI16>>,
) -> Result<cpal::Stream, String> {
    let err = |e| eprintln!("audio stream error: {e}");
    let cfg = config.config();
    let stream = match config.sample_format() {
        cpal::SampleFormat::I16 => device.build_output_stream(
            &cfg,
            move |out: &mut [i16], _| {
                let mut q = queue.lock().unwrap();
                for s in out.iter_mut() {
                    *s = q.pop().unwrap_or(0);
                }
            },
            err,
            None,
        ),
        cpal::SampleFormat::F32 => device.build_output_stream(
            &cfg,
            move |out: &mut [f32], _| {
                let mut q = queue.lock().unwrap();
                for s in out.iter_mut() {
                    *s = q.pop().unwrap_or(0) as f32 / 32768.0;
                }
            },
            err,
            None,
        ),
        f => return Err(format!("unsupported sample format: {f:?}")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}
