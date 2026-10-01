// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gb/gb.c and include/mgba/internal/gb/gb.h.

use rgba_core::cheats::CheatDevice;
use rgba_core::mlog;
use rgba_core::timing::Timing;
use rgba_core::Level;

use crate::audio::Audio;
use crate::cpu::{Sm83, SM83_CORE_FETCH};
use crate::io::*;
use crate::video::GB_VIDEO_TOTAL_LENGTH;
use crate::memory::{MbcType, Memory};
use crate::sio::Sio;
use crate::timer::Timer;
use crate::video::Video;

pub const CGB_SM83_FREQUENCY: u32 = 0x800000;
pub const SGB_SM83_FREQUENCY: u32 = 0x418B1E;

pub const KNOWN_HEADER: [u8; 4] = [0xCE, 0xED, 0x66, 0x66];
pub const KNOWN_HEADER_SACHEN: [u8; 4] = [0x7C, 0xE7, 0xC0, 0x00];
pub const REGISTERED_TRADEMARK: [u8; 8] = [0x3C, 0x42, 0xB9, 0xA5, 0xB9, 0xA5, 0x42, 0x3C];

pub const CGB_BIOS_HRAM: [u8; 0x7F] = [
    0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D,
    0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99,
    0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x71, 0x02, 0x4D, 0x01, 0xC1, 0xFF, 0x0D, 0x00, 0xD3, 0x05, 0xF9, 0x00, 0x00,
];

pub const DMG0_BIOS_CHECKSUM: u32 = 0xC2F5CC97;
pub const DMG_BIOS_CHECKSUM: u32 = 0x59C8598E;
pub const MGB_BIOS_CHECKSUM: u32 = 0xE6920754;
pub const SGB_BIOS_CHECKSUM: u32 = 0xEC8A83B9;
pub const SGB2_BIOS_CHECKSUM: u32 = 0x53D0DD63;
pub const CGB_BIOS_CHECKSUM: u32 = 0x41884E46;
pub const CGB0_BIOS_CHECKSUM: u32 = 0xE8EF5318;
pub const CGBE_BIOS_CHECKSUM: u32 = 0xE95DC95D;
pub const AGB_BIOS_CHECKSUM: u32 = 0xFFD6B0F1;
pub const AGB0_BIOS_CHECKSUM: u32 = 0x570337EA;
pub const FORTUNE_BIOS_CHECKSUM: u32 = 0x66CC6D94;
pub const GAMEFIGHTER_BIOS_CHECKSUM: u32 = 0x908BA8DE;
pub const KONGFENG_GBBC_BIOS_CHECKSUM: u32 = 0x69236128;
pub const MAXSTATION_BIOS_CHECKSUM: u32 = 0x783E69C2;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum GbModel {
    Autodetect = -1,
    Dmg = 0x001,
    Sgb = 0x002,
    Mgb = 0x004,
    Sgb2 = 0x020,
    Cgb = 0x100,
    Agb = 0x200 | 0x100,
    Scgb = 0x400,
}

impl GbModel {
    pub fn is_cgb(self) -> bool {
        (self as i32) >= GbModel::Cgb as i32
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum EventId {
    EiPending = 0,
    VideoMode = 1,
    VideoFrame = 2,
    AudioFrame = 3,
    AudioSample = 4,
    TimerIrq = 5,
    Timer = 6,
    Sio = 7,
    Dma = 8,
    Hdma = 9,
}

impl EventId {
    pub fn priority(self) -> u32 {
        match self {
            EventId::EiPending => 0,
            EventId::VideoMode => 8,
            EventId::VideoFrame => 9,
            EventId::AudioFrame => 0x10,
            EventId::AudioSample => 0x18,
            EventId::TimerIrq => 0x20,
            EventId::Timer => 0x21,
            EventId::Sio => 0x30,
            EventId::Dma => 0x40,
            EventId::Hdma => 0x41,
        }
    }
}

impl From<EventId> for u32 {
    fn from(e: EventId) -> u32 {
        e as u32
    }
}

impl Gb {
    /// Convenience: schedule with the event's canonical priority. This is for
    /// code OUTSIDE timing callbacks (CPU microcode, IO writes): the C reads
    /// `*timing->relativeCycles == cpu->cycles` and `*timing->nextEvent ==
    /// cpu->nextEvent` through shared pointers, so we sync both directions.
    pub fn schedule(&mut self, id: EventId, when: i32) {
        self.timing.set_relative_cycles(self.cpu.cycles);
        self.timing.schedule(id.into(), id.priority(), when);
        self.cpu.next_event = self.timing.next_event_cycles_owned();
    }
    pub fn deschedule(&mut self, id: EventId) {
        self.timing.deschedule(id.into());
    }
    pub fn is_scheduled(&self, id: EventId) -> bool {
        self.timing.is_scheduled(id.into())
    }
    /// mTimingUntil (reads relativeCycles).
    pub fn until(&mut self, id: EventId) -> i32 {
        self.timing.set_relative_cycles(self.cpu.cycles);
        self.timing.until(id.into())
    }

    /// mTimingCurrentTime(): the C reads cpu->cycles through the shared
    /// relativeCycles pointer, so use the live value here.
    pub fn current_time(&self) -> i32 {
        self.timing.master_cycles as i32 + self.cpu.cycles
    }

    /// Wall-clock time for RTC-backed carts (mRTCSource::unixTime). The
    /// frontend can swap `self.rtc_time_fn`.
    pub fn unix_time(&self) -> i64 {
        (self.rtc_time_fn)()
    }
}

fn default_rtc_time() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Clone, Copy, Default)]
pub struct GbxMetadata {
    pub mbc: MbcType,
    pub battery: bool,
    pub rumble: bool,
    pub timer: bool,
    pub rom_size: u32,
    pub ram_size: u32,
    pub mapper_vars: [u8; 32],
}

pub struct Gb {
    pub cpu: Sm83,
    pub memory: Memory,
    pub video: Video,
    pub timer: Timer,
    pub audio: Audio,
    pub sio: Sio,
    pub model: GbModel,
    pub gbx: GbxMetadata,

    pub timing: Timing,

    pub is_pristine: bool,
    pub pristine_rom_size: usize,
    pub yanked_rom_size: usize,
    pub yanked_mbc: MbcType,
    pub rom_crc32: u32,
    pub sram_size: usize,
    pub sram_dirty: i32,
    pub sram_dirt_age: u32,

    pub sgb_bit: i32,
    pub current_sgb_bits: u8,
    pub sgb_packet: [u8; 16],
    pub sgb_controllers: u8,
    pub sgb_current_controller: u8,
    pub sgb_increment: bool,

    pub cpu_blocked: bool,
    pub early_exit: bool,
    pub double_speed: bool,

    /// The cheat device (mCheatDevice hung off cpu->components in the C).
    /// Refreshed once per frame from `frame_ended`.
    pub cheats: CheatDevice,

    /// The debugger (mDebugger + SM83Debugger platform, hung off
    /// cpu->components in the C).
    pub debugger: Option<Box<crate::debugger::GbDebugger>>,
    /// Memory-shim installed flag (C: SM83/ARM memory shim function pointers).
    pub dbg_watchpoints_active: bool,

    /// 10-button state in mCore key order (1 = pressed).
    pub keys: u32,
    pub allow_opposing_directions: bool,

    /// mRTCSource::unixTime; swap out for deterministic time in tests.
    pub rtc_time_fn: fn() -> i64,
}

impl Gb {
    pub fn new() -> Box<Gb> {
        Box::new(Gb {
            cpu: Sm83::new(),
            memory: Memory::new(),
            video: *Video::new(),
            timer: Timer::new(),
            audio: Audio::new(),
            sio: Sio::new(),
            model: GbModel::Autodetect,
            gbx: GbxMetadata::default(),
            timing: Timing::new(),
            is_pristine: false,
            pristine_rom_size: 0,
            yanked_rom_size: 0,
            yanked_mbc: MbcType::Autodetect,
            rom_crc32: 0,
            sram_size: 0,
            sram_dirty: 0,
            sram_dirt_age: 0,
            sgb_bit: -1,
            current_sgb_bits: 0,
            sgb_packet: [0; 16],
            sgb_controllers: 0,
            sgb_current_controller: 0,
            sgb_increment: false,
            cpu_blocked: false,
            early_exit: false,
            double_speed: false,
            cheats: CheatDevice::new(),
            debugger: None,
            dbg_watchpoints_active: false,
            keys: 0,
            allow_opposing_directions: false,
            rtc_time_fn: default_rtc_time,
        })
    }

    /// GBIsROM
    pub fn is_rom(rom: &[u8]) -> bool {
        if rom.len() < 0x200 {
            return false;
        }
        let header = &rom[0x100..0x200];
        if header[4..8] == KNOWN_HEADER {
            return true;
        }
        if header[4..8] == KNOWN_HEADER_SACHEN {
            return true;
        }
        // Sachen scrambled headers
        if header[0x04] == KNOWN_HEADER[0]
            && header[0x44] == KNOWN_HEADER[1]
            && header[0x14] == KNOWN_HEADER[2]
            && header[0x54] == KNOWN_HEADER[3]
        {
            return true;
        }
        if header[0x04] == KNOWN_HEADER_SACHEN[0]
            && header[0x44] == KNOWN_HEADER_SACHEN[1]
            && header[0x14] == KNOWN_HEADER_SACHEN[2]
            && header[0x54] == KNOWN_HEADER_SACHEN[3]
        {
            return true;
        }
        // GBX footer
        if rom.len() >= 0x40 {
            let footer = &rom[rom.len() - 16..];
            let size = u32::from_be_bytes([footer[0], footer[1], footer[2], footer[3]]);
            let vers = u32::from_be_bytes([footer[4], footer[5], footer[6], footer[7]]);
            if &footer[12..16] == b"GBX!" && size == 0x40 && vers == 1 {
                return true;
            }
        }
        false
    }

    /// GBLoadGBX (slice variant)
    pub fn load_gbx(&mut self, rom: &[u8]) -> bool {
        if rom.len() < 0x40 + 16 {
            return false;
        }
        let footer = &rom[rom.len() - 16..];
        let gbx_size = u32::from_be_bytes([footer[0], footer[1], footer[2], footer[3]]);
        let vers = u32::from_be_bytes([footer[4], footer[5], footer[6], footer[7]]);
        if &footer[12..16] != b"GBX!" || gbx_size != 0x40 || vers != 1 {
            return false;
        }
        let block = &rom[rom.len() - 0x40..rom.len() - 0x40 + 16]; // first 16 bytes of the 0x40 block
        self.gbx = GbxMetadata::default();
        self.gbx.mbc = crate::mbc::mbc_from_gbx(block);
        let flags = &rom[rom.len() - 0x40..];
        if flags[4] == 1 {
            self.gbx.battery = true;
        }
        if flags[5] == 1 {
            self.gbx.rumble = true;
            if self.gbx.mbc == MbcType::Mbc5 {
                self.gbx.mbc = MbcType::Mbc5Rumble;
            }
        }
        if flags[6] == 1 {
            self.gbx.timer = true;
            if self.gbx.mbc == MbcType::Mbc3 {
                self.gbx.mbc = MbcType::Mbc3Rtc;
            }
        }
        self.gbx.rom_size = u32::from_be_bytes([flags[8], flags[9], flags[10], flags[11]]);
        self.gbx.ram_size = u32::from_be_bytes([flags[12], flags[13], flags[14], flags[15]]);
        self.gbx
            .mapper_vars
            .copy_from_slice(&flags[16..48]);
        if &flags[0..4] == b"MBC1" {
            self.gbx.mapper_vars[0] = 5;
        } else if &flags[0..4] == b"MB1M" {
            self.gbx.mapper_vars[0] = 4;
        }
        true
    }

    /// GBLoadROM with an owned ROM image.
    pub fn load_rom(&mut self, rom: Vec<u8>) -> bool {
        self.unload_rom();
        let file_size = rom.len();

        if !self.load_gbx(&rom) {
            self.pristine_rom_size = file_size;
        } else {
            if (self.gbx.rom_size as usize) <= file_size.saturating_sub(0x40) {
                self.pristine_rom_size = self.gbx.rom_size as usize;
            } else {
                mlog!(
                    Level::Warn,
                    rgba_core::log::GB,
                    "GBX file size {} is larger than real file size {}",
                    self.gbx.rom_size,
                    file_size - 0x40
                );
                self.pristine_rom_size = file_size - 0x40;
            }
        }

        if self.pristine_rom_size > crate::memory::GB_SIZE_CART_MAX {
            self.pristine_rom_size = crate::memory::GB_SIZE_CART_MAX;
        }
        let mut rom = rom;
        rom.truncate(self.pristine_rom_size);
        if rom.len() < crate::memory::GB_SIZE_CART_BANK0 {
            self.is_pristine = false;
            self.memory.rom = vec![0xFF; crate::memory::GB_SIZE_CART_MAX];
            self.memory.rom_size = crate::memory::GB_SIZE_CART_BANK0;
            self.memory.rom[..rom.len()].copy_from_slice(&rom);
        } else {
            self.is_pristine = true;
            self.memory.rom_size = rom.len();
            self.memory.rom = rom;
        }
        self.yanked_rom_size = 0;
        self.rom_crc32 = crate::crc32(&self.memory.rom[..self.memory.rom_size]);
        self.mbc_reset();

        if !self.memory.rom_base_is_mapped() {
            self.mbc_switch_bank0(0);
        }
        self.set_active_region(self.cpu.pc);
        true
    }

    /// GBYankROM
    pub fn yank_rom(&mut self) {
        self.yanked_rom_size = self.memory.rom_size;
        self.yanked_mbc = self.memory.mbc_type;
        self.memory.rom_size = 0;
        self.memory.mbc_type = MbcType::None;
        self.mbc_reset();
        self.set_active_region(self.cpu.pc);
    }

    pub fn unload_rom(&mut self) {
        // Owned buffers; just drop everything.
        self.memory.rom = Vec::new();
        self.memory.rom_size = 0;
        self.memory.mbc_type = MbcType::Autodetect;
        self.is_pristine = false;
        self.pristine_rom_size = 0;
        self.sram_size = 0;
    }

    /// GBReset (the irqh.reset hook)
    pub fn irq_reset(&mut self) {
        self.gb_reset_impl();
    }

    fn gb_reset_impl(&mut self) {
        self.memory.rom_base_reset();
        self.detect_model();

        self.cpu.b = 0;
        self.cpu.d = 0;

        self.timer.internal_div = 0;

        self.cpu_blocked = false;
        self.early_exit = false;
        self.double_speed = false;

        if self.yanked_rom_size != 0 {
            self.memory.rom_size = self.yanked_rom_size;
            self.memory.mbc_type = self.yanked_mbc;
            self.yanked_rom_size = 0;
        }

        self.sgb_bit = -1;
        self.sgb_controllers = 0;
        self.sgb_current_controller = 0;
        self.current_sgb_bits = 0;
        self.sgb_increment = false;
        self.sgb_packet = [0; 16];

        self.timing.clear();

        self.memory_reset();

        if let Some(bios) = self.memory.bios.clone() {
            if Gb::is_compatible_bios(&bios, self.model) {
                self.map_bios(&bios);
                self.cpu.a = 0;
                self.cpu.f.packed = 0;
                self.cpu.c = 0;
                self.cpu.e = 0;
                self.cpu.h = 0;
                self.cpu.l = 0;
                self.cpu.sp = 0;
                self.cpu.pc = 0;
            } else {
                self.memory.bios = None;
            }
        }

        match self.model {
            GbModel::Dmg | GbModel::Sgb | GbModel::Autodetect => {
                self.audio.style = crate::audio::GbAudioStyle::Dmg;
            }
            GbModel::Mgb | GbModel::Sgb2 => {
                self.audio.style = crate::audio::GbAudioStyle::Mgb;
            }
            GbModel::Agb | GbModel::Cgb | GbModel::Scgb => {
                self.audio.style = crate::audio::GbAudioStyle::Cgb;
            }
            _ => {
                self.audio.style = crate::audio::GbAudioStyle::Dmg;
            }
        }

        self.video_reset();
        self.timer_reset();
        self.io_reset();
        self.audio_reset();
        if self.memory.bios_mapped.is_none() && !self.memory.rom.is_empty() {
            self.skip_bios();
        } else {
            self.schedule(EventId::Timer, 0);
        }

        self.sio_reset();

        self.set_active_region(self.cpu.pc);
    }

    /// GBSkipBIOS
    pub fn skip_bios(&mut self) {
        let mut next_div = 0;
        let cgb_flag = self.cart_cgb_flag();

        match self.model {
            GbModel::Autodetect => {
                self.model = GbModel::Dmg;
                self.skip_bios_dmg_regs(&mut next_div);
            }
            GbModel::Dmg => self.skip_bios_dmg_regs(&mut next_div),
            GbModel::Sgb => {
                self.cpu.a = 1;
                self.cpu.f.packed = 0x00;
                self.cpu.c = 0x14;
                self.cpu.e = 0x00;
                self.cpu.h = 0xC0;
                self.cpu.l = 0x60;
                self.timer.internal_div = 0xD85;
                next_div = 8;
            }
            GbModel::Mgb => {
                self.cpu.a = 0xFF;
                self.cpu.f.packed = 0xB0;
                self.cpu.c = 0x13;
                self.cpu.e = 0xD8;
                self.cpu.h = 1;
                self.cpu.l = 0x4D;
                self.timer.internal_div = 0xABC;
                next_div = 4;
            }
            GbModel::Sgb2 => {
                self.cpu.a = 0xFF;
                self.cpu.f.packed = 0x00;
                self.cpu.c = 0x14;
                self.cpu.e = 0x00;
                self.cpu.h = 0xC0;
                self.cpu.l = 0x60;
                self.timer.internal_div = 0xD84;
                next_div = 8;
            }
            GbModel::Agb | GbModel::Cgb | GbModel::Scgb => {
                if self.model == GbModel::Agb {
                    self.cpu.b = 1;
                }
                self.cpu.a = 0x11;
                if self.model == GbModel::Agb {
                    self.cpu.f.packed = 0x00;
                } else {
                    self.cpu.f.packed = 0x80;
                }
                self.cpu.c = 0;
                self.cpu.h = 0;
                if cgb_flag & 0x80 != 0 {
                    self.cpu.d = 0xFF;
                    self.cpu.e = 0x56;
                    self.cpu.l = 0x0D;
                    self.timer.internal_div = 0x2F0;
                } else {
                    self.cpu.e = 0x08;
                    self.cpu.l = 0x7C;
                    self.timer.internal_div = 0x260;
                    self.model = GbModel::Dmg;
                    self.memory.io[GB_REG_KEY1 as usize] = 0xFF;
                    self.memory.io[GB_REG_BCPS as usize] = 0x88; // Faked writing 4 BG palette entries
                    self.memory.io[GB_REG_OCPS as usize] = 0x90; // Faked writing 8 OBJ palette entries
                    self.memory.io[GB_REG_SVBK as usize] = 0xFF;
                    self.video_disable_cgb();
                }
                self.memory.hram[..0x7F].copy_from_slice(&CGB_BIOS_HRAM);
                next_div = 0xC;
            }
        }

        // VRAM logo scribble (matches the C exactly).
        for i in 0..48u16 {
            let byte = self.load8(0x104 + i);

            let mut output0 = 0u8;
            let mut output1 = 0u8;

            output0 |= byte & 0x80;
            output0 |= (byte & 0x40) >> 1;
            output0 |= (byte & 0x20) >> 2;
            output0 |= (byte & 0x10) >> 3;
            output0 |= output0 >> 1;

            output1 |= (byte & 0x08) << 3;
            output1 |= (byte & 0x04) << 2;
            output1 |= (byte & 0x02) << 1;
            output1 |= byte & 0x01;
            output1 |= output1 << 1;

            self.patch8(0x8010 + i * 8, output0, None, -1);
            self.patch8(0x8012 + i * 8, output0, None, -1);
            self.patch8(0x8014 + i * 8, output1, None, -1);
            self.patch8(0x8016 + i * 8, output1, None, -1);
        }
        for i in 0..8u16 {
            self.patch8(0x8190 + i * 2, REGISTERED_TRADEMARK[i as usize], None, -1);
        }
        if !self.model.is_cgb() {
            for i in 0..12u16 {
                self.patch8(0x9904 + i, (i + 1) as u8, None, -1);
                self.patch8(0x9924 + i, (i + 13) as u8, None, -1);
            }
            self.patch8(0x9910, 0x19, None, -1);
        }

        if self.memory.mbc_type == MbcType::UnlSachenMmc2 {
            self.memory.mbc_state.sachen.locked = crate::mbc::GB_SACHEN_UNLOCKED;
        }

        self.cpu.sp = 0xFFFE;
        self.cpu.pc = 0x100;

        self.timer.next_div = crate::timer::GB_DMG_DIV_PERIOD as i32 * (16 - next_div);

        self.deschedule(EventId::Timer);
        let next = self.timer.next_div;
        self.schedule(EventId::Timer, next);

        if self.memory.bios_mapped.is_some() {
            self.unmap_bios();
        }

        self.io_write(GB_REG_NR52 as u16, 0xF1);
        self.io_write(GB_REG_NR14 as u16, 0x3F);
        self.io_write(GB_REG_NR10 as u16, 0x80);
        self.io_write(GB_REG_NR11 as u16, 0xBF);
        self.io_write(GB_REG_NR12 as u16, 0xF3);
        self.io_write(GB_REG_NR13 as u16, 0xF3);
        self.io_write(GB_REG_NR24 as u16, 0x3F);
        self.io_write(GB_REG_NR21 as u16, 0x3F);
        self.io_write(GB_REG_NR22 as u16, 0x00);
        self.io_write(GB_REG_NR34 as u16, 0x3F);
        self.io_write(GB_REG_NR30 as u16, 0x7F);
        self.io_write(GB_REG_NR31 as u16, 0xFF);
        self.io_write(GB_REG_NR32 as u16, 0x9F);
        self.io_write(GB_REG_NR44 as u16, 0x3F);
        self.io_write(GB_REG_NR41 as u16, 0xFF);
        self.io_write(GB_REG_NR42 as u16, 0x00);
        self.io_write(GB_REG_NR43 as u16, 0x00);
        self.io_write(GB_REG_NR50 as u16, 0x77);
        self.io_write(GB_REG_NR51 as u16, 0xF3);
        self.io_write(GB_REG_LCDC as u16, 0x91);
        self.memory.io[GB_REG_BANK as usize] = 0x1;
        self.video_skip_bios();
    }

    fn skip_bios_dmg_regs(&mut self, next_div: &mut i32) {
        self.cpu.a = 1;
        self.cpu.f.packed = 0xB0;
        self.cpu.c = 0x13;
        self.cpu.e = 0xD8;
        self.cpu.h = 1;
        self.cpu.l = 0x4D;
        self.timer.internal_div = 0xABC;
        *next_div = 4;
    }

    /// GBMapBIOS: copy BIOS into romBase buffer. We model romBase as an enum
    /// on Memory; mapping materializes a padded bios buffer.
    pub fn map_bios(&mut self, bios: &[u8]) {
        let mut buf = vec![0u8; crate::memory::GB_SIZE_CART_BANK0];
        let n = bios.len().min(buf.len());
        buf[..n].copy_from_slice(&bios[..n]);
        if !self.memory.rom.is_empty() {
            let copy_len = buf.len().saturating_sub(n);
            let src = &self.memory.rom[n..n + copy_len.min(self.memory.rom.len() - n)];
            buf[n..n + src.len()].copy_from_slice(src);
            if bios.len() > 0x100 {
                let hdr = 0x100..n.min(0x200).max(0x100);
                if hdr.end > hdr.start {
                    buf[hdr.clone()].copy_from_slice(&self.memory.rom[hdr]);
                }
            }
        }
        self.memory.bios_mapped = Some(buf);
    }

    /// GBUnmapBIOS
    pub fn unmap_bios(&mut self) {
        if self.memory.io[GB_REG_BANK as usize] == 0xFF && self.memory.bios_mapped.is_some() {
            self.memory.bios_mapped = None;
            if self.memory.mbc_type == MbcType::Mmm01 {
                let banks = self.memory.rom_size / crate::memory::GB_SIZE_CART_BANK0;
                self.mbc_switch_bank0(banks.wrapping_sub(2) as i32);
            } else {
                self.mbc_switch_bank0(0);
            }
        }
        // XXX: Force AGB registers for AGB-mode
        if self.model == GbModel::Agb && self.cpu.pc == 0x100 {
            self.cpu.b = 1;
        }
    }

    /// GBDetectModel
    pub fn detect_model(&mut self) {
        if self.model != GbModel::Autodetect {
            return;
        }
        if let Some(bios) = self.memory.bios.clone() {
            match crate::crc32(&bios) {
                DMG_BIOS_CHECKSUM | DMG0_BIOS_CHECKSUM | FORTUNE_BIOS_CHECKSUM
                | GAMEFIGHTER_BIOS_CHECKSUM | MAXSTATION_BIOS_CHECKSUM => {
                    self.model = GbModel::Dmg;
                }
                MGB_BIOS_CHECKSUM => self.model = GbModel::Mgb,
                SGB_BIOS_CHECKSUM => self.model = GbModel::Sgb,
                SGB2_BIOS_CHECKSUM => self.model = GbModel::Sgb2,
                CGB_BIOS_CHECKSUM | CGB0_BIOS_CHECKSUM | CGBE_BIOS_CHECKSUM
                | KONGFENG_GBBC_BIOS_CHECKSUM => self.model = GbModel::Cgb,
                AGB_BIOS_CHECKSUM | AGB0_BIOS_CHECKSUM => self.model = GbModel::Agb,
                _ => {
                    self.memory.bios = None;
                }
            }
        }
        if self.model == GbModel::Autodetect && !self.memory.rom.is_empty() {
            let cart = self.cart_offset144(); // 0x100-based header views
            let cgb = self.memory.rom[cart + 0x43];
            let sgb = self.memory.rom[cart + 0x46];
            let old_licensee = self.memory.rom[cart + 0x4B];
            if cgb & 0x80 != 0 {
                self.model = GbModel::Cgb;
            } else if sgb == 0x03 && old_licensee == 0x33 {
                self.model = GbModel::Sgb;
            } else {
                self.model = GbModel::Dmg;
            }
        }
    }

    #[inline]
    fn cart_offset144(&self) -> usize {
        // GBCartridge starts at 0x100
        0x100
    }

    #[inline]
    fn cart_cgb_flag(&self) -> u8 {
        if self.memory.rom.len() > 0x143 {
            self.memory.rom[0x143]
        } else {
            0
        }
    }

    pub fn is_compatible_bios(bios: &[u8], model: GbModel) -> bool {
        match crate::crc32(bios) {
            DMG_BIOS_CHECKSUM | DMG0_BIOS_CHECKSUM | MGB_BIOS_CHECKSUM | SGB_BIOS_CHECKSUM
            | SGB2_BIOS_CHECKSUM | FORTUNE_BIOS_CHECKSUM | GAMEFIGHTER_BIOS_CHECKSUM
            | MAXSTATION_BIOS_CHECKSUM => !model.is_cgb(),
            CGB_BIOS_CHECKSUM | CGB0_BIOS_CHECKSUM | CGBE_BIOS_CHECKSUM | AGB_BIOS_CHECKSUM
            | AGB0_BIOS_CHECKSUM | KONGFENG_GBBC_BIOS_CHECKSUM => model.is_cgb(),
            _ => false,
        }
    }

    /// GBUpdateIRQs
    pub fn update_irqs(&mut self) {
        let irqs = self.memory.ie & self.memory.io[GB_REG_IF as usize] & 0x1F;
        if irqs == 0 {
            self.cpu.irq_pending = false;
            return;
        }
        self.cpu.halted = false;
        if !self.memory.ime {
            self.cpu.irq_pending = false;
            return;
        }
        if self.cpu.irq_pending {
            return;
        }
        self.sm83_raise_irq();
    }

    fn advance_cycles(&mut self) {
        let state_mask = (4 * (2 - self.double_speed as i32)) as i32 - 1;
        let state_offset =
            ((self.cpu.next_event - self.cpu.cycles) & state_mask) >> (!self.double_speed) as i32;
        self.cpu.cycles = self.cpu.next_event;
        self.cpu.execution_state = (self.cpu.execution_state + state_offset) & 3;
    }

    /// GBProcessEvents; called from the SM83 run loop when the cycle budget
    /// expires. Dispatches timing events.
    pub fn process_events(&mut self) {
        let was_halted = self.cpu.halted;
        loop {
            loop {
                let cycles = self.cpu.cycles;

                self.cpu.cycles = 0;
                self.cpu.next_event = i32::MAX;

                let mut timing = std::mem::take(&mut self.timing);
                // During callbacks the C reads cpu->cycles (== 0 now) as
                // relativeCycles.
                timing.set_relative_cycles(0);
                timing.set_next_event(i32::MAX);
                let mut next_event = cycles;
                loop {
                    next_event = timing.tick(next_event, self, &mut |gb: &mut Gb, timing, id, late| {
                        gb.process_event(timing, id, late);
                    });
                    if !self.cpu_blocked {
                        break;
                    }
                }
                self.timing = timing;
                // This loop cannot early exit until the SM83 run loop properly
                // handles mid-M-cycle-exits
                self.cpu.next_event = next_event;

                if self.cpu.halted {
                    self.advance_cycles();
                    if self.memory.ie == 0 || !self.memory.ime {
                        break;
                    }
                }
                if self.early_exit {
                    break;
                }
                if self.cpu.cycles < self.cpu.next_event {
                    break;
                }
            }
            if self.cpu_blocked {
                self.advance_cycles();
            }
            if !was_halted || (self.cpu.execution_state & 3) == SM83_CORE_FETCH {
                break;
            }
            let next_fetch = (SM83_CORE_FETCH - self.cpu.execution_state) * self.cpu.t_multiplier;
            if next_fetch < self.cpu.next_event {
                self.cpu.cycles += next_fetch;
                self.cpu.execution_state = SM83_CORE_FETCH;
                break;
            }
            self.advance_cycles();
        }
        self.early_exit = false;
    }

    /// Dispatch for timing events (replaces the C callback function pointers).
    /// `timing` is the live scheduler (inside Timing::tick).
    pub fn process_event(&mut self, timing: &mut Timing, id: u32, cycles_late: i32) {
        match id {
            x if x == EventId::EiPending as u32 => {
                self.memory.ime = true;
                self.update_irqs();
            }
            x if x == EventId::VideoMode as u32 => self.video_mode_event(timing, cycles_late as u32),
            x if x == EventId::VideoFrame as u32 => self.video_frame_ended_event(timing),
            x if x == EventId::AudioFrame as u32 => self.audio_frame_event(timing, cycles_late),
            x if x == EventId::AudioSample as u32 => self.audio_sample_event(timing, cycles_late),
            x if x == EventId::TimerIrq as u32 => self.timer_irq_event(timing, cycles_late),
            x if x == EventId::Timer as u32 => self.timer_event(timing, cycles_late),
            x if x == EventId::Sio as u32 => self.sio_event(timing, cycles_late as u32),
            x if x == EventId::Dma as u32 => self.memory_dma_service(timing, cycles_late as u32),
            x if x == EventId::Hdma as u32 => self.memory_hdma_service(timing, cycles_late as u32),
            _ => unreachable!(),
        }
    }

    /// GBSetInterrupts
    pub fn gb_set_interrupts(&mut self, enable: bool) {
        self.deschedule(EventId::EiPending);
        if !enable {
            self.memory.ime = false;
            self.update_irqs();
        } else {
            self.schedule(EventId::EiPending, 4 * self.cpu.t_multiplier);
        }
    }

    /// GBIRQVector
    pub fn irq_vector(&mut self) -> u16 {
        let irqs = self.memory.ie & self.memory.io[GB_REG_IF as usize];
        let iff = GB_REG_IF as usize;
        if irqs & (1 << GB_IRQ_VBLANK) != 0 {
            self.memory.io[iff] &= !(1 << GB_IRQ_VBLANK);
            return 0x40;
        }
        if irqs & (1 << GB_IRQ_LCDSTAT) != 0 {
            self.memory.io[iff] &= !(1 << GB_IRQ_LCDSTAT);
            return 0x48;
        }
        if irqs & (1 << GB_IRQ_TIMER) != 0 {
            self.memory.io[iff] &= !(1 << GB_IRQ_TIMER);
            return 0x50;
        }
        if irqs & (1 << GB_IRQ_SIO) != 0 {
            self.memory.io[iff] &= !(1 << GB_IRQ_SIO);
            return 0x58;
        }
        if irqs & (1 << GB_IRQ_KEYPAD) != 0 {
            self.memory.io[iff] &= !(1 << GB_IRQ_KEYPAD);
            return 0x60;
        }
        0
    }

    /// GBHalt
    pub fn gb_halt(&mut self) {
        if self.memory.ie & self.memory.io[GB_REG_IF as usize] & 0x1F == 0 {
            self.advance_cycles();
            self.cpu.execution_state = (self.cpu.execution_state - 1) & 3;
            self.cpu.halted = true;
        } else if !self.memory.ime {
            mlog!(Level::GameError, rgba_core::log::GB, "HALT bug");
            self.cpu.execution_state = crate::cpu::SM83_CORE_HALT_BUG;
        }
    }

    /// GBStop
    pub fn gb_stop(&mut self) {
        if self.model.is_cgb() && self.memory.io[GB_REG_KEY1 as usize] & 1 != 0 {
            self.double_speed = !self.double_speed;
            self.cpu.t_multiplier = 2 - self.double_speed as i32;
            self.memory.io[GB_REG_KEY1 as usize] = 0;
            self.memory.io[GB_REG_KEY1 as usize] |= (self.double_speed as u8) << 7;
        } else {
            // No sleep callback in this port; just log.
            let sleep = !(self.memory.io[GB_REG_JOYP as usize] & 0x30);
            let _ = sleep;
            mlog!(Level::Info, rgba_core::log::GB, "STOP executed");
        }
    }

    /// GBIllegal
    pub fn gb_hit_illegal(&mut self) {
        mlog!(
            Level::GameError,
            rgba_core::log::GB,
            "Hit illegal opcode at address {:04X}:{:02X}",
            self.cpu.pc,
            self.cpu.bus
        );
        self.memory.ime = false;
        self.cpu.pc = self.cpu.pc.wrapping_sub(1);
    }

    /// GBInterrupt
    pub fn gb_interrupt(&mut self) {
        self.early_exit = true;
        self.timing.interrupt();
    }

    /// GBFrameStarted
    pub fn frame_started(&mut self) {
        self.test_keypad_irq();
    }

    /// GBFrameEnded
    pub fn frame_ended(&mut self) {
        self.sram_clean();
        self.cheat_apply();
    }

    /// GBTestKeypadIRQ (from io.c, kept here like the C exposes it on GB)
    pub fn test_keypad_irq(&mut self) {
        crate::io::test_keypad_irq(self);
    }

    fn sram_clean(&mut self) {
        crate::memory::sram_clean(self);
    }

    /// Call `f` with the timing moved out, for subsystem code that runs
    /// OUTSIDE Timing::tick (IO-write handlers etc.) but must schedule events
    /// exactly like the C does with the live struct. After f, next_event is
    /// synced back into the CPU, as the C's shared pointer does.
    pub fn with_timing<R>(&mut self, f: impl FnOnce(&mut Self, &mut Timing) -> R) -> R {
        let mut timing = std::mem::take(&mut self.timing);
        timing.set_relative_cycles(self.cpu.cycles);
        let r = f(self, &mut timing);
        self.timing = timing;
        self.cpu.next_event = self.timing.next_event_cycles_owned();
        r
    }

    /// _GBCoreRunFrame
    pub fn run_frame(&mut self) {
        let frame_counter = self.video.frame_counter;
        while self.video.frame_counter == frame_counter {
            self.sm83_run();
        }
    }

    /// _GBCoreStep (run one instruction, for the debugger).
    pub fn step(&mut self) {
        self.sm83_tick();
    }
}

impl Default for Box<Gb> {
    fn default() -> Self {
        Gb::new()
    }
}

impl Default for Sm83 {
    fn default() -> Self {
        Sm83::new()
    }
}

/// Decoded cartridge title etc. (GBGetGameInfo), for the frontend.
pub struct GameInfo {
    pub system: &'static str,
    pub title: String,
    pub version: u8,
}

impl Gb {
    pub fn game_info(&self) -> Option<GameInfo> {
        if self.memory.rom.len() < 0x150 {
            return None;
        }
        let cart = &self.memory.rom[0x100..];
        let system = if cart[0x43] == 0xC0 { "CGB" } else { "DMG" };
        let title_bytes = if cart[0x4B] != 0x33 {
            &cart[0x34..0x44]
        } else {
            &cart[0x34..0x3F]
        };
        let end = title_bytes
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(title_bytes.len());
        let title = String::from_utf8_lossy(&title_bytes[..end]).into_owned();
        Some(GameInfo {
            system,
            title,
            version: cart[0x4C],
        })
    }
}

// re-export for isa table paths
pub use crate::cpu::isa::INSTRUCTION_TABLE as _INSTRUCTION_TABLE_DOCTEST;

// ---------------------------------------------------------------------------
// rgba_core::Core impl

use rgba_core::core::{Core, CoreInfo, Platform};
use rgba_core::ring::RingI16;

impl Core for Gb {
    fn platform(&self) -> Platform {
        Platform::Gb
    }
    fn info(&self) -> CoreInfo {
        CoreInfo {
            platform: Platform::Gb,
            width: 160,
            height: 144,
            frequency: self.frequency(),
            frame_cycles: self.frame_cycles(),
        }
    }
    fn is_rom(rom: &[u8]) -> bool {
        Gb::is_rom(rom)
    }
    fn reset(&mut self) {
        self.sm83_reset();
    }
    fn unload_rom(&mut self) {
        Gb::unload_rom(self);
    }
    fn run_frame(&mut self) {
        Gb::run_frame(self);
    }
    fn step(&mut self) {
        Gb::step(self);
    }
    fn set_keys(&mut self, keys: u32) {
        self.keys = keys;
        self.test_keypad_irq();
    }
    fn keys(&self) -> u32 {
        self.keys
    }
    fn video_buffer(&self) -> &[u32] {
        &self.video.output
    }
    fn base_video_size(&self) -> (u32, u32) {
        (self.video_framebuffer_size().0 as u32, self.video_framebuffer_size().1 as u32)
    }
    fn audio_buffer(&mut self) -> &mut RingI16 {
        &mut self.audio.buffer
    }
    fn audio_sample_rate(&self) -> i32 {
        131072 // 2 * 65536 — the GB sample rate in mGBA's default config
    }
    fn set_audio_buffer_size(&mut self, _samples: usize) {}
    fn debugger_attach(&mut self) -> bool {
        self.debugger_attach();
        true
    }

    fn debugger_run_frame(&mut self) {
        // Inherent method (impl Gb in crate::debugger) — the C's
        // mCoreThread loop calling mDebuggerRunFrame.
        self.debugger_run_frame();
    }

    fn debugger_break(&mut self) {
        use rgba_debugger::debugger::{DebuggerEntryInfo, DebuggerEntryReason};
        self.debugger_enter(DebuggerEntryReason::Manual, DebuggerEntryInfo::default());
    }

    fn debugger_attached(&self) -> bool {
        self.debugger.is_some()
    }

    fn frame_counter(&self) -> u32 {
        self.video.frame_counter
    }
    fn frame_cycles(&self) -> i32 {
        GB_VIDEO_TOTAL_LENGTH * if self.model_is_cgb_fast() { 2 } else { 1 }
    }
    fn frequency(&self) -> i32 {
        if self.model_is_cgb_fast() {
            0x800000
        } else {
            0x400000
        }
    }
    fn state_size(&mut self) -> usize {
        crate::serialize::GB_SAVESTATE_SIZE
    }
    fn save_state(&mut self, out: &mut Vec<u8>) -> Result<(), &'static str> {
        let v = crate::serialize::serialize(self)?;
        *out = v;
        Ok(())
    }
    fn load_state(&mut self, state: &[u8]) -> Result<(), &'static str> {
        crate::serialize::deserialize(self, state)
    }
    fn savedata(&self) -> Option<&[u8]> {
        if self.memory.sram.is_empty() {
            None
        } else {
            self.memory.sram_save_bytes()
        }
    }
    fn savedata_mut(&mut self) -> Option<&mut [u8]> {
        if self.memory.sram.is_empty() {
            None
        } else {
            self.memory.sram_save_bytes_mut()
        }
    }
    fn synchronize_savedata(&mut self) {
        crate::memory::sram_clean(self);
    }
}

impl Gb {
    fn model_is_cgb_fast(&self) -> bool {
        self.model.is_cgb()
    }
}
