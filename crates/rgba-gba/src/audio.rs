// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gba/audio.c and include/mgba/internal/gba/audio.h,
// with the PSG (GBAudio in GB_AUDIO_GBA style) from mgba/src/gb/audio.c and
// include/mgba/internal/gb/audio.h.
//
// The GBA audio subsystem: the GB-style PSG (square 1 with sweep, square 2,
// wave channel with 2 banks and a 64-sample window, noise) in GB_AUDIO_GBA
// style (timingFactor 4, 3-bit wave volume, wavedata32 nibble stepping,
// envelope step reload, no DC offset / noise coalescing at mix time), plus
// the two DMA FIFO channels A/B, the SOUNDCNT/SOUNDBIAS mixer with the
// hardware DC bias, and the sample event.
//
// Differences forced by the Rust port (no behavior change intended):
// - `audio->p` back-pointer: the console owns the audio state, so GBAAudio
//   fields are read via the `Gba` receiver. `audio->psg.p` is NULL in the C
//   (GBAAudioInit), which has two consequences baked in here:
//   * GBAudioRun's `audio->p && channels != 0x1F` catch-up of the PSG's
//     private GBAudioSample never fires; that dead path (and the
//     capLeft/capRight DC filter it feeds) is not ported. The fields exist
//     for state parity but are inert.
//   * GBAudioWriteNR52's skipFrame latch and the io[] clears guarded by
//     `audio->p` never run (GBAAudioWriteSOUNDCNT_X does its own clears).
// - `audio->psg.nr52` (a pointer to the low byte of
//   memory.io[GBA_REG(SOUNDCNT_X)]) becomes direct access to
//   `memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize]` bits 0..4.
// - The mAudioBuffer + mCoreSync/stream production layer is replaced by
//   `buffer: RingI16` on `Audio` (this is the C's shared
//   `audio->psg.buffer`); the sample event pushes each batch with
//   `buffer.write_stereo(left, right)`. Ring capacity is
//   AUDIO_BUFFER_SAMPLES * 2 i16 samples — the C allocates
//   samples * channels * sizeof(int16_t).
// - `struct mTimingEvent sampleEvent` is folded into
//   `EventId::AudioSample` (priority 0x18).
// - `struct mTimingEvent psg.frameEvent` (_updateFrame, priority 0x10,
//   rescheduled every psg.timingFactor * FRAME_CYCLES = 4 * 8192 = 32768
//   master cycles) is folded into the sample event: the C schedules both
//   events with delay 0 in GBAAudioReset and re-arms them on fixed grids, so
//   the frame event always fires exactly every 32nd sample event
//   (32768 / SAMPLE_INTERVAL(1024)), strictly before it (0x10 < 0x18).
//   `frame_phase` counts sample events; when it hits 0 the sample event
//   first runs GBAAudioSample + GBAudioUpdateFrame (exactly the C frame
//   event's body, with the real current timestamp), then the normal sample
//   event body.
// - The C's wavedata32[8]/wavedata8[16] union is stored as wavedata32; the
//   single GB-style byte access left in GBAudioWriteNR32 goes through
//   `wave_byte()` (little-endian byte view of the union).
//
// Sample rates (mgba/src/gba/core.c _GBACoreAudioSampleRate returns
// GBA_ARM7TDMI_FREQUENCY / audio->sampleInterval): the default sample
// interval from GBAAudioInit/GBAAudioReset is
// GBA_ARM7TDMI_FREQUENCY / 0x8000 = 0x1000000 / 32768 = 512 master cycles,
// i.e. 32768 Hz at the default SOUNDBIAS resolution of 0. The sample event
// itself is scheduled every SAMPLE_INTERVAL = GBA_ARM7TDMI_FREQUENCY /
// 0x4000 = 1024 master cycles (gba/audio.c _sample), producing
// 2 << resolution samples per firing spaced sample_interval apart.

use rgba_core::ring::RingI16;
use rgba_core::{mlog, Level};

use crate::gba::{EventId, Gba, GBA_ARM7TDMI_FREQUENCY};
use crate::io::regs::{
    GBA_REG_FIFO_A_LO, GBA_REG_FIFO_B_LO, GBA_REG_SOUND1CNT_LO, GBA_REG_SOUNDCNT_HI,
    GBA_REG_SOUNDCNT_X,
};
use crate::memory::GBA_BASE_IO;

/// GB_MAX_SAMPLES (gb/audio.h) — size of the PSG's inert private batch buffer
pub const GB_MAX_SAMPLES: usize = 32;
/// GBA_MAX_SAMPLES (gba/audio.h)
pub const GBA_MAX_SAMPLES: usize = 16;
/// GBA_AUDIO_FIFO_SIZE
pub const GBA_AUDIO_FIFO_SIZE: usize = 8;
/// GBA_AUDIO_SAMPLES (gba/audio.c) — default `samples`
pub const GBA_AUDIO_SAMPLES: usize = 2048;
/// GBA_AUDIO_VOLUME_MAX
pub const GBA_AUDIO_VOLUME_MAX: i32 = 0x100;
/// GB_AUDIO_VOLUME_MAX (gb/audio.c)
pub const GB_AUDIO_VOLUME_MAX: i32 = 0x100;
/// AUDIO_BUFFER_SAMPLES (gb/audio.c mAudioBufferInit capacity, per channel)
const AUDIO_BUFFER_SAMPLES: usize = 0x4000;
/// SAMPLE_INTERVAL (gba/audio.c): master cycles between sample events.
const SAMPLE_INTERVAL: i32 = (GBA_ARM7TDMI_FREQUENCY / 0x4000) as i32;
/// DMG_SM83_FREQUENCY (gb/audio.c), for the frame sequencer cadence
const DMG_SM83_FREQUENCY: i32 = 0x400000;
/// FRAME_CYCLES (gb/audio.c)
const FRAME_CYCLES: i32 = DMG_SM83_FREQUENCY >> 9;
/// Sample events between frame sequencer ticks:
/// psg.timingFactor * FRAME_CYCLES / SAMPLE_INTERVAL = 4 * 8192 / 1024 = 32.
const FRAME_EVENT_PHASE_MOD: i32 = 4 * FRAME_CYCLES / SAMPLE_INTERVAL;

/// enum GBADMAControl GBA_DMA_FIXED (dma.h; local copy — dma.c not yet ported)
const GBA_DMA_FIXED: u16 = 2;
/// enum GBADMATiming GBA_DMA_TIMING_CUSTOM
const GBA_DMA_TIMING_CUSTOM: u16 = 3;

/// GBADMARegisterGetTiming (DECL_BITS(GBADMARegister, Timing, 12, 2))
#[inline]
fn gba_dma_register_get_timing(reg: u16) -> u16 {
    (reg >> 12) & 0x3
}

/// GBADMARegisterIsEnable (DECL_BIT(GBADMARegister, Enable, 15))
#[inline]
fn gba_dma_register_is_enable(reg: u16) -> bool {
    reg & (1 << 15) != 0
}

/// GBADMARegisterSetDestControl (DECL_BITS(GBADMARegister, DestControl, 5, 2))
#[inline]
fn gba_dma_register_set_dest_control(reg: u16, control: u16) -> u16 {
    (reg & !(0x3 << 5)) | ((control & 0x3) << 5)
}

/// GBADMARegisterSetWidth (DECL_BIT(GBADMARegister, Width, 10))
#[inline]
fn gba_dma_register_set_width(reg: u16, width: u16) -> u16 {
    (reg & !(1 << 10)) | ((width & 1) << 10)
}

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

// Register bitfield accessors (DECL_BITFIELD in gb/audio.h + gba/audio.h),
// as packed-int getters mirroring the C-generated names:

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

/// GBAudioRegisterBankGetSize (DECL_BIT(GBAudioRegisterBank, Size, 5))
#[inline]
fn gb_audio_register_bank_get_size(v: u8) -> bool {
    v & 0x20 != 0
}

/// GBAudioRegisterBankGetBank (DECL_BIT(GBAudioRegisterBank, Bank, 6))
#[inline]
fn gb_audio_register_bank_get_bank(v: u8) -> bool {
    v & 0x40 != 0
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

/// GBAudioRegisterBankVolumeGetVolumeGBA (DECL_BITS(GBAudioRegisterBankVolume, VolumeGBA, 5, 3))
#[inline]
fn gb_audio_register_bank_volume_get_volume_gba(v: u8) -> i32 {
    ((v >> 5) & 0x7) as i32
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

/// GBARegisterSOUNDBIASGetBias (DECL_BITS(GBARegisterSOUNDBIAS, Bias, 0, 10))
#[inline]
fn gba_register_soundbias_get_bias(v: u16) -> i32 {
    (v & 0x3FF) as i32
}

/// GBARegisterSOUNDBIASGetResolution (DECL_BITS(GBARegisterSOUNDBIAS, Resolution, 14, 2))
#[inline]
fn gba_register_soundbias_get_resolution(v: u16) -> i32 {
    ((v >> 14) & 0x3) as i32
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
/// stored as wavedata32 (the GB_AUDIO_GBA stepping model rotates nibbles
/// through the 32-bit words); GB-style byte reads (GBAudioWriteNR32) use the
/// `wave_byte` helper.
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
    pub wavedata32: [u32; 8],
    pub next_update: i32,
}

/// Little-endian byte view of the wavedata32 union (C: ch3.wavedata8[i]).
#[inline]
fn wave_byte(ch3: &GbAudioWaveChannel, i: usize) -> u8 {
    (ch3.wavedata32[i >> 2] >> ((i & 3) * 8)) as u8
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

/// struct GBAudioFIFO
#[derive(Default)]
pub struct Fifo {
    pub fifo: [u32; GBA_AUDIO_FIFO_SIZE],
    pub fifo_write: i32,
    pub fifo_read: i32,
    pub internal_sample: u32,
    pub internal_remaining: i32,
    pub dma_source: i32,
    pub samples: [i8; GBA_MAX_SAMPLES],
}

/// struct GBAudio (gb/audio.h) instantiated with style GB_AUDIO_GBA — the
/// PSG half of GBAAudio. The C's `p`/`timing`/`nr52` back-pointers, the
/// frameEvent slot and the sample-event slot are replaced by `impl Gba`
/// methods / EventId dispatch (see the file header comment).
pub struct Psg {
    pub timing_factor: i32,
    pub ch1: GbAudioSquareChannel,
    pub ch2: GbAudioSquareChannel,
    pub ch3: GbAudioWaveChannel,
    pub ch4: GbAudioNoiseChannel,

    // Inert on GB_AUDIO_GBA (psg.p == NULL gates the private sample path
    // that would use them); kept for state parity with the C struct.
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
    pub skip_frame: bool,

    // sample_interval/last_sample/sample_index/current_samples belong to
    // the PSG's private batch sampler, which is dead on GB_AUDIO_GBA (the
    // GBAAudio sample path drives everything); kept for state parity.
    pub sample_interval: i32,
    pub last_sample: i32,
    pub sample_index: usize,
    pub current_samples: [StereoSample; GB_MAX_SAMPLES],

    pub enable: bool,

    pub samples: usize,
    pub force_disable_ch: [bool; 4],
    pub master_volume: i32,
}

/// struct GBAAudio (gba/audio.h). The C's `p` back-pointer and the
/// mTimingEvent slot are replaced by `impl Gba` methods / EventId dispatch.
pub struct Audio {
    pub psg: Psg,
    pub ch_a: Fifo,
    pub ch_b: Fifo,

    pub volume: u8,
    pub volume_ch_a: bool,
    pub volume_ch_b: bool,
    pub ch_a_right: bool,
    pub ch_a_left: bool,
    pub ch_a_timer: bool,
    pub ch_b_right: bool,
    pub ch_b_left: bool,
    pub ch_b_timer: bool,
    pub enable: bool,

    pub samples: usize,
    pub soundbias: u16,

    pub sample_interval: i32,

    pub last_sample: i32,
    pub sample_index: usize,
    pub current_samples: [StereoSample; GBA_MAX_SAMPLES],

    pub force_disable_ch_a: bool,
    pub force_disable_ch_b: bool,
    pub master_volume: i32,

    /// The C's shared mAudioBuffer (audio->psg.buffer): interleaved stereo.
    pub buffer: RingI16,

    /// Counts sample events to fold the C's psg.frameEvent (priority 0x10,
    /// period 32768 master cycles) into the sample event; see header comment.
    pub frame_phase: i32,
}

const DEFAULT_SAMPLE_INTERVAL: i32 = (GBA_ARM7TDMI_FREQUENCY / 0x8000) as i32;

impl Psg {
    /// GBAudioInit(audio, samples, nr52, GB_AUDIO_GBA) + the channel state
    /// left by GBAudioReset (sweep.time = 8, envelopes dead = 2), so the
    /// state is valid before the first Gba::audio_reset.
    fn new() -> Self {
        Psg {
            timing_factor: 4, // GB_AUDIO_GBA
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
            skip_frame: false,
            sample_interval: DEFAULT_SAMPLE_INTERVAL,
            last_sample: 0,
            sample_index: 0,
            current_samples: [StereoSample::default(); GB_MAX_SAMPLES],
            enable: false,
            samples: GBA_AUDIO_SAMPLES,
            force_disable_ch: [false; 4],
            master_volume: GB_AUDIO_VOLUME_MAX,
        }
    }

    /// GBAudioRun (GB_AUDIO_GBA). In the C the initial catch-up call to the
    /// PSG's private GBAudioSample is guarded by `audio->p &&`, and
    /// GBAAudioInit sets `audio->psg.p = NULL`, so that block never runs in
    /// this core and is not ported (GBAAudioSample drives sampling instead).
    fn run(&mut self, timestamp: i32, channels: i32) {
        if !self.enable {
            return;
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
                // GB_AUDIO_GBA: nibble-rotate the active wave bank(s) through
                // the 64-sample (or 32-sample) window.
                let mut start = 7;
                let mut end = 0;
                let mut mask = 0x1F;
                if self.ch3.size {
                    mask = 0x3F;
                } else if self.ch3.bank {
                    end = 4;
                } else {
                    start = 3;
                }
                let mut iter = 0;
                while iter < (diff & mask) {
                    let mut bits_carry = self.ch3.wavedata32[end as usize] & 0x000000F0;
                    let mut i = start;
                    while i >= end {
                        let bits = self.ch3.wavedata32[i as usize] & 0x000000F0;
                        self.ch3.wavedata32[i as usize] = ((self.ch3.wavedata32[i as usize]
                            & 0x0F0F0F0F)
                            << 4)
                            | ((self.ch3.wavedata32[i as usize] & 0xF0F0F000) >> 12);
                        self.ch3.wavedata32[i as usize] |= bits_carry << 20;
                        bits_carry = bits;
                        i -= 1;
                    }
                    self.ch3.sample = (bits_carry >> 4) as i8;
                    iter += 1;
                }
                if self.ch3.volume > 3 {
                    // AGB 75% volume
                    self.ch3.sample = self.ch3.sample.wrapping_add(self.ch3.sample << 1);
                }
                self.ch3.sample = self.ch3.sample.wrapping_shr(volume as u32);
                self.ch3.next_update = self.ch3.next_update.wrapping_add(diff * cycles);
                self.ch3.readable = true;
            }
            // The `style == GB_AUDIO_DMG && readable` re-lock check is false
            // on GB_AUDIO_GBA; not ported.
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

    /// GBAudioSamplePSG (GB_AUDIO_GBA: no DC offset, noise mixed as
    /// `ch4.sample << 3` without _coalesceNoiseChannel).
    fn sample_psg(&self) -> (i16, i16) {
        let dc_offset: i32 = 0;
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
            let sample = (self.ch4.sample as i32) << 3;
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

impl Audio {
    /// GBAAudioInit(audio, 2048) + the state left by GBAAudioReset, so a
    /// never-reset Audio matches a reset one.
    ///
    /// Buffer capacity: mAudioBufferInit(&audio->psg.buffer,
    /// AUDIO_BUFFER_SAMPLES, 2) allocates AUDIO_BUFFER_SAMPLES * 2 i16
    /// samples; RingI16's capacity is in i16 samples, hence the *2.
    pub fn new() -> Self {
        Audio {
            psg: Psg::new(),
            ch_a: Fifo {
                dma_source: 1,
                ..Default::default()
            },
            ch_b: Fifo {
                dma_source: 2,
                ..Default::default()
            },
            volume: 0,
            volume_ch_a: false,
            volume_ch_b: false,
            ch_a_right: false,
            ch_a_left: false,
            ch_a_timer: false,
            ch_b_right: false,
            ch_b_left: false,
            ch_b_timer: false,
            enable: false,
            samples: GBA_AUDIO_SAMPLES,
            soundbias: 0x200,
            sample_interval: DEFAULT_SAMPLE_INTERVAL,
            last_sample: 0,
            sample_index: 0,
            current_samples: [StereoSample::default(); GBA_MAX_SAMPLES],
            force_disable_ch_a: false,
            force_disable_ch_b: false,
            master_volume: GBA_AUDIO_VOLUME_MAX,
            buffer: RingI16::new(AUDIO_BUFFER_SAMPLES * 2),
            frame_phase: 0,
        }
    }

    /// GBAAudioDeinit/GBAudioDeinit — the ring frees itself (mirrors the C).
    pub fn deinit(&mut self) {}

    /// _applyBias
    fn apply_bias(&self, sample: i32) -> i32 {
        let mut sample = sample + gba_register_soundbias_get_bias(self.soundbias);
        if sample >= 0x400 {
            sample = 0x3FF;
        } else if sample < 0 {
            sample = 0;
        }
        ((sample - gba_register_soundbias_get_bias(self.soundbias)) * self.master_volume * 3) >> 4
    }

    /// GBAAudioSample: fill current_samples[] up to the resolution-dependent
    /// batch size (`2 << resolution`), mixing at every sample_interval
    /// master cycles.
    fn sample(&mut self, timestamp: i32) {
        let mut timestamp = timestamp.wrapping_sub(self.last_sample);
        // TODO: This can break if the interval changes between samples
        timestamp = timestamp.wrapping_sub(self.sample_index as i32 * self.sample_interval);

        let max_sample = (2 << gba_register_soundbias_get_resolution(self.soundbias)) as usize;
        let mut sample = self.sample_index;
        while timestamp >= self.sample_interval && sample < max_sample {
            let psg_shift = 4 - self.volume as i32;
            self.psg.run(
                (sample as i32)
                    .wrapping_mul(self.sample_interval)
                    .wrapping_add(self.last_sample),
                0xF,
            );
            // GBAudioSamplePSG(&psg, &sampleLeft, &sampleRight) writes into
            // the int16_t out-params, then the C shifts them in place.
            let (l, r) = self.psg.sample_psg();
            let mut sample_left = l >> psg_shift;
            let mut sample_right = r >> psg_shift;

            if !self.force_disable_ch_a {
                if self.ch_a_left {
                    sample_left = sample_left.wrapping_add(
                        ((self.ch_a.samples[sample] as i32) << 2 >> (!self.volume_ch_a) as i32)
                            as i16,
                    );
                }
                if self.ch_a_right {
                    sample_right = sample_right.wrapping_add(
                        ((self.ch_a.samples[sample] as i32) << 2 >> (!self.volume_ch_a) as i32)
                            as i16,
                    );
                }
            }

            if !self.force_disable_ch_b {
                if self.ch_b_left {
                    sample_left = sample_left.wrapping_add(
                        ((self.ch_b.samples[sample] as i32) << 2 >> (!self.volume_ch_b) as i32)
                            as i16,
                    );
                }
                if self.ch_b_right {
                    sample_right = sample_right.wrapping_add(
                        ((self.ch_b.samples[sample] as i32) << 2 >> (!self.volume_ch_b) as i32)
                            as i16,
                    );
                }
            }

            sample_left = self.apply_bias(sample_left as i32) as i16;
            sample_right = self.apply_bias(sample_right as i32) as i16;
            self.current_samples[sample].left = sample_left;
            self.current_samples[sample].right = sample_right;

            sample += 1;
            timestamp -= self.sample_interval;
        }

        self.sample_index = sample;
        if sample == max_sample {
            self.last_sample = self.last_sample.wrapping_add(SAMPLE_INTERVAL);
            self.sample_index = 0;
        }
    }

    /// Pick chA/chB by fifo id (GBAAudioSampleFIFO's fifoId).
    fn channel(&mut self, fifo_id: i32) -> &mut Fifo {
        if fifo_id == 0 {
            &mut self.ch_a
        } else {
            &mut self.ch_b
        }
    }

    /// timer.c GBATimerUpdate: `(gba->audio.chARight || gba->audio.chALeft)`
    pub fn ch_a_left_or_right(&self) -> bool {
        self.ch_a_left || self.ch_a_right
    }

    /// timer.c GBATimerUpdate: `gba->audio.chATimer` (bool as the timer index)
    pub fn ch_a_timer(&self) -> u32 {
        self.ch_a_timer as u32
    }

    /// timer.c GBATimerUpdate: `(gba->audio.chBRight || gba->audio.chBLeft)`
    pub fn ch_b_left_or_right(&self) -> bool {
        self.ch_b_left || self.ch_b_right
    }

    /// timer.c GBATimerUpdate: `gba->audio.chBTimer` (bool as the timer index)
    pub fn ch_b_timer(&self) -> u32 {
        self.ch_b_timer as u32
    }

    /// The `ch3.volume` half of GBAAudioWriteSOUND3CNT_HI: sets the 3-bit
    /// AGB wave volume register directly. Note this does NOT go through
    /// GBAudioWriteNR32, so unlike on GB the current sample is not rescaled.
    pub fn ch3_volume_gba(&mut self, value: u8) {
        self.psg.ch3.volume = gb_audio_register_bank_volume_get_volume_gba(value);
    }
}

impl Default for Audio {
    fn default() -> Self {
        Self::new()
    }
}

/// _resetEnvelope (GB_AUDIO_GBA)
fn reset_envelope(envelope: &mut GbAudioEnvelope) -> bool {
    envelope.current_volume = envelope.initial_volume;
    envelope.next_step = envelope.step_time;
    update_envelope_dead(envelope);
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

/// _writeEnvelope (GB_AUDIO_GBA: neither the DMG nor the CGB "zombie" mode
/// branch applies; volume is just masked).
fn write_envelope(envelope: &mut GbAudioEnvelope, value: u8) -> bool {
    envelope.step_time = gb_audio_register_sweep_get_step_time(value);
    envelope.direction = gb_audio_register_sweep_get_direction(value);
    envelope.initial_volume = gb_audio_register_sweep_get_initial_volume(value);
    if envelope.step_time == 0 {
        // TODO: Improve "zombie" mode
        envelope.current_volume &= 0xF;
    }
    update_envelope_dead(envelope);
    envelope.initial_volume != 0 || envelope.direction
}

/// _updateSquareSample
fn update_square_sample(ch: &mut GbAudioSquareChannel) {
    ch.sample = (SQUARE_CHANNEL_DUTY[ch.envelope.duty as usize][ch.index as usize]
        * ch.envelope.current_volume) as i8;
}

// _coalesceNoiseChannel is only used by GBAudioSamplePSG for styles other
// than GB_AUDIO_GBA; not ported.

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

/// _updateEnvelopeDead (GB_AUDIO_GBA reloads nextStep when undeadening)
fn update_envelope_dead(envelope: &mut GbAudioEnvelope) {
    if envelope.step_time == 0 {
        envelope.dead = if envelope.current_volume != 0 { 1 } else { 2 };
    } else if !envelope.direction && envelope.current_volume == 0 {
        envelope.dead = 2;
    } else if envelope.direction && envelope.current_volume == 0xF {
        envelope.dead = 1;
    } else if envelope.dead != 0 {
        // TODO: Figure out if this happens on DMG/CGB or just AGB
        // TODO: Figure out the exact circumstances that lead to reloading the step
        // GB_AUDIO_GBA:
        envelope.next_step = envelope.step_time;
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

impl Gba {
    /// GBAAudioReset
    pub fn audio_reset(&mut self) {
        // GBAudioReset(&audio->psg):
        self.audio.psg.ch1 = GbAudioSquareChannel {
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
        self.audio.psg.ch2 = GbAudioSquareChannel {
            envelope: GbAudioEnvelope {
                dead: 2,
                ..Default::default()
            },
            ..Default::default()
        };
        self.audio.psg.ch3 = GbAudioWaveChannel::default();
        // TODO: DMG randomness
        // The `style != GB_AUDIO_GBA` wave-RAM pattern fill is skipped on
        // GB_AUDIO_GBA; wavedata stays zeroed.
        self.audio.psg.ch4 = GbAudioNoiseChannel {
            envelope: GbAudioEnvelope {
                dead: 2,
                ..Default::default()
            },
            ..Default::default()
        };
        self.audio.psg.frame = 0;
        // GBAudioReset sets psg.sampleInterval = SAMPLE_INTERVAL *
        // GB_MAX_SAMPLES here; GBAAudioReset forces it to
        // GBA_ARM7TDMI_FREQUENCY / 0x8000 below, so set the final value.
        self.audio.psg.sample_interval = DEFAULT_SAMPLE_INTERVAL;
        self.audio.psg.last_sample = 0;
        self.audio.psg.sample_index = 0;
        self.audio.psg.cap_left = 0;
        self.audio.psg.cap_right = 0;
        self.audio.buffer.clear();
        self.audio.psg.playing_ch1 = false;
        self.audio.psg.playing_ch2 = false;
        self.audio.psg.playing_ch3 = false;
        self.audio.psg.playing_ch4 = false;
        // The `audio->p && !(audio->p->model & GB_MODEL_SGB)` block that
        // would set playingCh1/enable/*nr52 is skipped: psg.p is NULL.

        // The C reschedules psg.frameEvent (delay 0) then the sample event
        // (delay 0); the frame event is folded into the sample event.
        self.audio.frame_phase = 0;
        self.deschedule(EventId::AudioSample);
        self.schedule(EventId::AudioSample, 0);

        self.audio.ch_a.dma_source = 1;
        self.audio.ch_b.dma_source = 2;
        self.audio.ch_a.fifo_write = 0;
        self.audio.ch_a.fifo_read = 0;
        self.audio.ch_a.internal_sample = 0;
        self.audio.ch_a.internal_remaining = 0;
        self.audio.ch_a.fifo = [0; GBA_AUDIO_FIFO_SIZE];
        self.audio.ch_b.fifo_write = 0;
        self.audio.ch_b.fifo_read = 0;
        self.audio.ch_b.internal_sample = 0;
        self.audio.ch_b.internal_remaining = 0;
        self.audio.ch_b.fifo = [0; GBA_AUDIO_FIFO_SIZE];
        self.audio.ch_a.samples = [0; GBA_MAX_SAMPLES];
        self.audio.ch_b.samples = [0; GBA_MAX_SAMPLES];
        self.audio.soundbias = 0x200;
        self.audio.volume = 0;
        self.audio.volume_ch_a = false;
        self.audio.volume_ch_b = false;
        self.audio.last_sample = 0;
        self.audio.sample_index = 0;
        self.audio.ch_a_right = false;
        self.audio.ch_a_left = false;
        self.audio.ch_a_timer = false;
        self.audio.ch_b_right = false;
        self.audio.ch_b_left = false;
        self.audio.ch_b_timer = false;
        self.audio.enable = false;
        // GBAAudioInit/GBAAudioReset pin the sample interval at
        // GBA_ARM7TDMI_FREQUENCY / 0x8000 (the stream audioRateChanged
        // notification has no stream layer here).
        self.audio.sample_interval = DEFAULT_SAMPLE_INTERVAL;
        self.audio.psg.sample_interval = self.audio.sample_interval;
    }

    /// GBAAudioResizeBuffer (the C's mCoreSync lock/consume is frontend
    /// thread glue; this port has no sync layer).
    pub fn audio_resize_buffer(&mut self, samples: usize) {
        self.audio.samples = samples;
        self.audio.psg.samples = samples;
    }

    /// GBAAudioWriteSOUND1CNT_LO
    pub fn audio_write_sound1cnt_lo(&mut self, value: u16) {
        let now = self.current_time();
        self.audio_sample(now);
        self.audio_write_nr10(value as u8);
    }

    /// GBAAudioWriteSOUND1CNT_HI
    pub fn audio_write_sound1cnt_hi(&mut self, value: u16) {
        let now = self.current_time();
        self.audio_sample(now);
        self.audio_write_nr11(value as u8);
        self.audio_write_nr12((value >> 8) as u8);
    }

    /// GBAAudioWriteSOUND1CNT_X
    pub fn audio_write_sound1cnt_x(&mut self, value: u16) {
        let now = self.current_time();
        self.audio_sample(now);
        self.audio_write_nr13(value as u8);
        self.audio_write_nr14((value >> 8) as u8);
    }

    /// GBAAudioWriteSOUND2CNT_LO
    pub fn audio_write_sound2cnt_lo(&mut self, value: u16) {
        let now = self.current_time();
        self.audio_sample(now);
        self.audio_write_nr21(value as u8);
        self.audio_write_nr22((value >> 8) as u8);
    }

    /// GBAAudioWriteSOUND2CNT_HI
    pub fn audio_write_sound2cnt_hi(&mut self, value: u16) {
        let now = self.current_time();
        self.audio_sample(now);
        self.audio_write_nr23(value as u8);
        self.audio_write_nr24((value >> 8) as u8);
    }

    /// GBAAudioWriteSOUND3CNT_LO
    pub fn audio_write_sound3cnt_lo(&mut self, value: u16) {
        let now = self.current_time();
        self.audio_sample(now);
        self.audio.psg.ch3.size = gb_audio_register_bank_get_size(value as u8);
        self.audio.psg.ch3.bank = gb_audio_register_bank_get_bank(value as u8);
        self.audio_write_nr30(value as u8);
    }

    /// GBAAudioWriteSOUND3CNT_HI
    pub fn audio_write_sound3cnt_hi(&mut self, value: u16) {
        let now = self.current_time();
        self.audio_sample(now);
        self.audio_write_nr31(value as u8);
        self.audio.ch3_volume_gba((value >> 8) as u8);
    }

    /// GBAAudioWriteSOUND3CNT_X
    pub fn audio_write_sound3cnt_x(&mut self, value: u16) {
        let now = self.current_time();
        self.audio_sample(now);
        self.audio_write_nr33(value as u8);
        self.audio_write_nr34((value >> 8) as u8);
    }

    /// GBAAudioWriteSOUND4CNT_LO
    pub fn audio_write_sound4cnt_lo(&mut self, value: u16) {
        let now = self.current_time();
        self.audio_sample(now);
        self.audio_write_nr41(value as u8);
        self.audio_write_nr42((value >> 8) as u8);
    }

    /// GBAAudioWriteSOUND4CNT_HI
    pub fn audio_write_sound4cnt_hi(&mut self, value: u16) {
        let now = self.current_time();
        self.audio_sample(now);
        self.audio_write_nr43(value as u8);
        self.audio_write_nr44((value >> 8) as u8);
    }

    /// GBAAudioWriteSOUNDCNT_LO
    pub fn audio_write_soundcnt_lo(&mut self, value: u16) {
        let now = self.current_time();
        self.audio_sample(now);
        self.audio_write_nr50(value as u8);
        self.audio_write_nr51((value >> 8) as u8);
    }

    /// GBAAudioWriteSOUNDCNT_HI
    pub fn audio_write_soundcnt_hi(&mut self, value: u16) {
        self.audio.volume = (value & 0x3) as u8;
        self.audio.volume_ch_a = value & 0x0004 != 0;
        self.audio.volume_ch_b = value & 0x0008 != 0;
        self.audio.ch_a_right = value & 0x0100 != 0;
        self.audio.ch_a_left = value & 0x0200 != 0;
        self.audio.ch_a_timer = value & 0x0400 != 0;
        if value & 0x0800 != 0 {
            self.audio.ch_a.fifo_write = 0;
            self.audio.ch_a.fifo_read = 0;
        }
        self.audio.ch_b_right = value & 0x1000 != 0;
        self.audio.ch_b_left = value & 0x2000 != 0;
        self.audio.ch_b_timer = value & 0x4000 != 0;
        if value & 0x8000 != 0 {
            self.audio.ch_b.fifo_write = 0;
            self.audio.ch_b.fifo_read = 0;
        }
    }

    /// GBAAudioWriteSOUNDCNT_X
    pub fn audio_write_soundcnt_x(&mut self, value: u16) {
        let now = self.current_time();
        self.audio_sample(now);
        self.audio.enable = gb_audio_enable_get_enable(value as u8);
        self.audio_write_nr52(value as u8);
        if !self.audio.enable {
            let mut i = GBA_REG_SOUND1CNT_LO;
            while i < GBA_REG_SOUNDCNT_HI {
                self.memory.io[(i >> 1) as usize] = 0;
                i += 2;
            }
            self.audio.psg.ch3.size = false;
            self.audio.psg.ch3.bank = false;
            self.audio.psg.ch3.volume = 0;
            self.audio.volume = 0;
            self.audio.volume_ch_a = false;
            self.audio.volume_ch_b = false;
            self.memory.io[(GBA_REG_SOUNDCNT_HI >> 1) as usize] &= 0xFF00;
        }
    }

    /// GBAAudioWriteSOUNDBIAS
    pub fn audio_write_soundbias(&mut self, value: u16) {
        let timestamp = self.current_time();
        self.audio_sample(timestamp);
        self.audio.soundbias = value;
        let old_sample_interval = self.audio.sample_interval;
        self.audio.sample_interval = 0x200 >> gba_register_soundbias_get_resolution(value);
        if old_sample_interval != self.audio.sample_interval {
            let timestamp = timestamp.wrapping_sub(self.audio.last_sample);
            self.audio.sample_index = ((timestamp
                >> (9 - gba_register_soundbias_get_resolution(value)))
                as u32) as usize;
            if self.audio.sample_index >= GBA_MAX_SAMPLES {
                self.audio.sample_index = 0;
            }
            // The C notifies stream->audioRateChanged here; no stream layer.
        }
    }

    /// GBAAudioWriteWaveRAM. `bank` is the C's `address` parameter (the
    /// 0..3 word index within the selected bank's 4 words).
    pub fn audio_write_waveram(&mut self, bank: i32, value: u32) {
        let mut bank_sel = !self.audio.psg.ch3.bank as i32;

        // When the audio hardware is turned off, it acts like bank 0 has
        // been selected in SOUND3CNT_L, so any read comes from bank 1.
        if !self.audio.enable {
            bank_sel = 1;
        }

        let now = self.current_time();
        self.audio.psg.run(now, 0x4);
        self.audio.psg.ch3.wavedata32[(bank | (bank_sel * 4)) as usize] = value;
    }

    /// GBAAudioReadWaveRAM
    pub fn audio_read_waveram(&mut self, bank: i32) -> u32 {
        let mut bank_sel = !self.audio.psg.ch3.bank as i32;

        // When the audio hardware is turned off, it acts like bank 0 has
        // been selected in SOUND3CNT_L, so any read comes from bank 1.
        if !self.audio.enable {
            bank_sel = 1;
        }

        let now = self.current_time();
        self.audio.psg.run(now, 0x4);
        self.audio.psg.ch3.wavedata32[(bank | (bank_sel * 4)) as usize]
    }

    /// GBAAudioWriteFIFO. Returns the value actually stored in io (the C
    /// returns fifo[fifoWrite] *after* the increment/wrap, i.e. the stale
    /// entry at the new write pointer — kept as-is).
    pub fn audio_write_fifo(&mut self, address: u32, value: u32) -> u32 {
        let channel = match address {
            GBA_REG_FIFO_A_LO => &mut self.audio.ch_a,
            GBA_REG_FIFO_B_LO => &mut self.audio.ch_b,
            _ => {
                mlog!(
                    Level::Error,
                    rgba_core::log::GBA_AUDIO,
                    "Bad FIFO write to address 0x{:03x}",
                    address
                );
                return value;
            }
        };
        channel.fifo[channel.fifo_write as usize] = value;
        channel.fifo_write += 1;
        if channel.fifo_write == GBA_AUDIO_FIFO_SIZE as i32 {
            channel.fifo_write = 0;
        }
        channel.fifo[channel.fifo_write as usize]
    }

    /// GBAAudioSampleFIFO (called from the timer event when the associated
    /// timer overflows).
    pub fn audio_sample_fifo(&mut self, fifo_id: i32, cycles_late: u32) {
        if fifo_id != 0 && fifo_id != 1 {
            mlog!(
                Level::Error,
                rgba_core::log::GBA_AUDIO,
                "Bad FIFO write to address 0x{:03x}",
                fifo_id
            );
            return;
        }
        let (fifo_size, dma_source);
        {
            let channel = self.audio.channel(fifo_id);
            if channel.fifo_write >= channel.fifo_read {
                fifo_size = channel.fifo_write - channel.fifo_read;
            } else {
                fifo_size = GBA_AUDIO_FIFO_SIZE as i32 - channel.fifo_read + channel.fifo_write;
            }
            dma_source = channel.dma_source;
        }
        if GBA_AUDIO_FIFO_SIZE as i32 - fifo_size > 4 && dma_source > 0 {
            if gba_dma_register_get_timing(self.dma[dma_source as usize].reg) == GBA_DMA_TIMING_CUSTOM
            {
                self.dma[dma_source as usize].when =
                    self.current_time().wrapping_sub(cycles_late as i32);
                self.dma[dma_source as usize].next_count = 4;
                self.dma_recalculate_cycles();
                self.audio_schedule_fifo_dma(dma_source);
                self.dma_update();
            }
        }
        {
            let channel = self.audio.channel(fifo_id);
            if channel.internal_remaining == 0 && fifo_size != 0 {
                channel.internal_sample = channel.fifo[channel.fifo_read as usize];
                channel.internal_remaining = 4;
                channel.fifo_read += 1;
                if channel.fifo_read == GBA_AUDIO_FIFO_SIZE as i32 {
                    channel.fifo_read = 0;
                }
            }
        }
        // NOTE (no behavior change intended): mTimingUntil(&audio->p->timing,
        // &audio->sampleEvent) — but audio_sample_fifo runs inside a
        // Timing::tick callback, where `self.timing` is the emptied shell
        // (process_events takes it out), so `until` degenerates to
        // i32::MAX and the whole samples[] window is filled with the
        // internal sample. The wrapping ops mirror the C int32 arithmetic.
        let mut until = self.until(EventId::AudioSample).wrapping_sub(1);
        let bits = 2 << gba_register_soundbias_get_resolution(self.audio.soundbias);
        until = until.wrapping_add(1 << (9 - gba_register_soundbias_get_resolution(
            self.audio.soundbias,
        )));
        until >>= 9 - gba_register_soundbias_get_resolution(self.audio.soundbias);
        if bits < until {
            until = bits;
        }
        let channel = self.audio.channel(fifo_id);
        let mut i = bits - until;
        while i < bits {
            channel.samples[i as usize] = channel.internal_sample as i8;
            i += 1;
        }
        if channel.internal_remaining != 0 {
            channel.internal_sample >>= 8;
            channel.internal_remaining -= 1;
        }
    }

    /// GBAAudioScheduleFifoDma (audio owns the association; operates on
    /// self.dma[number] like the C's `struct GBADMA* info`).
    fn audio_schedule_fifo_dma(&mut self, number: i32) {
        let dest;
        {
            let dma = &mut self.dma[number as usize];
            dma.reg = gba_dma_register_set_dest_control(dma.reg, GBA_DMA_FIXED);
            dma.reg = gba_dma_register_set_width(dma.reg, 1);
            dma.dest_offset = 0;
            // The width was just forced to 32-bit, but sourceOffset was
            // cached at CNT_HI write time from the width the game
            // programmed. Rescale it, keeping the direction the source
            // control selected.
            if dma.source_offset > 0 {
                dma.source_offset = 4;
            } else if dma.source_offset < 0 {
                dma.source_offset = -4;
            }
            dest = dma.dest;
        }
        if dest == GBA_BASE_IO | GBA_REG_FIFO_A_LO {
            self.audio.ch_a.dma_source = number;
        } else if dest == GBA_BASE_IO | GBA_REG_FIFO_B_LO {
            self.audio.ch_b.dma_source = number;
        } else {
            mlog!(
                Level::GameError,
                rgba_core::log::GBA_AUDIO,
                "Invalid FIFO destination: 0x{:08X}",
                dest
            );
        }
    }

    /// GBADMAUpdate (mgba/src/gba/dma.c). dma.c is not ported yet and
    /// GBAAudioSampleFIFO is its only caller reachable today, so it lives
    /// here for now; MOVE THIS to dma.rs when the DMA engine is ported (the
    /// duplicate `pub fn dma_update` there will flag it).

    /// GBAAudioSample (catch the PSG state up to `now`)
    pub fn audio_sample(&mut self, now: i32) {
        self.audio.sample(now);
    }

    /// GBAudioWriteNR10
    pub fn audio_write_nr10(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0x1);
        if !write_sweep(&mut self.audio.psg.ch1.sweep, value) {
            self.audio.psg.playing_ch1 = false;
            self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] &= !0x1u16;
        }
    }

    /// GBAudioWriteNR11
    pub fn audio_write_nr11(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0x1);
        write_duty(&mut self.audio.psg.ch1.envelope, value);
        self.audio.psg.ch1.control.length = 64 - self.audio.psg.ch1.envelope.length;
    }

    /// GBAudioWriteNR12
    pub fn audio_write_nr12(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0x1);
        if !write_envelope(&mut self.audio.psg.ch1.envelope, value) {
            self.audio.psg.playing_ch1 = false;
            self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] &= !0x1u16;
        }
    }

    /// GBAudioWriteNR13
    pub fn audio_write_nr13(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0x1);
        self.audio.psg.ch1.control.frequency &= 0x700;
        self.audio.psg.ch1.control.frequency |=
            gb_audio_register_control_get_frequency(value as u16);
    }

    /// GBAudioWriteNR14
    pub fn audio_write_nr14(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0x1);
        self.audio.psg.ch1.control.frequency &= 0xFF;
        self.audio.psg.ch1.control.frequency |=
            gb_audio_register_control_get_frequency((value as u16) << 8);
        let was_stop = self.audio.psg.ch1.control.stop;
        self.audio.psg.ch1.control.stop = gb_audio_register_control_get_stop((value as u16) << 8);
        if !was_stop
            && self.audio.psg.ch1.control.stop
            && self.audio.psg.ch1.control.length != 0
            && self.audio.psg.frame & 1 == 0
        {
            self.audio.psg.ch1.control.length -= 1;
            if self.audio.psg.ch1.control.length == 0 {
                self.audio.psg.playing_ch1 = false;
            }
        }
        if gb_audio_register_control_is_restart((value as u16) << 8) {
            self.audio.psg.playing_ch1 = reset_envelope(&mut self.audio.psg.ch1.envelope);
            self.audio.psg.ch1.sweep.real_frequency = self.audio.psg.ch1.control.frequency;
            reset_sweep(&mut self.audio.psg.ch1.sweep);
            if self.audio.psg.playing_ch1 && self.audio.psg.ch1.sweep.shift != 0 {
                self.audio.psg.playing_ch1 = update_sweep(&mut self.audio.psg.ch1, true);
            }
            if self.audio.psg.ch1.control.length == 0 {
                self.audio.psg.ch1.control.length = 64;
                if self.audio.psg.ch1.control.stop && self.audio.psg.frame & 1 == 0 {
                    self.audio.psg.ch1.control.length -= 1;
                }
            }
            update_square_sample(&mut self.audio.psg.ch1);
        }
        self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] &= !0x1u16;
        self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] |= self.audio.psg.playing_ch1 as u16;
    }

    /// GBAudioWriteNR21
    pub fn audio_write_nr21(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0x2);
        write_duty(&mut self.audio.psg.ch2.envelope, value);
        self.audio.psg.ch2.control.length = 64 - self.audio.psg.ch2.envelope.length;
    }

    /// GBAudioWriteNR22
    pub fn audio_write_nr22(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0x2);
        if !write_envelope(&mut self.audio.psg.ch2.envelope, value) {
            self.audio.psg.playing_ch2 = false;
            self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] &= !0x2u16;
        }
    }

    /// GBAudioWriteNR23
    pub fn audio_write_nr23(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0x2);
        self.audio.psg.ch2.control.frequency &= 0x700;
        self.audio.psg.ch2.control.frequency |=
            gb_audio_register_control_get_frequency(value as u16);
    }

    /// GBAudioWriteNR24
    pub fn audio_write_nr24(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0x2);
        self.audio.psg.ch2.control.frequency &= 0xFF;
        self.audio.psg.ch2.control.frequency |=
            gb_audio_register_control_get_frequency((value as u16) << 8);
        let was_stop = self.audio.psg.ch2.control.stop;
        self.audio.psg.ch2.control.stop = gb_audio_register_control_get_stop((value as u16) << 8);
        if !was_stop
            && self.audio.psg.ch2.control.stop
            && self.audio.psg.ch2.control.length != 0
            && self.audio.psg.frame & 1 == 0
        {
            self.audio.psg.ch2.control.length -= 1;
            if self.audio.psg.ch2.control.length == 0 {
                self.audio.psg.playing_ch2 = false;
            }
        }
        if gb_audio_register_control_is_restart((value as u16) << 8) {
            self.audio.psg.playing_ch2 = reset_envelope(&mut self.audio.psg.ch2.envelope);

            if self.audio.psg.ch2.control.length == 0 {
                self.audio.psg.ch2.control.length = 64;
                if self.audio.psg.ch2.control.stop && self.audio.psg.frame & 1 == 0 {
                    self.audio.psg.ch2.control.length -= 1;
                }
            }
            update_square_sample(&mut self.audio.psg.ch2);
        }
        self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] &= !0x2u16;
        self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] |=
            (self.audio.psg.playing_ch2 as u16) << 1;
    }

    /// GBAudioWriteNR30
    pub fn audio_write_nr30(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0x4);
        self.audio.psg.ch3.enable = gb_audio_register_bank_get_enable(value);
        if !self.audio.psg.ch3.enable {
            self.audio.psg.playing_ch3 = false;
            self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] &= !0x4u16;
        }
    }

    /// GBAudioWriteNR31
    pub fn audio_write_nr31(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0x4);
        self.audio.psg.ch3.length = 256 - value as u32;
    }

    /// GBAudioWriteNR32 (only reached via the NR52-off cascade on GBA —
    /// GBAAudioWriteSOUND3CNT_HI writes ch3.volume directly instead). The
    /// wavedata8 access is the C union's byte view.
    pub fn audio_write_nr32(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0x4);
        self.audio.psg.ch3.volume = gb_audio_register_bank_volume_get_volume_gb(value);

        let mut sample = wave_byte(&self.audio.psg.ch3, (self.audio.psg.ch3.window >> 1) as usize)
            as i8;
        if self.audio.psg.ch3.window & 1 == 0 {
            sample = sample.wrapping_shr(4);
        }
        sample &= 0xF;
        let volume = match self.audio.psg.ch3.volume {
            0 => 4,
            1 => 0,
            2 => 1,
            _ => 2,
        };
        sample = sample.wrapping_shr(volume as u32);
        self.audio.psg.ch3.sample = sample;
    }

    /// GBAudioWriteNR33
    pub fn audio_write_nr33(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0x4);
        self.audio.psg.ch3.rate &= 0x700;
        self.audio.psg.ch3.rate |= gb_audio_register_control_get_rate(value as u16);
    }

    /// GBAudioWriteNR34
    pub fn audio_write_nr34(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0x4);
        self.audio.psg.ch3.rate &= 0xFF;
        self.audio.psg.ch3.rate |= gb_audio_register_control_get_rate((value as u16) << 8);
        let was_stop = self.audio.psg.ch3.stop;
        self.audio.psg.ch3.stop = gb_audio_register_control_get_stop((value as u16) << 8);
        if !was_stop
            && self.audio.psg.ch3.stop
            && self.audio.psg.ch3.length != 0
            && self.audio.psg.frame & 1 == 0
        {
            self.audio.psg.ch3.length -= 1;
            if self.audio.psg.ch3.length == 0 {
                self.audio.psg.playing_ch3 = false;
            }
        }
        if gb_audio_register_control_is_restart((value as u16) << 8) {
            self.audio.psg.playing_ch3 = self.audio.psg.ch3.enable;
            if self.audio.psg.ch3.length == 0 {
                self.audio.psg.ch3.length = 256;
                if self.audio.psg.ch3.stop && self.audio.psg.frame & 1 == 0 {
                    self.audio.psg.ch3.length -= 1;
                }
            }

            // The DMG wave-RAM corruption and sample zeroing are
            // `style == GB_AUDIO_DMG` only; not ported.
            self.audio.psg.ch3.window = 0;
        }
        if self.audio.psg.playing_ch3 {
            self.audio.psg.ch3.readable = true; // style != GB_AUDIO_DMG
            // TODO: Where does this cycle delay come from?
            self.audio.psg.ch3.next_update = now.wrapping_add(
                (6 + 2 * (2048 - self.audio.psg.ch3.rate)) * self.audio.psg.timing_factor,
            );
        }
        self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] &= !0x4u16;
        self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] |=
            (self.audio.psg.playing_ch3 as u16) << 2;
    }

    /// GBAudioWriteNR41
    pub fn audio_write_nr41(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0x8);
        write_duty(&mut self.audio.psg.ch4.envelope, value);
        self.audio.psg.ch4.length = 64 - self.audio.psg.ch4.envelope.length;
    }

    /// GBAudioWriteNR42
    pub fn audio_write_nr42(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0x8);
        if !write_envelope(&mut self.audio.psg.ch4.envelope, value) {
            self.audio.psg.playing_ch4 = false;
            self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] &= !0x8u16;
        }
    }

    /// GBAudioWriteNR43
    pub fn audio_write_nr43(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0x8);
        self.audio.psg.ch4.ratio = gb_audio_register_noise_feedback_get_ratio(value);
        self.audio.psg.ch4.frequency = gb_audio_register_noise_feedback_get_frequency(value);
        self.audio.psg.ch4.power = gb_audio_register_noise_feedback_get_power(value);
    }

    /// GBAudioWriteNR44
    pub fn audio_write_nr44(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0x8);
        let was_stop = self.audio.psg.ch4.stop;
        self.audio.psg.ch4.stop = gb_audio_register_noise_control_get_stop(value);
        if !was_stop
            && self.audio.psg.ch4.stop
            && self.audio.psg.ch4.length != 0
            && self.audio.psg.frame & 1 == 0
        {
            self.audio.psg.ch4.length -= 1;
            if self.audio.psg.ch4.length == 0 {
                self.audio.psg.playing_ch4 = false;
            }
        }
        if gb_audio_register_noise_control_is_restart(value) {
            self.audio.psg.playing_ch4 = reset_envelope(&mut self.audio.psg.ch4.envelope);

            self.audio.psg.ch4.lfsr = 0;
            if self.audio.psg.ch4.length == 0 {
                self.audio.psg.ch4.length = 64;
                if self.audio.psg.ch4.stop && self.audio.psg.frame & 1 == 0 {
                    self.audio.psg.ch4.length -= 1;
                }
            }
            if self.audio.psg.playing_ch4 {
                self.audio.psg.ch4.last_event = now as u32;
            }
        }
        self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] &= !0x8u16;
        self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] |=
            (self.audio.psg.playing_ch4 as u16) << 3;
    }

    /// GBAudioWriteNR50
    pub fn audio_write_nr50(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0xF);
        self.audio.psg.volume_right = gb_register_nr50_get_volume_right(value);
        self.audio.psg.volume_left = gb_register_nr50_get_volume_left(value);
    }

    /// GBAudioWriteNR51 (GBRegisterNR51GetChXRight/ChXLeft per bit)
    pub fn audio_write_nr51(&mut self, value: u8) {
        let now = self.current_time();
        self.audio.psg.run(now, 0xF);
        self.audio.psg.ch1_right = value & 0x01 != 0;
        self.audio.psg.ch2_right = value & 0x02 != 0;
        self.audio.psg.ch3_right = value & 0x04 != 0;
        self.audio.psg.ch4_right = value & 0x08 != 0;
        self.audio.psg.ch1_left = value & 0x10 != 0;
        self.audio.psg.ch2_left = value & 0x20 != 0;
        self.audio.psg.ch3_left = value & 0x40 != 0;
        self.audio.psg.ch4_left = value & 0x80 != 0;
    }

    /// GBAudioWriteNR52. On GB_AUDIO_GBA `audio->p` (the GB back pointer in
    /// the PSG) is NULL, so the io[] register clears below the channel
    /// writes are skipped — GBAAudioWriteSOUNDCNT_X does its own — and the
    /// skipFrame latch (which needs p->timer.internalDiv) never sets.
    pub fn audio_write_nr52(&mut self, value: u8) {
        let was_enable = self.audio.psg.enable;
        self.audio.psg.enable = gb_audio_enable_get_enable(value);
        if !self.audio.psg.enable {
            self.audio.psg.playing_ch1 = false;
            self.audio.psg.playing_ch2 = false;
            self.audio.psg.playing_ch3 = false;
            self.audio.psg.playing_ch4 = false;
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
            // style != GB_AUDIO_DMG:
            self.audio_write_nr11(0);
            self.audio_write_nr21(0);
            self.audio_write_nr31(0);
            self.audio_write_nr41(0);

            self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] &= !0xFu16;
        } else if !was_enable {
            self.audio.psg.skip_frame = false;
            self.audio.psg.frame = 7;

            // `audio->p && audio->p->timer.internalDiv & ...` skipFrame
            // check — psg.p is NULL, skipped.
        }
    }

    /// GBAudioUpdateFrame (the frame sequencer; on GBA driven by the folded
    /// frame event — see the header comment). `now` is the true current
    /// time from the event callback, standing in for
    /// mTimingCurrentTime(audio->timing).
    fn audio_update_frame(&mut self, now: i32) {
        if !self.audio.psg.enable {
            return;
        }
        if self.audio.psg.skip_frame {
            self.audio.psg.skip_frame = false;
            return;
        }
        self.audio.psg.run(now, 0x7);

        let frame = (self.audio.psg.frame + 1) & 7;
        self.audio.psg.frame = frame;

        match frame {
            0 | 2 | 4 | 6 => {
                if frame == 2 || frame == 6 {
                    if self.audio.psg.ch1.sweep.enable {
                        self.audio.psg.ch1.sweep.step -= 1;
                        if self.audio.psg.ch1.sweep.step == 0 {
                            if !update_sweep(&mut self.audio.psg.ch1, false) {
                                self.audio.psg.playing_ch1 = false;
                            }
                            self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] &= !0x1u16;
                            self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] |=
                                self.audio.psg.playing_ch1 as u16;
                        }
                    }
                    // Fall through (C switch falls into case 0/4)
                }
                if self.audio.psg.ch1.control.length != 0 && self.audio.psg.ch1.control.stop {
                    self.audio.psg.ch1.control.length -= 1;
                    if self.audio.psg.ch1.control.length == 0 {
                        self.audio.psg.playing_ch1 = false;
                        self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] &= !0x1u16;
                    }
                }

                if self.audio.psg.ch2.control.length != 0 && self.audio.psg.ch2.control.stop {
                    self.audio.psg.ch2.control.length -= 1;
                    if self.audio.psg.ch2.control.length == 0 {
                        self.audio.psg.playing_ch2 = false;
                        self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] &= !0x2u16;
                    }
                }

                if self.audio.psg.ch3.length != 0 && self.audio.psg.ch3.stop {
                    self.audio.psg.ch3.length -= 1;
                    if self.audio.psg.ch3.length == 0 {
                        self.audio.psg.playing_ch3 = false;
                        self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] &= !0x4u16;
                    }
                }

                if self.audio.psg.ch4.length != 0 && self.audio.psg.ch4.stop {
                    self.audio.psg.ch4.length -= 1;
                    if self.audio.psg.ch4.length == 0 {
                        self.audio.psg.playing_ch4 = false;
                        self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] &= !0x8u16;
                    }
                }
            }
            7 => {
                if self.audio.psg.playing_ch1 && self.audio.psg.ch1.envelope.dead == 0 {
                    self.audio.psg.ch1.envelope.next_step -= 1;
                    if self.audio.psg.ch1.envelope.next_step == 0 {
                        update_envelope(&mut self.audio.psg.ch1.envelope);
                        update_square_sample(&mut self.audio.psg.ch1);
                    }
                }

                if self.audio.psg.playing_ch2 && self.audio.psg.ch2.envelope.dead == 0 {
                    self.audio.psg.ch2.envelope.next_step -= 1;
                    if self.audio.psg.ch2.envelope.next_step == 0 {
                        update_envelope(&mut self.audio.psg.ch2.envelope);
                        update_square_sample(&mut self.audio.psg.ch2);
                    }
                }

                if self.audio.psg.playing_ch4 && self.audio.psg.ch4.envelope.dead == 0 {
                    self.audio.psg.ch4.envelope.next_step -= 1;
                    if self.audio.psg.ch4.envelope.next_step == 0 {
                        let sample = self.audio.psg.ch4.sample;
                        update_envelope(&mut self.audio.psg.ch4.envelope);
                        self.audio.psg.ch4.sample =
                            ((sample > 0) as i32 * self.audio.psg.ch4.envelope.current_volume)
                                as i8;
                        if self.audio.psg.ch4.n_samples != 0 {
                            self.audio.psg.ch4.samples -= sample as i32;
                            self.audio.psg.ch4.samples += self.audio.psg.ch4.sample as i32;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// _sample (gba/audio.c): the sample event callback. The C locks the
    /// sync object, writes currentSamples into the mAudioBuffer, notifies
    /// the stream and reschedules; here the batch goes straight into the
    /// ring.
    ///
    /// The folded frame event (_updateFrame, gb/audio.c, priority 0x10)
    /// runs first every 32nd firing, exactly as the C's earlier-higher-
    /// priority event would (see the header comment).
    pub fn audio_sample_event(&mut self, cycles_late: i32) {
        let now = self.current_time();
        if self.audio.frame_phase == 0 {
            // _updateFrame body: sample up to now, then clock the sequencer.
            self.audio_sample(now);
            self.audio_update_frame(now);
        }
        self.audio.frame_phase = (self.audio.frame_phase + 1) & (FRAME_EVENT_PHASE_MOD - 1);

        self.audio_sample(now.wrapping_sub(cycles_late));

        let samples =
            (2 << gba_register_soundbias_get_resolution(self.audio.soundbias)) as usize;
        let fill = self.audio.ch_a.samples[samples - 1];
        self.audio.ch_a.samples = [fill; GBA_MAX_SAMPLES];
        let fill = self.audio.ch_b.samples[samples - 1];
        self.audio.ch_b.samples = [fill; GBA_MAX_SAMPLES];

        for i in 0..samples {
            let sample = self.audio.current_samples[i];
            self.audio.buffer.write_stereo(sample.left, sample.right);
        }
        self.schedule(
            EventId::AudioSample,
            SAMPLE_INTERVAL.wrapping_sub(cycles_late),
        );
    }

    /// There is no GBAAudioSetSampleInterval in the C (the GBA sample rate
    /// is driven by the SOUNDBIAS resolution, see audio_write_soundbias);
    /// like the GB port's setter, keep the rescheduled event in sync on
    /// change — with the GBA's fixed batch cadence SAMPLE_INTERVAL.
    pub fn audio_set_sample_interval(&mut self, interval: i32) {
        self.audio.sample_interval = interval;
        self.audio.psg.sample_interval = interval;
        self.deschedule(EventId::AudioSample);
        self.schedule(EventId::AudioSample, SAMPLE_INTERVAL);
    }
}
