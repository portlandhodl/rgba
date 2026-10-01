// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gb/serialize.c, mgba/src/gb/video.c (GBVideoSerialize,
// GBSGBSerialize parts), mgba/src/gb/audio.c (GBAudioSerialize/PSGSerialize),
// mgba/src/gb/memory.c (GBMemorySerialize) and mgba/src/gb/io.c
// (GBIOSerialize).
//
// The produced byte layout matches mGBA's GBSerializedState v3 (0x11800
// bytes), so states are cross-compatible with mGBA 0.10+.

use rgba_core::serialize::{Deserializer, Serializer};
use rgba_core::{mlog, Level};

use crate::gb::{EventId, Gb, GbModel};
use crate::io::GB_REG_BANK;
use crate::memory::{MbcType, GB_SIZE_CART_BANK0, GB_SIZE_HRAM, GB_SIZE_IO, GB_SIZE_WORKING_RAM};
use crate::video::{
    ModeEventKind, GB_SIZE_OAM, GB_SIZE_VRAM, GB_VIDEO_HORIZONTAL_PIXELS,
    GB_VIDEO_VERTICAL_PIXELS, GB_VIDEO_VERTICAL_TOTAL_PIXELS,
};
use crate::video::{SGB_SIZE_ATF_RAM, SGB_SIZE_CHAR_RAM, SGB_SIZE_MAP_RAM, SGB_SIZE_PAL_RAM};

pub const GB_SAVESTATE_MAGIC: u32 = 0x00400000;
pub const GB_SAVESTATE_VERSION: u32 = 0x00000003;
pub const GB_SAVESTATE_SIZE: usize = 0x11800;

fn model_to_u8(model: GbModel) -> u8 {
    model as i32 as u8
}
fn model_from_u8(v: u8) -> GbModel {
    match v as i32 as u32 {
        x if x == GbModel::Dmg as u32 => GbModel::Dmg,
        x if x == GbModel::Sgb as u32 => GbModel::Sgb,
        x if x == GbModel::Mgb as u32 => GbModel::Mgb,
        x if x == GbModel::Sgb2 as u32 => GbModel::Sgb2,
        x if x == GbModel::Cgb as u32 => GbModel::Cgb,
        x if x == GbModel::Agb as u32 => GbModel::Agb,
        x if x == GbModel::Scgb as u32 => GbModel::Scgb,
        _ => GbModel::Autodetect,
    }
    .into_model_safe()
}

trait IntoModelSafe {
    fn into_model_safe(self) -> GbModel;
}
impl IntoModelSafe for GbModel {
    fn into_model_safe(self) -> GbModel {
        self
    }
}

// ---------------------------------------------------------------------------
// Serialize
// ---------------------------------------------------------------------------

/// GBSerialize: writes exactly GB_SAVESTATE_SIZE bytes.
pub fn serialize(gb: &mut Gb) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::with_capacity(GB_SAVESTATE_SIZE);
    {
        let s = &mut Serializer::new(&mut out);
        s.put_u32(GB_SAVESTATE_MAGIC + GB_SAVESTATE_VERSION);
        s.put_u32(gb.rom_crc32);
        s.put_u8(model_to_u8(gb.model));
        s.put_bytes(&[0; 3]); // reservedHeader
        s.put_u32(gb.timing.master_cycles);

        if !gb.memory.rom.is_empty() && gb.memory.rom.len() >= 0x144 {
            s.put_bytes(&gb.memory.rom[0x134..0x144]);
        } else {
            s.put_bytes(&[0; 16]);
        }

        // cpu
        s.put_u8(gb.cpu.a);
        s.put_u8(gb.cpu.f.packed);
        s.put_u8(gb.cpu.b);
        s.put_u8(gb.cpu.c);
        s.put_u8(gb.cpu.d);
        s.put_u8(gb.cpu.e);
        s.put_u8(gb.cpu.h);
        s.put_u8(gb.cpu.l);
        s.put_u16(gb.cpu.sp);
        s.put_u16(gb.cpu.pc);
        s.put_i32(gb.cpu.cycles);
        s.put_i32(gb.cpu.next_event);
        s.put_u16(0); // reservedInstruction
        s.put_u16(gb.cpu.index);
        s.put_u8(gb.cpu.bus);
        s.put_u8(gb.cpu.execution_state as u8);
        s.put_u16(0); // reserved

        let ei_pending_when = gb.until(EventId::EiPending);
        s.put_u32(if gb.is_scheduled(EventId::EiPending) {
            ei_pending_when as u32
        } else {
            // matching C: eiPending.when - currentTime even when descheduled
            ei_pending_when as u32
        });
        s.put_i32(0); // reservedDiPending

        let mut flags: u32 = 0;
        flags |= gb.cpu.condition as u32;
        flags |= (gb.cpu.irq_pending as u32) << 1;
        flags |= (gb.double_speed as u32) << 2;
        flags |= (gb.is_scheduled(EventId::EiPending) as u32) << 3;
        flags |= (gb.cpu.halted as u32) << 4;
        flags |= (gb.cpu_blocked as u32) << 5;
        s.put_u32(flags);

        // audio (GBAudioSerialize)
        audio_serialize(gb, s);

        // video (GBVideoSerialize)
        video_serialize(gb, s);

        // timer (GBTimerSerialize)
        s.put_u32(gb.until(EventId::Timer) as u32);
        s.put_u32(gb.until(EventId::TimerIrq) as u32);
        s.put_u32(gb.timer.next_div as u32);
        s.put_u32(gb.timer.internal_div);
        s.put_u8(gb.timer.tima_period as u8);
        let mut tflags: u8 = 0;
        tflags |= gb.is_scheduled(EventId::TimerIrq) as u8;
        s.put_u8(tflags);
        s.put_u16(0); // reserved

        // memory (GBMemorySerialize)
        memory_serialize(gb, s);

        s.put_u64(gb.timing.global_cycles);
        s.put_u16(gb.memory.cart_bus_pc);
        s.put_bytes(&[0; 54]); // reserved[27]

        // audio2
        s.put_i32(gb.audio.last_sample);
        s.put_u8(gb.audio.sample_index as u8);
        s.put_bytes(&[0; 3]);
        for sample in gb.audio.current_samples.iter() {
            s.put_i16(sample.left);
            s.put_i16(sample.right);
        }

        s.put_bytes(&gb.video.oam);
        s.put_bytes(&gb.memory.io);
        s.put_bytes(&gb.memory.hram[..GB_SIZE_HRAM]);
        s.put_u8(gb.memory.ie);

        s.put_bytes(&gb.video.vram);
        s.put_bytes(&gb.memory.wram);

        s.put_bytes(&[0; 0x290]); // reserved2[0xA4]

        // mbc register unions
        match gb.memory.mbc_type {
            MbcType::HuC3 => {
                let mut buf = [0u8; 0x80];
                for i in 0..0x80 {
                    buf[i] = (gb.memory.mbc_state.huc3.registers[i * 2] & 0xF)
                        | (gb.memory.mbc_state.huc3.registers[i * 2 + 1] << 4);
                }
                s.put_bytes(&buf);
            }
            MbcType::PocketCam => {
                s.put_bytes(&gb.memory.mbc_state.pocket_cam.registers);
                s.put_bytes(&[0; 0x80 - 0x36]);
            }
            MbcType::Tama5 => {
                let st = &gb.memory.mbc_state.tama5;
                let mut buf = [0u8; 4];
                for i in 0..4 {
                    buf[i] = (st.registers[i * 2] & 0xF) | (st.registers[i * 2 + 1] << 4);
                }
                s.put_bytes(&buf);
                s.put_bytes(&[0; 4]);
                let mut packs = |page: &[u8; 16]| {
                    let mut b = [0u8; 8];
                    for i in 0..8 {
                        b[i] = (page[i * 2] & 0xF) | (page[i * 2 + 1] << 4);
                    }
                    s.put_bytes(&b);
                };
                packs(&st.rtc_timer_page);
                packs(&st.rtc_alarm_page);
                packs(&st.rtc_free_page0);
                packs(&st.rtc_free_page1);
            }
            _ => {
                s.put_bytes(&[0; 0x80]);
            }
        }

        // sgb
        sgb_serialize(gb, s);

        debug_assert_eq!(out.len(), GB_SAVESTATE_SIZE);
    }
    Ok(out)
}

fn audio_serialize(gb: &mut Gb, s: &mut Serializer) {
    // GBAudioPSGSerialize
    let now = gb.current_time();
    let a = &mut gb.audio;
    let mut flags: u32 = 0;
    flags |= (a.frame as u32 & 7) << 22;
    flags |= ((a.frame_skip != 0) as u32) << 28;

    let frame_next = 0u32; // GB stores frame event separately; GBA-only field
    // ch1
    let mut ch1_flags: u32 = 0;
    flags |= (a.ch1.envelope.current_volume as u32 & 0xF) << 0;
    flags |= (a.ch1.envelope.dead as u32 & 0x3) << 4;
    flags |= (a.ch1.sweep.enable as u32) << 25;
    flags |= (a.ch1.sweep.occurred as u32) << 26;
    ch1_flags |= (a.ch1.control.length as u32 & 0x7F) << 0;
    ch1_flags |= (a.ch1.envelope.next_step as u32 & 7) << 7;
    ch1_flags |= (a.ch1.sweep.real_frequency as u32 & 0x7FF) << 10;
    ch1_flags |= (a.ch1.index as u32 & 7) << 21;
    s.put_u32(ch1_flags);
    s.put_i32(frame_next as i32); // ch1.nextFrame
    s.put_i32(0); // reserved
    let sweepv = (a.ch1.sweep.time & 7) as u32;
    s.put_u32(sweepv);
    s.put_u32(a.ch1.last_update.wrapping_sub(now) as u32);

    // ch2
    let mut ch2_flags: u32 = 0;
    flags |= (a.ch2.envelope.current_volume as u32 & 0xF) << 8;
    flags |= (a.ch2.envelope.dead as u32 & 0x3) << 12;
    ch2_flags |= (a.ch2.control.length as u32 & 0x7F) << 0;
    ch2_flags |= (a.ch2.envelope.next_step as u32 & 7) << 7;
    ch2_flags |= (a.ch2.index as u32 & 7) << 21;
    s.put_u32(ch2_flags);
    s.put_i32(0);
    s.put_i32(0);
    s.put_u32(a.ch2.last_update.wrapping_sub(now) as u32);

    // ch3
    flags |= (a.ch3.readable as u32) << 27;
    // The C source is wavedata32[8] (= wave RAM plus 16 bytes of padding);
    // only the first 16 bytes are real wave data.
    for i in 0..4 {
        let b = &a.ch3.wavedata8[i * 4..i * 4 + 4];
        s.put_u32(u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    }
    for _ in 0..4 {
        s.put_u32(0);
    }
    s.put_i16(a.ch3.length as i16);
    s.put_i16(0); // reserved
    s.put_u32(a.ch3.next_update.wrapping_sub(now) as u32);

    // ch4
    flags |= (a.ch4.envelope.current_volume as u32 & 0xF) << 16;
    flags |= (a.ch4.envelope.dead as u32 & 0x3) << 20;
    s.put_u32(a.ch4.lfsr);
    let mut ch4_flags: u32 = 0;
    ch4_flags |= (a.ch4.length as u32 & 0x7F) << 0;
    ch4_flags |= (a.ch4.envelope.next_step as u32 & 7) << 7;
    s.put_u32(ch4_flags);
    s.put_i32(a.ch4.last_event as i32);
    let mut cycles = if a.ch4.ratio != 0 { 2 * a.ch4.ratio } else { 1 };
    cycles <<= a.ch4.frequency;
    cycles *= 8 * a.timing_factor;
    s.put_u32(a.ch4.last_event.wrapping_add(cycles as u32));

    s.put_u32(flags);
    s.put_i32(a.cap_left);
    s.put_i32(a.cap_right);
    let sample_until = gb.until(EventId::AudioSample) as u32;
    s.put_u32(sample_until);
}

fn video_serialize(gb: &mut Gb, s: &mut Serializer) {
    let frame_until = gb.until(EventId::VideoFrame) as u32;
    let mode_until = gb.until(EventId::VideoMode) as u32;
    let v = &gb.video;
    s.put_i16(v.x as i16);
    s.put_i16(v.ly as i16);
    s.put_u32(frame_until);
    s.put_u32(0); // reserved
    s.put_u32(mode_until);
    s.put_i32(v.dot_clock);
    s.put_u32(v.frame_counter);
    s.put_u8(v.vram_current_bank as u8);
    let mut flags: u8 = 0;
    flags |= v.bcp_increment as u8;
    flags |= (v.ocp_increment as u8) << 1;
    flags |= ((v.mode as u8) & 3) << 2;
    flags |= ((!gb.is_scheduled(EventId::VideoMode)) as u8) << 4;
    flags |= ((!gb.is_scheduled(EventId::VideoFrame)) as u8) << 5;
    s.put_u8(flags);
    s.put_u16(0); // reserved2
    s.put_u16(v.bcp_index as u16);
    s.put_u16(v.ocp_index as u16);
    for i in 0..64 {
        s.put_u16(v.palette[i]);
    }
}

fn memory_serialize(gb: &mut Gb, s: &mut Serializer) {
    let dma_until = gb.until(EventId::Dma) as u32;
    let hdma_until = gb.until(EventId::Hdma) as u32;
    let m = &gb.memory;
    s.put_u16(m.current_bank as u16);
    s.put_u8(m.wram_current_bank as u8);
    s.put_u8(m.sram_current_bank as u8);
    s.put_u32(dma_until);
    s.put_u16(m.dma_source);
    s.put_u16(m.dma_dest);
    s.put_u32(hdma_until);
    s.put_u16(m.hdma_source);
    s.put_u16(m.hdma_dest);
    s.put_u16(m.hdma_remaining as u16);
    s.put_u8(m.dma_remaining as u8);
    s.put_bytes(&m.rtc_regs);

    // MBC union (16 bytes)
    match m.mbc_type {
        MbcType::Mbc1 => {
            s.put_u8(m.mbc_state.mbc1.mode as u8);
            s.put_u8(m.mbc_state.mbc1.multicart_stride as u8);
            s.put_u8(m.mbc_state.mbc1.bank_lo);
            s.put_u8(m.mbc_state.mbc1.bank_hi);
            s.put_bytes(&[0; 12]);
        }
        MbcType::Mbc3Rtc => {
            s.put_u64(m.rtc_last_latch as u64);
            s.put_bytes(&[0; 8]);
        }
        MbcType::Mbc6 => {
            let mut f: u8 = 0;
            f |= m.mbc_state.mbc6.flash_bank0 as u8;
            f |= (m.mbc_state.mbc6.flash_bank1 as u8) << 1;
            s.put_u8(f);
            s.put_u8(m.current_bank1 as u8);
            s.put_u8(m.current_sram_bank1 as u8);
            s.put_bytes(&[0; 13]);
        }
        MbcType::Mbc7 => {
            let st = &m.mbc_state.mbc7;
            s.put_u8(st.state as u8);
            s.put_u8(st.eeprom);
            s.put_u8(st.address);
            s.put_u8(st.access);
            s.put_u8(st.latch);
            s.put_u8(st.sr_bits as u8);
            s.put_u16(st.sr);
            s.put_u32(st.writable as u32);
            s.put_bytes(&[0; 16 - 12]);
        }
        MbcType::Mmm01 => {
            s.put_u8(m.mbc_state.mmm01.locked as u8);
            s.put_u8(m.mbc_state.mmm01.current_bank0 as u8);
            s.put_bytes(&[0; 14]);
        }
        MbcType::PocketCam => {
            s.put_u8(m.mbc_state.pocket_cam.registers_active as u8);
            s.put_bytes(&[0; 15]);
        }
        MbcType::Tama5 => {
            s.put_u64(m.rtc_last_latch as u64);
            s.put_u8(m.mbc_state.tama5.reg);
            s.put_bytes(&[0; 7]);
        }
        MbcType::HuC3 => {
            s.put_u64(m.rtc_last_latch as u64);
            s.put_u8(m.mbc_state.huc3.index);
            s.put_u8(m.mbc_state.huc3.value);
            s.put_u8(m.mbc_state.huc3.mode);
            s.put_bytes(&[0; 5]);
        }
        MbcType::M161 => {
            s.put_u8(m.mbc_state.m161.bank);
            s.put_u8(m.mbc_state.m161.locked as u8);
            s.put_bytes(&[0; 14]);
        }
        MbcType::UnlNtOld1 | MbcType::UnlNtOld2 => {
            let mut f: u8 = 0;
            f |= m.mbc_state.nt_old.swapped as u8;
            f |= (m.mbc_state.nt_old.rumble as u8) << 1;
            s.put_u8(f);
            s.put_u8(m.mbc_state.nt_old.base_bank);
            s.put_u8(m.mbc_state.nt_old.bank_count);
            s.put_bytes(&[0; 13]);
        }
        MbcType::UnlNtNew => {
            s.put_u8(m.mbc_state.nt_new.split_mode as u8);
            s.put_u8(m.current_bank1 as u8);
            s.put_bytes(&[0; 14]);
        }
        MbcType::UnlBbd | MbcType::UnlHitek | MbcType::UnlGgb81 => {
            s.put_u8(m.mbc_state.bbd.data_swap_mode as u8);
            s.put_u8(m.mbc_state.bbd.bank_swap_mode as u8);
            s.put_bytes(&[0; 14]);
        }
        MbcType::UnlSachenMmc1 | MbcType::UnlSachenMmc2 => {
            let mut f: u8 = 0;
            f |= (m.mbc_state.sachen.transition as u8 & 0x3F) as u8;
            f |= ((m.mbc_state.sachen.locked as u8) & 3) << 6;
            s.put_u8(f);
            s.put_u8(m.mbc_state.sachen.mask);
            s.put_u8(m.mbc_state.sachen.unmasked_bank);
            s.put_u8(m.mbc_state.sachen.base_bank);
            s.put_bytes(&[0; 12]);
        }
        MbcType::UnlSintax => {
            s.put_u8(m.mbc_state.sintax.mode);
            s.put_bytes(&m.mbc_state.sintax.xor_values);
            s.put_u8(m.mbc_state.sintax.bank_no);
            s.put_u8(m.mbc_state.sintax.rom_bank_xor);
            s.put_bytes(&[0; 9]);
        }
        _ => {
            s.put_bytes(&[0; 16]);
        }
    }

    let mut flags: u16 = 0;
    flags |= m.sram_access as u16;
    flags |= (m.rtc_access as u16) << 1;
    flags |= (m.rtc_latched as u16) << 2;
    flags |= (m.ime as u16) << 3;
    flags |= (m.is_hdma as u16) << 4;
    flags |= ((m.active_rtc_reg as u16) & 7) << 5;
    s.put_u16(flags);
    s.put_u8(m.cart_bus);
    s.put_u8(0); // reserved
}

fn sgb_serialize(gb: &mut Gb, s: &mut Serializer) {
    if let Some(attrs) = &gb.video.sgb_attributes {
        s.put_bytes(&attrs[..90]);
    } else {
        s.put_bytes(&[0; 90]);
    }
    s.put_u8(gb.video.sgb_command_header);
    s.put_u8(gb.sgb_bit as u8);
    let mut flags: u32 = 0;
    flags |= (gb.current_sgb_bits & 3) as u32;
    flags |= ((gb.video.sgb_render_mode & 3) as u32) << 2;
    flags |= ((gb.video.sgb_buffer_index & 7) as u32) << 4;
    flags |= ((gb.sgb_current_controller & 3) as u32) << 7;
    flags |= ((gb.sgb_controllers & 3) as u32) << 9;
    flags |= (gb.sgb_increment as u32) << 11;
    s.put_u32(flags);
    s.put_bytes(&gb.sgb_packet);
    s.put_bytes(&gb.video.sgb_packet_buffer);

    if let Some(buf) = &gb.video.sgb_char_ram {
        s.put_bytes(buf.as_ref());
    } else {
        s.put_bytes(&[0; SGB_SIZE_CHAR_RAM]);
    }
    if let Some(buf) = &gb.video.sgb_map_ram {
        s.put_bytes(buf.as_ref());
    } else {
        s.put_bytes(&[0; SGB_SIZE_MAP_RAM]);
    }
    if let Some(buf) = &gb.video.sgb_pal_ram {
        s.put_bytes(buf.as_ref());
    } else {
        s.put_bytes(&[0; SGB_SIZE_PAL_RAM]);
    }
    if let Some(buf) = &gb.video.sgb_attribute_files {
        s.put_bytes(buf.as_ref());
    } else {
        s.put_bytes(&[0; SGB_SIZE_ATF_RAM]);
    }
}

// ---------------------------------------------------------------------------
// Deserialize
// ---------------------------------------------------------------------------

pub fn deserialize(gb: &mut Gb, state: &[u8]) -> Result<(), &'static str> {
    if state.len() < GB_SAVESTATE_SIZE {
        return Err("savestate too small");
    }
    let mut d = Deserializer::new(state);

    let mut error = false;
    let version = d.get_u32();
    let mut can_sgb = false;
    if version > GB_SAVESTATE_MAGIC + GB_SAVESTATE_VERSION {
        mlog!(Level::Warn, "GB State", "Invalid or too new savestate: expected {:08X}, got {:08X}", GB_SAVESTATE_MAGIC + GB_SAVESTATE_VERSION, version);
        error = true;
    } else if version < GB_SAVESTATE_MAGIC {
        mlog!(Level::Warn, "GB State", "Invalid savestate");
        error = true;
    } else if version < GB_SAVESTATE_MAGIC + GB_SAVESTATE_VERSION {
        mlog!(Level::Warn, "GB State", "Old savestate, continuing anyway");
    }
    can_sgb = version >= GB_SAVESTATE_MAGIC + 2;
    let _ = can_sgb;

    let rom_crc32 = d.get_u32();
    if rom_crc32 != gb.rom_crc32 {
        mlog!(Level::Warn, "GB State", "Savestate is for a different version of the game");
    }
    let model = model_from_u8(d.get_u8());
    d.skip(3);
    let master_cycles = d.get_u32();
    let mut title = [0u8; 16];
    d.get_bytes(&mut title);
    if !gb.memory.rom.is_empty() && gb.memory.rom.len() >= 0x144 {
        if title != gb.memory.rom[0x134..0x144] {
            if version > GB_SAVESTATE_MAGIC + 2 {
                mlog!(Level::Warn, "GB State", "Savestate is for a different game");
                error = true;
            } else if title != gb.memory.rom[0x134..0x144] {
                mlog!(Level::Warn, "GB State", "Savestate is for a different game (legacy path)");
                error = true;
            }
        }
    }

    // cpu
    let mut cpu_a = d.get_u8();
    let mut cpu_f = d.get_u8();
    let mut cpu_b = d.get_u8();
    let mut cpu_c = d.get_u8();
    let mut cpu_d = d.get_u8();
    let mut cpu_e = d.get_u8();
    let mut cpu_h = d.get_u8();
    let mut cpu_l = d.get_u8();
    let cpu_sp = d.get_u16();
    let cpu_pc = d.get_u16();
    let cpu_cycles = d.get_i32();
    let cpu_next_event = d.get_i32();
    let _reserved_instr = d.get_u16();
    let cpu_index = d.get_u16();
    let cpu_bus = d.get_u8();
    let cpu_execution_state = d.get_u8();
    let _ = d.get_u16();
    let cpu_ei_pending = d.get_u32();
    let _ = d.get_i32();
    let cpu_flags = d.get_u32();

    if cpu_cycles < 0 {
        mlog!(Level::Warn, "GB State", "Savestate is corrupted: CPU cycles are negative");
        error = true;
    }
    if cpu_execution_state as i32 != crate::cpu::SM83_CORE_FETCH {
        mlog!(Level::Warn, "GB State", "Savestate is corrupted: Execution state is not FETCH");
        error = true;
    }
    if cpu_cycles as u32 >= crate::gb::CGB_SM83_FREQUENCY {
        mlog!(Level::Warn, "GB State", "Savestate is corrupted: CPU cycles are too high");
        error = true;
    }

    // --- audio+
    let ch1_flags = d.get_u32();
    let _ch1_next_frame = d.get_i32();
    let _ = d.get_i32(); // reserved
    let ch1_sweep = d.get_u32();
    let ch1_last_update_delta = d.get_u32() as i32;
    let ch2_flags = d.get_u32();
    let _ = d.get_i32();
    let _ = d.get_i32();
    let ch2_last_update_delta = d.get_u32() as i32;
    let mut wavebanks = [0u32; 8];
    for w in wavebanks.iter_mut() {
        *w = d.get_u32();
    }
    let ch3_length = d.get_i16();
    let _ = d.get_i16();
    let ch3_next_update_delta = d.get_u32() as i32;
    let ch4_lfsr = d.get_u32();
    let ch4_flags = d.get_u32();
    let ch4_last_event = d.get_i32();
    let ch4_next_event = d.get_u32();
    let audio_flags = d.get_u32();
    let cap_left = d.get_i32();
    let cap_right = d.get_i32();
    let audio_next_sample = d.get_u32();

    // --- video
    let video_x = d.get_i16();
    let video_ly = d.get_i16();
    let video_next_frame = d.get_u32();
    let _ = d.get_u32();
    let video_next_mode = d.get_u32();
    let video_dot_clock = d.get_i32();
    let video_frame_counter = d.get_u32();
    let video_vram_bank = d.get_u8();
    let video_flags = d.get_u8();
    let _ = d.get_u16();
    let video_bcp_index = d.get_u16();
    let video_ocp_index = d.get_u16();
    let mut video_palette = [0u16; 64];
    for p in video_palette.iter_mut() {
        *p = d.get_u16();
    }

    if video_x < -7 || video_x as i32 > GB_VIDEO_HORIZONTAL_PIXELS {
        mlog!(Level::Warn, "GB State", "Savestate is corrupted: video x is out of range");
        error = true;
    }
    if video_ly < 0 || video_ly as i32 > GB_VIDEO_VERTICAL_TOTAL_PIXELS {
        error = true;
    }
    let mode = (video_flags >> 2) & 3;
    if video_ly as i32 >= GB_VIDEO_VERTICAL_PIXELS && mode != 1 {
        mlog!(Level::Warn, "GB State", "Savestate is corrupted: video y in vblank but mode != vblank");
        error = true;
    }

    // --- timer
    let timer_next_event = d.get_u32();
    let timer_next_irq = d.get_u32();
    let timer_next_div = d.get_u32();
    let timer_internal_div = d.get_u32();
    let timer_tima_period = d.get_u8();
    let timer_flags = d.get_u8();
    let _ = d.get_u16();

    // --- memory
    let mem_current_bank = d.get_u16();
    let mem_wram_current_bank = d.get_u8();
    let mem_sram_current_bank = d.get_u8();
    let mem_dma_next = d.get_u32();
    let mem_dma_source = d.get_u16();
    let mem_dma_dest = d.get_u16();
    let mem_hdma_next = d.get_u32();
    let mem_hdma_source = d.get_u16();
    let mem_hdma_dest = d.get_u16();
    let mem_hdma_remaining = d.get_u16();
    let mem_dma_remaining = d.get_u8();
    let mut mem_rtc_regs = [0u8; 5];
    d.get_bytes(&mut mem_rtc_regs);
    let mut mbc_blob = [0u8; 16];
    d.get_bytes(&mut mbc_blob);
    let mem_flags = d.get_u16();
    let mem_cart_bus = d.get_u8();
    let _ = d.get_u8();

    if mem_dma_dest as u32 + mem_dma_remaining as u32 > GB_SIZE_OAM as u32 {
        mlog!(Level::Warn, "GB State", "Savestate is corrupted: DMA destination is out of range");
        error = true;
    }
    if video_bcp_index >= 0x40 {
        mlog!(Level::Warn, "GB State", "Savestate is corrupted: BCPS out of range");
    }
    if video_ocp_index >= 0x40 {
        mlog!(Level::Warn, "GB State", "Savestate is corrupted: OCPS out of range");
    }

    let global_cycles = d.get_u64();
    let cart_bus_pc = d.get_u16();
    d.skip(54);

    // audio2
    let audio_last_sample = d.get_i32();
    let audio_sample_index = d.get_u8();
    d.skip(3);
    let mut current_samples = [crate::audio::StereoSample { left: 0, right: 0 }; 32];
    for smp in current_samples.iter_mut() {
        smp.left = d.get_i16();
        smp.right = d.get_i16();
    }

    let mut oam = [0u8; GB_SIZE_OAM];
    d.get_bytes(&mut oam);
    let mut io = [0u8; GB_SIZE_IO];
    d.get_bytes(&mut io);
    let mut hram = [0u8; GB_SIZE_HRAM];
    d.get_bytes(&mut hram);
    let ie = d.get_u8();
    let mut vram = vec![0u8; GB_SIZE_VRAM];
    d.get_bytes(&mut vram);
    let mut wram = vec![0u8; GB_SIZE_WORKING_RAM];
    d.get_bytes(&mut wram);

    d.skip(0x290);

    // mbc register unions
    let mut huc3_registers = [0u8; 0x80];
    let mut pocket_cam_registers = [0u8; 0x36];
    let mut tama5_registers = [0u8; 4];
    let mut tama5_pages = [[0u8; 8]; 4];
    match gb.memory.mbc_type {
        MbcType::HuC3 => {
            d.get_bytes(&mut huc3_registers);
        }
        MbcType::PocketCam => {
            d.get_bytes(&mut pocket_cam_registers);
            d.skip(0x80 - 0x36);
        }
        MbcType::Tama5 => {
            d.get_bytes(&mut tama5_registers);
            d.skip(4);
            for p in tama5_pages.iter_mut() {
                d.get_bytes(p);
            }
            d.skip(0x80 - 4 - 4 - 32);
        }
        _ => {
            d.skip(0x80);
        }
    }

    // sgb
    let mut sgb_attributes = [0u8; 90];
    d.get_bytes(&mut sgb_attributes);
    let sgb_command = d.get_u8();
    let sgb_bits = d.get_u8();
    let sgb_flags = d.get_u32();
    let mut sgb_in_progress = [0u8; 16];
    d.get_bytes(&mut sgb_in_progress);
    let mut sgb_packet = [0u8; 128];
    d.get_bytes(&mut sgb_packet);
    let mut sgb_char_ram = vec![0u8; SGB_SIZE_CHAR_RAM];
    d.get_bytes(&mut sgb_char_ram);
    let mut sgb_map_ram = vec![0u8; SGB_SIZE_MAP_RAM];
    d.get_bytes(&mut sgb_map_ram);
    let mut sgb_pal_ram = vec![0u8; SGB_SIZE_PAL_RAM];
    d.get_bytes(&mut sgb_pal_ram);
    let mut sgb_atf_ram = vec![0u8; SGB_SIZE_ATF_RAM];
    d.get_bytes(&mut sgb_atf_ram);

    if error {
        return Err("corrupt state");
    }

    // --- Commit (matching GBDeserialize's order) ---
    gb.timing.clear();
    gb.timing.master_cycles = master_cycles;
    gb.timing.global_cycles = global_cycles;

    gb.cpu.a = cpu_a;
    gb.cpu.f.packed = cpu_f;
    gb.cpu.b = cpu_b;
    gb.cpu.c = cpu_c;
    gb.cpu.d = cpu_d;
    gb.cpu.e = cpu_e;
    gb.cpu.h = cpu_h;
    gb.cpu.l = cpu_l;
    gb.cpu.sp = cpu_sp;
    gb.cpu.pc = cpu_pc;
    gb.cpu.index = cpu_index;
    gb.cpu.bus = cpu_bus;
    gb.cpu.execution_state = cpu_execution_state as i32;
    gb.cpu.condition = cpu_flags & 1 != 0;
    gb.cpu.irq_pending = cpu_flags & 2 != 0;
    gb.double_speed = cpu_flags & 4 != 0;
    gb.cpu.t_multiplier = 2 - gb.double_speed as i32;
    gb.cpu.halted = cpu_flags & 16 != 0;
    gb.cpu_blocked = cpu_flags & 32 != 0;
    gb.cpu.cycles = cpu_cycles;
    gb.cpu.next_event = cpu_next_event;

    if cpu_flags & 8 != 0 {
        gb.schedule(EventId::EiPending, cpu_ei_pending as i32);
    }

    gb.model = model;
    if !gb.model.is_cgb() {
        gb.audio.style = crate::audio::GbAudioStyle::Dmg;
    } else {
        gb.audio.style = crate::audio::GbAudioStyle::Cgb;
    }

    gb.unmap_bios();

    // GBMemoryDeserialize
    gb.memory.wram.copy_from_slice(&wram);
    gb.memory.hram[..GB_SIZE_HRAM].copy_from_slice(&hram);
    gb.memory.io = io;
    gb.memory.ie = ie;
    gb.memory.current_bank = mem_current_bank as i32;
    gb.memory.wram_current_bank = mem_wram_current_bank as i32;
    gb.memory.sram_current_bank = mem_sram_current_bank as i32;
    gb.memory_switch_wram_bank(gb.memory.wram_current_bank);
    if gb.memory.mbc_type != MbcType::Mbc6 && gb.memory.mbc_type != MbcType::UnlNtNew {
        gb.mbc_switch_bank(gb.memory.current_bank);
        gb.mbc_switch_sram_bank(gb.memory.sram_current_bank);
    }

    gb.memory.dma_source = mem_dma_source;
    gb.memory.dma_dest = mem_dma_dest;
    gb.memory.hdma_source = mem_hdma_source;
    gb.memory.hdma_dest = mem_hdma_dest;
    gb.memory.hdma_remaining = mem_hdma_remaining as i32;
    gb.memory.dma_remaining = mem_dma_remaining as i32;
    gb.memory.rtc_regs = mem_rtc_regs;

    if gb.memory.dma_remaining != 0 {
        gb.schedule(EventId::Dma, mem_dma_next as i32);
    }
    if gb.memory.hdma_remaining != 0 {
        gb.schedule(EventId::Hdma, mem_hdma_next as i32);
    }

    gb.memory.sram_access = mem_flags & 1 != 0;
    gb.memory.rtc_access = mem_flags & 2 != 0;
    gb.memory.rtc_latched = mem_flags & 4 != 0;
    gb.memory.ime = mem_flags & 8 != 0;
    gb.memory.is_hdma = mem_flags & 16 != 0;
    gb.memory.active_rtc_reg = ((mem_flags >> 5) & 7) as i32;
    gb.memory.cart_bus = mem_cart_bus;
    gb.memory.cart_bus_pc = cart_bus_pc;

    {
        let mbc_type = gb.memory.mbc_type;
        match mbc_type {
            MbcType::Mbc1 => {
                let st = &mut gb.memory.mbc_state.mbc1;
                st.mode = mbc_blob[0] as i32;
                st.multicart_stride = mbc_blob[1] as i32;
                st.bank_lo = mbc_blob[2];
                st.bank_hi = mbc_blob[3];
                if st.bank_lo == 0 && st.bank_hi == 0 {
                    st.bank_lo = (gb.memory.current_bank & ((1 << st.multicart_stride) - 1)) as u8;
                    st.bank_hi = (gb.memory.current_bank >> st.multicart_stride) as u8;
                }
                if st.mode != 0 {
                    let b0 = st.bank_hi as i32 * (1 << st.multicart_stride);
                    let _ = st;
                    gb.mbc_switch_bank0(b0);
                }
            }
            MbcType::Mbc3Rtc => {
                let mut b8 = [0u8; 8];
                b8.copy_from_slice(&mbc_blob[..8]);
                gb.memory.rtc_last_latch = u64::from_le_bytes(b8) as i64;
            }
            MbcType::Mbc6 => {
                gb.memory.mbc_state.mbc6.flash_bank0 = mbc_blob[0] & 1 != 0;
                gb.memory.mbc_state.mbc6.flash_bank1 = mbc_blob[0] & 2 != 0;
                gb.memory.current_bank1 = mbc_blob[1] as i8 as i32;
                gb.memory.current_sram_bank1 = mbc_blob[2] as i8 as i32;
                gb.mbc_switch_half_bank(0, gb.memory.current_bank);
                gb.mbc_switch_half_bank(1, gb.memory.current_bank1);
                gb.mbc_switch_sram_half_bank(0, gb.memory.sram_current_bank);
                gb.mbc_switch_sram_half_bank(1, gb.memory.current_sram_bank1);
            }
            MbcType::Mbc7 => {
                let st = &mut gb.memory.mbc_state.mbc7;
                st.state = mbc_blob[0] as i32;
                st.eeprom = mbc_blob[1];
                st.address = mbc_blob[2] & 0x7F;
                st.access = mbc_blob[3];
                st.latch = mbc_blob[4];
                st.sr_bits = mbc_blob[5] as i32;
                st.sr = mbc_blob[6] as u16 | (mbc_blob[7] as u16) << 8;
                st.writable = (mbc_blob[8] as u32
                    | (mbc_blob[9] as u32) << 8
                    | (mbc_blob[10] as u32) << 16
                    | (mbc_blob[11] as u32) << 24)
                    != 0;
            }
            MbcType::Mmm01 => {
                gb.memory.mbc_state.mmm01.locked = mbc_blob[0] != 0;
                gb.memory.mbc_state.mmm01.current_bank0 = mbc_blob[1] as i8 as i32;
                if gb.memory.mbc_state.mmm01.locked {
                    gb.mbc_switch_bank0(gb.memory.mbc_state.mmm01.current_bank0);
                } else {
                    let banks = gb.memory.rom_size / GB_SIZE_CART_BANK0;
                    gb.mbc_switch_bank0(banks.wrapping_sub(2) as i32);
                }
            }
            MbcType::PocketCam => {
                gb.memory.mbc_state.pocket_cam.registers_active = mbc_blob[0] != 0;
                gb.memory.mbc_state.pocket_cam.registers.copy_from_slice(&pocket_cam_registers);
            }
            MbcType::Tama5 => {
                let mut b8 = [0u8; 8];
                b8.copy_from_slice(&mbc_blob[..8]);
                gb.memory.rtc_last_latch = u64::from_le_bytes(b8) as i64;
                gb.memory.mbc_state.tama5.reg = mbc_blob[8];
                for i in 0..4 {
                    gb.memory.mbc_state.tama5.registers[i * 2] = tama5_registers[i] & 0xF;
                    gb.memory.mbc_state.tama5.registers[i * 2 + 1] = tama5_registers[i] >> 4;
                }
                for p in 0..4 {
                    for i in 0..8 {
                        let lo = tama5_pages[p][i] & 0xF;
                        let hi = tama5_pages[p][i] >> 4;
                        let st = &mut gb.memory.mbc_state.tama5;
                        let page: &mut [u8; 16] = match p {
                            0 => &mut st.rtc_timer_page,
                            1 => &mut st.rtc_alarm_page,
                            2 => &mut st.rtc_free_page0,
                            _ => &mut st.rtc_free_page1,
                        };
                        page[i * 2] = lo;
                        page[i * 2 + 1] = hi;
                    }
                }
            }
            MbcType::HuC3 => {
                let mut b8 = [0u8; 8];
                b8.copy_from_slice(&mbc_blob[..8]);
                gb.memory.rtc_last_latch = u64::from_le_bytes(b8) as i64;
                gb.memory.mbc_state.huc3.index = mbc_blob[8];
                gb.memory.mbc_state.huc3.value = mbc_blob[9];
                gb.memory.mbc_state.huc3.mode = mbc_blob[10];
                for i in 0..0x80 {
                    gb.memory.mbc_state.huc3.registers[i * 2] = huc3_registers[i] & 0xF;
                    gb.memory.mbc_state.huc3.registers[i * 2 + 1] = huc3_registers[i] >> 4;
                }
            }
            MbcType::M161 => {
                gb.memory.mbc_state.m161.bank = mbc_blob[0] & 7;
                gb.memory.mbc_state.m161.locked = mbc_blob[1] != 0;
                gb.mbc_switch_bank0(gb.memory.mbc_state.m161.bank as i32 * 2);
                gb.mbc_switch_bank(gb.memory.mbc_state.m161.bank as i32 * 2 + 1);
            }
            MbcType::UnlNtOld1 | MbcType::UnlNtOld2 => {
                gb.memory.mbc_state.nt_old.swapped = mbc_blob[0] & 1 != 0;
                gb.memory.mbc_state.nt_old.rumble = mbc_blob[0] & 2 != 0;
                gb.memory.mbc_state.nt_old.base_bank = mbc_blob[1];
                gb.memory.mbc_state.nt_old.bank_count = mbc_blob[2];
                gb.mbc_switch_bank0(gb.memory.mbc_state.nt_old.base_bank as i32);
            }
            MbcType::UnlNtNew => {
                gb.memory.mbc_state.nt_new.split_mode = mbc_blob[0] != 0;
                gb.memory.current_bank1 = mbc_blob[1] as i8 as i32;
                if gb.memory.mbc_state.nt_new.split_mode {
                    gb.mbc_switch_half_bank(0, gb.memory.current_bank);
                    gb.mbc_switch_half_bank(1, gb.memory.current_bank1);
                } else {
                    gb.mbc_switch_bank(gb.memory.current_bank);
                }
            }
            MbcType::UnlBbd | MbcType::UnlHitek | MbcType::UnlGgb81 => {
                gb.memory.mbc_state.bbd.data_swap_mode = (mbc_blob[0] & 7) as i32;
                gb.memory.mbc_state.bbd.bank_swap_mode = (mbc_blob[1] & 7) as i32;
            }
            MbcType::UnlSachenMmc1 | MbcType::UnlSachenMmc2 => {
                gb.memory.mbc_state.sachen.transition = (mbc_blob[0] & 0x3F) as i32;
                gb.memory.mbc_state.sachen.locked = ((mbc_blob[0] >> 6) & 3) as i32;
                gb.memory.mbc_state.sachen.mask = mbc_blob[1];
                gb.memory.mbc_state.sachen.unmasked_bank = mbc_blob[2];
                gb.memory.mbc_state.sachen.base_bank = mbc_blob[3];
                gb.mbc_switch_bank0(
                    (gb.memory.mbc_state.sachen.base_bank & gb.memory.mbc_state.sachen.mask) as i32,
                );
            }
            MbcType::UnlSintax => {
                gb.memory.mbc_state.sintax.mode = mbc_blob[0];
                gb.memory.mbc_state.sintax.xor_values.copy_from_slice(&mbc_blob[1..5]);
                gb.memory.mbc_state.sintax.bank_no = mbc_blob[5];
                gb.memory.mbc_state.sintax.rom_bank_xor = mbc_blob[6];
            }
            _ => {}
        }
    }

    // GBVideoDeserialize
    gb.video.x = video_x as i32;
    gb.video.ly = video_ly as i32;
    gb.video.frame_counter = video_frame_counter;
    gb.video.dot_clock = video_dot_clock;
    gb.video.vram_current_bank = video_vram_bank as i32;
    gb.video.bcp_increment = video_flags & 1 != 0;
    gb.video.ocp_increment = video_flags & 2 != 0;
    gb.video.mode = mode as i32;
    gb.video.bcp_index = (video_bcp_index & 0x3F) as i32;
    gb.video.ocp_index = (video_ocp_index & 0x3F) as i32;
    gb.video.mode_event = match mode {
        0 => ModeEventKind::EndMode0,
        1 => ModeEventKind::EndMode1,
        2 => ModeEventKind::EndMode2,
        _ => ModeEventKind::EndMode3,
    };
    if video_flags & 0x10 == 0 {
        gb.schedule(EventId::VideoMode, video_next_mode as i32);
    }
    if video_flags & 0x20 == 0 {
        gb.schedule(EventId::VideoFrame, video_next_frame as i32);
    }
    gb.video.renderer_init(gb.model, gb.video.sgb_borders);
    // GBVideoProxyRendererInit parity (renderer->init through the shim).
    if let Some(vl) = gb.video_logger.as_mut() {
        vl.logger.renderer_init();
    }
    for i in 0..64 {
        gb.video.palette[i] = video_palette[i];
        let v = gb.video.palette[i];
        gb.renderer_write_palette(i as i32, v);
    }
    gb.video.vram.copy_from_slice(&vram);
    gb.video.oam = oam;
    let ly = gb.video.ly;
    gb.clean_oam(ly);
    gb.video_switch_bank(video_vram_bank);

    // GBIODeserialize
    gb.io_deserialize();

    // GBTimerDeserialize
    gb.timer.next_div = timer_next_div as i32;
    gb.timer.internal_div = timer_internal_div;
    gb.timer.tima_period = timer_tima_period as u32;
    gb.schedule(EventId::Timer, timer_next_event as i32);
    if timer_flags & 1 != 0 {
        gb.schedule(EventId::TimerIrq, timer_next_irq as i32);
    }

    // GBAudioDeserialize
    let nr52 = gb_memory_nr52(gb);
    let now = gb.current_time();
    {
        let a = &mut gb.audio;
        a.playing_ch1 = nr52 & 0x1 != 0;
        a.playing_ch2 = nr52 & 0x2 != 0;
        a.playing_ch3 = nr52 & 0x4 != 0;
        a.playing_ch4 = nr52 & 0x8 != 0;
        a.enable = nr52 & 0x80 != 0;

        a.frame = ((audio_flags >> 22) & 7) as i32;
        a.frame_skip = ((audio_flags >> 28) & 1) as i32;

        a.ch1.envelope.current_volume = (audio_flags & 0xF) as i32;
        a.ch1.envelope.dead = ((audio_flags >> 4) & 3) as i32;
        a.ch1.sweep.enable = audio_flags & (1 << 25) != 0;
        a.ch1.sweep.occurred = audio_flags & (1 << 26) != 0;
        a.ch1.sweep.time = (ch1_sweep & 7) as i32;
        if a.ch1.sweep.time == 0 {
            a.ch1.sweep.time = 8;
        }
        a.ch1.control.length = (ch1_flags & 0x7F) as i32;
        a.ch1.envelope.next_step = ((ch1_flags >> 7) & 7) as i32;
        a.ch1.sweep.real_frequency = ((ch1_flags >> 10) & 0x7FF) as i32;
        a.ch1.index = ((ch1_flags >> 21) & 7) as u8;
        a.ch1.last_update = ch1_last_update_delta.wrapping_add(now);

        a.ch2.envelope.current_volume = ((audio_flags >> 8) & 0xF) as i32;
        a.ch2.envelope.dead = ((audio_flags >> 12) & 3) as i32;
        a.ch2.control.length = (ch2_flags & 0x7F) as i32;
        a.ch2.envelope.next_step = ((ch2_flags >> 7) & 7) as i32;
        a.ch2.index = ((ch2_flags >> 21) & 7) as u8;
        a.ch2.last_update = ch2_last_update_delta.wrapping_add(now);

        a.ch3.readable = audio_flags & (1 << 27) != 0;
        for i in 0..4 {
            a.ch3.wavedata8[i * 4..i * 4 + 4].copy_from_slice(&wavebanks[i].to_le_bytes());
        }
        a.ch3.length = ch3_length as u32;
        a.ch3.next_update = ch3_next_update_delta.wrapping_add(now);

        a.ch4.envelope.current_volume = ((audio_flags >> 16) & 0xF) as i32;
        a.ch4.envelope.dead = ((audio_flags >> 20) & 3) as i32;
        a.ch4.length = (ch4_flags & 0x7F) as i32;
        a.ch4.envelope.next_step = ((ch4_flags >> 7) & 7) as i32;
        a.ch4.lfsr = ch4_lfsr;
        a.ch4.last_event = ch4_last_event as u32;
        let when = ch4_next_event;
        if a.ch4.envelope.dead < 2 && a.playing_ch4 {
            if a.ch4.last_event == 0 {
                let current_time = now as u32;
                let mut cycles = if a.ch4.ratio != 0 { 2 * a.ch4.ratio } else { 1 };
                cycles <<= a.ch4.frequency;
                cycles *= 8 * a.timing_factor;
                a.ch4.last_event = current_time
                    .wrapping_add(when & (cycles as u32 - 1))
                    .wrapping_sub(cycles as u32);
            }
        }
        a.ch4.n_samples = 0;
        a.ch4.samples = 0;
        a.cap_left = cap_left;
        a.cap_right = cap_right;
        for i in 0..32 {
            a.current_samples[i] = current_samples[i];
        }
        a.last_sample = audio_last_sample;
        a.sample_index = audio_sample_index as usize;
    }
    gb.schedule(EventId::AudioSample, audio_next_sample as i32);

    if gb.memory.io[GB_REG_BANK as usize] == 0xFF {
        if gb.memory.bios.is_some() {
            let bios = gb.memory.bios.clone().unwrap();
            gb.map_bios(&bios);
        }
    }

    // GBSGBDeserialize
    if gb.model == GbModel::Sgb {
        gb.video.sgb_command_header = sgb_command;
        gb.sgb_bit = sgb_bits as i32;
        gb.current_sgb_bits = (sgb_flags & 3) as u8;
        gb.video.sgb_render_mode = ((sgb_flags >> 2) & 3) as i32;
        gb.video.sgb_buffer_index = ((sgb_flags >> 4) & 7) as i32;
        gb.sgb_controllers = ((sgb_flags >> 9) & 3) as u8;
        gb.sgb_current_controller = ((sgb_flags >> 7) & 3) as u8;
        gb.sgb_increment = sgb_flags & (1 << 11) != 0;
        // Old versions of mGBA stored the increment bits here
        if gb.sgb_bit > 129 && gb.sgb_bit & 2 != 0 {
            gb.sgb_increment = true;
        }
        gb.video.sgb_packet_buffer = sgb_packet;
        gb.sgb_packet = sgb_in_progress;
        if gb.video.sgb_char_ram.is_none() {
            gb.video.sgb_char_ram = Some(Box::new([0; SGB_SIZE_CHAR_RAM]));
        }
        if gb.video.sgb_map_ram.is_none() {
            gb.video.sgb_map_ram = Some(Box::new([0; SGB_SIZE_MAP_RAM]));
        }
        if gb.video.sgb_pal_ram.is_none() {
            gb.video.sgb_pal_ram = Some(Box::new([0; SGB_SIZE_PAL_RAM]));
        }
        if gb.video.sgb_attribute_files.is_none() {
            gb.video.sgb_attribute_files = Some(Box::new([0; SGB_SIZE_ATF_RAM]));
        }
        if gb.video.sgb_attributes.is_none() {
            gb.video.sgb_attributes = Some(vec![0; 90 * 45]);
        }
        gb.video
            .sgb_char_ram
            .as_mut()
            .unwrap()
            .copy_from_slice(&sgb_char_ram);
        gb.video
            .sgb_map_ram
            .as_mut()
            .unwrap()
            .copy_from_slice(&sgb_map_ram);
        gb.video
            .sgb_pal_ram
            .as_mut()
            .unwrap()
            .copy_from_slice(&sgb_pal_ram);
        gb.video
            .sgb_attribute_files
            .as_mut()
            .unwrap()
            .copy_from_slice(&sgb_atf_ram);
        gb.video.sgb_attributes.as_mut().unwrap()[..90].copy_from_slice(&sgb_attributes);

        gb.video_write_sgb_packet(&[((12u8 << 3) | 1), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let _ = sgb_in_progress;
    }

    gb.set_active_region(gb.cpu.pc);
    gb.timing.interrupt();

    Ok(())
}

fn gb_memory_nr52(gb: &Gb) -> u8 {
    gb.memory.io[crate::io::GB_REG_NR52 as usize]
}
