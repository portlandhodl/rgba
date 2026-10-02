// Audio output. Mirrors mGBA's SDL audio path (src/platform/sdl/sdl-audio.c):
// the core's native-rate stereo ring is resampled with the sinc
// mAudioResampler into a staging ring and handed to SDL. Here SDL is used for
// audio only (video/input go through egui), via a queued device instead of
// the C's pull callback, so the frontend owns the latency bound.

use rgba_core::audio_resampler::{AudioResampler, InterpolatorType};
use rgba_core::ring::RingI16;

/// Bytes per stereo i16 frame in the SDL queue.
const FRAME_BYTES: u32 = 4;
/// Target queue depth, in output frames (~50 ms at 48 kHz): enough to ride
/// out a UI frame that runs zero emulated frames.
const TARGET_FRAMES: f64 = 2400.0;
/// Hard ceiling (~170 ms). Only reached while fast-forwarding, where the
/// excess is discarded like mGBA does.
const MAX_QUEUED_BYTES: u32 = 8192 * FRAME_BYTES;
/// Maximum rate correction (±0.5%): inaudible as pitch, but enough to absorb
/// the drift between the frame timer and the sound card's clock.
const MAX_RATE_ADJUST: f64 = 0.005;

pub struct Audio {
    _sdl: sdl2::Sdl,
    device: Option<sdl2::audio::AudioQueue<i16>>,
    resampler: AudioResampler,
    resampled: RingI16,
    scratch: Vec<i16>,
    dst_rate: f64,
    /// Device is playing (vs. priming its queue after start/underrun).
    playing: bool,
}

impl Audio {
    pub fn new(sample_rate: i32) -> Result<Audio, String> {
        let sdl = sdl2::init()?;
        let device = sdl.audio().ok().and_then(|sub| {
            let spec = sdl2::audio::AudioSpecDesired {
                freq: Some(sample_rate),
                channels: Some(2),
                samples: Some(1024),
            };
            sub.open_queue::<i16, _>(None, &spec).ok()
        });
        let dst_rate = device
            .as_ref()
            .map(|d| d.spec().freq as f64)
            .unwrap_or(sample_rate as f64);
        Ok(Audio {
            _sdl: sdl,
            device,
            resampler: AudioResampler::new(InterpolatorType::Sinc),
            // mAudioBufferInit(&context->buffer, context->samples, 2)
            resampled: RingI16::new(2048 * 2),
            scratch: Vec::new(),
            dst_rate,
            playing: false,
        })
    }

    /// Drain the core's audio ring: resample to the device rate, apply
    /// volume, and queue it (or discard it when muted / over the latency
    /// bound). Must be called every emulated frame so the core ring never
    /// overflows.
    pub fn pump(&mut self, src: &mut RingI16, src_rate: i32, volume: f32, muted: bool) {
        // Dynamic rate control: resample to slightly more output samples when
        // the queue is running low and slightly fewer when it is filling up,
        // so the queue hovers around TARGET_FRAMES without ever being cleared
        // or running dry (the source of audible gaps/clicks).
        let queued = self.device.as_ref().map_or(0, |d| d.size() / FRAME_BYTES) as f64;
        let error = ((TARGET_FRAMES - queued) / TARGET_FRAMES).clamp(-1.0, 1.0);
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
            if let Some(dev) = &self.device {
                if !muted && dev.size() < MAX_QUEUED_BYTES {
                    if volume < 0.999 {
                        for s in &mut self.scratch[..got] {
                            *s = (*s as f32 * volume) as i16;
                        }
                    }
                    let _ = dev.queue_audio(&self.scratch[..got]);
                }
            }
            if src.len() == 0 {
                break;
            }
        }
        // Prime before playing: hold playback until ~3/4 of the target is
        // queued, and go back to priming if the queue ever runs dry, so an
        // underrun costs one short pause instead of a string of clicks.
        if let Some(dev) = &self.device {
            let queued = (dev.size() / FRAME_BYTES) as f64;
            if !self.playing && queued >= TARGET_FRAMES * 0.75 {
                dev.resume();
                self.playing = true;
            } else if self.playing && queued == 0.0 {
                dev.pause();
                self.playing = false;
            }
        }
    }

    /// Drop anything queued (pause, reset, state load).
    pub fn clear(&mut self) {
        if let Some(dev) = &self.device {
            dev.clear();
            dev.pause();
        }
        self.playing = false;
        self.resampled.clear();
    }
}
