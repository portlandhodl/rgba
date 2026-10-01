// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/include/mgba/internal/gba/io.h (register indices) and
// constants. The GBAIORead/GBAIOWrite/GBAIOWrite8/GBAIOWrite32 dispatch lives
// here too (mgba/src/gba/io.c).

pub use rgba_core::{mlog, Level};
use crate::memory::GBA_SIZE_IO;

pub mod regs {
    // enum GBAIORegisters (halved indices live in memory.io[reg >> 1])
    pub const GBA_REG_DISPCNT: u32 = 0x000;
    pub const GBA_REG_STEREOCNT: u32 = 0x002;
    pub const GBA_REG_DISPSTAT: u32 = 0x004;
    pub const GBA_REG_VCOUNT: u32 = 0x006;
    pub const GBA_REG_BG0CNT: u32 = 0x008;
    pub const GBA_REG_BG1CNT: u32 = 0x00A;
    pub const GBA_REG_BG2CNT: u32 = 0x00C;
    pub const GBA_REG_BG3CNT: u32 = 0x00E;
    pub const GBA_REG_BG0HOFS: u32 = 0x010;
    pub const GBA_REG_BG0VOFS: u32 = 0x012;
    pub const GBA_REG_BG1HOFS: u32 = 0x014;
    pub const GBA_REG_BG1VOFS: u32 = 0x016;
    pub const GBA_REG_BG2HOFS: u32 = 0x018;
    pub const GBA_REG_BG2VOFS: u32 = 0x01A;
    pub const GBA_REG_BG3HOFS: u32 = 0x01C;
    pub const GBA_REG_BG3VOFS: u32 = 0x01E;
    pub const GBA_REG_BG2PA: u32 = 0x020;
    pub const GBA_REG_BG2PB: u32 = 0x022;
    pub const GBA_REG_BG2PC: u32 = 0x024;
    pub const GBA_REG_BG2PD: u32 = 0x026;
    pub const GBA_REG_BG2X_LO: u32 = 0x028;
    pub const GBA_REG_BG2X_HI: u32 = 0x02A;
    pub const GBA_REG_BG2Y_LO: u32 = 0x02C;
    pub const GBA_REG_BG2Y_HI: u32 = 0x02E;
    pub const GBA_REG_BG3PA: u32 = 0x030;
    pub const GBA_REG_BG3PB: u32 = 0x032;
    pub const GBA_REG_BG3PC: u32 = 0x034;
    pub const GBA_REG_BG3PD: u32 = 0x036;
    pub const GBA_REG_BG3X_LO: u32 = 0x038;
    pub const GBA_REG_BG3X_HI: u32 = 0x03A;
    pub const GBA_REG_BG3Y_LO: u32 = 0x03C;
    pub const GBA_REG_BG3Y_HI: u32 = 0x03E;
    pub const GBA_REG_WIN0H: u32 = 0x040;
    pub const GBA_REG_WIN1H: u32 = 0x042;
    pub const GBA_REG_WIN0V: u32 = 0x044;
    pub const GBA_REG_WIN1V: u32 = 0x046;
    pub const GBA_REG_WININ: u32 = 0x048;
    pub const GBA_REG_WINOUT: u32 = 0x04A;
    pub const GBA_REG_MOSAIC: u32 = 0x04C;
    pub const GBA_REG_BLDCNT: u32 = 0x050;
    pub const GBA_REG_BLDALPHA: u32 = 0x052;
    pub const GBA_REG_BLDY: u32 = 0x054;

    pub const GBA_REG_SOUND1CNT_LO: u32 = 0x060;
    pub const GBA_REG_SOUND1CNT_HI: u32 = 0x062;
    pub const GBA_REG_SOUND1CNT_X: u32 = 0x064;
    pub const GBA_REG_SOUND2CNT_LO: u32 = 0x068;
    pub const GBA_REG_SOUND2CNT_HI: u32 = 0x06C;
    pub const GBA_REG_SOUND3CNT_LO: u32 = 0x070;
    pub const GBA_REG_SOUND3CNT_HI: u32 = 0x072;
    pub const GBA_REG_SOUND3CNT_X: u32 = 0x074;
    pub const GBA_REG_SOUND4CNT_LO: u32 = 0x078;
    pub const GBA_REG_SOUND4CNT_HI: u32 = 0x07C;
    pub const GBA_REG_SOUNDCNT_LO: u32 = 0x080;
    pub const GBA_REG_SOUNDCNT_HI: u32 = 0x082;
    pub const GBA_REG_SOUNDCNT_X: u32 = 0x084;
    pub const GBA_REG_SOUNDBIAS: u32 = 0x088;
    pub const GBA_REG_WAVE_RAM0_LO: u32 = 0x090;
    pub const GBA_REG_WAVE_RAM0_HI: u32 = 0x092;
    pub const GBA_REG_WAVE_RAM1_LO: u32 = 0x094;
    pub const GBA_REG_WAVE_RAM1_HI: u32 = 0x096;
    pub const GBA_REG_WAVE_RAM2_LO: u32 = 0x098;
    pub const GBA_REG_WAVE_RAM2_HI: u32 = 0x09A;
    pub const GBA_REG_WAVE_RAM3_LO: u32 = 0x09C;
    pub const GBA_REG_WAVE_RAM3_HI: u32 = 0x09E;
    pub const GBA_REG_FIFO_A_LO: u32 = 0x0A0;
    pub const GBA_REG_FIFO_A_HI: u32 = 0x0A2;
    pub const GBA_REG_FIFO_B_LO: u32 = 0x0A4;
    pub const GBA_REG_FIFO_B_HI: u32 = 0x0A6;

    pub const GBA_REG_DMA0SAD_LO: u32 = 0x0B0;
    pub const GBA_REG_DMA0SAD_HI: u32 = 0x0B2;
    pub const GBA_REG_DMA0DAD_LO: u32 = 0x0B4;
    pub const GBA_REG_DMA0DAD_HI: u32 = 0x0B6;
    pub const GBA_REG_DMA0CNT_LO: u32 = 0x0B8;
    pub const GBA_REG_DMA0CNT_HI: u32 = 0x0BA;
    pub const GBA_REG_DMA1SAD_LO: u32 = 0x0BC;
    pub const GBA_REG_DMA1SAD_HI: u32 = 0x0BE;
    pub const GBA_REG_DMA1DAD_LO: u32 = 0x0C0;
    pub const GBA_REG_DMA1DAD_HI: u32 = 0x0C2;
    pub const GBA_REG_DMA1CNT_LO: u32 = 0x0C4;
    pub const GBA_REG_DMA1CNT_HI: u32 = 0x0C6;
    pub const GBA_REG_DMA2SAD_LO: u32 = 0x0C8;
    pub const GBA_REG_DMA2SAD_HI: u32 = 0x0CA;
    pub const GBA_REG_DMA2DAD_LO: u32 = 0x0CC;
    pub const GBA_REG_DMA2DAD_HI: u32 = 0x0CE;
    pub const GBA_REG_DMA2CNT_LO: u32 = 0x0D0;
    pub const GBA_REG_DMA2CNT_HI: u32 = 0x0D2;
    pub const GBA_REG_DMA3SAD_LO: u32 = 0x0D4;
    pub const GBA_REG_DMA3SAD_HI: u32 = 0x0D6;
    pub const GBA_REG_DMA3DAD_LO: u32 = 0x0D8;
    pub const GBA_REG_DMA3DAD_HI: u32 = 0x0DA;
    pub const GBA_REG_DMA3CNT_LO: u32 = 0x0DC;
    pub const GBA_REG_DMA3CNT_HI: u32 = 0x0DE;

    pub const GBA_REG_TM0CNT_LO: u32 = 0x100;
    pub const GBA_REG_TM0CNT_HI: u32 = 0x102;
    pub const GBA_REG_TM1CNT_LO: u32 = 0x104;
    pub const GBA_REG_TM1CNT_HI: u32 = 0x106;
    pub const GBA_REG_TM2CNT_LO: u32 = 0x108;
    pub const GBA_REG_TM2CNT_HI: u32 = 0x10A;
    pub const GBA_REG_TM3CNT_LO: u32 = 0x10C;
    pub const GBA_REG_TM3CNT_HI: u32 = 0x10E;

    pub const GBA_REG_SIODATA32_LO: u32 = 0x120;
    pub const GBA_REG_SIODATA32_HI: u32 = 0x122;
    pub const GBA_REG_SIOMULTI0: u32 = 0x120;
    pub const GBA_REG_SIOMULTI1: u32 = 0x122;
    pub const GBA_REG_SIOMULTI2: u32 = 0x124;
    pub const GBA_REG_SIOMULTI3: u32 = 0x126;
    pub const GBA_REG_SIOCNT: u32 = 0x128;
    pub const GBA_REG_SIOMLT_SEND: u32 = 0x12A;
    pub const GBA_REG_KEYINPUT: u32 = 0x130;
    pub const GBA_REG_KEYCNT: u32 = 0x132;
    pub const GBA_REG_RCNT: u32 = 0x134;
    pub const GBA_REG_JOYCNT: u32 = 0x140;
    pub const GBA_REG_JOY_RECV_LO: u32 = 0x150;
    pub const GBA_REG_JOY_RECV_HI: u32 = 0x152;
    pub const GBA_REG_JOY_TRANS_LO: u32 = 0x154;
    pub const GBA_REG_JOY_TRANS_HI: u32 = 0x156;
    pub const GBA_REG_JOYSTAT: u32 = 0x158;

    pub const GBA_REG_IE: u32 = 0x200;
    pub const GBA_REG_IF: u32 = 0x202;
    pub const GBA_REG_WAITCNT: u32 = 0x204;
    pub const GBA_REG_IME: u32 = 0x208;
    pub const GBA_REG_POSTFLG: u32 = 0x300;
    pub const GBA_REG_HALTCNT: u32 = 0x301;

    pub const GBA_REG_EXWAITCNT_LO: u32 = 0x4000;
    pub const GBA_REG_EXWAITCNT_HI: u32 = 0x4002;

    pub const GBA_REG_DEBUG_ENABLE: u32 = 0x41F8;
    pub const GBA_REG_DEBUG_FLAGS: u32 = 0x41FA;
    pub const GBA_REG_DEBUG_STRING: u32 = 0x4A00;

    pub const GBA_REG_MAX: u32 = 0x400;
    pub const GBA_REG_INTERNAL_EXWAITCNT_LO: u32 = GBA_REG_MAX;
    pub const GBA_REG_INTERNAL_EXWAITCNT_HI: u32 = GBA_REG_MAX + 2;
    pub const GBA_REG_INTERNAL_MAX: u32 = GBA_REG_MAX + 4;

    pub const RCNT_INITIAL: u16 = 0x8001;
    pub const JOYSTAT_RECV: u16 = 0x02;
    pub const JOYSTAT_TRANS: u16 = 0x08;

    pub const SIOCNT_DREQ: u16 = 0x0800;

    // KEYINPUT bits
    pub const GBA_KEY_A: u16 = 0;
    pub const GBA_KEY_B: u16 = 1;
    pub const GBA_KEY_SELECT: u16 = 2;
    pub const GBA_KEY_START: u16 = 3;
    pub const GBA_KEY_RIGHT: u16 = 4;
    pub const GBA_KEY_LEFT: u16 = 5;
    pub const GBA_KEY_UP: u16 = 6;
    pub const GBA_KEY_DOWN: u16 = 7;
    pub const GBA_KEY_R: u16 = 8;
    pub const GBA_KEY_L: u16 = 9;
}

pub use regs::*;
// (GBA_IO_REGISTER_NAMES lives at io scope, outside the `regs` block constants)

use crate::gba::Gba;

impl Gba {
    /// GBAIOInit
    pub fn io_init(&mut self) {
        self.memory.io[(GBA_REG_DISPCNT >> 1) as usize] = 0x0080;
        self.memory.io[(GBA_REG_RCNT >> 1) as usize] = RCNT_INITIAL;
        self.memory.io[(GBA_REG_KEYINPUT >> 1) as usize] = 0x3FF;
        self.memory.io[(GBA_REG_SOUNDBIAS >> 1) as usize] = 0x200;
        self.memory.io[(GBA_REG_BG2PA >> 1) as usize] = 0x100;
        self.memory.io[(GBA_REG_BG2PD >> 1) as usize] = 0x100;
        self.memory.io[(GBA_REG_BG3PA >> 1) as usize] = 0x100;
        self.memory.io[(GBA_REG_BG3PD >> 1) as usize] = 0x100;
        self.memory.io[(GBA_REG_INTERNAL_EXWAITCNT_LO >> 1) as usize] = 0x20;
        self.memory.io[(GBA_REG_INTERNAL_EXWAITCNT_HI >> 1) as usize] = 0xD00;

        if self.bios_none() {
            self.memory.io[(GBA_REG_VCOUNT >> 1) as usize] = 0x7E;
            self.memory.io[(GBA_REG_POSTFLG >> 1) as usize] = 1;
        }
    }
}

impl Gba {
    /// GBAIOWrite (16-bit IO write dispatch)
    pub fn io_write_real(&mut self, address: u32, value: u16) {
        let mut value = value;
        if address < GBA_REG_SOUND1CNT_LO && (address > GBA_REG_VCOUNT || address < GBA_REG_DISPSTAT) {
            let v = self.renderer_write_video_register(address, value);
            self.memory.io[(address >> 1) as usize] = v;
            return;
        }

        if (GBA_REG_SOUND1CNT_LO..=GBA_REG_SOUNDCNT_LO).contains(&address) && !self.audio.enable {
            // Ignore writes to most audio registers if the hardware is off.
            return;
        }

        match address {
            GBA_REG_DISPSTAT => {
                value = self.video_write_dispstat(value);
            }
            GBA_REG_VCOUNT => {
                mlog!(Level::GameError, rgba_core::log::GBA, "Write to read-only I/O register: {:03X}", address);
                return;
            }
            // Audio
            GBA_REG_SOUND1CNT_LO => {
                self.audio_write_sound1cnt_lo(value);
                value &= 0x007F;
            }
            GBA_REG_SOUND1CNT_HI => {
                self.audio_write_sound1cnt_hi(value);
                value &= 0xFFC0;
            }
            GBA_REG_SOUND1CNT_X => {
                self.audio_write_sound1cnt_x(value);
                value &= 0x4000;
            }
            GBA_REG_SOUND2CNT_LO => {
                self.audio_write_sound2cnt_lo(value);
                value &= 0xFFC0;
            }
            GBA_REG_SOUND2CNT_HI => {
                self.audio_write_sound2cnt_hi(value);
                value &= 0x4000;
            }
            GBA_REG_SOUND3CNT_LO => {
                self.audio_write_sound3cnt_lo(value);
                value &= 0x00E0;
            }
            GBA_REG_SOUND3CNT_HI => {
                self.audio_write_sound3cnt_hi(value);
                value &= 0xE000;
            }
            GBA_REG_SOUND3CNT_X => {
                self.audio_write_sound3cnt_x(value);
                value &= 0x4000;
            }
            GBA_REG_SOUND4CNT_LO => {
                self.audio_write_sound4cnt_lo(value);
                value &= 0xFF00;
            }
            GBA_REG_SOUND4CNT_HI => {
                self.audio_write_sound4cnt_hi(value);
                value &= 0x40FF;
            }
            GBA_REG_SOUNDCNT_LO => {
                self.audio_write_soundcnt_lo(value);
                value &= 0xFF77;
            }
            GBA_REG_SOUNDCNT_HI => {
                self.audio_write_soundcnt_hi(value);
                value &= 0x770F;
            }
            GBA_REG_SOUNDCNT_X => {
                self.audio_write_soundcnt_x(value);
                value &= 0x0080;
                value |= self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] & 0xF;
            }
            GBA_REG_SOUNDBIAS => {
                value &= 0xC3FE;
                self.audio_write_soundbias(value);
            }
            GBA_REG_WAVE_RAM0_LO | GBA_REG_WAVE_RAM1_LO | GBA_REG_WAVE_RAM2_LO | GBA_REG_WAVE_RAM3_LO => {
                let hi = self.memory.io[(address >> 1) as usize + 1] as u32;
                self.io_write32(address, (hi << 16) | value as u32);
                return;
            }
            GBA_REG_WAVE_RAM0_HI | GBA_REG_WAVE_RAM1_HI | GBA_REG_WAVE_RAM2_HI | GBA_REG_WAVE_RAM3_HI => {
                let lo = self.memory.io[((address >> 1) - 1) as usize] as u32;
                self.io_write32(address - 2, lo | ((value as u32) << 16));
                return;
            }
            GBA_REG_FIFO_A_LO | GBA_REG_FIFO_B_LO => {
                let hi = self.memory.io[(address >> 1) as usize + 1] as u32;
                self.io_write32(address, (hi << 16) | value as u32);
                return;
            }
            GBA_REG_FIFO_A_HI | GBA_REG_FIFO_B_HI => {
                let lo = self.memory.io[((address >> 1) - 1) as usize] as u32;
                self.io_write32(address - 2, lo | ((value as u32) << 16));
                return;
            }
            // DMA
            GBA_REG_DMA0SAD_LO | GBA_REG_DMA0DAD_LO | GBA_REG_DMA1SAD_LO | GBA_REG_DMA1DAD_LO
            | GBA_REG_DMA2SAD_LO | GBA_REG_DMA2DAD_LO | GBA_REG_DMA3SAD_LO | GBA_REG_DMA3DAD_LO => {
                let hi = self.memory.io[(address >> 1) as usize + 1] as u32;
                self.io_write32(address, (hi << 16) | value as u32);
                return;
            }
            GBA_REG_DMA0SAD_HI | GBA_REG_DMA0DAD_HI | GBA_REG_DMA1SAD_HI | GBA_REG_DMA1DAD_HI
            | GBA_REG_DMA2SAD_HI | GBA_REG_DMA2DAD_HI | GBA_REG_DMA3SAD_HI | GBA_REG_DMA3DAD_HI => {
                let lo = self.memory.io[((address >> 1) - 1) as usize] as u32;
                self.io_write32(address - 2, lo | ((value as u32) << 16));
                return;
            }
            GBA_REG_DMA0CNT_LO | GBA_REG_DMA1CNT_LO | GBA_REG_DMA2CNT_LO | GBA_REG_DMA3CNT_LO => {
                // Handled inside of DMA routines
            }
            GBA_REG_DMA0CNT_HI => value = self.dma_write_cnt_hi(0, value),
            GBA_REG_DMA1CNT_HI => value = self.dma_write_cnt_hi(1, value),
            GBA_REG_DMA2CNT_HI => value = self.dma_write_cnt_hi(2, value),
            GBA_REG_DMA3CNT_HI => value = self.dma_write_cnt_hi(3, value),

            // Timers
            GBA_REG_TM0CNT_LO => {
                self.timer_write_tmcnt_lo(0, value);
                return;
            }
            GBA_REG_TM1CNT_LO => {
                self.timer_write_tmcnt_lo(1, value);
                return;
            }
            GBA_REG_TM2CNT_LO => {
                self.timer_write_tmcnt_lo(2, value);
                return;
            }
            GBA_REG_TM3CNT_LO => {
                self.timer_write_tmcnt_lo(3, value);
                return;
            }
            GBA_REG_TM0CNT_HI => {
                value &= 0x00C7;
                self.timer_write_tmcnt_hi(0, value);
            }
            GBA_REG_TM1CNT_HI => {
                value &= 0x00C7;
                self.timer_write_tmcnt_hi(1, value);
            }
            GBA_REG_TM2CNT_HI => {
                value &= 0x00C7;
                self.timer_write_tmcnt_hi(2, value);
            }
            GBA_REG_TM3CNT_HI => {
                value &= 0x00C7;
                self.timer_write_tmcnt_hi(3, value);
            }

            // SIO
            GBA_REG_SIOCNT => {
                value &= 0x7FFF;
                self.sio_write_siocnt(value);
            }
            GBA_REG_RCNT => {
                value &= 0xC1FF;
                self.sio_write_rcnt(value);
            }
            GBA_REG_JOY_TRANS_LO | GBA_REG_JOY_TRANS_HI => {
                self.memory.io[(GBA_REG_JOYSTAT >> 1) as usize] |= JOYSTAT_TRANS;
                value = self.sio_write_register(address, value);
            }
            GBA_REG_SIODATA32_LO | GBA_REG_SIODATA32_HI | GBA_REG_SIOMLT_SEND | GBA_REG_JOYCNT
            | GBA_REG_JOYSTAT | GBA_REG_JOY_RECV_LO | GBA_REG_JOY_RECV_HI => {
                value = self.sio_write_register(address, value);
            }

            // Interrupts and misc
            GBA_REG_KEYCNT => {
                value &= 0xC3FF;
                if (self.keys_last as i32) < 0x400 {
                    let cur = self.memory.io[(address >> 1) as usize];
                    self.keys_last &= (cur | !value) as u16 & 0x3FF;
                }
                self.memory.io[(address >> 1) as usize] = value;
                self.test_keypad_irq();
                return;
            }
            GBA_REG_WAITCNT => {
                value &= 0x5FFF;
                self.adjust_waitstates(value);
            }
            GBA_REG_IE => {
                self.memory.io[(GBA_REG_IE >> 1) as usize] = value;
                self.test_irq(1);
                return;
            }
            GBA_REG_IF => {
                let v = self.memory.io[(GBA_REG_IF >> 1) as usize] & !value;
                self.memory.io[(GBA_REG_IF >> 1) as usize] = v;
                self.test_irq(1);
                return;
            }
            GBA_REG_IME => {
                self.memory.io[(GBA_REG_IME >> 1) as usize] = value & 1;
                self.test_irq(1);
                return;
            }
            GBA_REG_MAX => {
                // Some bad interrupt libraries will write to this
            }
            GBA_REG_POSTFLG => {
                if self.memory.active_region == crate::memory::GBA_REGION_BIOS as i32 {
                    if self.memory.io[(address >> 1) as usize] != 0 {
                        if value & 0x8000 != 0 {
                            self.stop();
                        } else {
                            self.halt();
                        }
                    }
                    value &= !0x8000;
                } else {
                    mlog!(Level::GameError, rgba_core::log::GBA, "Write to BIOS-only I/O register: {:03X}", address);
                    return;
                }
            }
            GBA_REG_EXWAITCNT_HI => {
                // This register sits outside of the normal I/O block...
                let address2 = GBA_REG_INTERNAL_EXWAITCNT_HI;
                value &= 0xFF00;
                self.adjust_ewram_waitstates(value);
                self.memory.io[(address2 >> 1) as usize] = value;
                return;
            }
            GBA_REG_DEBUG_ENABLE => {
                self.debug = value == 0xC0DE;
                return;
            }
            GBA_REG_DEBUG_FLAGS => {
                if self.debug {
                    self.debug_write(value); // added below on Gba
                    return;
                } else {
                    mlog!(Level::Stub, rgba_core::log::GBA, "Stub I/O register write: {:03X}", address);
                    if address >= GBA_REG_MAX {
                        mlog!(Level::GameError, rgba_core::log::GBA, "Write to unused I/O register: {:03X}", address);
                        return;
                    }
                }
            }
            _ => {
                if (GBA_REG_DEBUG_STRING..GBA_REG_DEBUG_STRING + 0x100).contains(&address) {
                    let off = (address - GBA_REG_DEBUG_STRING) as usize;
                    let bytes = value.to_le_bytes();
                    self.debug_string[off] = bytes[0];
                    if off + 1 < self.debug_string.len() {
                        self.debug_string[off + 1] = bytes[1];
                    }
                    return;
                }
                mlog!(Level::Stub, rgba_core::log::GBA, "Stub I/O register write: {:03X}", address);
                if address >= GBA_REG_MAX {
                    mlog!(Level::GameError, rgba_core::log::GBA, "Write to unused I/O register: {:03X}", address);
                    return;
                }
            }
        }
        self.memory.io[(address >> 1) as usize] = value;
    }

    /// GBAIOWrite8
    pub fn io_write8_real(&mut self, address: u32, value: u8) {
        if (GBA_REG_DEBUG_STRING..GBA_REG_DEBUG_STRING + 0x100).contains(&address) {
            self.debug_string[(address - GBA_REG_DEBUG_STRING) as usize] = value;
            return;
        }
        if address > GBA_SIZE_IO as u32 {
            return;
        }
        match address {
            GBA_REG_SOUND1CNT_HI => {
                let now = self.current_time();
                self.audio_sample(now);
                self.audio_write_nr11(value);
                let m = &mut self.memory.io[(GBA_REG_SOUND1CNT_HI >> 1) as usize];
                *m &= 0xFF00;
                *m |= (value as u16) & 0xC0;
            }
            0x63 => {
                let now = self.current_time();
                self.audio_sample(now);
                self.audio_write_nr12(value);
                let m = &mut self.memory.io[(GBA_REG_SOUND1CNT_HI >> 1) as usize];
                *m &= 0x00C0;
                *m |= (value as u16) << 8;
            }
            GBA_REG_SOUND1CNT_X => {
                let now = self.current_time();
                self.audio_sample(now);
                self.audio_write_nr13(value);
            }
            0x65 => {
                let now = self.current_time();
                self.audio_sample(now);
                self.audio_write_nr14(value);
                self.memory.io[(GBA_REG_SOUND1CNT_X >> 1) as usize] = ((value as u16) & 0x40) << 8;
            }
            GBA_REG_SOUND2CNT_LO => {
                let now = self.current_time();
                self.audio_sample(now);
                self.audio_write_nr21(value);
                let m = &mut self.memory.io[(GBA_REG_SOUND2CNT_LO >> 1) as usize];
                *m &= 0xFF00;
                *m |= (value as u16) & 0xC0;
            }
            0x69 => {
                let now = self.current_time();
                self.audio_sample(now);
                self.audio_write_nr22(value);
                let m = &mut self.memory.io[(GBA_REG_SOUND2CNT_LO >> 1) as usize];
                *m &= 0x00C0;
                *m |= (value as u16) << 8;
            }
            GBA_REG_SOUND2CNT_HI => {
                let now = self.current_time();
                self.audio_sample(now);
                self.audio_write_nr23(value);
            }
            0x6D => {
                let now = self.current_time();
                self.audio_sample(now);
                self.audio_write_nr24(value);
                self.memory.io[(GBA_REG_SOUND2CNT_HI >> 1) as usize] = ((value as u16) & 0x40) << 8;
            }
            GBA_REG_SOUND3CNT_HI => {
                let now = self.current_time();
                self.audio_sample(now);
                self.audio_write_nr31(value);
            }
            0x73 => {
                let now = self.current_time();
                self.audio_sample(now);
                self.audio.ch3_volume_gba(value);
                self.memory.io[(GBA_REG_SOUND3CNT_HI >> 1) as usize] = ((value as u16) & 0xE0) << 8;
            }
            GBA_REG_SOUND3CNT_X => {
                let now = self.current_time();
                self.audio_sample(now);
                self.audio_write_nr33(value);
            }
            0x75 => {
                let now = self.current_time();
                self.audio_sample(now);
                self.audio_write_nr34(value);
                self.memory.io[(GBA_REG_SOUND3CNT_X >> 1) as usize] = ((value as u16) & 0x40) << 8;
            }
            GBA_REG_SOUND4CNT_LO => {
                let now = self.current_time();
                self.audio_sample(now);
                self.audio_write_nr41(value);
            }
            0x79 => {
                let now = self.current_time();
                self.audio_sample(now);
                self.audio_write_nr42(value);
                self.memory.io[(GBA_REG_SOUND4CNT_LO >> 1) as usize] = (value as u16) << 8;
            }
            GBA_REG_SOUND4CNT_HI => {
                let now = self.current_time();
                self.audio_sample(now);
                self.audio_write_nr43(value);
                let m = &mut self.memory.io[(GBA_REG_SOUND4CNT_HI >> 1) as usize];
                *m &= 0x4000;
                *m |= value as u16;
            }
            0x7D => {
                let now = self.current_time();
                self.audio_sample(now);
                self.audio_write_nr44(value);
                let m = &mut self.memory.io[(GBA_REG_SOUND4CNT_HI >> 1) as usize];
                *m &= 0x00FF;
                *m |= ((value as u16) & 0x40) << 8;
            }
            _ => {
                let value16 = (value as u16) << (8 * (address & 1));
                let value16 = value16
                    | self.memory.io[((address as usize) & (GBA_SIZE_IO - 1)) >> 1]
                        & !(0xFFu16 << (8 * (address & 1)));
                self.io_write(address & 0xFFFFFFFE, value16);
            }
        }
    }

    /// GBAIOWrite32
    pub fn io_write32_real(&mut self, address: u32, value: u32) {
        let mut value = value;
        match address {
            GBA_REG_WAVE_RAM0_LO => {
                self.audio_write_waveram(0, value);
            }
            GBA_REG_WAVE_RAM1_LO => {
                self.audio_write_waveram(1, value);
            }
            GBA_REG_WAVE_RAM2_LO => {
                self.audio_write_waveram(2, value);
            }
            GBA_REG_WAVE_RAM3_LO => {
                self.audio_write_waveram(3, value);
            }
            GBA_REG_FIFO_A_LO | GBA_REG_FIFO_B_LO => {
                value = self.audio_write_fifo(address, value);
            }
            GBA_REG_DMA0SAD_LO => value = self.dma_write_sad(0, value),
            GBA_REG_DMA0DAD_LO => value = self.dma_write_dad(0, value),
            GBA_REG_DMA1SAD_LO => value = self.dma_write_sad(1, value),
            GBA_REG_DMA1DAD_LO => value = self.dma_write_dad(1, value),
            GBA_REG_DMA2SAD_LO => value = self.dma_write_sad(2, value),
            GBA_REG_DMA2DAD_LO => value = self.dma_write_dad(2, value),
            GBA_REG_DMA3SAD_LO => value = self.dma_write_sad(3, value),
            GBA_REG_DMA3DAD_LO => value = self.dma_write_dad(3, value),
            _ => {
                if (GBA_REG_DEBUG_STRING..GBA_REG_DEBUG_STRING + 0x100).contains(&address) {
                    let off = (address - GBA_REG_DEBUG_STRING) as usize;
                    self.debug_string[off..off + 4].copy_from_slice(&value.to_le_bytes());
                    return;
                }
                self.io_write(address, (value & 0xFFFF) as u16);
                self.io_write(address | 2, (value >> 16) as u16);
                return;
            }
        }
        self.memory.io[(address >> 1) as usize] = value as u16;
        self.memory.io[((address >> 1) + 1) as usize] = (value >> 16) as u16;
    }

    /// GBAIOIsReadConstant
    pub fn io_is_read_constant(address: u32) -> bool {
        matches!(
            address,
            GBA_REG_BG0CNT | GBA_REG_BG1CNT | GBA_REG_BG2CNT | GBA_REG_BG3CNT | GBA_REG_WININ
            | GBA_REG_WINOUT | GBA_REG_BLDCNT | GBA_REG_BLDALPHA | GBA_REG_SOUND1CNT_LO
            | GBA_REG_SOUND1CNT_HI | GBA_REG_SOUND1CNT_X | GBA_REG_SOUND2CNT_LO
            | GBA_REG_SOUND2CNT_HI | GBA_REG_SOUND3CNT_LO | GBA_REG_SOUND3CNT_HI
            | GBA_REG_SOUND3CNT_X | GBA_REG_SOUND4CNT_LO | GBA_REG_SOUND4CNT_HI
            | GBA_REG_SOUNDCNT_LO | GBA_REG_SOUNDCNT_HI | GBA_REG_TM0CNT_HI | GBA_REG_TM1CNT_HI
            | GBA_REG_TM2CNT_HI | GBA_REG_TM3CNT_HI | GBA_REG_KEYINPUT | GBA_REG_KEYCNT
            | GBA_REG_IE
        )
    }

    /// GBAIORead
    pub fn io_read_real(&mut self, address: u32) -> u16 {
        if !Gba::io_is_read_constant(address) {
            // Most IO reads need to disable idle removal
            self.halt_pending = false;
        }

        match address {
            GBA_REG_TM0CNT_LO => self.timer_update_register(0, 2),
            GBA_REG_TM1CNT_LO => self.timer_update_register(1, 2),
            GBA_REG_TM2CNT_LO => self.timer_update_register(2, 2),
            GBA_REG_TM3CNT_LO => self.timer_update_register(3, 2),
            GBA_REG_KEYINPUT => {
                // keyCallback (GBP virtual controller, gbp.c _gbpRead)
                let mut allow_opposing = self.allow_opposing_directions;
                if self.sio.gbp.key_override {
                    self.keys_active = crate::sio::gbp::gbp_read_keys(self);
                    // GBP requires opposing directions
                    if !allow_opposing {
                        allow_opposing = true;
                    }
                }
                let mut input = self.keys_active;
                if !allow_opposing {
                    let rl = input & 0x030;
                    let ud = input & 0x0C0;
                    input &= 0x30F;
                    if rl != 0x030 {
                        input |= rl & 0x30;
                    }
                    if ud != 0x0C0 {
                        input |= ud & 0xC0;
                    }
                }
                self.memory.io[(GBA_REG_KEYINPUT >> 1) as usize] = 0x3FF ^ input;
            }
            GBA_REG_SIOCNT => return self.sio.siocnt,
            GBA_REG_RCNT => return self.sio.rcnt,
            GBA_REG_BG0HOFS | GBA_REG_BG0VOFS | GBA_REG_BG1HOFS | GBA_REG_BG1VOFS
            | GBA_REG_BG2HOFS | GBA_REG_BG2VOFS | GBA_REG_BG3HOFS | GBA_REG_BG3VOFS
            | GBA_REG_BG2PA | GBA_REG_BG2PB | GBA_REG_BG2PC | GBA_REG_BG2PD | GBA_REG_BG2X_LO
            | GBA_REG_BG2X_HI | GBA_REG_BG2Y_LO | GBA_REG_BG2Y_HI | GBA_REG_BG3PA
            | GBA_REG_BG3PB | GBA_REG_BG3PC | GBA_REG_BG3PD | GBA_REG_BG3X_LO | GBA_REG_BG3X_HI
            | GBA_REG_BG3Y_LO | GBA_REG_BG3Y_HI | GBA_REG_WIN0H | GBA_REG_WIN1H | GBA_REG_WIN0V
            | GBA_REG_WIN1V | GBA_REG_MOSAIC | GBA_REG_BLDY | GBA_REG_FIFO_A_LO
            | GBA_REG_FIFO_A_HI | GBA_REG_FIFO_B_LO | GBA_REG_FIFO_B_HI | GBA_REG_DMA0SAD_LO
            | GBA_REG_DMA0SAD_HI | GBA_REG_DMA0DAD_LO | GBA_REG_DMA0DAD_HI | GBA_REG_DMA1SAD_LO
            | GBA_REG_DMA1SAD_HI | GBA_REG_DMA1DAD_LO | GBA_REG_DMA1DAD_HI | GBA_REG_DMA2SAD_LO
            | GBA_REG_DMA2SAD_HI | GBA_REG_DMA2DAD_LO | GBA_REG_DMA2DAD_HI | GBA_REG_DMA3SAD_LO
            | GBA_REG_DMA3SAD_HI | GBA_REG_DMA3DAD_LO | GBA_REG_DMA3DAD_HI => {
                mlog!(Level::GameError, rgba_core::log::GBA, "Read from write-only I/O register: {:03X}", address);
                return self.load_bad() as u16;
            }
            GBA_REG_DMA0CNT_LO | GBA_REG_DMA1CNT_LO | GBA_REG_DMA2CNT_LO | GBA_REG_DMA3CNT_LO => {
                // Many, many things read from the DMA register
            }
            GBA_REG_MAX => {
                // Some bad interrupt libraries will read from this
                // (Silent) write-only register
                return 0;
            }
            GBA_REG_JOY_RECV_LO | GBA_REG_JOY_RECV_HI => {
                self.memory.io[(GBA_REG_JOYSTAT >> 1) as usize] &= !JOYSTAT_RECV;
            }
            GBA_REG_WAVE_RAM0_LO => return (self.audio_read_waveram(0) & 0xFFFF) as u16,
            GBA_REG_WAVE_RAM0_HI => return (self.audio_read_waveram(0) >> 16) as u16,
            GBA_REG_WAVE_RAM1_LO => return (self.audio_read_waveram(1) & 0xFFFF) as u16,
            GBA_REG_WAVE_RAM1_HI => return (self.audio_read_waveram(1) >> 16) as u16,
            GBA_REG_WAVE_RAM2_LO => return (self.audio_read_waveram(2) & 0xFFFF) as u16,
            GBA_REG_WAVE_RAM2_HI => return (self.audio_read_waveram(2) >> 16) as u16,
            GBA_REG_WAVE_RAM3_LO => return (self.audio_read_waveram(3) & 0xFFFF) as u16,
            GBA_REG_WAVE_RAM3_HI => return (self.audio_read_waveram(3) >> 16) as u16,
            GBA_REG_SOUND1CNT_LO | GBA_REG_SOUND1CNT_HI | GBA_REG_SOUND1CNT_X
            | GBA_REG_SOUND2CNT_LO | GBA_REG_SOUND2CNT_HI | GBA_REG_SOUND3CNT_LO
            | GBA_REG_SOUND3CNT_HI | GBA_REG_SOUND3CNT_X | GBA_REG_SOUND4CNT_LO
            | GBA_REG_SOUND4CNT_HI | GBA_REG_SOUNDCNT_LO => {
                if self.memory.io[(GBA_REG_SOUNDCNT_X >> 1) as usize] & 0x80 == 0 {
                    // TODO: Is writing allowed when the circuit is disabled?
                    return 0;
                }
            }
            GBA_REG_DISPCNT | GBA_REG_STEREOCNT | GBA_REG_DISPSTAT | GBA_REG_VCOUNT
            | GBA_REG_BG0CNT | GBA_REG_BG1CNT | GBA_REG_BG2CNT | GBA_REG_BG3CNT | GBA_REG_WININ
            | GBA_REG_WINOUT | GBA_REG_BLDCNT | GBA_REG_BLDALPHA | GBA_REG_SOUNDCNT_HI
            | GBA_REG_SOUNDCNT_X | GBA_REG_SOUNDBIAS | GBA_REG_DMA0CNT_HI | GBA_REG_DMA1CNT_HI
            | GBA_REG_DMA2CNT_HI | GBA_REG_DMA3CNT_HI | GBA_REG_TM0CNT_HI | GBA_REG_TM1CNT_HI
            | GBA_REG_TM2CNT_HI | GBA_REG_TM3CNT_HI | GBA_REG_KEYCNT | GBA_REG_SIOMULTI0
            | GBA_REG_SIOMULTI1 | GBA_REG_SIOMULTI2 | GBA_REG_SIOMULTI3 | GBA_REG_SIOMLT_SEND
            | GBA_REG_JOYCNT | GBA_REG_JOY_TRANS_LO | GBA_REG_JOY_TRANS_HI | GBA_REG_JOYSTAT
            | GBA_REG_IE | GBA_REG_IF | GBA_REG_WAITCNT | GBA_REG_IME | GBA_REG_POSTFLG => {
                // Handled transparently by registers
            }
            0x066 | 0x06A | 0x06E | 0x076 | 0x07A | 0x07E | 0x086 | 0x08A | 0x136 | 0x142
            | 0x15A | 0x206 | 0x302 => {
                mlog!(Level::GameError, rgba_core::log::GBA, "Read from unused I/O register: {:03X}", address);
                return 0;
            }
            GBA_REG_EXWAITCNT_LO | GBA_REG_EXWAITCNT_HI => {
                return self.memory.io
                    [((address + GBA_REG_INTERNAL_EXWAITCNT_LO - GBA_REG_EXWAITCNT_LO) >> 1) as usize];
            }
            GBA_REG_DEBUG_ENABLE => {
                if self.debug {
                    return 0x1DEA;
                }
                mlog!(Level::GameError, rgba_core::log::GBA, "Read from unused I/O register: {:03X}", address);
                return self.load_bad() as u16;
            }
            _ => {
                mlog!(Level::GameError, rgba_core::log::GBA, "Read from unused I/O register: {:03X}", address);
                return self.load_bad() as u16;
            }
        }
        self.memory.io[(address >> 1) as usize]
    }
}

// GBAIORegisterNames (debugger)
pub const GBA_IO_REGISTER_NAMES: [Option<&str>; (GBA_REG_MAX / 2) as usize] = build_io_register_names();

const fn build_io_register_names() -> [Option<&'static str>; (GBA_REG_MAX / 2) as usize] {
    let mut t: [Option<&'static str>; (GBA_REG_MAX / 2) as usize] = [None; (GBA_REG_MAX / 2) as usize];
    t[(GBA_REG_DISPCNT >> 1) as usize] = Some("DISPCNT");
    t[(GBA_REG_DISPSTAT >> 1) as usize] = Some("DISPSTAT");
    t[(GBA_REG_VCOUNT >> 1) as usize] = Some("VCOUNT");
    t[(GBA_REG_BG0CNT >> 1) as usize] = Some("BG0CNT");
    t[(GBA_REG_BG1CNT >> 1) as usize] = Some("BG1CNT");
    t[(GBA_REG_BG2CNT >> 1) as usize] = Some("BG2CNT");
    t[(GBA_REG_BG3CNT >> 1) as usize] = Some("BG3CNT");
    t[(GBA_REG_BG0HOFS >> 1) as usize] = Some("BG0HOFS");
    t[(GBA_REG_BG0VOFS >> 1) as usize] = Some("BG0VOFS");
    t[(GBA_REG_BG1HOFS >> 1) as usize] = Some("BG1HOFS");
    t[(GBA_REG_BG1VOFS >> 1) as usize] = Some("BG1VOFS");
    t[(GBA_REG_BG2HOFS >> 1) as usize] = Some("BG2HOFS");
    t[(GBA_REG_BG2VOFS >> 1) as usize] = Some("BG2VOFS");
    t[(GBA_REG_BG3HOFS >> 1) as usize] = Some("BG3HOFS");
    t[(GBA_REG_BG3VOFS >> 1) as usize] = Some("BG3VOFS");
    t[(GBA_REG_BG2PA >> 1) as usize] = Some("BG2PA");
    t[(GBA_REG_BG2PB >> 1) as usize] = Some("BG2PB");
    t[(GBA_REG_BG2PC >> 1) as usize] = Some("BG2PC");
    t[(GBA_REG_BG2PD >> 1) as usize] = Some("BG2PD");
    t[(GBA_REG_BG2X_LO >> 1) as usize] = Some("BG2X_LO");
    t[(GBA_REG_BG2X_HI >> 1) as usize] = Some("BG2X_HI");
    t[(GBA_REG_BG2Y_LO >> 1) as usize] = Some("BG2Y_LO");
    t[(GBA_REG_BG2Y_HI >> 1) as usize] = Some("BG2Y_HI");
    t[(GBA_REG_BG3PA >> 1) as usize] = Some("BG3PA");
    t[(GBA_REG_BG3PB >> 1) as usize] = Some("BG3PB");
    t[(GBA_REG_BG3PC >> 1) as usize] = Some("BG3PC");
    t[(GBA_REG_BG3PD >> 1) as usize] = Some("BG3PD");
    t[(GBA_REG_BG3X_LO >> 1) as usize] = Some("BG3X_LO");
    t[(GBA_REG_BG3X_HI >> 1) as usize] = Some("BG3X_HI");
    t[(GBA_REG_BG3Y_LO >> 1) as usize] = Some("BG3Y_LO");
    t[(GBA_REG_BG3Y_HI >> 1) as usize] = Some("BG3Y_HI");
    t[(GBA_REG_WIN0H >> 1) as usize] = Some("WIN0H");
    t[(GBA_REG_WIN1H >> 1) as usize] = Some("WIN1H");
    t[(GBA_REG_WIN0V >> 1) as usize] = Some("WIN0V");
    t[(GBA_REG_WIN1V >> 1) as usize] = Some("WIN1V");
    t[(GBA_REG_WININ >> 1) as usize] = Some("WININ");
    t[(GBA_REG_WINOUT >> 1) as usize] = Some("WINOUT");
    t[(GBA_REG_MOSAIC >> 1) as usize] = Some("MOSAIC");
    t[(GBA_REG_BLDCNT >> 1) as usize] = Some("BLDCNT");
    t[(GBA_REG_BLDALPHA >> 1) as usize] = Some("BLDALPHA");
    t[(GBA_REG_BLDY >> 1) as usize] = Some("BLDY");
    t[(GBA_REG_SOUND1CNT_LO >> 1) as usize] = Some("SOUND1CNT_LO");
    t[(GBA_REG_SOUND1CNT_HI >> 1) as usize] = Some("SOUND1CNT_HI");
    t[(GBA_REG_SOUND1CNT_X >> 1) as usize] = Some("SOUND1CNT_X");
    t[(GBA_REG_SOUND2CNT_LO >> 1) as usize] = Some("SOUND2CNT_LO");
    t[(GBA_REG_SOUND2CNT_HI >> 1) as usize] = Some("SOUND2CNT_HI");
    t[(GBA_REG_SOUND3CNT_LO >> 1) as usize] = Some("SOUND3CNT_LO");
    t[(GBA_REG_SOUND3CNT_HI >> 1) as usize] = Some("SOUND3CNT_HI");
    t[(GBA_REG_SOUND3CNT_X >> 1) as usize] = Some("SOUND3CNT_X");
    t[(GBA_REG_SOUND4CNT_LO >> 1) as usize] = Some("SOUND4CNT_LO");
    t[(GBA_REG_SOUND4CNT_HI >> 1) as usize] = Some("SOUND4CNT_HI");
    t[(GBA_REG_SOUNDCNT_LO >> 1) as usize] = Some("SOUNDCNT_LO");
    t[(GBA_REG_SOUNDCNT_HI >> 1) as usize] = Some("SOUNDCNT_HI");
    t[(GBA_REG_SOUNDCNT_X >> 1) as usize] = Some("SOUNDCNT_X");
    t[(GBA_REG_SOUNDBIAS >> 1) as usize] = Some("SOUNDBIAS");
    t[(GBA_REG_WAVE_RAM0_LO >> 1) as usize] = Some("WAVE_RAM0_LO");
    t[(GBA_REG_WAVE_RAM0_HI >> 1) as usize] = Some("WAVE_RAM0_HI");
    t[(GBA_REG_WAVE_RAM1_LO >> 1) as usize] = Some("WAVE_RAM1_LO");
    t[(GBA_REG_WAVE_RAM1_HI >> 1) as usize] = Some("WAVE_RAM1_HI");
    t[(GBA_REG_WAVE_RAM2_LO >> 1) as usize] = Some("WAVE_RAM2_LO");
    t[(GBA_REG_WAVE_RAM2_HI >> 1) as usize] = Some("WAVE_RAM2_HI");
    t[(GBA_REG_WAVE_RAM3_LO >> 1) as usize] = Some("WAVE_RAM3_LO");
    t[(GBA_REG_WAVE_RAM3_HI >> 1) as usize] = Some("WAVE_RAM3_HI");
    t[(GBA_REG_FIFO_A_LO >> 1) as usize] = Some("FIFO_A_LO");
    t[(GBA_REG_FIFO_A_HI >> 1) as usize] = Some("FIFO_A_HI");
    t[(GBA_REG_FIFO_B_LO >> 1) as usize] = Some("FIFO_B_LO");
    t[(GBA_REG_FIFO_B_HI >> 1) as usize] = Some("FIFO_B_HI");
    t[(GBA_REG_DMA0SAD_LO >> 1) as usize] = Some("DMA0SAD_LO");
    t[(GBA_REG_DMA0SAD_HI >> 1) as usize] = Some("DMA0SAD_HI");
    t[(GBA_REG_DMA0DAD_LO >> 1) as usize] = Some("DMA0DAD_LO");
    t[(GBA_REG_DMA0DAD_HI >> 1) as usize] = Some("DMA0DAD_HI");
    t[(GBA_REG_DMA0CNT_LO >> 1) as usize] = Some("DMA0CNT_LO");
    t[(GBA_REG_DMA0CNT_HI >> 1) as usize] = Some("DMA0CNT_HI");
    t[(GBA_REG_DMA1SAD_LO >> 1) as usize] = Some("DMA1SAD_LO");
    t[(GBA_REG_DMA1SAD_HI >> 1) as usize] = Some("DMA1SAD_HI");
    t[(GBA_REG_DMA1DAD_LO >> 1) as usize] = Some("DMA1DAD_LO");
    t[(GBA_REG_DMA1DAD_HI >> 1) as usize] = Some("DMA1DAD_HI");
    t[(GBA_REG_DMA1CNT_LO >> 1) as usize] = Some("DMA1CNT_LO");
    t[(GBA_REG_DMA1CNT_HI >> 1) as usize] = Some("DMA1CNT_HI");
    t[(GBA_REG_DMA2SAD_LO >> 1) as usize] = Some("DMA2SAD_LO");
    t[(GBA_REG_DMA2SAD_HI >> 1) as usize] = Some("DMA2SAD_HI");
    t[(GBA_REG_DMA2DAD_LO >> 1) as usize] = Some("DMA2DAD_LO");
    t[(GBA_REG_DMA2DAD_HI >> 1) as usize] = Some("DMA2DAD_HI");
    t[(GBA_REG_DMA2CNT_LO >> 1) as usize] = Some("DMA2CNT_LO");
    t[(GBA_REG_DMA2CNT_HI >> 1) as usize] = Some("DMA2CNT_HI");
    t[(GBA_REG_DMA3SAD_LO >> 1) as usize] = Some("DMA3SAD_LO");
    t[(GBA_REG_DMA3SAD_HI >> 1) as usize] = Some("DMA3SAD_HI");
    t[(GBA_REG_DMA3DAD_LO >> 1) as usize] = Some("DMA3DAD_LO");
    t[(GBA_REG_DMA3DAD_HI >> 1) as usize] = Some("DMA3DAD_HI");
    t[(GBA_REG_DMA3CNT_LO >> 1) as usize] = Some("DMA3CNT_LO");
    t[(GBA_REG_DMA3CNT_HI >> 1) as usize] = Some("DMA3CNT_HI");
    t[(GBA_REG_TM0CNT_LO >> 1) as usize] = Some("TM0CNT_LO");
    t[(GBA_REG_TM0CNT_HI >> 1) as usize] = Some("TM0CNT_HI");
    t[(GBA_REG_TM1CNT_LO >> 1) as usize] = Some("TM1CNT_LO");
    t[(GBA_REG_TM1CNT_HI >> 1) as usize] = Some("TM1CNT_HI");
    t[(GBA_REG_TM2CNT_LO >> 1) as usize] = Some("TM2CNT_LO");
    t[(GBA_REG_TM2CNT_HI >> 1) as usize] = Some("TM2CNT_HI");
    t[(GBA_REG_TM3CNT_LO >> 1) as usize] = Some("TM3CNT_LO");
    t[(GBA_REG_TM3CNT_HI >> 1) as usize] = Some("TM3CNT_HI");
    t[(GBA_REG_SIOMULTI0 >> 1) as usize] = Some("SIOMULTI0");
    t[(GBA_REG_SIOMULTI1 >> 1) as usize] = Some("SIOMULTI1");
    t[(GBA_REG_SIOMULTI2 >> 1) as usize] = Some("SIOMULTI2");
    t[(GBA_REG_SIOMULTI3 >> 1) as usize] = Some("SIOMULTI3");
    t[(GBA_REG_SIOCNT >> 1) as usize] = Some("SIOCNT");
    t[(GBA_REG_SIOMLT_SEND >> 1) as usize] = Some("SIOMLT_SEND");
    t[(GBA_REG_KEYINPUT >> 1) as usize] = Some("KEYINPUT");
    t[(GBA_REG_KEYCNT >> 1) as usize] = Some("KEYCNT");
    t[(GBA_REG_RCNT >> 1) as usize] = Some("RCNT");
    t[(GBA_REG_JOYCNT >> 1) as usize] = Some("JOYCNT");
    t[(GBA_REG_JOY_RECV_LO >> 1) as usize] = Some("JOY_RECV_LO");
    t[(GBA_REG_JOY_RECV_HI >> 1) as usize] = Some("JOY_RECV_HI");
    t[(GBA_REG_JOY_TRANS_LO >> 1) as usize] = Some("JOY_TRANS_LO");
    t[(GBA_REG_JOY_TRANS_HI >> 1) as usize] = Some("JOY_TRANS_HI");
    t[(GBA_REG_JOYSTAT >> 1) as usize] = Some("JOYSTAT");
    t[(GBA_REG_IE >> 1) as usize] = Some("IE");
    t[(GBA_REG_IF >> 1) as usize] = Some("IF");
    t[(GBA_REG_WAITCNT >> 1) as usize] = Some("WAITCNT");
    t[(GBA_REG_IME >> 1) as usize] = Some("IME");
    t
}
