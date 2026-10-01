// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gba/serialize.c (GBASerialize/GBADeserialize),
// mgba/src/gba/io.c (GBAIOSerialize/GBAIODeserialize),
// mgba/src/gba/dma.c (GBADMASerialize/GBADMADeserialize),
// mgba/src/gba/video.c (GBAVideoSerialize/GBAVideoDeserialize),
// mgba/src/gba/audio.c (GBAAudioSerialize/Deserialize; the PSG half is
// GBAudioPSGSerialize/Deserialize from mgba/src/gb/audio.c),
// mgba/src/gba/memory.c (GBAMemorySerialize: wram/iwram blobs),
// mgba/src/gba/savedata.c (GBASavedataSerialize/Deserialize) and
// mgba/src/gba/cart/gpio.c (GBAHardwareSerialize/Deserialize).
// sio.c has no serialize hooks of its own; only the SIO complete-event `when`
// (hw.sioNextEvent) and the siocnt/rcnt replays covered here.
//
// The produced byte layout matches mGBA's GBASerializedState v0xB
// (0x61000 bytes, see mgba/include/mgba/internal/gba/serialize.h), so
// states are cross-compatible with mGBA 0.11-compatible builds. Writes
// follow the struct field order; loads parse the whole state into locals,
// validate, then commit in GBADeserialize's order (video -> memory -> io
// [register replay order matters] -> audio -> savedata).
//
// Known deltas forced by this port (all downstream of unported subsystems):
// - The special-cart union (matrix mappings / multicart / VFAME) and
//   hw.unlCartFlags are zeroed/skipped: cart/unlicensed.c + matrix are
//   stubs here (matrix_active() is always false).
// - biosStall is written as 0 and not restored (bios.rs HLE is a stub).
// - The GBP legacy fields (inputsPosted/txPosition in hw.flags2) are 0;
//   sio.gbp here has no such fields.
// - Descheduled timer/SIO events have no retained stale `when` in this
//   port; their nextEvent slots serialize as i32::MAX (mGBA stores its own
//   stale absolute value; both sides ignore it unless the event would be
//   rescheduled anyway).
// - The savedata "settling dust" timer is a countdown (`settling_pending`)
//   rather than a scheduled event; we round-trip the countdown in the same
//   slot.

use rgba_core::serialize::{Deserializer, Serializer};
use rgba_core::{mlog, Level};

use crate::arm::{ExecutionMode, ARM_PC, WORD_SIZE_ARM, WORD_SIZE_THUMB};
use crate::audio::{GBA_AUDIO_FIFO_SIZE, GBA_MAX_SAMPLES};
use crate::dma::DMA_OFFSET;
use crate::gba::{EventId, Gba, GBA_ARM7TDMI_FREQUENCY, HW_GPIO};
use crate::io::*;
use crate::memory::{
    GBA_BASE_OAM, GBA_BASE_PALETTE_RAM, GBA_BASE_ROM0, GBA_BASE_SRAM, GBA_REGION_ROM0,
    GBA_REGION_ROM1, GBA_REGION_ROM2, GBA_SIZE_BIOS, GBA_SIZE_EWRAM, GBA_SIZE_IWRAM, GBA_SIZE_IO,
    GBA_SIZE_PALETTE_RAM, GBA_SIZE_ROM0, GBA_SIZE_VRAM,
};
use crate::savedata::SavedataType;
use crate::timers::{TIMER_COUNT_UP, TIMER_ENABLE};
use crate::video::{dispstat_in_hblank, VideoEventKind, GBA_SIZE_OAM};

pub const GBA_SAVESTATE_MAGIC: u32 = 0x01000000;
pub const GBA_SAVESTATE_VERSION: u32 = 0x0000000B;
/// Everything up to the io image (0x400), then the fixed memory blobs.
pub const GBA_SAVESTATE_SIZE: usize =
    0x400 + GBA_SIZE_IO + GBA_SIZE_PALETTE_RAM + GBA_SIZE_OAM + GBA_SIZE_VRAM + GBA_SIZE_IWRAM + GBA_SIZE_EWRAM;
// == 0x61000, matching the C's static_assert on sizeof(GBASerializedState).

/// GBA_BIOS_CHECKSUM (mgba/src/gba/bios.c)
const GBA_BIOS_CHECKSUM: u32 = 0xBAAE187F;

/// SAMPLE_INTERVAL (gba/audio.c) — local copy; the one in audio.rs is
/// private.
const AUDIO_SAMPLE_INTERVAL: i32 = (GBA_ARM7TDMI_FREQUENCY / 0x4000) as i32;
/// psg.timingFactor * FRAME_CYCLES / SAMPLE_INTERVAL = 4 * 8192 / 1024.
/// The C's psg.frameEvent (period 32768) is folded into the sample event;
/// see audio.rs's header comment.
const FRAME_EVENT_MOD: i32 = 32;

// ---------------------------------------------------------------------------
// _isValidRegister / _isRSpecialRegister / _isWSpecialRegister (io.c)
// ---------------------------------------------------------------------------
//
// The C tables have 0x10A entries (266 = GBA_REG(INTERNAL_MAX) where
// GBA_REG(x) = x >> 1 and INTERNAL_MAX = 0x214); a handful of trailing
// entries (0x21* rows onward) are implicit zeros. Entry 0x108/0x109 are the
// C's *internal* EXWAITCNT slots (fake register addresses 0x210/0x212, which
// land inside the serialized io block). This port keeps the internal
// EXWAITCNT at memory.io[0x200/0x201] (GBA_REG_INTERNAL_EXWAITCNT_LO >> 1 in
// io.rs), so the loops below map idx 0x108/0x109 to those slots.

const IO_TABLE_ENTRIES: usize = 0x10A;

const fn make_table(rows: [[u8; 8]; 0x21], internal: [u8; 2]) -> [bool; IO_TABLE_ENTRIES] {
    let mut t = [false; IO_TABLE_ENTRIES];
    let mut r = 0;
    while r < 0x21 {
        let mut c = 0;
        while c < 8 {
            t[r * 8 + c] = rows[r][c] != 0;
            c += 1;
        }
        r += 1;
    }
    t[0x108] = internal[0] != 0;
    t[0x109] = internal[1] != 0;
    t
}

/// GBA_REG_INTERNAL_MAX (in the C; this port's io.rs relocates the internal
/// registers) — the serialize loop bound.
const GBA_REG_INTERNAL_MAX_ADDR: u32 = 0x214;
/// GBA_REG_MAX (in the C) — the deserialize replay loop bound.
const GBA_REG_MAX_ADDR: u32 = 0x20A;

const IS_VALID_REGISTER: [bool; IO_TABLE_ENTRIES] = make_table(
    [
        [1, 0, 1, 1, 1, 1, 1, 1], // 00
        [1, 1, 1, 1, 1, 1, 1, 1], // 01
        [1, 1, 1, 1, 1, 1, 1, 1], // 02
        [1, 1, 1, 1, 1, 1, 1, 1], // 03
        [1, 1, 1, 1, 1, 1, 1, 0], // 04
        [1, 1, 1, 0, 0, 0, 0, 0], // 05
        [1, 1, 1, 0, 1, 0, 1, 0], // 06
        [1, 1, 1, 0, 1, 0, 1, 0], // 07
        [1, 1, 1, 0, 1, 0, 0, 0], // 08
        [1, 1, 1, 1, 1, 1, 1, 1], // 09
        [1, 1, 1, 1, 0, 0, 0, 0], // 0A
        [1, 1, 1, 1, 1, 1, 1, 1], // 0B
        [1, 1, 1, 1, 1, 1, 1, 1], // 0C
        [1, 1, 1, 1, 1, 1, 1, 1], // 0D
        [0, 0, 0, 0, 0, 0, 0, 0], // 0E
        [0, 0, 0, 0, 0, 0, 0, 0], // 0F
        [1, 1, 1, 1, 1, 1, 1, 1], // 10
        [0, 0, 0, 0, 0, 0, 0, 0], // 11
        [1, 1, 1, 1, 1, 0, 0, 0], // 12
        [1, 1, 1, 0, 0, 0, 0, 0], // 13
        [1, 0, 0, 0, 0, 0, 0, 0], // 14
        [1, 1, 1, 1, 1, 0, 0, 0], // 15
        [0, 0, 0, 0, 0, 0, 0, 0], // 16
        [0, 0, 0, 0, 0, 0, 0, 0], // 17
        [0, 0, 0, 0, 0, 0, 0, 0], // 18
        [0, 0, 0, 0, 0, 0, 0, 0], // 19
        [0, 0, 0, 0, 0, 0, 0, 0], // 1A
        [0, 0, 0, 0, 0, 0, 0, 0], // 1B
        [0, 0, 0, 0, 0, 0, 0, 0], // 1C
        [0, 0, 0, 0, 0, 0, 0, 0], // 1D
        [0, 0, 0, 0, 0, 0, 0, 0], // 1E
        [0, 0, 0, 0, 0, 0, 0, 0], // 1F
        [1, 1, 1, 0, 1, 0, 0, 0], // 20
    ],
    [1, 1], // internal EXWAITCNT registers
);

const IS_RSPECIAL_REGISTER: [bool; IO_TABLE_ENTRIES] = make_table(
    [
        [0, 0, 1, 1, 0, 0, 0, 0], // 00
        [1, 1, 1, 1, 1, 1, 1, 1], // 01
        [1, 1, 1, 1, 1, 1, 1, 1], // 02
        [1, 1, 1, 1, 1, 1, 1, 1], // 03
        [1, 1, 1, 1, 1, 1, 1, 1], // 04
        [1, 1, 1, 1, 1, 1, 1, 1], // 05
        [0, 0, 1, 0, 0, 0, 1, 0], // 06
        [0, 0, 1, 0, 0, 0, 1, 0], // 07
        [0, 0, 0, 0, 1, 0, 0, 0], // 08
        [1, 1, 1, 1, 1, 1, 1, 1], // 09
        [1, 1, 1, 1, 0, 0, 0, 0], // 0A
        [1, 1, 1, 1, 1, 1, 1, 1], // 0B
        [1, 1, 1, 1, 1, 1, 1, 1], // 0C
        [1, 1, 1, 1, 1, 1, 1, 1], // 0D
        [0, 0, 0, 0, 0, 0, 0, 0], // 0E
        [0, 0, 0, 0, 0, 0, 0, 0], // 0F
        [1, 1, 1, 1, 1, 1, 1, 1], // 10
        [0, 0, 0, 0, 0, 0, 0, 0], // 11
        [1, 1, 1, 1, 0, 0, 0, 0], // 12
        [1, 1, 0, 0, 0, 0, 0, 0], // 13
        [1, 0, 0, 0, 0, 0, 0, 0], // 14
        [1, 1, 1, 1, 1, 0, 0, 0], // 15
        [0, 0, 0, 0, 0, 0, 0, 0], // 16
        [0, 0, 0, 0, 0, 0, 0, 0], // 17
        [0, 0, 0, 0, 0, 0, 0, 0], // 18
        [0, 0, 0, 0, 0, 0, 0, 0], // 19
        [0, 0, 0, 0, 0, 0, 0, 0], // 1A
        [0, 0, 0, 0, 0, 0, 0, 0], // 1B
        [0, 0, 0, 0, 0, 0, 0, 0], // 1C
        [0, 0, 0, 0, 0, 0, 0, 0], // 1D
        [0, 0, 0, 0, 0, 0, 0, 0], // 1E
        [0, 0, 0, 0, 0, 0, 0, 0], // 1F
        [0, 0, 0, 0, 0, 0, 0, 0], // 20
    ],
    [1, 1],
);

const IS_WSPECIAL_REGISTER: [bool; IO_TABLE_ENTRIES] = make_table(
    [
        [0, 0, 1, 1, 0, 0, 0, 0], // 00
        [0, 0, 0, 0, 0, 0, 0, 0], // 01
        [0, 0, 0, 0, 0, 0, 0, 0], // 02
        [0, 0, 0, 0, 0, 0, 0, 0], // 03
        [0, 0, 0, 0, 0, 0, 0, 0], // 04
        [0, 0, 0, 0, 0, 0, 0, 0], // 05
        [0, 0, 1, 0, 0, 0, 1, 0], // 06
        [0, 0, 1, 0, 0, 0, 1, 0], // 07
        [0, 0, 1, 0, 0, 0, 0, 0], // 08
        [1, 1, 1, 1, 1, 1, 1, 1], // 09
        [1, 1, 1, 1, 0, 0, 0, 0], // 0A
        [0, 0, 0, 0, 1, 1, 0, 0], // 0B
        [0, 0, 1, 1, 0, 0, 0, 0], // 0C
        [1, 1, 0, 0, 0, 0, 1, 1], // 0D
        [0, 0, 0, 0, 0, 0, 0, 0], // 0E
        [0, 0, 0, 0, 0, 0, 0, 0], // 0F
        [1, 1, 1, 1, 1, 1, 1, 1], // 10
        [0, 0, 0, 0, 0, 0, 0, 0], // 11
        [1, 1, 1, 1, 1, 0, 0, 0], // 12
        [1, 1, 1, 0, 0, 0, 0, 0], // 13
        [1, 0, 0, 0, 0, 0, 0, 0], // 14
        [1, 1, 1, 1, 1, 0, 0, 0], // 15
        [0, 0, 0, 0, 0, 0, 0, 0], // 16
        [0, 0, 0, 0, 0, 0, 0, 0], // 17
        [0, 0, 0, 0, 0, 0, 0, 0], // 18
        [0, 0, 0, 0, 0, 0, 0, 0], // 19
        [0, 0, 0, 0, 0, 0, 0, 0], // 1A
        [0, 0, 0, 0, 0, 0, 0, 0], // 1B
        [0, 0, 0, 0, 0, 0, 0, 0], // 1C
        [0, 0, 0, 0, 0, 0, 0, 0], // 1D
        [0, 0, 0, 0, 0, 0, 0, 0], // 1E
        [0, 0, 0, 0, 0, 0, 0, 0], // 1F
        [1, 1, 0, 0, 1, 0, 0, 0], // 20
    ],
    [1, 1],
);

fn timer_event_id(t: usize) -> EventId {
    match t {
        0 => EventId::Timer0,
        1 => EventId::Timer1,
        2 => EventId::Timer2,
        _ => EventId::Timer3,
    }
}

// ---------------------------------------------------------------------------
// Serialize
// ---------------------------------------------------------------------------

/// GBASerialize: writes exactly GBA_SAVESTATE_SIZE bytes.
pub fn serialize(gba: &mut Gba) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::with_capacity(GBA_SAVESTATE_SIZE);
    {
        let s = &mut Serializer::new(&mut out);
        s.put_u32(GBA_SAVESTATE_MAGIC + GBA_SAVESTATE_VERSION);
        s.put_u32(gba.bios_checksum);
        s.put_u32(gba.rom_crc32);
        s.put_u32(gba.timing.master_cycles);

        // GBACartridge overlay of the ROM header: title at 0xA0, id at 0xAC.
        if gba.memory.rom.len() >= 0xB0 {
            s.put_bytes(&gba.memory.rom[0xA0..0xAC]);
            s.put_u32(u32::from_le_bytes(
                gba.memory.rom[0xAC..0xB0].try_into().unwrap(),
            ));
        } else {
            s.put_bytes(&[0; 12]);
            s.put_u32(0);
        }

        // cpu (struct ARMRegisterFile order)
        for i in 0..16 {
            s.put_i32(gba.cpu.gprs[i]);
        }
        s.put_i32(gba.cpu.cpsr.packed);
        s.put_i32(gba.cpu.spsr.packed);
        s.put_i32(gba.cpu.cycles);
        s.put_i32(gba.cpu.next_event);
        for i in 0..6 {
            for j in 0..7 {
                s.put_i32(gba.cpu.banked_registers[i][j]);
            }
        }
        for i in 0..6 {
            s.put_i32(gba.cpu.banked_spsrs[i]);
        }

        debug_assert_eq!(s.offset(), 0x130);
        audio_serialize_head(gba, s);

        debug_assert_eq!(s.offset(), 0x1F0);
        // video (GBAVideoSerialize's scalar parts; vram/oam/pram memcpy at
        // their own offsets below)
        s.put_u32(0); // reserved
        s.put_i32(gba.until(EventId::Video));
        let mut vflags: u32 = 0;
        vflags |= match gba.video.event_kind {
            VideoEventKind::StartHdraw => 1,
            VideoEventKind::StartHblank => 2,
        };
        s.put_u32(vflags);
        s.put_u32(gba.video.frame_counter);

        debug_assert_eq!(s.offset(), 0x200);
        // timers (the second half of GBAIOSerialize)
        let now = gba.current_time();
        for i in 0..4 {
            s.put_u16(gba.timers[i].reload);
            s.put_u16(0); // reserved0
            s.put_i32(gba.timers[i].last_event.wrapping_sub(now));
            // C stores event.when - currentTime; when descheduled we have no
            // stale `when` (until() returns i32::MAX).
            s.put_i32(gba.until(timer_event_id(i)));
            s.put_u32(0); // reserved1
            s.put_u32(gba.timers[i].flags);
        }

        debug_assert_eq!(s.offset(), 0x250);
        // dma channels (GBADMASerialize)
        for i in 0..4 {
            s.put_u32(gba.dma[i].next_source);
            s.put_u32(gba.dma[i].next_dest);
            s.put_i32(gba.dma[i].next_count);
            s.put_i32(gba.dma[i].when);
        }

        debug_assert_eq!(s.offset(), 0x290);
        hw_serialize(gba, s);

        debug_assert_eq!(s.offset(), 0x2C8);
        s.put_u32(gba.dma[0].latch); // dmaTransferRegister
        s.put_u32(gba.dma_pc); // dmaBlockPC

        // matrix memory command buffer (unported; matrix never active)
        s.put_bytes(&[0; 16]);

        debug_assert_eq!(s.offset(), 0x2E0);
        savedata_serialize(gba, s);

        debug_assert_eq!(s.offset(), 0x2F4);
        s.put_u32(gba.memory.bios_prefetch);
        s.put_u32(gba.cpu.prefetch[0]);
        s.put_u32(gba.cpu.prefetch[1]);

        s.put_bytes(&[0; 16]); // reservedCpu

        debug_assert_eq!(s.offset(), 0x310);
        s.put_u64(gba.timing.global_cycles);
        s.put_u32(gba.memory.last_prefetched_pc);

        // miscFlags
        let mut misc_flags: u32 = 0;
        misc_flags |= (gba.cpu.halted & 1) as u32;
        misc_flags |= ((gba.memory.io[(GBA_REG_POSTFLG >> 1) as usize] & 1) as u32) << 1;
        let irq_pending = gba.is_scheduled(EventId::IrqEvent);
        misc_flags |= (irq_pending as u32) << 2;
        misc_flags |= (gba.cpu_blocked as u32) << 3;
        misc_flags |= ((gba.keys_last as u32) & 0x7FF) << 4;
        s.put_u32(misc_flags);
        s.put_i32(if irq_pending {
            gba.until(EventId::IrqEvent)
        } else {
            0
        });
        s.put_i32(0); // biosStall (HLE BIOS interruption unported)

        // special cartridge state union (matrix2/multicart/vfame) — 0x48
        s.put_bytes(&[0; 0x48]);

        debug_assert_eq!(s.offset(), 0x370);
        audio_serialize_tail(gba, s);

        debug_assert_eq!(s.offset(), 0x3D0);
        s.put_u32(gba.bus);
        s.put_u32(gba.dma[1].latch);
        s.put_u32(gba.dma[2].latch);
        s.put_u32(gba.dma[3].latch);
        for i in 0..4 {
            s.put_u16(gba.dma[i].count as u16);
        }

        s.put_bytes(&[0; 24]); // reserved[6]

        debug_assert_eq!(s.offset(), 0x400);
        let io_words = io_serialize(gba);
        for w in io_words.iter() {
            s.put_u16(*w);
        }

        debug_assert_eq!(s.offset(), 0x800);
        s.put_bytes(&gba.video.palette);
        s.put_bytes(&gba.video.oam);
        debug_assert_eq!(s.offset(), 0x1000);
        s.put_bytes(&gba.video.vram);
        debug_assert_eq!(s.offset(), 0x19000);
        s.put_bytes(&gba.memory.iwram);
        debug_assert_eq!(s.offset(), 0x21000);
        s.put_bytes(&gba.memory.wram);
        debug_assert_eq!(s.offset(), GBA_SAVESTATE_SIZE);
    }
    Ok(out)
}

/// GBAudioPSGSerialize + the scalar half of GBAAudioSerialize. Everything
/// here lives in `state->audio` (0x130-0x1EF), in struct order.
fn audio_serialize_head(gba: &mut Gba, s: &mut Serializer) {
    let now = gba.current_time();
    // Until the folded-in frame event; taken before borrowing `gba.audio`.
    let sample_until = gba.until(EventId::AudioSample);
    let a = &gba.audio;
    let p = &a.psg;

    let mut flags: u32 = 0;
    flags |= ((p.frame as u32) & 7) << 22;
    flags |= (p.skip_frame as u32) << 28;

    // ch1
    flags |= (p.ch1.envelope.current_volume as u32) & 0xF;
    flags |= ((p.ch1.envelope.dead as u32) & 3) << 4;
    flags |= (p.ch1.sweep.enable as u32) << 25;
    flags |= (p.ch1.sweep.occurred as u32) << 26;
    let mut ch1_flags: u32 = 0;
    ch1_flags |= (p.ch1.control.length as u32) & 0x7F;
    ch1_flags |= ((p.ch1.envelope.next_step as u32) & 7) << 7;
    ch1_flags |= ((p.ch1.sweep.real_frequency as u32) & 0x7FF) << 10;
    ch1_flags |= ((p.ch1.index as u32) & 7) << 21;
    s.put_u32(ch1_flags);
    // state->audio.psg.ch1.nextFrame is the PSG frame sequencer event's
    // `when`. Here it's folded into the sample event: it fires at the next
    // sample event whose entering frame_phase is 0 (every 32 sample events).
    let frame_extra = ((FRAME_EVENT_MOD - a.frame_phase) & (FRAME_EVENT_MOD - 1))
        .wrapping_mul(AUDIO_SAMPLE_INTERVAL);
    s.put_i32(sample_until.wrapping_add(frame_extra));
    s.put_i32(0); // reserved
    s.put_u32((p.ch1.sweep.time & 7) as u32);
    s.put_u32(p.ch1.last_update.wrapping_sub(now) as u32);

    // ch2
    flags |= ((p.ch2.envelope.current_volume as u32) & 0xF) << 8;
    flags |= ((p.ch2.envelope.dead as u32) & 3) << 12;
    let mut ch2_flags: u32 = 0;
    ch2_flags |= (p.ch2.control.length as u32) & 0x7F;
    ch2_flags |= ((p.ch2.envelope.next_step as u32) & 7) << 7;
    ch2_flags |= ((p.ch2.index as u32) & 7) << 21;
    s.put_u32(ch2_flags);
    s.put_i32(0); // reserved[0]
    s.put_i32(0); // reserved[1]
    s.put_u32(p.ch2.last_update.wrapping_sub(now) as u32);

    // ch3
    flags |= (p.ch3.readable as u32) << 27;
    for i in 0..8 {
        s.put_u32(p.ch3.wavedata32[i]);
    }
    s.put_i16(p.ch3.length as i16);
    s.put_i16(0); // reserved
    s.put_u32(p.ch3.next_update.wrapping_sub(now) as u32);

    // ch4
    flags |= ((p.ch4.envelope.current_volume as u32) & 0xF) << 16;
    flags |= ((p.ch4.envelope.dead as u32) & 3) << 20;
    s.put_u32(p.ch4.lfsr);
    let mut ch4_flags: u32 = 0;
    ch4_flags |= (p.ch4.length as u32) & 0x7F;
    ch4_flags |= ((p.ch4.envelope.next_step as u32) & 7) << 7;
    s.put_u32(ch4_flags);
    s.put_u32(p.ch4.last_event);
    let mut cycles: i32 = if p.ch4.ratio != 0 { 2 * p.ch4.ratio } else { 1 };
    cycles <<= p.ch4.frequency;
    cycles = cycles.wrapping_mul(8 * p.timing_factor);
    s.put_u32(p.ch4.last_event.wrapping_add(cycles as u32));

    // FIFO contents, read pointer first
    let mut read_a = a.ch_a.fifo_read;
    let mut read_b = a.ch_b.fifo_read;
    for _ in 0..GBA_AUDIO_FIFO_SIZE {
        s.put_u32(a.ch_a.fifo[read_a as usize]);
        read_a += 1;
        if read_a == GBA_AUDIO_FIFO_SIZE as i32 {
            read_a = 0;
        }
    }
    for _ in 0..GBA_AUDIO_FIFO_SIZE {
        s.put_u32(a.ch_b.fifo[read_b as usize]);
        read_b += 1;
        if read_b == GBA_AUDIO_FIFO_SIZE as i32 {
            read_b = 0;
        }
    }

    s.put_u32(a.ch_a.internal_sample);
    s.put_u32(a.ch_b.internal_sample);
    s.put_i32(sample_until); // nextSample
    s.put_i8(0); // sampleA (never stored by the C; stays zero)
    s.put_i8(0); // sampleB

    let fifo_size_a = if a.ch_a.fifo_write >= a.ch_a.fifo_read {
        a.ch_a.fifo_write - a.ch_a.fifo_read
    } else {
        GBA_AUDIO_FIFO_SIZE as i32 - a.ch_a.fifo_read + a.ch_a.fifo_write
    };
    let fifo_size_b = if a.ch_b.fifo_write >= a.ch_b.fifo_read {
        a.ch_b.fifo_write - a.ch_b.fifo_read
    } else {
        GBA_AUDIO_FIFO_SIZE as i32 - a.ch_b.fifo_read + a.ch_b.fifo_write
    };
    let mut gba_flags: u16 = 0;
    gba_flags |= (fifo_size_b as u16 & 0x7) << 2; // FIFOSamplesB (legacy order)
    gba_flags |= (a.ch_b.internal_remaining as u16 & 0x3) << 0;
    gba_flags |= (a.ch_a.internal_remaining as u16 & 0x3) << 5;
    gba_flags |= (fifo_size_a as u16 & 0x7) << 7;
    s.put_u16(gba_flags);

    s.put_u32(flags);
    s.put_i32(a.last_sample);

    let mut gba_flags2: u32 = 0;
    gba_flags2 |= (a.sample_index as u32) & 0xF;
    // Stored +1 so 0 means "absent" (field introduced in 0.11)
    gba_flags2 |= (((a.ch_a.dma_source as u32) + 1) & 0x3) << 4;
    gba_flags2 |= (((a.ch_b.dma_source as u32) + 1) & 0x3) << 6;
    s.put_u32(gba_flags2);

    s.put_bytes(&[0; 8]); // reserved[2]
}

/// The `samples`/`currentSamples` halves of GBAAudioSerialize (0x370+).
fn audio_serialize_tail(gba: &mut Gba, s: &mut Serializer) {
    for i in 0..GBA_MAX_SAMPLES {
        s.put_i8(gba.audio.ch_a.samples[i]);
    }
    for i in 0..GBA_MAX_SAMPLES {
        s.put_i8(gba.audio.ch_b.samples[i]);
    }
    for i in 0..GBA_MAX_SAMPLES {
        s.put_i16(gba.audio.current_samples[i].left);
        s.put_i16(gba.audio.current_samples[i].right);
    }
}

/// GBAHardwareSerialize.
fn hw_serialize(gba: &mut Gba, s: &mut Serializer) {
    let hw = &gba.hw;
    let mut flags1: u16 = 0;
    flags1 |= (hw.read_write as u16) & 1;
    s.put_u8(hw.pin_state);
    s.put_u8(hw.write_latch);
    s.put_u8(hw.direction);
    let flags3: u8 = hw.rtc_sio_output as u8; // GBASerializedHWFlags3
    s.put_u8(flags3);

    s.put_i32(hw.rtc_bytes_remaining);
    s.put_i32(0); // reserved0
    s.put_i32(hw.rtc_bits_read);
    s.put_i32(hw.rtc_bits);
    s.put_i32(hw.rtc_command_active as i32);
    s.put_u32(hw.rtc_command);
    s.put_u8(hw.rtc_control);
    s.put_bytes(&[0; 3]); // reserved1
    flags1 |= (hw.rtc_sck_edge as u16) << 3;
    s.put_bytes(&hw.rtc_time);

    s.put_u8(hw.devices as u8); // hw.devices (a uint32 truncated to a byte)

    s.put_u16(hw.gyro_sample);
    flags1 |= (hw.gyro_edge as u16) << 1;
    s.put_u16(hw.tilt_x);
    s.put_u16(hw.tilt_y);
    flags1 |= (hw.light_edge as u16) << 2;
    flags1 |= (hw.light_counter & 0xFFF) << 4;
    s.put_u16(flags1);
    s.put_u8(hw.light_sample);

    let mut flags2: u8 = 0;
    flags2 |= (hw.tilt_state as u8) & 3;
    // GBP/SIO legacy bits (GbpInputsPosted/GbpTxPosition) are 0: the GBP
    // driver state isn't ported.
    s.put_u8(flags2);

    s.put_u16(0); // unlCartFlags: no unlicensed-cart state
    s.put_i32(gba.until(EventId::Sio)); // sioNextEvent
}

/// GBASavedataSerialize.
fn savedata_serialize(gba: &mut Gba, s: &mut Serializer) {
    let sd = &gba.savedata;
    s.put_u8(sd.savedata_type as i32 as u8);
    s.put_u8(sd.command);
    let mut flags: u8 = 0;
    flags |= sd.flash_state & 3;
    flags |= (sd.current_bank == 1) as u8; // FlashBank
    if sd.settling_pending > 0 {
        flags |= 1 << 5; // DustSettling
    }
    s.put_u8(flags);
    s.put_i8(sd.read_bits_remaining);
    // settlingDust: the C schedules a dust event and stores its `when` as a
    // delta; ours is a plain countdown.
    if sd.settling_pending > 0 {
        s.put_u32(sd.settling_pending as u32);
    } else {
        s.put_u32(0);
    }
    s.put_u32(sd.read_address);
    s.put_u32(sd.write_address);
    s.put_u16(sd.settling as u16);
    s.put_u16(0); // reserved
}

/// The register-snapshot half of GBAIOSerialize. The C loop runs over
/// GBA_REG_INTERNAL_MAX (0x214): on top of the 0x400 actual register bytes it
/// also stores the two internal EXWAITCNT registers at io-block bytes
/// 0x210/0x212 (mGBA never reads them back; written for byte parity). This
/// port keeps the EXWAITCNT state at memory.io[0x200/0x201], so the loop maps
/// the table's 0x108/0x109 entries there.
fn io_serialize(gba: &mut Gba) -> [u16; GBA_SIZE_IO / 2] {
    let mut io = [0u16; GBA_SIZE_IO / 2];
    let mut i: u32 = 0;
    while i < GBA_REG_INTERNAL_MAX_ADDR {
        let idx = (i >> 1) as usize;
        let src = if idx == 0x108 || idx == 0x109 {
            0x200 + (idx - 0x108) // internal EXWAITCNT lives here in this port
        } else {
            idx
        };
        if IS_RSPECIAL_REGISTER[idx] {
            io[idx] = gba.memory.io[src];
        } else if IS_VALID_REGISTER[idx] {
            io[idx] = gba.io_read(i);
        }
        i += 2;
    }
    // The raw DMA count latches are stored again alongside the timers in the
    // C (same value; the main loop's GBAIORead returns memory.io for these).
    for c in 0..4u32 {
        let idx = ((GBA_REG_DMA0CNT_LO + c * 12) >> 1) as usize;
        io[idx] = gba.memory.io[idx];
    }
    io
}

// ---------------------------------------------------------------------------
// Deserialize
// ---------------------------------------------------------------------------

struct ParsedTimer {
    reload: u16,
    last_event: u32,
    next_event: u32,
    flags: u32,
}

struct ParsedDma {
    next_source: u32,
    next_dest: u32,
    next_count: i32,
    when: i32,
}

#[derive(Default, Clone, Copy)]
struct ParsedAudio {
    ch1_flags: u32,
    next_frame: i32,
    ch1_sweep: u32,
    ch1_last_update_delta: i32,
    ch2_flags: u32,
    ch2_last_update_delta: i32,
    wavebanks: [u32; 8],
    ch3_length: i16,
    ch3_next_event_delta: i32,
    ch4_lfsr: u32,
    ch4_flags: u32,
    ch4_last_event: u32,
    ch4_next_event: u32,
    fifo_a: [u32; GBA_AUDIO_FIFO_SIZE],
    fifo_b: [u32; GBA_AUDIO_FIFO_SIZE],
    internal_a: u32,
    internal_b: u32,
    next_sample: i32,
    gba_flags: u16,
    flags: u32,
    last_sample: i32,
    gba_flags2: u32,
    samples_a: [i8; GBA_MAX_SAMPLES],
    samples_b: [i8; GBA_MAX_SAMPLES],
    current_samples: [crate::audio::StereoSample; GBA_MAX_SAMPLES],
}

struct ParsedHw {
    pin_state: u8,
    write_latch: u8,
    pin_direction: u8,
    flags3: u8,
    rtc_bytes_remaining: i32,
    rtc_bits_read: i32,
    rtc_bits: i32,
    rtc_command_active: i32,
    rtc_command: u32,
    rtc_control: u8,
    time: [u8; 7],
    devices: u8,
    gyro_sample: u16,
    tilt_x: u16,
    tilt_y: u16,
    flags1: u16,
    light_sample: u8,
    flags2: u8,
    unl_cart_flags: u16,
    sio_next_event: u32,
}

struct ParsedSavedata {
    savedata_type: u8,
    command: u8,
    flags: u8,
    read_bits_remaining: i8,
    settling_dust: u32,
    read_address: u32,
    write_address: u32,
    settling_sector: u16,
}

pub fn deserialize(gba: &mut Gba, state: &[u8]) -> Result<(), &'static str> {
    if state.len() < GBA_SAVESTATE_SIZE {
        return Err("savestate too small");
    }
    let mut d = Deserializer::new(state);

    // --- Parse (buffer/struct order) ---
    let version = d.get_u32();
    let bios_checksum = d.get_u32();
    let rom_crc32 = d.get_u32();
    let master_cycles = d.get_u32();
    let mut title = [0u8; 12];
    d.get_bytes(&mut title);
    let id = d.get_u32();

    let mut gprs = [0i32; 16];
    for g in gprs.iter_mut() {
        *g = d.get_i32();
    }
    let cpsr = d.get_i32();
    let spsr = d.get_i32();
    let cpu_cycles = d.get_i32();
    let cpu_next_event = d.get_i32();
    let mut banked_registers = [[0i32; 7]; 6];
    for i in 0..6 {
        for j in 0..7 {
            banked_registers[i][j] = d.get_i32();
        }
    }
    let mut banked_spsrs = [0i32; 6];
    for b in banked_spsrs.iter_mut() {
        *b = d.get_i32();
    }

    // audio head (audio.psg + fifo/internal/sample fields)
    let mut pa = ParsedAudio {
        ..Default::default()
    };
    pa.ch1_flags = d.get_u32();
    pa.next_frame = d.get_i32();
    let _ = d.get_i32(); // reserved
    pa.ch1_sweep = d.get_u32();
    pa.ch1_last_update_delta = d.get_u32() as i32;
    pa.ch2_flags = d.get_u32();
    let _ = d.get_i32();
    let _ = d.get_i32();
    pa.ch2_last_update_delta = d.get_u32() as i32;
    for w in pa.wavebanks.iter_mut() {
        *w = d.get_u32();
    }
    pa.ch3_length = d.get_i16();
    let _ = d.get_i16();
    pa.ch3_next_event_delta = d.get_u32() as i32;
    pa.ch4_lfsr = d.get_u32();
    pa.ch4_flags = d.get_u32();
    pa.ch4_last_event = d.get_u32();
    pa.ch4_next_event = d.get_u32();
    for w in pa.fifo_a.iter_mut() {
        *w = d.get_u32();
    }
    for w in pa.fifo_b.iter_mut() {
        *w = d.get_u32();
    }
    pa.internal_a = d.get_u32();
    pa.internal_b = d.get_u32();
    pa.next_sample = d.get_i32();
    let _ = d.get_i8(); // sampleA
    let _ = d.get_i8(); // sampleB
    pa.gba_flags = d.get_u16();
    pa.flags = d.get_u32();
    pa.last_sample = d.get_i32();
    pa.gba_flags2 = d.get_u32();
    d.skip(8);

    // video
    let _ = d.get_u32(); // reserved
    let video_next_event = d.get_u32();
    let video_flags = d.get_u32();
    let video_frame_counter = d.get_u32();

    // timers
    let mut timers = [
        ParsedTimer { reload: 0, last_event: 0, next_event: 0, flags: 0 },
        ParsedTimer { reload: 0, last_event: 0, next_event: 0, flags: 0 },
        ParsedTimer { reload: 0, last_event: 0, next_event: 0, flags: 0 },
        ParsedTimer { reload: 0, last_event: 0, next_event: 0, flags: 0 },
    ];
    for t in timers.iter_mut() {
        t.reload = d.get_u16();
        let _ = d.get_u16();
        t.last_event = d.get_u32();
        t.next_event = d.get_u32();
        let _ = d.get_u32();
        t.flags = d.get_u32();
    }

    // dma channels
    let mut dmas = [
        ParsedDma { next_source: 0, next_dest: 0, next_count: 0, when: 0 },
        ParsedDma { next_source: 0, next_dest: 0, next_count: 0, when: 0 },
        ParsedDma { next_source: 0, next_dest: 0, next_count: 0, when: 0 },
        ParsedDma { next_source: 0, next_dest: 0, next_count: 0, when: 0 },
    ];
    for dm in dmas.iter_mut() {
        dm.next_source = d.get_u32();
        dm.next_dest = d.get_u32();
        dm.next_count = d.get_i32();
        dm.when = d.get_i32();
    }

    // hw
    let hw = ParsedHw {
        pin_state: d.get_u8(),
        write_latch: d.get_u8(),
        pin_direction: d.get_u8(),
        flags3: d.get_u8(),
        rtc_bytes_remaining: d.get_i32(),
        rtc_bits_read: {
            let _reserved0 = d.get_i32();
            d.get_i32()
        },
        rtc_bits: d.get_i32(),
        rtc_command_active: d.get_i32(),
        rtc_command: d.get_u32(),
        rtc_control: d.get_u8(),
        time: {
            d.skip(3); // reserved1
            let mut t = [0u8; 7];
            d.get_bytes(&mut t);
            t
        },
        devices: d.get_u8(),
        gyro_sample: d.get_u16(),
        tilt_x: d.get_u16(),
        tilt_y: d.get_u16(),
        flags1: d.get_u16(),
        light_sample: d.get_u8(),
        flags2: d.get_u8(),
        unl_cart_flags: d.get_u16(),
        sio_next_event: d.get_u32(),
    };

    let dma_transfer_register = d.get_u32();
    let dma_block_pc = d.get_u32();

    d.skip(16); // matrix command buffer (unported)

    let sd = ParsedSavedata {
        savedata_type: d.get_u8(),
        command: d.get_u8(),
        flags: d.get_u8(),
        read_bits_remaining: d.get_i8(),
        settling_dust: d.get_u32(),
        read_address: d.get_u32(),
        write_address: d.get_u32(),
        settling_sector: d.get_u16(),
    };
    let _ = d.get_u16(); // savedata reserved

    let bios_prefetch = d.get_u32();
    let mut cpu_prefetch = [0u32; 2];
    cpu_prefetch[0] = d.get_u32();
    cpu_prefetch[1] = d.get_u32();

    d.skip(16); // reservedCpu

    let global_cycles = d.get_u64();
    let last_prefetched_pc = d.get_u32();
    let misc_flags = d.get_u32();
    let next_irq = d.get_i32();
    let _bios_stall = d.get_i32(); // HLE BIOS stall unported

    d.skip(0x48); // special-cart union (matrix2/multicart/vfame)

    for v in pa.samples_a.iter_mut() {
        *v = d.get_i8();
    }
    for v in pa.samples_b.iter_mut() {
        *v = d.get_i8();
    }
    for v in pa.current_samples.iter_mut() {
        v.left = d.get_i16();
        v.right = d.get_i16();
    }

    let bus = d.get_u32();
    let mut dma_latch = [0u32; 3];
    for l in dma_latch.iter_mut() {
        *l = d.get_u32();
    }
    let mut dma_count_latch = [0u16; 4];
    for l in dma_count_latch.iter_mut() {
        *l = d.get_u16();
    }

    d.skip(24); // reserved

    let mut io_words = [0u16; GBA_SIZE_IO / 2];
    for w in io_words.iter_mut() {
        *w = d.get_u16();
    }
    let mut pram = [0u8; GBA_SIZE_PALETTE_RAM];
    d.get_bytes(&mut pram);
    let mut oam = [0u8; GBA_SIZE_OAM];
    d.get_bytes(&mut oam);
    let mut vram = vec![0u8; GBA_SIZE_VRAM];
    d.get_bytes(&mut vram);
    let mut iwram = vec![0u8; GBA_SIZE_IWRAM];
    d.get_bytes(&mut iwram);
    let mut wram = vec![0u8; GBA_SIZE_EWRAM];
    d.get_bytes(&mut wram);

    // --- Validate (GBADeserialize) ---
    let mut error = false;
    if version > GBA_SAVESTATE_MAGIC + GBA_SAVESTATE_VERSION {
        mlog!(Level::Warn, "GBA State", "Invalid or too new savestate: expected {:08X}, got {:08X}", GBA_SAVESTATE_MAGIC + GBA_SAVESTATE_VERSION, version);
        error = true;
    } else if version < GBA_SAVESTATE_MAGIC {
        mlog!(Level::Warn, "GBA State", "Invalid savestate: expected {:08X}, got {:08X}", GBA_SAVESTATE_MAGIC + GBA_SAVESTATE_VERSION, version);
        error = true;
    } else if version < GBA_SAVESTATE_MAGIC + GBA_SAVESTATE_VERSION {
        mlog!(Level::Warn, "GBA State", "Old savestate: expected {:08X}, got {:08X}, continuing anyway", GBA_SAVESTATE_MAGIC + GBA_SAVESTATE_VERSION, version);
    }
    if bios_checksum != gba.bios_checksum {
        mlog!(Level::Warn, "GBA State", "Savestate created using a different version of the BIOS: expected {:08X}, got {:08X}", gba.bios_checksum, bios_checksum);
        let pc = gprs[ARM_PC] as u32;
        if (bios_checksum == GBA_BIOS_CHECKSUM || gba.bios_checksum == GBA_BIOS_CHECKSUM)
            && pc < GBA_SIZE_BIOS as u32
            && pc >= 0x20
        {
            error = true;
        }
    }
    if !gba.memory.rom.is_empty() {
        let (cart_id, cart_title) = if gba.memory.rom.len() >= 0xB0 {
            (
                u32::from_le_bytes(gba.memory.rom[0xAC..0xB0].try_into().unwrap()),
                &gba.memory.rom[0xA0..0xAC],
            )
        } else {
            (0, &[][..])
        };
        if id != cart_id || cart_title != &title[..] {
            mlog!(Level::Warn, "GBA State", "Savestate is for a different game");
            error = true;
        }
    } else if id != 0 {
        mlog!(Level::Warn, "GBA State", "Savestate is for a game, but no game loaded");
        error = true;
    }
    if rom_crc32 != gba.rom_crc32 {
        mlog!(Level::Warn, "GBA State", "Savestate is for a different version of the game");
    }
    if cpu_cycles < 0 {
        mlog!(Level::Warn, "GBA State", "Savestate is corrupted: CPU cycles are negative");
        error = true;
    }
    if cpu_cycles >= GBA_ARM7TDMI_FREQUENCY as i32 {
        mlog!(Level::Warn, "GBA State", "Savestate is corrupted: CPU cycles are too high");
        error = true;
    }
    {
        let check = gprs[ARM_PC] as u32;
        let region = check >> 24;
        if (region == GBA_REGION_ROM0 || region == GBA_REGION_ROM1 || region == GBA_REGION_ROM2)
            && (check.wrapping_sub(WORD_SIZE_ARM as u32) & GBA_SIZE_ROM0 as u32)
                >= (gba.memory.rom_size as u32).wrapping_sub(WORD_SIZE_ARM as u32)
        {
            mlog!(Level::Warn, "GBA State", "Savestate created using a differently sized version of the ROM");
            error = true;
        }
    }
    if error {
        return Err("corrupt state");
    }

    // --- Commit (GBADeserialize's order) ---
    gba.timing.clear();
    gba.timing.master_cycles = master_cycles;
    gba.timing.global_cycles = global_cycles;

    gba.cpu.gprs = gprs;
    gba.cpu.cpsr.packed = cpsr;
    gba.cpu.spsr.packed = spsr;
    gba.cpu.cycles = cpu_cycles;
    gba.cpu.next_event = cpu_next_event;
    gba.cpu.banked_registers = banked_registers;
    gba.cpu.banked_spsrs = banked_spsrs;
    gba.cpu.privilege_mode = gba.cpu.cpsr.priv_mode();
    if gba.cpu.gprs[ARM_PC] & 1 != 0 {
        mlog!(Level::Warn, "GBA State", "Savestate has unaligned PC and is probably corrupted");
        gba.cpu.gprs[ARM_PC] &= !1;
    }

    // GBAUnlCartDeserialize would run here (remaps the ROM before the
    // pipeline reset). Unlicensed carts are unported; warn if the state
    // expects one.
    if hw.unl_cart_flags & 0x1F != 0 {
        mlog!(Level::Warn, "GBA State", "Save state expects unlicensed cart state; not restoring bootleg state");
    }

    gba.memory.active_region = -1;
    let pc = gba.cpu.gprs[ARM_PC] as u32;
    gba.set_active_region(pc);
    if bios_prefetch != 0 {
        gba.memory.bios_prefetch = bios_prefetch;
    }
    gba.memory.last_prefetched_pc = last_prefetched_pc;
    if gba.cpu.cpsr.t() {
        gba.cpu.execution_mode = ExecutionMode::Thumb;
        if cpu_prefetch[0] != 0 && cpu_prefetch[1] != 0 {
            gba.cpu.prefetch = [cpu_prefetch[0] & 0xFFFF, cpu_prefetch[1] & 0xFFFF];
        } else {
            // Maintain backwards compat
            gba.cpu.prefetch[0] = gba
                .active_region_load16(pc.wrapping_sub(WORD_SIZE_THUMB as u32) & gba.cpu.active_mask);
            gba.cpu.prefetch[1] = gba.active_region_load16(pc & gba.cpu.active_mask);
        }
    } else {
        gba.cpu.execution_mode = ExecutionMode::Arm;
        if cpu_prefetch[0] != 0 && cpu_prefetch[1] != 0 {
            gba.cpu.prefetch = cpu_prefetch;
        } else {
            // Maintain backwards compat
            gba.cpu.prefetch[0] = gba
                .active_region_load32(pc.wrapping_sub(WORD_SIZE_ARM as u32) & gba.cpu.active_mask);
            gba.cpu.prefetch[1] = gba.active_region_load32(pc & gba.cpu.active_mask);
        }
    }

    gba.cpu.halted = (misc_flags & 1) as i32;
    gba.memory.io[(GBA_REG_POSTFLG >> 1) as usize] = ((misc_flags >> 1) & 1) as u16;
    if misc_flags & 4 != 0 {
        gba.schedule(EventId::IrqEvent, next_irq);
    }
    gba.cpu_blocked = misc_flags & 8 != 0;
    gba.keys_last = ((misc_flags >> 4) & 0x7FF) as u16;
    // biosStall: discarded (HLE BIOS stall unported)

    video_deserialize_commit(gba, &io_words, &pa, &pram, &oam, vram, video_next_event, video_flags, video_frame_counter, version);

    // GBAMemoryDeserialize
    gba.memory.wram.copy_from_slice(&wram);
    gba.memory.iwram.copy_from_slice(&iwram);

    io_deserialize_commit(gba, version, &io_words, &timers, &dmas, bus, dma_transfer_register, dma_latch, dma_count_latch, dma_block_pc, &hw);

    audio_deserialize_commit(gba, version, &io_words, &pa);

    savedata_deserialize_commit(gba, &sd);

    gba.timing.interrupt();

    Ok(())
}

/// GBAVideoDeserialize. Runs before the IO commit, reading DISPSTAT/DISPCNT
/// from the state's io copy exactly like the C.
#[allow(clippy::too_many_arguments)]
fn video_deserialize_commit(
    gba: &mut Gba,
    io: &[u16; GBA_SIZE_IO / 2],
    pa: &ParsedAudio,
    pram: &[u8; GBA_SIZE_PALETTE_RAM],
    oam: &[u8; GBA_SIZE_OAM],
    vram: Vec<u8>,
    video_next_event: u32,
    video_flags: u32,
    video_frame_counter: u32,
    version: u32,
) {
    gba.video.vram.copy_from_slice(&vram);
    let mut cc: i32 = 0;
    for i in (0..GBA_SIZE_OAM).step_by(2) {
        let value = u16::from_le_bytes([oam[i], oam[i + 1]]);
        gba.store16(GBA_BASE_OAM | i as u32, value as i32, &mut cc);
    }
    for i in (0..GBA_SIZE_PALETTE_RAM).step_by(2) {
        let value = u16::from_le_bytes([pram[i], pram[i + 1]]);
        gba.store16(GBA_BASE_PALETTE_RAM | i as u32, value as i32, &mut cc);
    }
    gba.video.frame_counter = video_frame_counter;

    gba.video.stall_mask = 0;
    let dispstat = io[(GBA_REG_DISPSTAT >> 1) as usize];
    match video_flags & 3 {
        0 => {
            if dispstat_in_hblank(dispstat) {
                gba.video.event_kind = VideoEventKind::StartHdraw;
            } else {
                gba.video.event_kind = VideoEventKind::StartHblank;
            }
        }
        1 => {
            gba.video.event_kind = VideoEventKind::StartHdraw;
        }
        2 => {
            gba.video.event_kind = VideoEventKind::StartHblank;
            gba.video.stall_mask = calculate_stall_mask(gba, io[(GBA_REG_DISPCNT >> 1) as usize]);
        }
        _ => {
            gba.video.event_kind = VideoEventKind::StartHdraw;
        }
    }
    let when = if version < 0x01000007 {
        // This field was moved in v7
        pa.last_sample as u32
    } else {
        video_next_event
    };
    gba.schedule(EventId::Video, when as i32);

    gba.video.vcount = io[(GBA_REG_VCOUNT >> 1) as usize] as i32;
    gba.renderer_reset();
}

/// _calculateStallMask (gba/video.c). Reads BGCNT from live memory.io like
/// the C does (GBAVideoDeserialize runs before the io commit, so those are
/// the pre-restore values — as in the C).
fn calculate_stall_mask(gba: &Gba, dispcnt: u16) -> u32 {
    let mut mask: u32 = 0;
    if crate::video::dispcnt_forced_blank(dispcnt) {
        return 0;
    }
    let bg_enabled = |bg: i32| dispcnt & (0x100 << bg) != 0;
    match crate::video::dispcnt_mode(dispcnt) {
        0 => {
            for bg in 0..4 {
                if bg_enabled(bg) {
                    let cnt = gba.memory.io[((GBA_REG_BG0CNT as usize + bg as usize * 2) >> 1) as usize];
                    if crate::video::bgcnt_is_256color(cnt) {
                        mask |= 0x010 << bg;
                    } else {
                        mask |= 0x011 << bg;
                    }
                }
            }
        }
        1 => {
            for bg in 0..2 {
                if bg_enabled(bg) {
                    let cnt = gba.memory.io[((GBA_REG_BG0CNT as usize + bg as usize * 2) >> 1) as usize];
                    if crate::video::bgcnt_is_256color(cnt) {
                        mask |= 0x010 << bg;
                    } else {
                        mask |= 0x011 << bg;
                    }
                }
            }
            if bg_enabled(2) {
                mask |= crate::video::GBA_VSTALL_A2;
            }
        }
        2 => {
            if bg_enabled(2) {
                mask |= crate::video::GBA_VSTALL_A2;
            }
            if bg_enabled(3) {
                mask |= crate::video::GBA_VSTALL_A3;
            }
        }
        3..=5 => {
            if bg_enabled(2) {
                mask |= crate::video::GBA_VSTALL_B;
            }
        }
        _ => {}
    }
    mask
}

/// GBAIODeserialize: ordered register replay, timers, sio latch, bus, then
/// GBADMADeserialize and GBAHardwareDeserialize.
#[allow(clippy::too_many_arguments)]
fn io_deserialize_commit(
    gba: &mut Gba,
    version: u32,
    io: &[u16; GBA_SIZE_IO / 2],
    timers: &[ParsedTimer; 4],
    dmas: &[ParsedDma; 4],
    bus: u32,
    dma_transfer_register: u32,
    dma_latch: [u32; 3],
    dma_count_latch: [u16; 4],
    dma_block_pc: u32,
    hw: &ParsedHw,
) {
    gba.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] = io[(GBA_REG_SOUNDCNT_X >> 1) as usize];
    let soundcnt = gba.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize];
    gba.audio_write_soundcnt_x(soundcnt);

    let mut i: u32 = 0;
    // GBA_REG_MAX is 0x20A in the vendored C (io.rs has 0x400, but every
    // table entry at 0x20A..0x400 is zero anyway).
    while i < GBA_REG_MAX_ADDR {
        let idx = (i >> 1) as usize;
        if IS_WSPECIAL_REGISTER[idx] {
            gba.memory.io[idx] = io[idx];
        } else if IS_VALID_REGISTER[idx] {
            gba.io_write(i, io[idx]);
        }
        i += 2;
    }
    if version >= 0x01000006 {
        let v = gba.memory.io[(GBA_REG_INTERNAL_EXWAITCNT_HI >> 1) as usize];
        gba.io_write(GBA_REG_EXWAITCNT_HI, v);
    }

    let now = gba.current_time();
    for t in 0..4 {
        gba.timers[t].reload = timers[t].reload;
        gba.timers[t].flags = timers[t].flags;
        gba.timers[t].last_event = (timers[t].last_event as i32).wrapping_add(now);
        if (t < 1 || timers[t].flags & TIMER_COUNT_UP == 0) && timers[t].flags & TIMER_ENABLE != 0 {
            gba.schedule(timer_event_id(t), timers[t].next_event as i32);
        }
        // else: the C only stashes event.when; our envents carry no stale
        // when, so nothing to do.
    }
    gba.sio.siocnt = gba.memory.io[(GBA_REG_SIOCNT >> 1) as usize];
    let rcnt = gba.memory.io[(GBA_REG_RCNT >> 1) as usize];
    gba.sio_write_rcnt(rcnt);

    gba.bus = bus;

    // GBADMADeserialize
    for t in 0..4 {
        let count_io = gba.memory.io[((GBA_REG_DMA0CNT_LO + 12 * t as u32) >> 1) as usize] as i32;
        let d = &mut gba.dma[t];
        d.reg = io[((GBA_REG_DMA0CNT_HI + 12 * t as u32) >> 1) as usize];
        d.next_source = dmas[t].next_source;
        d.next_dest = dmas[t].next_dest;
        d.next_count = dmas[t].next_count;
        d.when = dmas[t].when;
        if version >= GBA_SAVESTATE_MAGIC + 0xB {
            d.count = dma_count_latch[t] as i32;
        } else {
            d.count = count_io;
        }
        if t == 3 {
            if d.count == 0 {
                d.count = 0x10000;
            }
        } else {
            d.count &= 0x3FFF;
            if d.count == 0 {
                d.count = 0x4000;
            }
        }
        let width = 2i32 << ((d.reg >> 10) & 1);
        if d.source >= GBA_BASE_ROM0 && d.source < GBA_BASE_SRAM {
            d.source_offset = width;
        } else {
            d.source_offset = DMA_OFFSET[((d.reg >> 7) & 3) as usize] * width;
        }
        d.dest_offset = DMA_OFFSET[((d.reg >> 5) & 3) as usize] * width;
    }
    gba.dma[0].latch = dma_transfer_register;
    if version >= GBA_SAVESTATE_MAGIC + 0xA {
        gba.dma[1].latch = dma_latch[0];
        gba.dma[2].latch = dma_latch[1];
        gba.dma[3].latch = dma_latch[2];
    } else {
        gba.dma[1].latch = gba.dma[0].latch;
        gba.dma[2].latch = gba.dma[0].latch;
        gba.dma[3].latch = gba.dma[0].latch;
    }
    gba.dma_pc = dma_block_pc;
    gba.dma_recalculate_cycles();
    gba.dma_update();

    // GBAHardwareDeserialize
    gba.hw.read_write = (hw.flags1 & 1) as u16;
    gba.hw.write_latch = hw.write_latch & 0xF;
    gba.hw.pin_state = hw.pin_state & 0xF;
    gba.hw.direction = hw.pin_direction & 0xF;
    gba.hw.devices = hw.devices as u32;

    if gba.hw.devices & HW_GPIO != 0 {
        // gpioBase is the ROM's 0xC4 window here; mirror the C's st store.
        let g = 0xC4usize;
        if gba.hw.read_write != 0 {
            if gba.memory.rom.len() > g + 5 {
                let ps = gba.hw.pin_state as u16;
                gba.memory.rom[g] = ps as u8;
                gba.memory.rom[g + 1] = (ps >> 8) as u8;
                let dir = gba.hw.direction as u16;
                gba.memory.rom[g + 2] = dir as u8;
                gba.memory.rom[g + 3] = (dir >> 8) as u8;
                let rw = gba.hw.read_write;
                gba.memory.rom[g + 4] = rw as u8;
                gba.memory.rom[g + 5] = (rw >> 8) as u8;
            }
        } else {
            for k in 0..3 {
                if gba.memory.rom.len() > g + k * 2 + 1 {
                    gba.memory.rom[g + k * 2] = 0;
                    gba.memory.rom[g + k * 2 + 1] = 0;
                }
            }
        }
    }

    gba.hw.rtc_sio_output = hw.flags3 & 1 != 0;
    gba.hw.rtc_bytes_remaining = hw.rtc_bytes_remaining;
    gba.hw.rtc_bits_read = hw.rtc_bits_read;
    gba.hw.rtc_bits = hw.rtc_bits;
    gba.hw.rtc_command_active = hw.rtc_command_active != 0;
    gba.hw.rtc_sck_edge = hw.flags1 & (1 << 3) != 0;
    gba.hw.rtc_command = hw.rtc_command;
    gba.hw.rtc_control = hw.rtc_control;
    gba.hw.rtc_time = hw.time;

    gba.hw.gyro_sample = hw.gyro_sample;
    gba.hw.gyro_edge = hw.flags1 & (1 << 1) != 0;
    gba.hw.tilt_x = hw.tilt_x;
    gba.hw.tilt_y = hw.tilt_y;
    gba.hw.tilt_state = (hw.flags2 & 3) as i32;
    gba.hw.light_counter = hw.flags1 >> 4;
    gba.hw.light_sample = hw.light_sample;
    gba.hw.light_edge = hw.flags1 & (1 << 2) != 0;

    // GBP/SIO legacy bits (GbpInputsPosted/GbpTxPosition): no such state.
    // HW_GB_PLAYER would install the GBP driver; not ported.

    if gba.memory.io[(GBA_REG_SIOCNT >> 1) as usize] & 0x0080 != 0 && hw.sio_next_event < 0x20000 {
        gba.schedule(EventId::Sio, hw.sio_next_event as i32);
    }
}

/// GBAAudioDeserialize (GBAudioPSGDeserialize + GBA side).
fn audio_deserialize_commit(
    gba: &mut Gba,
    version: u32,
    io: &[u16; GBA_SIZE_IO / 2],
    pa: &ParsedAudio,
) {
    let nr52 = (gba.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] & 0xFF) as u8;
    let now = gba.current_time();
    {
        let a = &mut gba.audio;
        let p = &mut a.psg;
        p.playing_ch1 = nr52 & 0x1 != 0;
        p.playing_ch2 = nr52 & 0x2 != 0;
        p.playing_ch3 = nr52 & 0x4 != 0;
        p.playing_ch4 = nr52 & 0x8 != 0;
        p.enable = nr52 & 0x80 != 0;

        // The C arms psg.frameEvent at ch1.nextFrame (GB_AUDIO_GBA). Our
        // frame event is folded into the sample event: choose the phase so
        // the next frame fires after `next_frame` cycles on the 4*8192-grid.
        let extra = ((pa.next_frame.wrapping_sub(pa.next_sample) >> 10) & 31) as i32;
        a.frame_phase = (FRAME_EVENT_MOD - extra) & (FRAME_EVENT_MOD - 1);

        p.frame = ((pa.flags >> 22) & 7) as i32;
        p.skip_frame = ((pa.flags >> 28) & 1) != 0;

        p.ch1.envelope.current_volume = (pa.flags & 0xF) as i32;
        p.ch1.envelope.dead = ((pa.flags >> 4) & 3) as i32;
        p.ch1.sweep.enable = pa.flags & (1 << 25) != 0;
        p.ch1.sweep.occurred = pa.flags & (1 << 26) != 0;
        p.ch1.sweep.time = (pa.ch1_sweep & 7) as i32;
        if p.ch1.sweep.time == 0 {
            p.ch1.sweep.time = 8;
        }
        p.ch1.control.length = (pa.ch1_flags & 0x7F) as i32;
        p.ch1.envelope.next_step = ((pa.ch1_flags >> 7) & 7) as i32;
        p.ch1.sweep.real_frequency = ((pa.ch1_flags >> 10) & 0x7FF) as i32;
        p.ch1.index = ((pa.ch1_flags >> 21) & 7) as u8;
        p.ch1.last_update = pa.ch1_last_update_delta.wrapping_add(now);

        p.ch2.envelope.current_volume = ((pa.flags >> 8) & 0xF) as i32;
        p.ch2.envelope.dead = ((pa.flags >> 12) & 3) as i32;
        p.ch2.control.length = (pa.ch2_flags & 0x7F) as i32;
        p.ch2.envelope.next_step = ((pa.ch2_flags >> 7) & 7) as i32;
        p.ch2.index = ((pa.ch2_flags >> 21) & 7) as u8;
        p.ch2.last_update = pa.ch2_last_update_delta.wrapping_add(now);

        p.ch3.readable = pa.flags & (1 << 27) != 0;
        p.ch3.wavedata32 = pa.wavebanks;
        p.ch3.length = pa.ch3_length as u32;
        p.ch3.next_update = pa.ch3_next_event_delta.wrapping_add(now);

        p.ch4.envelope.current_volume = ((pa.flags >> 16) & 0xF) as i32;
        p.ch4.envelope.dead = ((pa.flags >> 20) & 3) as i32;
        p.ch4.length = (pa.ch4_flags & 0x7F) as i32;
        p.ch4.envelope.next_step = ((pa.ch4_flags >> 7) & 7) as i32;
        p.ch4.lfsr = pa.ch4_lfsr;
        p.ch4.last_event = pa.ch4_last_event;
        if p.ch4.envelope.dead < 2 && p.playing_ch4 {
            if p.ch4.last_event == 0 {
                // Back-compat: fake this value
                let current_time = now as u32;
                let mut cycles: i32 = if p.ch4.ratio != 0 { 2 * p.ch4.ratio } else { 1 };
                cycles <<= p.ch4.frequency;
                cycles = cycles.wrapping_mul(8 * p.timing_factor);
                p.ch4.last_event = current_time
                    .wrapping_add(pa.ch4_next_event & cycles.wrapping_sub(1) as u32)
                    .wrapping_sub(cycles as u32);
            }
        }
        p.ch4.n_samples = 0;
        p.ch4.samples = 0;
    }

    // Restart-channel IO writes (registers are direct-stored by the io
    // commit's table path; these replay the audio latching side effects).
    gba.io_write(GBA_REG_SOUND1CNT_X, io[(GBA_REG_SOUND1CNT_X >> 1) as usize] & 0x7FFF);
    gba.io_write(GBA_REG_SOUND2CNT_HI, io[(GBA_REG_SOUND2CNT_HI >> 1) as usize] & 0x7FFF);
    gba.io_write(GBA_REG_SOUND3CNT_X, io[(GBA_REG_SOUND3CNT_X >> 1) as usize] & 0x7FFF);
    gba.io_write(GBA_REG_SOUND4CNT_HI, io[(GBA_REG_SOUND4CNT_HI >> 1) as usize] & 0x7FFF);

    {
        let a = &mut gba.audio;
        a.ch_a.internal_sample = pa.internal_a;
        a.ch_b.internal_sample = pa.internal_b;
        a.ch_a.samples = pa.samples_a;
        a.ch_b.samples = pa.samples_b;
        a.current_samples = pa.current_samples;
        a.last_sample = pa.last_sample;

        a.ch_a.fifo = pa.fifo_a;
        a.ch_b.fifo = pa.fifo_b;
        a.ch_a.fifo_read = 0;
        a.ch_b.fifo_read = 0;

        a.ch_a.fifo_write = ((pa.gba_flags >> 7) & 0x7) as i32;
        a.ch_b.fifo_write = ((pa.gba_flags >> 2) & 0x7) as i32;
        a.ch_a.internal_remaining = ((pa.gba_flags >> 5) & 0x3) as i32;
        a.ch_b.internal_remaining = (pa.gba_flags & 0x3) as i32;

        a.sample_index = (pa.gba_flags2 & 0xF) as usize;
        if (pa.gba_flags2 >> 4) & 0x3 > 0 {
            a.ch_a.dma_source = ((pa.gba_flags2 >> 4) & 0x3) as i32 - 1;
        }
        if (pa.gba_flags2 >> 6) & 0x3 > 0 {
            a.ch_b.dma_source = ((pa.gba_flags2 >> 6) & 0x3) as i32 - 1;
        }

        if version < 0x01000007 {
            a.last_sample = pa.next_sample.wrapping_sub(AUDIO_SAMPLE_INTERVAL);
        }
    }
    gba.schedule(EventId::AudioSample, pa.next_sample);
}

/// GBASavedataDeserialize.
fn savedata_deserialize_commit(gba: &mut Gba, sd: &ParsedSavedata) {
    let st = match sd.savedata_type {
        0 => Some(SavedataType::ForceNone),
        1 => Some(SavedataType::Sram),
        2 => Some(SavedataType::Flash512),
        3 => Some(SavedataType::Flash1M),
        4 => Some(SavedataType::Eeprom),
        5 => Some(SavedataType::Eeprom512),
        6 => Some(SavedataType::Sram512),
        0xFF => Some(SavedataType::Autodetect),
        _ => None,
    };
    if let Some(st) = st {
        if st != gba.savedata.savedata_type {
            mlog!(Level::Debug, "GBA State", "Switching save types");
            gba.savedata_force_type(st);
        }
    }
    gba.savedata.command = sd.command;
    gba.savedata.flash_state = sd.flags & 3;
    gba.savedata.read_bits_remaining = sd.read_bits_remaining;
    gba.savedata.read_address = sd.read_address;
    gba.savedata.write_address = sd.write_address;
    gba.savedata.settling = sd.settling_sector as i32;

    if gba.savedata.savedata_type == SavedataType::Flash1M {
        // _flashSwitchBank with the type already Flash1M: just the selector.
        gba.savedata.current_bank = ((sd.flags >> 4) & 1) as usize;
    }
    if sd.flags & (1 << 5) != 0 {
        // The C schedules the dust event; ours is a countdown.
        gba.savedata.settling_pending = sd.settling_dust as i32;
    }
}

// ---------------------------------------------------------------------------
// Save-slot keeper (for F1-style slots in the SDL frontend)
// ---------------------------------------------------------------------------

/// Number of save slots in the keeper.
pub const GBA_STATE_SLOTS: usize = 256;

/// In-memory save slot store. Not thread-safe; the frontend owns it.
pub struct StateKeeper {
    slots: Box<[Option<Vec<u8>>; GBA_STATE_SLOTS]>,
}

impl StateKeeper {
    pub fn new() -> Self {
        StateKeeper {
            slots: Box::new(std::array::from_fn(|_| None)),
        }
    }

    /// Save a state into `slot` (overwriting).
    pub fn save(&mut self, gba: &mut Gba, slot: u8) -> Result<(), &'static str> {
        self.slots[slot as usize] = Some(serialize(gba)?);
        Ok(())
    }

    /// Restore the state in `slot`.
    pub fn load(&mut self, gba: &mut Gba, slot: u8) -> Result<(), &'static str> {
        match &self.slots[slot as usize] {
            Some(state) => deserialize(gba, state),
            None => Err("empty save slot"),
        }
    }

    pub fn clear(&mut self, slot: u8) {
        self.slots[slot as usize] = None;
    }

    pub fn is_used(&self, slot: u8) -> bool {
        self.slots[slot as usize].is_some()
    }
}

impl Default for StateKeeper {
    fn default() -> Self {
        Self::new()
    }
}

/// state_keeper: create the 256-cell slot list.
pub fn state_keeper() -> StateKeeper {
    StateKeeper::new()
}
