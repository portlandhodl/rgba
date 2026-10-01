// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gb/audio.c and include/mgba/internal/gb/audio.h.
//
// The DMG/CGB APU: 2 pulse channels, wave channel, noise channel, frame
// sequencer, envelope/sweep/length, mixing.
//
// Differences forced by the Rust port (no behavior change intended):
// - `audio->nr52` (a pointer into memory.io) is `memory.io[GB_REG_NR52]`.
// - `audio->p` back-pointer: the console owns the audio state, so the C's
//   `audio->p &&` guards are unconditionally true and GB fields are read via
//   the `Gb` receiver.
// - The mAudioBuffer + mCoreSync/stream production layer is replaced by
//   `buffer: RingI16`; `_sample` pushes each batch with
//   `buffer.write_stereo(left, right)` (i16 interleaved). Ring capacity is
//   AUDIO_BUFFER_SAMPLES * 2 i16 samples — the C allocates
//   samples * channels * sizeof(int16_t).
// - `struct mTimingEvent frameEvent/sampleEvent` are folded into
//   `EventId::AudioFrame` (priority 0x10) / `EventId::AudioSample`
//   (priority 0x18). In the C GB core the frame sequencer is driven by the
//   timer's div bit (see timer.rs), not by frameEvent (that callback body is
//   GBA-only); EventId::AudioFrame is kept for dispatch completeness and
//   nothing schedules it.
// - GB_AUDIO_MGB is an alias of GB_AUDIO_DMG in the C enum, so every
//   `style == GB_AUDIO_DMG` comparison in the C is true for MGB too; use
//   `GbAudioStyle::is_dmg_style()` for those. The GB_AUDIO_AGB/GB_AUDIO_GBA
//   styles (3-bit wave volume, wavedata32 nibble stepping, envelope step
//   reload) are never used by the GB core; their branches are documented at
//   each site instead of ported.
// - `skipFrame` is `frame_skip: i32` (per the crate's fixed surface) holding
//   the C bool's 0/1 value.
//
// Sample rates: GB_MAX_SAMPLES = 32 samples are produced per sample event,
// spaced SAMPLE_INTERVAL * timing_factor = 64 master cycles apart, and the
// event reschedules every sample_interval * timing_factor = 2048 master
// cycles, i.e. 0x800000 / 64 = 131072 Hz (cf. _GBCoreAudioSampleRate).

use rgba_core::ring::RingI16;
use rgba_core::timing::Timing;

use crate::gb::{EventId, Gb};
use crate::io::{
    GB_REG_NR10, GB_REG_NR11, GB_REG_NR12, GB_REG_NR13, GB_REG_NR14, GB_REG_NR21, GB_REG_NR22,
    GB_REG_NR23, GB_REG_NR24, GB_REG_NR30, GB_REG_NR31, GB_REG_NR32, GB_REG_NR33, GB_REG_NR34,
    GB_REG_NR41, GB_REG_NR42, GB_REG_NR43, GB_REG_NR44, GB_REG_NR50, GB_REG_NR51, GB_REG_NR52,
};

/// GB_MAX_SAMPLES
pub const GB_MAX_SAMPLES: usize = 32;
/// AUDIO_BUFFER_SAMPLES (mAudioBuffer capacity, per channel)
const AUDIO_BUFFER_SAMPLES: usize = 0x4000;
/// SAMPLE_INTERVAL
const SAMPLE_INTERVAL: i32 = 32;
/// FILTER (capacitance high-pass)
const FILTER: i32 = 65368;
/// GB_AUDIO_VOLUME_MAX
pub const GB_AUDIO_VOLUME_MAX: i32 = 0x100;
/// mTiming factor: GBAudioInit, GB styles all use 2 (GB_AUDIO_GBA would be 4;
/// that style is unreachable in the GB core).
const TIMING_FACTOR: i32 = 2;

/// _squareChannelDuty
static SQUARE_CHANNEL_DUTY: [[i32; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 1, 1, 1],
    [0, 1, 1, 1, 1, 1, 1, 0],
];

/// noiseMaskTable (GBAudioRun, ch4 batch path)
static NOISE_MASK_TABLE: [u16; 0x40] = [
    0x3f, 0x3e, 0x3c, 0x3d, 0x39, 0x38, 0x3a, 0x3b, 0x33, 0x32, 0x30, 0x31, 0x35, 0x34, 0x36, 0x37,
    0x27, 0x26, 0x24, 0x25, 0x21, 0x20, 0x22, 0x23, 0x2b, 0x2a, 0x28, 0x29, 0x2d, 0x2c, 0x2e, 0x2f,
    0x0f, 0x0e, 0x0c, 0x0d, 0x09, 0x08, 0x0a, 0x0b, 0x03, 0x02, 0x00, 0x01, 0x05, 0x04, 0x06, 0x07,
    0x17, 0x16, 0x14, 0x15, 0x11, 0x10, 0x12, 0x13, 0x1b, 0x1a, 0x18, 0x19, 0x1d, 0x1c, 0x1e, 0x1f,
];

/// noisePopulationTable (GBAudioRun, ch4 batch path)
static NOISE_POPULATION_TABLE: [u16; 0x40] = [
    6, 5, 4, 5, 4, 3, 4, 5, 4, 3, 2, 3, 4, 3, 4, 5, 4, 3, 2, 3, 2, 1, 2, 3, 4, 3, 2, 3, 4, 3, 4, 5,
    4, 3, 2, 3, 2, 1, 2, 3, 2, 1, 0, 1, 2, 1, 2, 3, 4, 3, 2, 3, 2, 1, 2, 3, 4, 3, 2, 3, 4, 3, 4, 5,
];

// Register bitfield accessors (DECL_BITFIELD in audio.h), as packed-int
// getters mirroring the C-generated names:

/// GBAudioRegisterDutyGetLength (DECL_BITS(GBAudioRegisterDuty, Length, 0, 6))
#[inline]
fn gb_audio_register_duty_get_length(v: u8) -> i32 {
    (v & 0x3F) as i32
}

/// GBAudioRegisterDutyGetDuty (DECL_BITS(GBAudioRegisterDuty, Duty, 6, 2))
#[inline]
fn gb_audio_register_duty_get_duty(v: u8) -> i32 {
    ((v >> 6) & 0x3) as i32
}

/// GBAudioRegisterSweepGetStepTime (DECL_BITS(GBAudioRegisterSweep, StepTime, 0, 3))
#[inline]
fn gb_audio_register_sweep_get_step_time(v: u8) -> i32 {
    (v & 0x7) as i32
}

/// GBAudioRegisterSweepGetDirection (DECL_BIT(GBAudioRegisterSweep, Direction, 3))
#[inline]
fn gb_audio_register_sweep_get_direction(v: u8) -> bool {
    v & 0x8 != 0
}

/// GBAudioRegisterSweepGetInitialVolume (DECL_BITS(GBAudioRegisterSweep, InitialVolume, 4, 4))
#[inline]
fn gb_audio_register_sweep_get_initial_volume(v: u8) -> i32 {
    ((v >> 4) & 0xF) as i32
}

/// GBAudioRegisterControlGetRate (DECL_BITS(GBAudioRegisterControl, Rate, 0, 11))
#[inline]
fn gb_audio_register_control_get_rate(v: u16) -> i32 {
    (v & 0x7FF) as i32
}

/// GBAudioRegisterControlGetFrequency (DECL_BITS(GBAudioRegisterControl, Frequency, 0, 11))
#[inline]
fn gb_audio_register_control_get_frequency(v: u16) -> i32 {
    (v & 0x7FF) as i32
}

/// GBAudioRegisterControlGetStop (DECL_BIT(GBAudioRegisterControl, Stop, 14))
#[inline]
fn gb_audio_register_control_get_stop(v: u16) -> bool {
    v & (1 << 14) != 0
}

/// GBAudioRegisterControlIsRestart (DECL_BIT(GBAudioRegisterControl, Restart, 15))
#[inline]
fn gb_audio_register_control_is_restart(v: u16) -> bool {
    v & (1 << 15) != 0
}

/// GBAudioRegisterSquareSweepGetShift (DECL_BITS(GBAudioRegisterSquareSweep, Shift, 0, 3))
#[inline]
fn gb_audio_register_square_sweep_get_shift(v: u8) -> i32 {
    (v & 0x7) as i32
}

/// GBAudioRegisterSquareSweepGetDirection (DECL_BIT(GBAudioRegisterSquareSweep, Direction, 3))
#[inline]
fn gb_audio_register_square_sweep_get_direction(v: u8) -> bool {
    v & 0x8 != 0
}

/// GBAudioRegisterSquareSweepGetTime (DECL_BITS(GBAudioRegisterSquareSweep, Time, 4, 3))
#[inline]
fn gb_audio_register_square_sweep_get_time(v: u8) -> i32 {
    ((v >> 4) & 0x7) as i32
}

/// GBAudioRegisterBankGetEnable (DECL_BIT(GBAudioRegisterBank, Enable, 7))
#[inline]
fn gb_audio_register_bank_get_enable(v: u8) -> bool {
    v & 0x80 != 0
}

/// GBAudioRegisterBankVolumeGetVolumeGB (DECL_BITS(GBAudioRegisterBankVolume, VolumeGB, 5, 2))
#[inline]
fn gb_audio_register_bank_volume_get_volume_gb(v: u8) -> i32 {
    ((v >> 5) & 0x3) as i32
}

/// GBAudioRegisterNoiseFeedbackGetRatio (DECL_BITS(GBAudioRegisterNoiseFeedback, Ratio, 0, 3))
#[inline]
fn gb_audio_register_noise_feedback_get_ratio(v: u8) -> i32 {
    (v & 0x7) as i32
}

/// GBAudioRegisterNoiseFeedbackGetPower (DECL_BIT(GBAudioRegisterNoiseFeedback, Power, 3))
#[inline]
fn gb_audio_register_noise_feedback_get_power(v: u8) -> bool {
    v & 0x8 != 0
}

/// GBAudioRegisterNoiseFeedbackGetFrequency (DECL_BITS(GBAudioRegisterNoiseFeedback, Frequency, 4, 4))
#[inline]
fn gb_audio_register_noise_feedback_get_frequency(v: u8) -> i32 {
    ((v >> 4) & 0xF) as i32
}

/// GBAudioRegisterNoiseControlGetStop (DECL_BIT(GBAudioRegisterNoiseControl, Stop, 6))
#[inline]
fn gb_audio_register_noise_control_get_stop(v: u8) -> bool {
    v & 0x40 != 0
}

/// GBAudioRegisterNoiseControlIsRestart (DECL_BIT(GBAudioRegisterNoiseControl, Restart, 7))
#[inline]
fn gb_audio_register_noise_control_is_restart(v: u8) -> bool {
    v & 0x80 != 0
}

/// GBRegisterNR50GetVolumeRight (DECL_BITS(GBRegisterNR50, VolumeRight, 0, 3))
#[inline]
fn gb_register_nr50_get_volume_right(v: u8) -> u8 {
    v & 0x7
}

/// GBRegisterNR50GetVolumeLeft (DECL_BITS(GBRegisterNR50, VolumeLeft, 4, 3))
#[inline]
fn gb_register_nr50_get_volume_left(v: u8) -> u8 {
    (v >> 4) & 0x7
}

/// GBAudioEnableGetEnable (DECL_BIT(GBAudioEnable, Enable, 7))
#[inline]
fn gb_audio_enable_get_enable(v: u8) -> bool {
    v & 0x80 != 0
}

/// enum GBAudioStyle (GB core only; GB_AUDIO_AGB/GB_AUDIO_GBA are not used)
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GbAudioStyle {
    /// GB_AUDIO_DMG
    Dmg,
    /// GB_AUDIO_MGB — an alias of GB_AUDIO_DMG in the C enum
    Mgb,
    /// GB_AUDIO_CGB
    Cgb,
}

impl GbAudioStyle {
    /// In the C, GB_AUDIO_MGB aliases GB_AUDIO_DMG, so every
    /// `style == GB_AUDIO_DMG` comparison matches MGB too.
    #[inline]
    fn is_dmg_style(self) -> bool {
        matches!(self, GbAudioStyle::Dmg | GbAudioStyle::Mgb)
    }
}

/// struct GBAudioEnvelope
#[derive(Default)]
pub struct GbAudioEnvelope {
    pub length: i32,
    pub duty: i32,
    pub step_time: i32,
    pub initial_volume: i32,
    pub current_volume: i32,
    pub direction: bool,
    pub dead: i32,
    pub next_step: i32,
}

/// struct GBAudioSquareControl
#[derive(Default)]
pub struct GbAudioSquareControl {
    pub frequency: i32,
    pub length: i32,
    pub stop: bool,
}

/// struct GBAudioSweep
#[derive(Default)]
pub struct GbAudioSweep {
    pub shift: i32,
    pub time: i32,
    pub step: i32,
    pub direction: bool,
    pub enable: bool,
    pub occurred: bool,
    pub real_frequency: i32,
}

/// struct GBAudioSquareChannel
#[derive(Default)]
pub struct GbAudioSquareChannel {
    pub sweep: GbAudioSweep,
    pub envelope: GbAudioEnvelope,
    pub control: GbAudioSquareControl,
    pub last_update: i32,
    pub index: u8,
    pub sample: i8,
}

/// struct GBAudioWaveChannel. The C's wavedata32[8]/wavedata8[16] union is
/// kept as wavedata8; the wavedata32 nibble-stepping branch is GB_AUDIO_GBA
/// (AGB wave) only, unreachable here.
#[derive(Default)]
pub struct GbAudioWaveChannel {
    pub size: bool,
    pub bank: bool,
    pub enable: bool,

    pub sample: i8,
    pub length: u32,
    pub volume: i32,

    pub rate: i32,
    pub stop: bool,

    pub window: i32,
    pub readable: bool,
    pub wavedata8: [u8; 16],
    pub next_update: i32,
}

/// struct GBAudioNoiseChannel
#[derive(Default)]
pub struct GbAudioNoiseChannel {
    pub envelope: GbAudioEnvelope,

    pub ratio: i32,
    pub frequency: i32,
    pub power: bool,
    pub stop: bool,
    pub length: i32,

    pub lfsr: u32,
    pub n_samples: i32,
    pub samples: i32,
    pub last_event: u32,

    pub sample: i8,
}

/// struct mStereoSample
#[derive(Clone, Copy, Default)]
pub struct StereoSample {
    pub left: i16,
    pub right: i16,
}

/// struct GBAudio. The C's `p`/`timing` back-pointers, `nr52` pointer and
/// mTimingEvent slots are replaced by `impl Gb` methods / EventId dispatch
/// (see the file header comment).
pub struct Audio {
    pub timing_factor: i32,
    pub ch1: GbAudioSquareChannel,
    pub ch2: GbAudioSquareChannel,
    pub ch3: GbAudioWaveChannel,
    pub ch4: GbAudioNoiseChannel,

    pub buffer: RingI16,
    pub cap_left: i32,
    pub cap_right: i32,

    pub volume_right: u8,
    pub volume_left: u8,
    // cGBAudioChannelFlags
    pub ch1_right: bool,
    pub ch2_right: bool,
    pub ch3_right: bool,
    pub ch4_right: bool,
    pub ch1_left: bool,
    pub ch2_left: bool,
    pub ch3_left: bool,
    pub ch4_left: bool,

    pub playing_ch1: bool,
    pub playing_ch2: bool,
    pub playing_ch3: bool,
    pub playing_ch4: bool,

    pub frame: i32,
    pub frame_skip: i32,

    pub sample_interval: i32,
    pub style: GbAudioStyle,

    pub last_sample: i32,
    pub sample_index: usize,
    pub current_samples: [StereoSample; GB_MAX_SAMPLES],

    pub enable: bool,

    pub samples: usize,
    pub force_disable_ch: [bool; 4],
    pub master_volume: i32,
}

impl Audio {
    /// GBAudioInit(audio, 2048, &gb->memory.io[GB_REG_NR52], GB_AUDIO_DMG).
    ///
    /// Buffer capacity: mAudioBufferInit(&audio->buffer, AUDIO_BUFFER_SAMPLES,
    /// 2) allocates AUDIO_BUFFER_SAMPLES * 2 i16 samples; RingI16's capacity
    /// is in i16 samples, hence the *2. Channels are set up as GBAudioReset
    /// leaves them (sweep.time = 8, envelopes dead = 2) so the state is valid
    /// before the first reset.
    pub fn new() -> Self {
        let mut audio = Audio {
            timing_factor: TIMING_FACTOR,
            ch1: GbAudioSquareChannel {
                sweep: GbAudioSweep {
                    time: 8,
                    ..Default::default()
                },
                envelope: GbAudioEnvelope {
                    dead: 2,
                    ..Default::default()
                },
                ..Default::default()
            },
            ch2: GbAudioSquareChannel {
                envelope: GbAudioEnvelope {
                    dead: 2,
                    ..Default::default()
                },
                ..Default::default()
            },
            ch3: GbAudioWaveChannel::default(),
            ch4: GbAudioNoiseChannel {
                envelope: GbAudioEnvelope {
                    dead: 2,
                    ..Default::default()
                },
                ..Default::default()
            },
            buffer: RingI16::new(AUDIO_BUFFER_SAMPLES * 2),
            cap_left: 0,
            cap_right: 0,
            volume_right: 0,
            volume_left: 0,
            ch1_right: false,
            ch2_right: false,
            ch3_right: false,
            ch4_right: false,
            ch1_left: false,
            ch2_left: false,
            ch3_left: false,
            ch4_left: false,
            playing_ch1: false,
            playing_ch2: false,
            playing_ch3: false,
            playing_ch4: false,
            frame: 0,
            frame_skip: 0,
            sample_interval: SAMPLE_INTERVAL * GB_MAX_SAMPLES as i32,
            style: GbAudioStyle::Dmg,
            last_sample: 0,
            sample_index: 0,
            current_samples: [StereoSample::default(); GB_MAX_SAMPLES],
            enable: false,
            samples: 2048,
            force_disable_ch: [false; 4],
            master_volume: GB_AUDIO_VOLUME_MAX,
        };
        // GBAudioReset's wave RAM pattern (style != GB_AUDIO_GBA, always true
        // in the GB core) so a never-reset Audio matches a reset one.
        let mut i = 0;
        while i < 8 {
            audio.ch3.wavedata8[i * 2] = 0x00;
            audio.ch3.wavedata8[i * 2 + 1] = 0xFF;
            i += 1;
        }
        audio
    }

    /// GBAudioDeinit — the ring frees itself (mirrors the C).
    pub fn deinit(&mut self) {}

    /// GBAudioRun. `audio->p` is always non-null in this port, so the C's
    /// `audio->p &&` on the sampling check folds away.
    fn run(&mut self, timestamp: i32, channels: i32) {
        if !self.enable {
            return;
        }
        if channels != 0x1F
            && timestamp.wrapping_sub(self.last_sample) > SAMPLE_INTERVAL * self.timing_factor
        {
            self.sample(timestamp);
        }

        if channels & 0x1 != 0
            && ((self.playing_ch1 && self.ch1.envelope.dead != 2)
                || timestamp.wrapping_sub(self.ch1.last_update) > 0x40000000
                || channels == 0x1)
        {
            let period = 4 * (2048 - self.ch1.control.frequency) * self.timing_factor;
            let mut diff = timestamp.wrapping_sub(self.ch1.last_update);
            if diff >= period {
                diff /= period;
                self.ch1.index = ((self.ch1.index as i32 + diff) & 7) as u8;
                self.ch1.last_update = self.ch1.last_update.wrapping_add(diff * period);
                update_square_sample(&mut self.ch1);
            }
        }
        if channels & 0x2 != 0
            && ((self.playing_ch2 && self.ch2.envelope.dead != 2)
                || timestamp.wrapping_sub(self.ch2.last_update) > 0x40000000
                || channels == 0x2)
        {
            let period = 4 * (2048 - self.ch2.control.frequency) * self.timing_factor;
            let mut diff = timestamp.wrapping_sub(self.ch2.last_update);
            if diff >= period {
                diff /= period;
                self.ch2.index = ((self.ch2.index as i32 + diff) & 7) as u8;
                self.ch2.last_update = self.ch2.last_update.wrapping_add(diff * period);
                update_square_sample(&mut self.ch2);
            }
        }
        if self.playing_ch3 && channels & 0x4 != 0 {
            let cycles = 2 * (2048 - self.ch3.rate) * self.timing_factor;
            let mut diff = timestamp.wrapping_sub(self.ch3.next_update);
            if diff >= 0 {
                diff = diff / cycles + 1;
                let volume = match self.ch3.volume {
                    0 => 4,
                    1 => 0,
                    2 => 1,
                    _ => 2,
                };
                // The C's switch on style has a GB_AUDIO_GBA branch doing
                // wavedata32 nibble stepping; that style is unreachable here,
                // so only the DMG/default branch is ported.
                self.ch3.window = self.ch3.window.wrapping_add(diff) & 0x1F;
                let mut sample = self.ch3.wavedata8[(self.ch3.window >> 1) as usize] as i8;
                if self.ch3.window & 1 == 0 {
                    sample = sample.wrapping_shr(4);
                }
                sample &= 0xF;
                if self.ch3.volume > 3 {
                    // AGB 75% volume; unreachable with the GB's 2-bit volume.
                    sample = sample.wrapping_add(sample << 1);
                }
                sample = sample.wrapping_shr(volume as u32);
                self.ch3.sample = sample;
                self.ch3.next_update = self.ch3.next_update.wrapping_add(diff * cycles);
                self.ch3.readable = true;
            }
            if self.style.is_dmg_style() && self.ch3.readable {
                diff = timestamp
                    .wrapping_sub(self.ch3.next_update)
                    .wrapping_add(cycles);
                if diff >= 4 {
                    self.ch3.readable = false;
                }
            }
        }
        if self.playing_ch4 && channels & 0x8 != 0 {
            let mut cycles: i32 = if self.ch4.ratio != 0 {
                2 * self.ch4.ratio
            } else {
                1
            };
            cycles <<= self.ch4.frequency;
            cycles *= 8 * self.timing_factor;

            let diff = timestamp.wrapping_sub(self.ch4.last_event as i32);
            if diff >= cycles {
                let mut last: i32 = 0;
                let mut samples: i32 = 0;
                let mut positive_samples: i32 = 0;
                let mut lsb: i32;
                let coeff: i32;
                if self.ch4.power {
                    // TODO: Can this be batched too?
                    lsb = 0;
                    coeff = 0x4040;
                } else {
                    let mut bits: i32 = 0;
                    // Batch 5 steps at a time when possible
                    while last + cycles * 5 <= diff {
                        bits = (self.ch4.lfsr & 0x3F) as i32;
                        self.ch4.lfsr >>= 5;
                        self.ch4.lfsr |=
                            ((0x4000 * NOISE_MASK_TABLE[bits as usize] as i32) >> 4) as u32;
                        self.ch4.lfsr &= 0x7FFF;
                        samples += 5;
                        positive_samples += NOISE_POPULATION_TABLE[bits as usize] as i32;
                        last += cycles * 5;
                    }
                    lsb = (NOISE_MASK_TABLE[bits as usize] & 1) as i32;
                    coeff = 0x4000;
                }
                while last + cycles <= diff {
                    lsb = ((self.ch4.lfsr ^ (self.ch4.lfsr >> 1) ^ 1) & 1) as i32;
                    self.ch4.lfsr >>= 1;
                    if lsb != 0 {
                        self.ch4.lfsr |= coeff as u32;
                    } else {
                        self.ch4.lfsr &= !(coeff as u32);
                    }
                    samples += 1;
                    positive_samples += lsb;
                    last += cycles;
                }
                self.ch4.sample = (lsb * self.ch4.envelope.current_volume) as i8;
                self.ch4.n_samples += samples;
                self.ch4.samples += positive_samples * self.ch4.envelope.current_volume;
                self.ch4.last_event = self.ch4.last_event.wrapping_add(last as u32);
            }
        }
    }

    /// GBAudioSample: fill current_samples[] up to GB_MAX_SAMPLES, mixing at
    /// every SAMPLE_INTERVAL * timing_factor master cycles.
    fn sample(&mut self, timestamp: i32) {
        let interval = SAMPLE_INTERVAL * self.timing_factor;
        let mut timestamp = timestamp.wrapping_sub(self.last_sample);
        timestamp = timestamp.wrapping_sub(self.sample_index as i32 * interval);

        let mut sample = self.sample_index;
        while timestamp >= interval && sample < GB_MAX_SAMPLES {
            self.run(
                (sample as i32 * interval).wrapping_add(self.last_sample),
                0x1F,
            );
            let (sample_left, sample_right) = self.sample_psg();
            let sample_left = ((sample_left as i32 * self.master_volume * 6) >> 7) as i16;
            let sample_right = ((sample_right as i32 * self.master_volume * 6) >> 7) as i16;

            let degraded_left = (sample_left as i32 - (self.cap_left >> 16)) as i16;
            let degraded_right = (sample_right as i32 - (self.cap_right >> 16)) as i16;
            self.cap_left = ((sample_left as i32) << 16)
                .wrapping_sub((degraded_left as i32).wrapping_mul(FILTER));
            self.cap_right = ((sample_right as i32) << 16)
                .wrapping_sub((degraded_right as i32).wrapping_mul(FILTER));

            self.current_samples[sample].left = degraded_left;
            self.current_samples[sample].right = degraded_right;

            sample += 1;
            timestamp -= interval;
        }

        self.sample_index = sample;
        if sample == GB_MAX_SAMPLES {
            self.last_sample = self
                .last_sample
                .wrapping_add(interval * GB_MAX_SAMPLES as i32);
            self.sample_index = 0;
        }
    }

    /// GBAudioSamplePSG
    fn sample_psg(&mut self) -> (i16, i16) {
        // GB_AUDIO_GBA would use 0; unreachable in this core.
        let dc_offset: i32 = -0x8;
        let mut sample_left = dc_offset;
        let mut sample_right = dc_offset;

        if !self.force_disable_ch[0] {
            if self.ch1_left {
                sample_left += self.ch1.sample as i32;
            }
            if self.ch1_right {
                sample_right += self.ch1.sample as i32;
            }
        }

        if !self.force_disable_ch[1] {
            if self.ch2_left {
                sample_left += self.ch2.sample as i32;
            }
            if self.ch2_right {
                sample_right += self.ch2.sample as i32;
            }
        }

        if !self.force_disable_ch[2] {
            if self.ch3_left {
                sample_left += self.ch3.sample as i32;
            }
            if self.ch3_right {
                sample_right += self.ch3.sample as i32;
            }
        }

        sample_left <<= 3;
        sample_right <<= 3;

        if !self.force_disable_ch[3] {
            // GB_AUDIO_GBA would use ch4.sample << 3; unreachable here.
            let sample = coalesce_noise_channel(&mut self.ch4) as i32;
            if self.ch4_left {
                sample_left += sample;
            }
            if self.ch4_right {
                sample_right += sample;
            }
        }

        (
            (sample_left * (1 + self.volume_left as i32)) as i16,
            (sample_right * (1 + self.volume_right as i32)) as i16,
        )
    }
}

impl Default for Audio {
    fn default() -> Self {
        Self::new()
    }
}

/// _resetEnvelope
fn reset_envelope(envelope: &mut GbAudioEnvelope, style: GbAudioStyle) -> bool {
    envelope.current_volume = envelope.initial_volume;
    envelope.next_step = envelope.step_time;
    update_envelope_dead(envelope, style);
    envelope.initial_volume != 0 || envelope.direction
}

/// _resetSweep
fn reset_sweep(sweep: &mut GbAudioSweep) {
    sweep.step = sweep.time;
    sweep.enable = sweep.step != 8 || sweep.shift != 0;
    sweep.occurred = false;
}

/// _writeSweep
fn write_sweep(sweep: &mut GbAudioSweep, value: u8) -> bool {
    sweep.shift = gb_audio_register_square_sweep_get_shift(value);
    let old_direction = sweep.direction;
    sweep.direction = gb_audio_register_square_sweep_get_direction(value);
    let mut on = true;
    if sweep.occurred && old_direction && !sweep.direction {
        on = false;
    }
    sweep.occurred = false;
    sweep.time = gb_audio_register_square_sweep_get_time(value);
    if sweep.time == 0 {
        sweep.time = 8;
    }
    on
}

/// _writeDuty
fn write_duty(envelope: &mut GbAudioEnvelope, value: u8) {
    envelope.length = gb_audio_register_duty_get_length(value);
    envelope.duty = gb_audio_register_duty_get_duty(value);
}

/// _writeEnvelope
fn write_envelope(envelope: &mut GbAudioEnvelope, value: u8, style: GbAudioStyle) -> bool {
    let old_direction = envelope.direction;
    envelope.step_time = gb_audio_register_sweep_get_step_time(value);
    envelope.direction = gb_audio_register_sweep_get_direction(value);
    envelope.initial_volume = gb_audio_register_sweep_get_initial_volume(value);
    if envelope.step_time == 0 {
        // TODO: Improve "zombie" mode
        if style.is_dmg_style() {
            envelope.current_volume += 1;
        } else {
            // GB_AUDIO_CGB
            if envelope.direction == old_direction {
                if envelope.direction {
                    envelope.current_volume += 1;
                } else {
                    envelope.current_volume += 2;
                }
            } else {
                envelope.current_volume = 0;
            }
        }
        envelope.current_volume &= 0xF;
    }
    update_envelope_dead(envelope, style);
    envelope.initial_volume != 0 || envelope.direction
}

/// _updateSquareSample
fn update_square_sample(ch: &mut GbAudioSquareChannel) {
    ch.sample = (SQUARE_CHANNEL_DUTY[ch.envelope.duty as usize][ch.index as usize]
        * ch.envelope.current_volume) as i8;
}

/// _coalesceNoiseChannel
fn coalesce_noise_channel(ch: &mut GbAudioNoiseChannel) -> i16 {
    if ch.n_samples <= 1 {
        return (ch.sample as i16) << 3;
    }
    // TODO keep track of timing
    let sample = ((ch.samples << 3) / ch.n_samples) as i16;
    ch.n_samples = 0;
    ch.samples = 0;
    sample
}

/// _updateEnvelope
fn update_envelope(envelope: &mut GbAudioEnvelope) {
    if envelope.direction {
        envelope.current_volume += 1;
    } else {
        envelope.current_volume -= 1;
    }
    if envelope.current_volume >= 15 {
        envelope.current_volume = 15;
        envelope.dead = 1;
    } else if envelope.current_volume <= 0 {
        envelope.current_volume = 0;
        envelope.dead = 2;
    } else {
        envelope.next_step = envelope.step_time;
    }
}

/// _updateEnvelopeDead
fn update_envelope_dead(envelope: &mut GbAudioEnvelope, _style: GbAudioStyle) {
    if envelope.step_time == 0 {
        envelope.dead = if envelope.current_volume != 0 { 1 } else { 2 };
    } else if !envelope.direction && envelope.current_volume == 0 {
        envelope.dead = 2;
    } else if envelope.direction && envelope.current_volume == 0xF {
        envelope.dead = 1;
    } else if envelope.dead != 0 {
        // TODO: Figure out if this happens on DMG/CGB or just AGB
        // TODO: Figure out the exact circumstances that lead to reloading the step
        // The C reloads nextStep here only for GB_AUDIO_GBA, a style this
        // core never uses.
        envelope.dead = 0;
    }
}

/// _updateSweep
fn update_sweep(ch: &mut GbAudioSquareChannel, initial: bool) -> bool {
    if initial || ch.sweep.time != 8 {
        let mut frequency = ch.sweep.real_frequency;
        if ch.sweep.direction {
            frequency -= frequency >> ch.sweep.shift;
            if !initial && frequency >= 0 {
                ch.control.frequency = frequency;
                ch.sweep.real_frequency = frequency;
            }
        } else {
            frequency += frequency >> ch.sweep.shift;
            if frequency < 2048 {
                if !initial && ch.sweep.shift != 0 {
                    ch.control.frequency = frequency;
                    ch.sweep.real_frequency = frequency;
                    if !update_sweep(ch, true) {
                        return false;
                    }
                }
            } else {
                return false;
            }
        }
        ch.sweep.occurred = true;
    }
    ch.sweep.step = ch.sweep.time;
    true
}

impl Gb {
    /// GBAudioReset
    pub fn audio_reset(&mut self) {
        // M_CORE_GB block
        self.deschedule(EventId::AudioSample);
        // style is never GB_AUDIO_GBA in the GB core
        self.schedule(EventId::AudioSample, 0);

        self.audio.ch1 = GbAudioSquareChannel {
            sweep: GbAudioSweep {
                time: 8,
                ..Default::default()
            },
            envelope: GbAudioEnvelope {
                dead: 2,
                ..Default::default()
            },
            ..Default::default()
        };
        self.audio.ch2 = GbAudioSquareChannel {
            envelope: GbAudioEnvelope {
                dead: 2,
                ..Default::default()
            },
            ..Default::default()
        };
        self.audio.ch3 = GbAudioWaveChannel {
            bank: false,
            ..Default::default()
        };
        // TODO: DMG randomness
        // style != GB_AUDIO_GBA is always true in this core.
        let mut i = 0;
        while i < 8 {
            self.audio.ch3.wavedata8[i * 2] = 0x00;
            self.audio.ch3.wavedata8[i * 2 + 1] = 0xFF;
            i += 1;
        }
        self.audio.ch4 = GbAudioNoiseChannel {
            envelope: GbAudioEnvelope {
                dead: 2,
                ..Default::default()
            },
            ..Default::default()
        };
        self.audio.frame = 0;
        self.audio.sample_interval = SAMPLE_INTERVAL * GB_MAX_SAMPLES as i32;
        self.audio.last_sample = 0;
        self.audio.sample_index = 0;
        self.audio.cap_left = 0;
        self.audio.cap_right = 0;
        self.audio.buffer.clear();
        self.audio.playing_ch1 = false;
        self.audio.playing_ch2 = false;
        self.audio.playing_ch3 = false;
        self.audio.playing_ch4 = false;
        // The C checks audio->p && !(audio->p->model & GB_MODEL_SGB).
        if !self.model_has_sgb() {
            self.audio.playing_ch1 = true;
            self.audio.enable = true;
            self.memory.io[GB_REG_NR52 as usize] |= 0x01;
        }
    }

    /// GBAudioResizeBuffer (the C's mCoreSync lock/consume is frontend thread
    /// glue; this port has no sync layer).
    pub fn audio_resize_buffer(&mut self, samples: usize) {
        self.audio.samples = samples;
    }

    /// GBAudioWriteNR10
    pub fn audio_write_nr10(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0x1);
        if !write_sweep(&mut self.audio.ch1.sweep, value) {
            self.audio.playing_ch1 = false;
            self.memory.io[GB_REG_NR52 as usize] &= !0x1;
        }
    }

    /// GBAudioWriteNR11
    pub fn audio_write_nr11(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0x1);
        write_duty(&mut self.audio.ch1.envelope, value);
        self.audio.ch1.control.length = 64 - self.audio.ch1.envelope.length;
    }

    /// GBAudioWriteNR12
    pub fn audio_write_nr12(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0x1);
        if !write_envelope(&mut self.audio.ch1.envelope, value, self.audio.style) {
            self.audio.playing_ch1 = false;
            self.memory.io[GB_REG_NR52 as usize] &= !0x1;
        }
    }

    /// GBAudioWriteNR13
    pub fn audio_write_nr13(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0x1);
        self.audio.ch1.control.frequency &= 0x700;
        self.audio.ch1.control.frequency |= gb_audio_register_control_get_frequency(value as u16);
    }

    /// GBAudioWriteNR14
    pub fn audio_write_nr14(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0x1);
        self.audio.ch1.control.frequency &= 0xFF;
        self.audio.ch1.control.frequency |=
            gb_audio_register_control_get_frequency((value as u16) << 8);
        let was_stop = self.audio.ch1.control.stop;
        self.audio.ch1.control.stop = gb_audio_register_control_get_stop((value as u16) << 8);
        if !was_stop
            && self.audio.ch1.control.stop
            && self.audio.ch1.control.length != 0
            && self.audio.frame & 1 == 0
        {
            self.audio.ch1.control.length -= 1;
            if self.audio.ch1.control.length == 0 {
                self.audio.playing_ch1 = false;
            }
        }
        if gb_audio_register_control_is_restart((value as u16) << 8) {
            self.audio.playing_ch1 = reset_envelope(&mut self.audio.ch1.envelope, self.audio.style);
            self.audio.ch1.sweep.real_frequency = self.audio.ch1.control.frequency;
            reset_sweep(&mut self.audio.ch1.sweep);
            if self.audio.playing_ch1 && self.audio.ch1.sweep.shift != 0 {
                self.audio.playing_ch1 = update_sweep(&mut self.audio.ch1, true);
            }
            if self.audio.ch1.control.length == 0 {
                self.audio.ch1.control.length = 64;
                if self.audio.ch1.control.stop && self.audio.frame & 1 == 0 {
                    self.audio.ch1.control.length -= 1;
                }
            }
            update_square_sample(&mut self.audio.ch1);
        }
        self.memory.io[GB_REG_NR52 as usize] &= !0x1;
        self.memory.io[GB_REG_NR52 as usize] |= self.audio.playing_ch1 as u8;
    }

    /// GBAudioWriteNR21
    pub fn audio_write_nr21(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0x2);
        write_duty(&mut self.audio.ch2.envelope, value);
        self.audio.ch2.control.length = 64 - self.audio.ch2.envelope.length;
    }

    /// GBAudioWriteNR22
    pub fn audio_write_nr22(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0x2);
        if !write_envelope(&mut self.audio.ch2.envelope, value, self.audio.style) {
            self.audio.playing_ch2 = false;
            self.memory.io[GB_REG_NR52 as usize] &= !0x2;
        }
    }

    /// GBAudioWriteNR23
    pub fn audio_write_nr23(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0x2);
        self.audio.ch2.control.frequency &= 0x700;
        self.audio.ch2.control.frequency |= gb_audio_register_control_get_frequency(value as u16);
    }

    /// GBAudioWriteNR24
    pub fn audio_write_nr24(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0x2);
        self.audio.ch2.control.frequency &= 0xFF;
        self.audio.ch2.control.frequency |=
            gb_audio_register_control_get_frequency((value as u16) << 8);
        let was_stop = self.audio.ch2.control.stop;
        self.audio.ch2.control.stop = gb_audio_register_control_get_stop((value as u16) << 8);
        if !was_stop
            && self.audio.ch2.control.stop
            && self.audio.ch2.control.length != 0
            && self.audio.frame & 1 == 0
        {
            self.audio.ch2.control.length -= 1;
            if self.audio.ch2.control.length == 0 {
                self.audio.playing_ch2 = false;
            }
        }
        if gb_audio_register_control_is_restart((value as u16) << 8) {
            self.audio.playing_ch2 = reset_envelope(&mut self.audio.ch2.envelope, self.audio.style);

            if self.audio.ch2.control.length == 0 {
                self.audio.ch2.control.length = 64;
                if self.audio.ch2.control.stop && self.audio.frame & 1 == 0 {
                    self.audio.ch2.control.length -= 1;
                }
            }
            update_square_sample(&mut self.audio.ch2);
        }
        self.memory.io[GB_REG_NR52 as usize] &= !0x2;
        self.memory.io[GB_REG_NR52 as usize] |= (self.audio.playing_ch2 as u8) << 1;
    }

    /// GBAudioWriteNR30
    pub fn audio_write_nr30(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0x4);
        self.audio.ch3.enable = gb_audio_register_bank_get_enable(value);
        if !self.audio.ch3.enable {
            self.audio.playing_ch3 = false;
            self.memory.io[GB_REG_NR52 as usize] &= !0x4;
        }
    }

    /// GBAudioWriteNR31
    pub fn audio_write_nr31(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0x4);
        self.audio.ch3.length = 256 - value as u32;
    }

    /// GBAudioWriteNR32
    pub fn audio_write_nr32(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0x4);
        self.audio.ch3.volume = gb_audio_register_bank_volume_get_volume_gb(value);

        let mut sample = self.audio.ch3.wavedata8[(self.audio.ch3.window >> 1) as usize] as i8;
        if self.audio.ch3.window & 1 == 0 {
            sample = sample.wrapping_shr(4);
        }
        sample &= 0xF;
        let volume = match self.audio.ch3.volume {
            0 => 4,
            1 => 0,
            2 => 1,
            _ => 2,
        };
        sample = sample.wrapping_shr(volume as u32);
        self.audio.ch3.sample = sample;
    }

    /// GBAudioWriteNR33
    pub fn audio_write_nr33(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0x4);
        self.audio.ch3.rate &= 0x700;
        self.audio.ch3.rate |= gb_audio_register_control_get_rate(value as u16);
    }

    /// GBAudioWriteNR34
    pub fn audio_write_nr34(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0x4);
        self.audio.ch3.rate &= 0xFF;
        self.audio.ch3.rate |= gb_audio_register_control_get_rate((value as u16) << 8);
        let was_stop = self.audio.ch3.stop;
        self.audio.ch3.stop = gb_audio_register_control_get_stop((value as u16) << 8);
        if !was_stop
            && self.audio.ch3.stop
            && self.audio.ch3.length != 0
            && self.audio.frame & 1 == 0
        {
            self.audio.ch3.length -= 1;
            if self.audio.ch3.length == 0 {
                self.audio.playing_ch3 = false;
            }
        }
        let was_enable = self.audio.playing_ch3;
        if gb_audio_register_control_is_restart((value as u16) << 8) {
            self.audio.playing_ch3 = self.audio.ch3.enable;
            if self.audio.ch3.length == 0 {
                self.audio.ch3.length = 256;
                if self.audio.ch3.stop && self.audio.frame & 1 == 0 {
                    self.audio.ch3.length -= 1;
                }
            }

            if self.audio.style.is_dmg_style()
                && was_enable
                && self.audio.playing_ch3
                && self.audio.ch3.readable
            {
                if self.audio.ch3.window < 8 {
                    self.audio.ch3.wavedata8[0] =
                        self.audio.ch3.wavedata8[(self.audio.ch3.window >> 1) as usize];
                } else {
                    let base = ((self.audio.ch3.window >> 1) & !3) as usize;
                    for i in 0..4 {
                        self.audio.ch3.wavedata8[i] = self.audio.ch3.wavedata8[base + i];
                    }
                }
            }
            self.audio.ch3.window = 0;
            if self.audio.style.is_dmg_style() {
                self.audio.ch3.sample = 0;
            }
        }
        if self.audio.playing_ch3 {
            self.audio.ch3.readable = !self.audio.style.is_dmg_style();
            // TODO: Where does this cycle delay come from?
            self.audio.ch3.next_update =
                now.wrapping_add((6 + 2 * (2048 - self.audio.ch3.rate)) * self.audio.timing_factor);
        }
        self.memory.io[GB_REG_NR52 as usize] &= !0x4;
        self.memory.io[GB_REG_NR52 as usize] |= (self.audio.playing_ch3 as u8) << 2;
    }

    /// GBAudioWriteNR41
    pub fn audio_write_nr41(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0x8);
        write_duty(&mut self.audio.ch4.envelope, value);
        self.audio.ch4.length = 64 - self.audio.ch4.envelope.length;
    }

    /// GBAudioWriteNR42
    pub fn audio_write_nr42(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0x8);
        if !write_envelope(&mut self.audio.ch4.envelope, value, self.audio.style) {
            self.audio.playing_ch4 = false;
            self.memory.io[GB_REG_NR52 as usize] &= !0x8;
        }
    }

    /// GBAudioWriteNR43
    pub fn audio_write_nr43(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0x8);
        self.audio.ch4.ratio = gb_audio_register_noise_feedback_get_ratio(value);
        self.audio.ch4.frequency = gb_audio_register_noise_feedback_get_frequency(value);
        self.audio.ch4.power = gb_audio_register_noise_feedback_get_power(value);
    }

    /// GBAudioWriteNR44
    pub fn audio_write_nr44(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0x8);
        let was_stop = self.audio.ch4.stop;
        self.audio.ch4.stop = gb_audio_register_noise_control_get_stop(value);
        if !was_stop
            && self.audio.ch4.stop
            && self.audio.ch4.length != 0
            && self.audio.frame & 1 == 0
        {
            self.audio.ch4.length -= 1;
            if self.audio.ch4.length == 0 {
                self.audio.playing_ch4 = false;
            }
        }
        if gb_audio_register_noise_control_is_restart(value) {
            self.audio.playing_ch4 = reset_envelope(&mut self.audio.ch4.envelope, self.audio.style);

            self.audio.ch4.lfsr = 0;
            if self.audio.ch4.length == 0 {
                self.audio.ch4.length = 64;
                if self.audio.ch4.stop && self.audio.frame & 1 == 0 {
                    self.audio.ch4.length -= 1;
                }
            }
            if self.audio.playing_ch4 {
                self.audio.ch4.last_event = now as u32;
            }
        }
        self.memory.io[GB_REG_NR52 as usize] &= !0x8;
        self.memory.io[GB_REG_NR52 as usize] |= (self.audio.playing_ch4 as u8) << 3;
    }

    /// GBAudioWriteNR50
    pub fn audio_write_nr50(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0xF);
        self.audio.volume_right = gb_register_nr50_get_volume_right(value);
        self.audio.volume_left = gb_register_nr50_get_volume_left(value);
    }

    /// GBAudioWriteNR51 (GBRegisterNR51GetChXRight/ChXLeft per bit)
    pub fn audio_write_nr51(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.run(now, 0xF);
        self.audio.ch1_right = value & 0x01 != 0;
        self.audio.ch2_right = value & 0x02 != 0;
        self.audio.ch3_right = value & 0x04 != 0;
        self.audio.ch4_right = value & 0x08 != 0;
        self.audio.ch1_left = value & 0x10 != 0;
        self.audio.ch2_left = value & 0x20 != 0;
        self.audio.ch3_left = value & 0x40 != 0;
        self.audio.ch4_left = value & 0x80 != 0;
    }

    /// GBAudioWriteNR52
    pub fn audio_write_nr52(&mut self, value: u8) {
        let was_enable = self.audio.enable;
        self.audio.enable = gb_audio_enable_get_enable(value);
        // The C guards the io[] clears on audio->p, always non-null here.
        if !self.audio.enable {
            self.audio.playing_ch1 = false;
            self.audio.playing_ch2 = false;
            self.audio.playing_ch3 = false;
            self.audio.playing_ch4 = false;
            self.audio_write_nr10(0);
            self.audio_write_nr12(0);
            self.audio_write_nr13(0);
            self.audio_write_nr14(0);
            self.audio_write_nr22(0);
            self.audio_write_nr23(0);
            self.audio_write_nr24(0);
            self.audio_write_nr30(0);
            self.audio_write_nr32(0);
            self.audio_write_nr33(0);
            self.audio_write_nr34(0);
            self.audio_write_nr42(0);
            self.audio_write_nr43(0);
            self.audio_write_nr44(0);
            self.audio_write_nr50(0);
            self.audio_write_nr51(0);
            if !self.audio.style.is_dmg_style() {
                self.audio_write_nr11(0);
                self.audio_write_nr21(0);
                self.audio_write_nr31(0);
                self.audio_write_nr41(0);
            }

            self.memory.io[GB_REG_NR10 as usize] = 0;
            self.memory.io[GB_REG_NR11 as usize] = 0;
            self.memory.io[GB_REG_NR12 as usize] = 0;
            self.memory.io[GB_REG_NR13 as usize] = 0;
            self.memory.io[GB_REG_NR14 as usize] = 0;
            self.memory.io[GB_REG_NR21 as usize] = 0;
            self.memory.io[GB_REG_NR22 as usize] = 0;
            self.memory.io[GB_REG_NR23 as usize] = 0;
            self.memory.io[GB_REG_NR24 as usize] = 0;
            self.memory.io[GB_REG_NR30 as usize] = 0;
            self.memory.io[GB_REG_NR31 as usize] = 0;
            self.memory.io[GB_REG_NR32 as usize] = 0;
            self.memory.io[GB_REG_NR33 as usize] = 0;
            self.memory.io[GB_REG_NR34 as usize] = 0;
            self.memory.io[GB_REG_NR42 as usize] = 0;
            self.memory.io[GB_REG_NR43 as usize] = 0;
            self.memory.io[GB_REG_NR44 as usize] = 0;
            self.memory.io[GB_REG_NR50 as usize] = 0;
            self.memory.io[GB_REG_NR51 as usize] = 0;
            if !self.audio.style.is_dmg_style() {
                // Kept from the C: writes four registers that were already
                // cleared above.
                self.memory.io[GB_REG_NR11 as usize] = 0;
                self.memory.io[GB_REG_NR21 as usize] = 0;
                self.memory.io[GB_REG_NR31 as usize] = 0;
                self.memory.io[GB_REG_NR41 as usize] = 0;
            }
            self.memory.io[GB_REG_NR52 as usize] &= !0xF;
        } else if !was_enable {
            self.audio.frame_skip = 0;
            self.audio.frame = 7;

            if self.timer.internal_div & (0x100u32 << self.double_speed as u32) != 0 {
                self.audio.frame_skip = 1;
            }
        }
    }

    /// GBAudioUpdateFrame (the frame sequencer; driven by the timer's div
    /// bit). NOTE: the C reads mTimingCurrentTime(audio->timing); here
    /// self.current_time() is exact in normal CPU context but degenerates to
    /// ~0 while the Timing is temporarily taken out inside Timing::tick
    /// callbacks (timer_event), making this GBAudioRun(0x7) mostly inert —
    /// channel catch-up then happens at the sample event / next register
    /// access with real timestamps, exactly as it does between the C's own
    /// 512 Hz frame ticks.
    pub fn audio_update_frame(&mut self) {
        if !self.audio.enable {
            return;
        }
        if self.audio.frame_skip != 0 {
            self.audio.frame_skip = 0;
            return;
        }
        let now = self.current_time();
        self.audio.run(now, 0x7);

        let frame = (self.audio.frame + 1) & 7;
        self.audio.frame = frame;

        match frame {
            0 | 2 | 4 | 6 => {
                if frame == 2 || frame == 6 {
                    if self.audio.ch1.sweep.enable {
                        self.audio.ch1.sweep.step -= 1;
                        if self.audio.ch1.sweep.step == 0 {
                            if !update_sweep(&mut self.audio.ch1, false) {
                                self.audio.playing_ch1 = false;
                            }
                            self.memory.io[GB_REG_NR52 as usize] &= !0x1;
                            self.memory.io[GB_REG_NR52 as usize] |= self.audio.playing_ch1 as u8;
                        }
                    }
                    // Fall through (C switch falls into case 0/4)
                }
                if self.audio.ch1.control.length != 0 && self.audio.ch1.control.stop {
                    self.audio.ch1.control.length -= 1;
                    if self.audio.ch1.control.length == 0 {
                        self.audio.playing_ch1 = false;
                        self.memory.io[GB_REG_NR52 as usize] &= !0x1;
                    }
                }

                if self.audio.ch2.control.length != 0 && self.audio.ch2.control.stop {
                    self.audio.ch2.control.length -= 1;
                    if self.audio.ch2.control.length == 0 {
                        self.audio.playing_ch2 = false;
                        self.memory.io[GB_REG_NR52 as usize] &= !0x2;
                    }
                }

                if self.audio.ch3.length != 0 && self.audio.ch3.stop {
                    self.audio.ch3.length -= 1;
                    if self.audio.ch3.length == 0 {
                        self.audio.playing_ch3 = false;
                        self.memory.io[GB_REG_NR52 as usize] &= !0x4;
                    }
                }

                if self.audio.ch4.length != 0 && self.audio.ch4.stop {
                    self.audio.ch4.length -= 1;
                    if self.audio.ch4.length == 0 {
                        self.audio.playing_ch4 = false;
                        self.memory.io[GB_REG_NR52 as usize] &= !0x8;
                    }
                }
            }
            7 => {
                if self.audio.playing_ch1 && self.audio.ch1.envelope.dead == 0 {
                    self.audio.ch1.envelope.next_step -= 1;
                    if self.audio.ch1.envelope.next_step == 0 {
                        update_envelope(&mut self.audio.ch1.envelope);
                        update_square_sample(&mut self.audio.ch1);
                    }
                }

                if self.audio.playing_ch2 && self.audio.ch2.envelope.dead == 0 {
                    self.audio.ch2.envelope.next_step -= 1;
                    if self.audio.ch2.envelope.next_step == 0 {
                        update_envelope(&mut self.audio.ch2.envelope);
                        update_square_sample(&mut self.audio.ch2);
                    }
                }

                if self.audio.playing_ch4 && self.audio.ch4.envelope.dead == 0 {
                    self.audio.ch4.envelope.next_step -= 1;
                    if self.audio.ch4.envelope.next_step == 0 {
                        let sample = self.audio.ch4.sample;
                        update_envelope(&mut self.audio.ch4.envelope);
                        self.audio.ch4.sample =
                            ((sample > 0) as i32 * self.audio.ch4.envelope.current_volume) as i8;
                        if self.audio.ch4.n_samples != 0 {
                            self.audio.ch4.samples -= sample as i32;
                            self.audio.ch4.samples += self.audio.ch4.sample as i32;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// GBAudioRun
    pub fn audio_run(&mut self, when: i32, mask: u8) {
        self.audio.run(when, mask as i32);
    }

    /// _updateFrame's dispatch target. In the C GB core the frame sequencer
    /// event callback body is GBA-only (_updateFrame in audio.c is compiled
    /// for M_CORE_GBA); GBUpdateFrame is driven by the timer instead. Nothing
    /// schedules EventId::AudioFrame in this port; if it ever fires, clock
    /// the frame sequencer.
    pub fn audio_frame_event(&mut self, _timing: &mut Timing, _cycles_late: i32) {
        self.audio_update_frame();
    }

    /// _sample: the sample event callback. The C locks the sync object,
    /// writes currentSamples into the mAudioBuffer, notifies the stream and
    /// reschedules; here the batch goes straight into the ring.
    pub fn audio_sample_event(&mut self, timing: &mut Timing, cycles_late: i32) {
        let now = timing.current_time();
        self.audio.sample(now);
        for i in 0..GB_MAX_SAMPLES {
            let sample = self.audio.current_samples[i];
            self.audio.buffer.write_stereo(sample.left, sample.right);
        }
        let next = self.audio.sample_interval * self.audio.timing_factor - cycles_late;
        timing.schedule(
            EventId::AudioSample.into(),
            EventId::AudioSample.priority(),
            next,
        );
    }

    /// There is no GBAudioSetSampleInterval in the C GB core (the rate is
    /// hard-wired: sampleInterval = SAMPLE_INTERVAL * GB_MAX_SAMPLES); like
    /// _sample's reschedule, the event period is always
    /// sampleInterval * timingFactor, so keep it in sync on change.
    pub fn audio_set_sample_interval(&mut self, interval: i32) {
        self.audio.sample_interval = interval;
        self.deschedule(EventId::AudioSample);
        let next = interval * self.audio.timing_factor;
        self.schedule(EventId::AudioSample, next);
    }
}
