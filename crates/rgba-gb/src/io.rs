// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/include/mgba/internal/gb/io.h and mgba/src/gb/io.c.

use rgba_core::mlog;
use rgba_core::Level;

use crate::gb::{EventId, Gb, GbModel};
use crate::audio;

// IO register offsets (enum GBIORegisters).
pub const GB_REG_JOYP: u16 = 0x00;
pub const GB_REG_SB: u16 = 0x01;
pub const GB_REG_SC: u16 = 0x02;
pub const GB_REG_DIV: u16 = 0x04;
pub const GB_REG_TIMA: u16 = 0x05;
pub const GB_REG_TMA: u16 = 0x06;
pub const GB_REG_TAC: u16 = 0x07;
pub const GB_REG_IF: u16 = 0x0F;
pub const GB_REG_IE: u16 = 0xFF;

pub const GB_REG_NR10: u16 = 0x10;
pub const GB_REG_NR11: u16 = 0x11;
pub const GB_REG_NR12: u16 = 0x12;
pub const GB_REG_NR13: u16 = 0x13;
pub const GB_REG_NR14: u16 = 0x14;
pub const GB_REG_NR21: u16 = 0x16;
pub const GB_REG_NR22: u16 = 0x17;
pub const GB_REG_NR23: u16 = 0x18;
pub const GB_REG_NR24: u16 = 0x19;
pub const GB_REG_NR30: u16 = 0x1A;
pub const GB_REG_NR31: u16 = 0x1B;
pub const GB_REG_NR32: u16 = 0x1C;
pub const GB_REG_NR33: u16 = 0x1D;
pub const GB_REG_NR34: u16 = 0x1E;
pub const GB_REG_NR41: u16 = 0x20;
pub const GB_REG_NR42: u16 = 0x21;
pub const GB_REG_NR43: u16 = 0x22;
pub const GB_REG_NR44: u16 = 0x23;
pub const GB_REG_NR50: u16 = 0x24;
pub const GB_REG_NR51: u16 = 0x25;
pub const GB_REG_NR52: u16 = 0x26;

pub const GB_REG_WAVE_0: u16 = 0x30;

pub const GB_REG_LCDC: u16 = 0x40;
pub const GB_REG_STAT: u16 = 0x41;
pub const GB_REG_SCY: u16 = 0x42;
pub const GB_REG_SCX: u16 = 0x43;
pub const GB_REG_LY: u16 = 0x44;
pub const GB_REG_LYC: u16 = 0x45;
pub const GB_REG_DMA: u16 = 0x46;
pub const GB_REG_BGP: u16 = 0x47;
pub const GB_REG_OBP0: u16 = 0x48;
pub const GB_REG_OBP1: u16 = 0x49;
pub const GB_REG_WY: u16 = 0x4A;
pub const GB_REG_WX: u16 = 0x4B;
pub const GB_REG_KEY0: u16 = 0x4C;
pub const GB_REG_KEY1: u16 = 0x4D;
pub const GB_REG_VBK: u16 = 0x4F;
pub const GB_REG_BANK: u16 = 0x50;
pub const GB_REG_HDMA1: u16 = 0x51;
pub const GB_REG_HDMA2: u16 = 0x52;
pub const GB_REG_HDMA3: u16 = 0x53;
pub const GB_REG_HDMA4: u16 = 0x54;
pub const GB_REG_HDMA5: u16 = 0x55;
pub const GB_REG_RP: u16 = 0x56;
pub const GB_REG_BCPS: u16 = 0x68;
pub const GB_REG_BCPD: u16 = 0x69;
pub const GB_REG_OCPS: u16 = 0x6A;
pub const GB_REG_OCPD: u16 = 0x6B;
pub const GB_REG_OPRI: u16 = 0x6C;
pub const GB_REG_SVBK: u16 = 0x70;
pub const GB_REG_PCM12: u16 = 0x76;
pub const GB_REG_PCM34: u16 = 0x77;
pub const GB_REG_MAX: u16 = 0x100;

const fn build_register_mask() -> [u8; 0x100] {
    let mut t = [0u8; 0x100];
    t[GB_REG_SC as usize] = 0x7E; // TODO: GBC differences
    t[GB_REG_IF as usize] = 0xE0;
    t[GB_REG_TAC as usize] = 0xF8;
    t[GB_REG_NR10 as usize] = 0x80;
    t[GB_REG_NR11 as usize] = 0x3F;
    t[GB_REG_NR12 as usize] = 0x00;
    t[GB_REG_NR13 as usize] = 0xFF;
    t[GB_REG_NR14 as usize] = 0xBF;
    t[GB_REG_NR21 as usize] = 0x3F;
    t[GB_REG_NR22 as usize] = 0x00;
    t[GB_REG_NR23 as usize] = 0xFF;
    t[GB_REG_NR24 as usize] = 0xBF;
    t[GB_REG_NR30 as usize] = 0x7F;
    t[GB_REG_NR31 as usize] = 0xFF;
    t[GB_REG_NR32 as usize] = 0x9F;
    t[GB_REG_NR33 as usize] = 0xFF;
    t[GB_REG_NR34 as usize] = 0xBF;
    t[GB_REG_NR41 as usize] = 0xFF;
    t[GB_REG_NR42 as usize] = 0x00;
    t[GB_REG_NR43 as usize] = 0x00;
    t[GB_REG_NR44 as usize] = 0xBF;
    t[GB_REG_NR50 as usize] = 0x00;
    t[GB_REG_NR51 as usize] = 0x00;
    t[GB_REG_NR52 as usize] = 0x70;
    t[GB_REG_STAT as usize] = 0x80;
    t[GB_REG_KEY1 as usize] = 0x7E;
    t[GB_REG_VBK as usize] = 0xFE;
    t[GB_REG_OCPS as usize] = 0x40;
    t[GB_REG_BCPS as usize] = 0x40;
    t[GB_REG_OPRI as usize] = 0xFE;
    t[GB_REG_SVBK as usize] = 0xF8;
    t[0x75] = 0x8F;
    t[GB_REG_IE as usize] = 0xE0;
    t
}

static REGISTER_MASK: [u8; 0x100] = build_register_mask();

impl Gb {
    pub fn model_has_sgb(&self) -> bool {
        // gb->model & GB_MODEL_SGB
        (self.model as i32) & (GbModel::Sgb as i32) != 0
    }

    /// GBIOReset
    pub fn io_reset(&mut self) {
        self.memory.io = [0; 0x80];

        self.io_write(GB_REG_TIMA, 0);
        self.io_write(GB_REG_TMA, 0);
        self.io_write(GB_REG_TAC, 0);
        self.io_write(GB_REG_IF, 1);
        self.io_write(GB_REG_LCDC, 0x00);
        self.io_write(GB_REG_SCY, 0x00);
        self.io_write(GB_REG_SCX, 0x00);
        self.io_write(GB_REG_LYC, 0x00);
        self.memory.io[GB_REG_DMA as usize] = 0xFF;
        self.io_write(GB_REG_BGP, 0xFC);
        if !self.model.is_cgb() {
            self.io_write(GB_REG_OBP0, 0xFF);
            self.io_write(GB_REG_OBP1, 0xFF);
        }
        self.io_write(GB_REG_WY, 0x00);
        self.io_write(GB_REG_WX, 0x00);
        self.memory.io[GB_REG_BANK as usize] = 0xFF;
        if self.model.is_cgb() {
            self.io_write(GB_REG_KEY0, 0);
            self.io_write(GB_REG_JOYP, 0xFF);
            self.io_write(GB_REG_VBK, 0);
            self.io_write(GB_REG_BCPS, 0x80);
            self.io_write(GB_REG_OCPS, 0);
            self.io_write(GB_REG_SVBK, 1);
            self.io_write(GB_REG_HDMA1, 0xFF);
            self.io_write(GB_REG_HDMA2, 0xFF);
            self.io_write(GB_REG_HDMA3, 0xFF);
            self.io_write(GB_REG_HDMA4, 0xFF);
            self.memory.io[GB_REG_HDMA5 as usize] = 0xFF;
        } else {
            // memset(&io[KEY0], 0xFF, PCM34 - KEY0 + 1)
            for r in 0x4C..=0x77 {
                self.memory.io[r] = 0xFF;
            }
        }

        if self.model_has_sgb() {
            self.io_write(GB_REG_JOYP, 0xFF);
        }
        self.io_write(GB_REG_IE, 0x00);
    }

    /// GBIOWrite (address is an IO offset 0x00..=0x7F, or 0xFF for IE)
    pub fn io_write(&mut self, address: u16, value: u8) {
        let mut value = value;
        match address {
            GB_REG_SB => self.sio_write_sb(value),
            GB_REG_SC => self.sio_write_sc(value),
            GB_REG_DIV => {
                self.timer_div_reset();
                return;
            }
            GB_REG_NR10 => {
                if self.audio.enable {
                    self.audio_write_nr10(value);
                } else {
                    value = 0;
                }
            }
            GB_REG_NR11 => {
                if self.audio.enable {
                    self.audio_write_nr11(value);
                } else {
                    if self.audio.style == audio::GbAudioStyle::Dmg {
                        self.audio_write_nr11(value & REGISTER_MASK[GB_REG_NR11 as usize]);
                    }
                    value = 0;
                }
            }
            GB_REG_NR12 => {
                if self.audio.enable {
                    self.audio_write_nr12(value);
                } else {
                    value = 0;
                }
            }
            GB_REG_NR13 => {
                if self.audio.enable {
                    self.audio_write_nr13(value);
                } else {
                    value = 0;
                }
            }
            GB_REG_NR14 => {
                if self.audio.enable {
                    self.audio_write_nr14(value);
                } else {
                    value = 0;
                }
            }
            GB_REG_NR21 => {
                if self.audio.enable {
                    self.audio_write_nr21(value);
                } else {
                    if self.audio.style == audio::GbAudioStyle::Dmg {
                        self.audio_write_nr21(value & REGISTER_MASK[GB_REG_NR21 as usize]);
                    }
                    value = 0;
                }
            }
            GB_REG_NR22 => {
                if self.audio.enable {
                    self.audio_write_nr22(value);
                } else {
                    value = 0;
                }
            }
            GB_REG_NR23 => {
                if self.audio.enable {
                    self.audio_write_nr23(value);
                } else {
                    value = 0;
                }
            }
            GB_REG_NR24 => {
                if self.audio.enable {
                    self.audio_write_nr24(value);
                } else {
                    value = 0;
                }
            }
            GB_REG_NR30 => {
                if self.audio.enable {
                    self.audio_write_nr30(value);
                } else {
                    value = 0;
                }
            }
            GB_REG_NR31 => {
                if self.audio.enable || self.audio.style == audio::GbAudioStyle::Dmg {
                    self.audio_write_nr31(value);
                } else {
                    value = 0;
                }
            }
            GB_REG_NR32 => {
                if self.audio.enable {
                    self.audio_write_nr32(value);
                } else {
                    value = 0;
                }
            }
            GB_REG_NR33 => {
                if self.audio.enable {
                    self.audio_write_nr33(value);
                } else {
                    value = 0;
                }
            }
            GB_REG_NR34 => {
                if self.audio.enable {
                    self.audio_write_nr34(value);
                } else {
                    value = 0;
                }
            }
            GB_REG_NR41 => {
                if self.audio.enable || self.audio.style == audio::GbAudioStyle::Dmg {
                    self.audio_write_nr41(value);
                } else {
                    value = 0;
                }
            }
            GB_REG_NR42 => {
                if self.audio.enable {
                    self.audio_write_nr42(value);
                } else {
                    value = 0;
                }
            }
            GB_REG_NR43 => {
                if self.audio.enable {
                    self.audio_write_nr43(value);
                } else {
                    value = 0;
                }
            }
            GB_REG_NR44 => {
                if self.audio.enable {
                    self.audio_write_nr44(value);
                } else {
                    value = 0;
                }
            }
            GB_REG_NR50 => {
                if self.audio.enable {
                    self.audio_write_nr50(value);
                } else {
                    value = 0;
                }
            }
            GB_REG_NR51 => {
                if self.audio.enable {
                    self.audio_write_nr51(value);
                } else {
                    value = 0;
                }
            }
            GB_REG_NR52 => {
                self.audio_write_nr52(value);
                value &= 0x80;
                value |= self.memory.io[GB_REG_NR52 as usize] & 0x0F;
            }
            GB_REG_WAVE_0..=0x3F => {
                let now = self.current_time();
                self.audio_run(now, 0x4);
                if !self.audio.playing_ch3 {
                    self.audio.ch3.wavedata8[(address - GB_REG_WAVE_0) as usize] = value;
                } else if self.audio.ch3.readable || self.audio.style == audio::GbAudioStyle::Cgb {
                    self.audio.ch3.wavedata8[(self.audio.ch3.window >> 1) as usize] = value;
                }
            }
            GB_REG_JOYP => {
                self.memory.io[GB_REG_JOYP as usize] = value | 0x0F;
                self.read_keys();
                if self.model_has_sgb() {
                    self.write_sgb_bits((value >> 4) & 3);
                }
                return;
            }
            GB_REG_TIMA => {
                if value != 0
                    && self.until(EventId::TimerIrq) > 2 - self.double_speed as i32
                {
                    self.deschedule(EventId::TimerIrq);
                }
                if self.until(EventId::TimerIrq) == self.double_speed as i32 - 2 {
                    return;
                }
            }
            GB_REG_TMA => {
                if self.until(EventId::TimerIrq) == self.double_speed as i32 - 2 {
                    self.memory.io[GB_REG_TIMA as usize] = value;
                }
            }
            GB_REG_TAC => {
                value = self.timer_update_tac(value);
            }
            GB_REG_IF => {
                self.memory.io[GB_REG_IF as usize] = value | 0xE0;
                self.update_irqs();
                return;
            }
            GB_REG_LCDC => {
                // TODO: handle GBC differences
                self.with_timing(|gb, t| gb.video_process_dots(t, 0));
                value = self.renderer_write_video_register(address, value);
                self.video_write_lcdc(value);
            }
            GB_REG_LYC => {
                self.video_write_lyc(value);
            }
            GB_REG_DMA => {
                self.memory_dma((value as u16) << 8);
            }
            GB_REG_SCY | GB_REG_SCX | GB_REG_WY | GB_REG_WX => {
                self.with_timing(|gb, t| gb.video_process_dots(t, 0));
                value = self.renderer_write_video_register(address, value);
            }
            GB_REG_BGP | GB_REG_OBP0 | GB_REG_OBP1 => {
                self.with_timing(|gb, t| gb.video_process_dots(t, 0));
                self.video_write_palette(address, value);
            }
            GB_REG_STAT => {
                self.video_write_stat(value);
                value = self.video.stat;
            }
            GB_REG_BANK => {
                if self.memory.io[GB_REG_BANK as usize] != 0xFF {
                    // "break" out of the switch; io[BANK] = value below
                } else {
                    self.unmap_bios();
                    if self.model.is_cgb() && self.memory.io[GB_REG_KEY0 as usize] < 0x80 {
                        self.model = GbModel::Dmg;
                        self.video_disable_cgb();
                    }
                }
            }
            GB_REG_IE => {
                self.memory.ie = value;
                self.update_irqs();
                return;
            }
            _ => {
                if self.model.is_cgb() {
                    match address {
                        GB_REG_KEY0 => {}
                        GB_REG_KEY1 => {
                            value &= 0x1;
                            value |= self.memory.io[address as usize] & 0x80;
                        }
                        GB_REG_VBK => {
                            self.video_switch_bank(value);
                        }
                        GB_REG_HDMA1 | GB_REG_HDMA2 | GB_REG_HDMA3 | GB_REG_HDMA4 => {
                            // Handled transparently by the registers
                        }
                        GB_REG_HDMA5 => {
                            value = self.memory_write_hdma5(value);
                        }
                        GB_REG_BCPS => {
                            self.video.bcp_index = (value & 0x3F) as i32;
                            self.video.bcp_increment = value & 0x80 != 0;
                            self.memory.io[GB_REG_BCPD as usize] = (self.video.palette
                                [(self.video.bcp_index >> 1) as usize]
                                >> (8 * (self.video.bcp_index & 1)))
                                as u8;
                        }
                        GB_REG_BCPD => {
                            self.video_write_palette(address, value);
                            return;
                        }
                        GB_REG_OCPS => {
                            self.video.ocp_index = (value & 0x3F) as i32;
                            self.video.ocp_increment = value & 0x80 != 0;
                            self.memory.io[GB_REG_OCPD as usize] = (self.video.palette
                                [8 * 4 + (self.video.ocp_index >> 1) as usize]
                                >> (8 * (self.video.ocp_index & 1)))
                                as u8;
                        }
                        GB_REG_OCPD => {
                            self.video_write_palette(address, value);
                            return;
                        }
                        GB_REG_SVBK => {
                            self.memory_switch_wram_bank(value as i32);
                            value &= 7;
                        }
                        _ => {
                            mlog!(
                                Level::GameError,
                                rgba_core::log::GB,
                                "Writing to unknown register FF{:02X}:{:02X}",
                                address,
                                value
                            );
                            return;
                        }
                    }
                    self.memory.io[address as usize] = value;
                    return;
                }
                mlog!(
                    Level::GameError,
                    rgba_core::log::GB,
                    "Writing to unknown register FF{:02X}:{:02X}",
                    address,
                    value
                );
                return;
            }
        }
        self.memory.io[address as usize] = value;
    }

    /// _readKeys
    pub fn read_keys(&mut self) -> u8 {
        let mut keys = self.keys as u8;
        if self.sgb_current_controller != 0 {
            keys = 0;
        }
        let joyp = self.memory.io[GB_REG_JOYP as usize];
        match joyp & 0x30 {
            // The C assigns keys = gb->sgbCurrentController here (a quirk).
            0x30 => keys = self.sgb_current_controller,
            0x20 => keys >>= 4,
            0x10 => {}
            0x00 => keys |= keys >> 4,
            _ => unreachable!(),
        }
        self.memory.io[GB_REG_JOYP as usize] = (0xCF | joyp) ^ (keys & 0xF);
        if joyp & !self.memory.io[GB_REG_JOYP as usize] & 0xF != 0 {
            self.memory.io[GB_REG_IF as usize] |= 1 << GB_IRQ_KEYPAD;
            self.update_irqs();
        }
        self.memory.io[GB_REG_JOYP as usize]
    }

    fn read_keys_filtered(&mut self) -> u8 {
        let mut keys = self.read_keys();
        if !self.allow_opposing_directions && (keys & 0x30) == 0x20 {
            let rl = keys & 0x03;
            let ud = keys & 0x0C;
            if rl == 0 {
                keys |= 0x03;
            }
            if ud == 0 {
                keys |= 0x0C;
            }
        }
        keys
    }

    fn write_sgb_bits(&mut self, bits: u8) {
        let bits = bits as i32;
        if bits == 0 {
            self.sgb_bit = -1;
            self.sgb_packet = [0; 16];
        }
        if bits == self.current_sgb_bits as i32 {
            return;
        }
        if bits & 2 != 0 {
            if self.sgb_increment {
                self.sgb_increment = false;
                self.sgb_current_controller =
                    (self.sgb_current_controller + 1) & self.sgb_controllers;
            }
        } else if self.current_sgb_bits & 2 != 0 {
            self.sgb_increment = !self.sgb_increment;
        }
        self.current_sgb_bits = bits as u8;
        if self.sgb_bit == 128 && bits == 2 {
            let pkt = self.sgb_packet;
            self.video_write_sgb_packet(&pkt);
            self.sgb_bit += 1;
        }
        if self.sgb_bit >= 128 {
            return;
        }
        match bits {
            1 => {
                if self.sgb_bit < 0 {
                    return;
                }
                self.sgb_packet[(self.sgb_bit >> 3) as usize] |= 1 << (self.sgb_bit & 7);
            }
            3 => {
                self.sgb_bit += 1;
            }
            _ => {}
        }
    }

    /// GBIORead
    pub fn io_read(&mut self, address: u16) -> u8 {
        match address {
            GB_REG_JOYP => return self.read_keys_filtered(),
            GB_REG_IE => return self.memory.ie,
            GB_REG_WAVE_0..=0x3F => {
                if self.audio.playing_ch3 {
                    let now = self.current_time();
                    self.audio_run(now, 0x4);
                    if self.audio.ch3.readable || self.audio.style == audio::GbAudioStyle::Cgb {
                        return self.audio.ch3.wavedata8[(self.audio.ch3.window >> 1) as usize];
                    } else {
                        return 0xFF;
                    }
                } else {
                    return self.audio.ch3.wavedata8[(address - GB_REG_WAVE_0) as usize];
                }
            }
            GB_REG_PCM12 => {
                if !self.model.is_cgb() {
                    mlog!(
                        Level::GameError,
                        rgba_core::log::GB,
                        "Reading from CGB register FF{:02X} in DMG mode",
                        address
                    );
                } else if self.audio.enable {
                    let now = self.current_time();
                    self.audio_run(now, 0x3);
                    return (self.audio.ch1.sample & 0xF) as u8
                        | ((self.audio.ch2.sample & 0xF) as u8) << 4;
                }
            }
            GB_REG_PCM34 => {
                if !self.model.is_cgb() {
                    mlog!(
                        Level::GameError,
                        rgba_core::log::GB,
                        "Reading from CGB register FF{:02X} in DMG mode",
                        address
                    );
                } else if self.audio.enable {
                    let now = self.current_time();
                    self.audio_run(now, 0xC);
                    return (self.audio.ch3.sample & 0xF) as u8
                        | ((self.audio.ch4.sample & 0xF) as u8) << 4;
                }
            }
            GB_REG_SB | GB_REG_SC | GB_REG_IF | GB_REG_NR10 | GB_REG_NR11 | GB_REG_NR12
            | GB_REG_NR14 | GB_REG_NR21 | GB_REG_NR22 | GB_REG_NR24 | GB_REG_NR30
            | GB_REG_NR32 | GB_REG_NR34 | GB_REG_NR41 | GB_REG_NR42 | GB_REG_NR43
            | GB_REG_NR44 | GB_REG_NR50 | GB_REG_NR51 | GB_REG_NR52 | GB_REG_DIV
            | GB_REG_TIMA | GB_REG_TMA | GB_REG_TAC | GB_REG_STAT | GB_REG_LCDC
            | GB_REG_SCY | GB_REG_SCX | GB_REG_LY | GB_REG_LYC | GB_REG_DMA | GB_REG_BGP
            | GB_REG_OBP0 | GB_REG_OBP1 | GB_REG_WY | GB_REG_WX => {
                // Handled transparently by the registers
            }
            GB_REG_KEY1 | GB_REG_VBK | GB_REG_HDMA1 | GB_REG_HDMA2 | GB_REG_HDMA3
            | GB_REG_HDMA4 | GB_REG_HDMA5 | GB_REG_BCPS | GB_REG_BCPD | GB_REG_OCPS
            | GB_REG_OCPD | GB_REG_SVBK | 0x72 | 0x73 | 0x75 => {
                if !self.model.is_cgb() {
                    mlog!(
                        Level::GameError,
                        rgba_core::log::GB,
                        "Reading from CGB register FF{:02X} in DMG mode",
                        address
                    );
                }
            }
            _ => {
                mlog!(
                    Level::Stub,
                    rgba_core::log::GB,
                    "Reading from unknown register FF{:02X}",
                    address
                );
                return 0xFF;
            }
        }
        self.memory.io[address as usize] | REGISTER_MASK[address as usize]
    }
}

/// GBTestKeypadIRQ
pub fn test_keypad_irq(gb: &mut Gb) {
    gb.read_keys();
}

// GB IRQ bit numbers (enum GBIRQ lives in gb.h).
pub const GB_IRQ_VBLANK: u8 = 0;
pub const GB_IRQ_LCDSTAT: u8 = 1;
pub const GB_IRQ_TIMER: u8 = 2;
pub const GB_IRQ_SIO: u8 = 3;
pub const GB_IRQ_KEYPAD: u8 = 4;

// GBIORegisterNames (debugger)
pub const GB_IO_REGISTER_NAMES: [&str; 0x100] = build_io_register_names();

const fn build_io_register_names() -> [&'static str; 0x100] {
    let mut t: [&'static str; 0x100] = [""; 0x100];
    t[0x00] = "JOYP";
    t[0x01] = "SB";
    t[0x02] = "SC";
    t[0x04] = "DIV";
    t[0x05] = "TIMA";
    t[0x06] = "TMA";
    t[0x07] = "TAC";
    t[0x0F] = "IF";
    t[0x10] = "NR10";
    t[0x11] = "NR11";
    t[0x12] = "NR12";
    t[0x13] = "NR13";
    t[0x14] = "NR14";
    t[0x16] = "NR21";
    t[0x17] = "NR22";
    t[0x18] = "NR23";
    t[0x19] = "NR24";
    t[0x1A] = "NR30";
    t[0x1B] = "NR31";
    t[0x1C] = "NR32";
    t[0x1D] = "NR33";
    t[0x1E] = "NR34";
    t[0x20] = "NR41";
    t[0x21] = "NR42";
    t[0x22] = "NR43";
    t[0x23] = "NR44";
    t[0x24] = "NR50";
    t[0x25] = "NR51";
    t[0x26] = "NR52";
    let mut i = 0x30;
    while i < 0x40 {
        t[i] = "WAVE";
        i += 1;
    }
    t[0x40] = "LCDC";
    t[0x41] = "STAT";
    t[0x42] = "SCY";
    t[0x43] = "SCX";
    t[0x44] = "LY";
    t[0x45] = "LYC";
    t[0x46] = "DMA";
    t[0x47] = "BGP";
    t[0x48] = "OBP0";
    t[0x49] = "OBP1";
    t[0x4A] = "WY";
    t[0x4B] = "WX";
    t[0x4C] = "KEY0";
    t[0x4D] = "KEY1";
    t[0x4F] = "VBK";
    t[0x50] = "BANK";
    t[0x51] = "HDMA1";
    t[0x52] = "HDMA2";
    t[0x53] = "HDMA3";
    t[0x54] = "HDMA4";
    t[0x55] = "HDMA5";
    t[0x56] = "RP";
    t[0x68] = "BCPS";
    t[0x69] = "BCPD";
    t[0x6A] = "OCPS";
    t[0x6B] = "OCPD";
    t[0x6C] = "OPRI";
    t[0x70] = "SVBK";
    t[0x76] = "PCM12";
    t[0x77] = "PCM34";
    t
}

impl Gb {
    /// GBIODeserialize: re-run audio register writes after the io block and
    /// resync the renderer registers.
    pub fn io_deserialize(&mut self) {
        self.audio.enable = self.memory.io[GB_REG_NR52 as usize] & 0x80 != 0;
        if self.audio.enable {
            self.audio.playing_ch1 = false;
            self.io_write(GB_REG_NR10, self.memory.io[GB_REG_NR10 as usize]);
            self.io_write(GB_REG_NR11, self.memory.io[GB_REG_NR11 as usize]);
            self.io_write(GB_REG_NR12, self.memory.io[GB_REG_NR12 as usize]);
            self.io_write(GB_REG_NR13, self.memory.io[GB_REG_NR13 as usize]);
            self.audio.ch1.control.frequency &= 0xFF;
            self.audio.ch1.control.frequency |=
                ((self.memory.io[GB_REG_NR14 as usize] as i32) << 8) & 0x700;
            self.audio.ch1.control.stop = self.memory.io[GB_REG_NR14 as usize] & 0x40 != 0;
            self.audio.playing_ch2 = false;
            self.io_write(GB_REG_NR21, self.memory.io[GB_REG_NR21 as usize]);
            self.io_write(GB_REG_NR22, self.memory.io[GB_REG_NR22 as usize]);
            self.io_write(GB_REG_NR23, self.memory.io[GB_REG_NR23 as usize]);
            self.audio.ch2.control.frequency &= 0xFF;
            self.audio.ch2.control.frequency |=
                ((self.memory.io[GB_REG_NR24 as usize] as i32) << 8) & 0x700;
            self.audio.ch2.control.stop = self.memory.io[GB_REG_NR24 as usize] & 0x40 != 0;
            self.audio.playing_ch3 = false;
            self.io_write(GB_REG_NR30, self.memory.io[GB_REG_NR30 as usize]);
            self.io_write(GB_REG_NR31, self.memory.io[GB_REG_NR31 as usize]);
            self.io_write(GB_REG_NR32, self.memory.io[GB_REG_NR32 as usize]);
            self.io_write(GB_REG_NR33, self.memory.io[GB_REG_NR33 as usize]);
            self.audio.ch3.rate &= 0xFF;
            self.audio.ch3.rate |=
                ((self.memory.io[GB_REG_NR34 as usize] as i32) << 8) & 0x700;
            self.audio.ch3.stop = self.memory.io[GB_REG_NR34 as usize] & 0x40 != 0;
            self.audio.playing_ch4 = false;
            self.io_write(GB_REG_NR41, self.memory.io[GB_REG_NR41 as usize]);
            self.io_write(GB_REG_NR42, self.memory.io[GB_REG_NR42 as usize]);
            self.io_write(GB_REG_NR43, self.memory.io[GB_REG_NR43 as usize]);
            self.audio.ch4.stop = self.memory.io[GB_REG_NR44 as usize] & 0x40 != 0;
            self.io_write(GB_REG_NR50, self.memory.io[GB_REG_NR50 as usize]);
            self.io_write(GB_REG_NR51, self.memory.io[GB_REG_NR51 as usize]);
        }

        self.renderer_write_video_register(GB_REG_LCDC, self.memory.io[GB_REG_LCDC as usize]);
        self.renderer_write_video_register(GB_REG_SCY, self.memory.io[GB_REG_SCY as usize]);
        self.renderer_write_video_register(GB_REG_SCX, self.memory.io[GB_REG_SCX as usize]);
        self.renderer_write_video_register(GB_REG_WY, self.memory.io[GB_REG_WY as usize]);
        self.renderer_write_video_register(GB_REG_WX, self.memory.io[GB_REG_WX as usize]);
        if self.model == crate::gb::GbModel::Sgb {
            self.renderer_write_video_register(GB_REG_BGP, self.memory.io[GB_REG_BGP as usize]);
            self.renderer_write_video_register(GB_REG_OBP0, self.memory.io[GB_REG_OBP0 as usize]);
            self.renderer_write_video_register(GB_REG_OBP1, self.memory.io[GB_REG_OBP1 as usize]);
        }
        self.video.stat = self.memory.io[GB_REG_STAT as usize];
    }
}
