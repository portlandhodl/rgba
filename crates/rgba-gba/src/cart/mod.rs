// Copyright (c) 2013-2021 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gba/cart/gpio.c.

pub mod ereader;
pub mod matrix;
pub mod unlicensed;

pub use ereader::EReader;
pub use matrix::Matrix;
pub use unlicensed::{UnlCart, UnlCartType};

use crate::gba::{Gba, HW_GYRO, HW_RTC, HW_RUMBLE, HW_SOLAR_SENSOR, HW_TILT};
use rgba_core::{mlog, Level};

pub const GBA_LUX_LEVELS: [i32; 10] = [5, 11, 18, 27, 42, 62, 84, 109, 139, 183];

// GPIO register offsets (cart space)
pub const GPIO_REG_DATA: u32 = 0xC4;
pub const GPIO_REG_DIRECTION: u32 = 0xC6;
pub const GPIO_REG_CONTROL: u32 = 0xC8;

pub const GPIO_WRITE_ONLY: u16 = 0;
pub const GPIO_READ_WRITE: u16 = 1;

pub const RTC_BYTES: [i32; 8] = [0, 0, 7, 0, 1, 0, 3, 0];

pub const RTC_CONTROL_HOUR24: u8 = 0x40;

pub const RTC_RESET: u32 = 0;
pub const RTC_DATETIME: u32 = 2;
pub const RTC_FORCE_IRQ: u32 = 3;
pub const RTC_CONTROL: u32 = 4;
pub const RTC_TIME: u32 = 6;

pub struct CartridgeHardware {
    pub devices: u32,
    pub read_write: u16,
    pub write_latch: u8,
    pub pin_state: u8,
    pub direction: u8,

    pub rtc_bytes_remaining: i32,
    pub rtc_bits_read: i32,
    pub rtc_bits: i32,
    pub rtc_command_active: bool,
    pub rtc_sck_edge: bool,
    pub rtc_sio_output: bool,
    pub rtc_command: u32,
    pub rtc_control: u8,
    pub rtc_time: [u8; 7],
    pub rtc_last_latch: i64,
    pub rtc_offset: i64,

    pub gyro_sample: u16,
    pub gyro_edge: bool,

    pub light_counter: u16,
    pub light_sample: u8,
    pub light_edge: bool,

    pub tilt_x: u16,
    pub tilt_y: u16,
    pub tilt_state: i32,
}

impl CartridgeHardware {
    pub fn new() -> Self {
        CartridgeHardware {
            devices: 0x8000, // HW_NO_OVERRIDE
            read_write: GPIO_WRITE_ONLY,
            write_latch: 0,
            pin_state: 0,
            direction: 0,
            rtc_bytes_remaining: 0,
            rtc_bits_read: 0,
            rtc_bits: 0,
            rtc_command_active: false,
            rtc_sck_edge: true,
            rtc_sio_output: true,
            rtc_command: 0,
            rtc_control: 0x40,
            rtc_time: [0; 7],
            rtc_last_latch: 0,
            rtc_offset: 0,
            gyro_sample: 0,
            gyro_edge: false,
            light_counter: 0,
            light_sample: 0xFF,
            light_edge: false,
            tilt_x: 0xFFF,
            tilt_y: 0xFFF,
            tilt_state: 0,
        }
    }
}

impl Default for CartridgeHardware {
    fn default() -> Self {
        Self::new()
    }
}

pub struct CivilTime {
    pub year: i64,
    pub month: i64,
    pub day: i64,
    pub wday: i64,
    pub hour: i64,
    pub min: i64,
    pub sec: i64,
}

/// Howard Hinnant's civil-from-days converter (days since 1970-01-01 → UTC
/// broken-down time) — the C used localtime_r; we use UTC + the user's offset.
pub fn unix_to_civil(t: i64) -> CivilTime {
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096).div_euclid(365);
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2).div_euclid(153);
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    let wday = (days.rem_euclid(7) + 4).rem_euclid(7);
    CivilTime {
        year,
        month: m,
        day: d,
        wday,
        hour: secs / 3600,
        min: (secs % 3600) / 60,
        sec: secs % 60,
    }
}

/// mGBA's _rtcBCD
pub fn rtc_bcd(value: u32) -> u8 {
    let counter = value % 10;
    let value = value / 10;
    (counter + ((value % 10) << 4)) as u8
}

fn rtc_command_data_magic(v: u32) -> u32 {
    v & 0xF
}
fn rtc_command_data_command(v: u32) -> u32 {
    (v >> 4) & 7
}
fn rtc_command_data_is_reading(v: u32) -> bool {
    v & 0x80 != 0
}

const CART_SPACE_OFFSET: u32 = 0xFFFFFF; // OFFSET_MASK

impl Gba {
    pub fn hw_devices(&self) -> u32 {
        self.hw.devices
    }
    pub fn hw_has_gpio(&self) -> bool {
        self.hw.devices & crate::gba::HW_GPIO != 0
    }
    pub fn hw_has_ereader(&self) -> bool {
        self.hw.devices & crate::gba::HW_EREADER != 0
    }

    /// GBAHardwareReset
    pub fn hw_reset(&mut self) {
        self.hw.read_write = GPIO_WRITE_ONLY;
        self.hw.write_latch = 0;
        self.hw.pin_state = 0;
        self.hw.direction = 0;
        self.hw.light_counter = 0;
        self.hw.light_edge = false;
        self.hw.light_sample = 0xFF;
        self.hw.gyro_sample = 0;
        self.hw.gyro_edge = false;
        self.hw.tilt_x = 0xFFF;
        self.hw.tilt_y = 0xFFF;
        self.hw.tilt_state = 0;
    }

    pub fn unix_time(&self) -> i64 {
        (self.rtc_time_fn)()
    }
    pub fn current_light(&self) -> u8 {
        self.light_sensor_level
    }


    fn rom_gpio_off(&self) -> usize {
        0xC4 // GPIO_REG_DATA offset in the ROM image
    }

    /// GBAHardwareGPIOWrite
    pub fn gpio_write(&mut self, address: u32, value: u16) {
        match address {
            GPIO_REG_DATA => {
                self.hw.write_latch = (value & 0xF) as u8;
                if !self.vba_bug_compat {
                    self.hw.pin_state &= !self.hw.direction;
                    self.hw.pin_state |= self.hw.write_latch & self.hw.direction;
                } else {
                    self.hw.pin_state = self.hw.write_latch;
                }
                self.gpio_read_pins();
            }
            GPIO_REG_DIRECTION => {
                self.hw.direction = (value & 0xF) as u8;
                if !self.vba_bug_compat {
                    self.hw.pin_state &= !self.hw.direction;
                    self.hw.pin_state |= self.hw.write_latch & self.hw.direction;
                    self.gpio_read_pins();
                }
            }
            GPIO_REG_CONTROL => {
                self.hw.read_write = value & 0x1;
            }
            _ => {
                mlog!(Level::Warn, rgba_core::log::GBA_HW, "Invalid GPIO address");
            }
        }
        if self.hw.read_write != 0 {
            let g = self.rom_gpio_off();
            if self.memory.rom.len() > g + 6 {
                let ps = self.hw.pin_state as u16;
                self.memory.rom[g] = ps as u8;
                self.memory.rom[g + 1] = (ps >> 8) as u8;
                let dir = self.hw.direction as u16;
                self.memory.rom[g + 2] = dir as u8;
                self.memory.rom[g + 3] = (dir >> 8) as u8;
                let rw = self.hw.read_write;
                self.memory.rom[g + 4] = rw as u8;
                self.memory.rom[g + 5] = (rw >> 8) as u8;
            }
        } else {
            let g = self.rom_gpio_off();
            for i in 0..3 {
                if self.memory.rom.len() > g + i * 2 {
                    self.memory.rom[g + i * 2] = 0;
                    self.memory.rom[g + i * 2 + 1] = 0;
                }
            }
        }
    }

    /// Read from the GPIO window (the C writes/reads these via the rom image).
    pub fn gpio_read(&self, reg: u32) -> u16 {
        let g = self.rom_gpio_off() + reg as usize - GPIO_REG_DATA as usize;
        if self.memory.rom.len() > g + 2 {
            self.memory.rom[g] as u16 | ((self.memory.rom[g + 1] as u16) << 8)
        } else {
            0
        }
    }

    fn gpio_read_pins(&mut self) {
        if self.hw.devices & HW_RTC != 0 {
            self.rtc_read_pins();
        }
        if self.hw.devices & HW_GYRO != 0 {
            self.gyro_read_pins();
        }
        if self.hw.devices & HW_RUMBLE != 0 {
            self.rumble_read_pins();
        }
        if self.hw.devices & HW_SOLAR_SENSOR != 0 {
            self.light_read_pins();
        }
    }

    fn gpio_output_pins(&mut self, pins: u8) {
        self.hw.pin_state &= self.hw.direction;
        self.hw.pin_state |= pins & !self.hw.direction & 0xF;
        if self.hw.read_write != 0 {
            let g = self.rom_gpio_off();
            if self.memory.rom.len() > g + 2 {
                let ps = self.hw.pin_state as u16;
                self.memory.rom[g] = ps as u8;
                self.memory.rom[g + 1] = (ps >> 8) as u8;
            }
        }
    }

    // --- RTC ---
    fn rtc_read_pins(&mut self) {
        self.gpio_output_pins(self.hw.pin_state & 2);

        if self.hw.pin_state & 4 == 0 {
            self.hw.rtc_bits_read = 0;
            self.hw.rtc_bytes_remaining = 0;
            self.hw.rtc_command_active = false;
            self.hw.rtc_command = 0;
            self.hw.rtc_sck_edge = true;
            self.hw.rtc_sio_output = true;
            self.gpio_output_pins(2);
            return;
        }

        if !self.hw.rtc_command_active {
            self.gpio_output_pins(2);
            if self.hw.pin_state & 1 == 0 {
                self.hw.rtc_bits &= !(1 << self.hw.rtc_bits_read);
                self.hw.rtc_bits |= (((self.hw.pin_state as i32) & 2) >> 1) << self.hw.rtc_bits_read;
            }
            if !self.hw.rtc_sck_edge && self.hw.pin_state & 1 != 0 {
                self.hw.rtc_bits_read += 1;
                if self.hw.rtc_bits_read == 8 {
                    self.rtc_begin_command();
                }
            }
        } else if !rtc_command_data_is_reading(self.hw.rtc_command) {
            self.gpio_output_pins(2);
            if self.hw.pin_state & 1 == 0 {
                self.hw.rtc_bits &= !(1 << self.hw.rtc_bits_read);
                self.hw.rtc_bits |= (((self.hw.pin_state as i32) & 2) >> 1) << self.hw.rtc_bits_read;
            }
            if !self.hw.rtc_sck_edge && self.hw.pin_state & 1 != 0 {
                if (((self.hw.rtc_bits >> self.hw.rtc_bits_read) & 1)
                    ^ (((self.hw.pin_state as i32) & 2) >> 1)) != 0
                {
                    self.hw.rtc_bits &= !(1 << self.hw.rtc_bits_read);
                }
                self.hw.rtc_bits_read += 1;
                if self.hw.rtc_bits_read == 8 {
                    self.rtc_process_byte();
                }
            }
        } else {
            if self.hw.rtc_sck_edge && self.hw.pin_state & 1 == 0 {
                let out = self.rtc_output();
                self.hw.rtc_sio_output = out;
                self.hw.rtc_bits_read += 1;
                if self.hw.rtc_bits_read == 8 {
                    self.hw.rtc_bytes_remaining -= 1;
                    if self.hw.rtc_bytes_remaining <= 0 {
                        self.hw.rtc_bytes_remaining =
                            RTC_BYTES[rtc_command_data_command(self.hw.rtc_command) as usize];
                    }
                    self.hw.rtc_bits_read = 0;
                }
            }
            self.gpio_output_pins((self.hw.rtc_sio_output as u8) << 1);
        }

        self.hw.rtc_sck_edge = self.hw.pin_state & 1 != 0;
    }

    fn rtc_begin_command(&mut self) {
        let command = self.hw.rtc_bits as u32;
        if rtc_command_data_magic(command) == 0x06 {
            self.hw.rtc_command = command;
            self.hw.rtc_bytes_remaining = RTC_BYTES[rtc_command_data_command(command) as usize];
            self.hw.rtc_command_active = true;
            match rtc_command_data_command(command) {
                RTC_RESET => {
                    self.hw.rtc_control = 0;
                }
                RTC_DATETIME | RTC_TIME => {
                    self.rtc_update_clock();
                }
                _ => {}
            }
        } else {
            mlog!(Level::Warn, rgba_core::log::GBA_HW, "Invalid RTC command byte: {:02X}", self.hw.rtc_bits);
        }
        self.hw.rtc_bits = 0;
        self.hw.rtc_bits_read = 0;
    }

    fn rtc_process_byte(&mut self) {
        if rtc_command_data_command(self.hw.rtc_command) == RTC_CONTROL {
            self.hw.rtc_control = self.hw.rtc_bits as u8;
        }
        self.hw.rtc_bits = 0;
        self.hw.rtc_bits_read = 0;
        self.hw.rtc_bytes_remaining -= 1;
        if self.hw.rtc_bytes_remaining <= 0 {
            self.hw.rtc_bytes_remaining =
                RTC_BYTES[rtc_command_data_command(self.hw.rtc_command) as usize];
        }
    }

    fn rtc_output(&mut self) -> bool {
        let mut output_byte = 0xFFu8;
        match rtc_command_data_command(self.hw.rtc_command) {
            RTC_CONTROL => {
                output_byte = self.hw.rtc_control;
            }
            RTC_DATETIME | RTC_TIME => {
                output_byte = self.hw.rtc_time[(7 - self.hw.rtc_bytes_remaining) as usize];
            }
            _ => {}
        }
        (output_byte >> self.hw.rtc_bits_read) & 1 != 0
    }

    fn rtc_update_clock(&mut self) {
        let t = self.unix_time();
        self.hw.rtc_last_latch = t;
        let t = t - self.hw.rtc_offset;
        let civil = unix_to_civil(t);
        self.hw.rtc_time[0] = rtc_bcd((civil.year - 2000) as u32);
        self.hw.rtc_time[1] = rtc_bcd(civil.month as u32);
        self.hw.rtc_time[2] = rtc_bcd(civil.day as u32);
        self.hw.rtc_time[3] = rtc_bcd(civil.wday as u32);
        if self.hw.rtc_control & RTC_CONTROL_HOUR24 != 0 {
            self.hw.rtc_time[4] = rtc_bcd(civil.hour as u32);
        } else {
            self.hw.rtc_time[4] = rtc_bcd((civil.hour % 12) as u32);
        }
        self.hw.rtc_time[5] = rtc_bcd(civil.min as u32);
        self.hw.rtc_time[6] = rtc_bcd(civil.sec as u32);
    }

    // --- Gyro ---
    fn gyro_read_pins(&mut self) {
        self.hw.gyro_edge = self.hw.pin_state & 2 != 0;
    }

    // --- Rumble ---
    fn rumble_read_pins(&mut self) {
        self.rumble_active = self.hw.pin_state & 8 != 0;
    }

    // --- Light sensor ---
    fn light_read_pins(&mut self) {
        if self.hw.pin_state & 4 != 0 {
            return;
        }
        if self.hw.pin_state & 2 != 0 {
            self.hw.light_counter = 0;
            self.hw.light_edge = true;
            self.hw.light_sample = self.current_light();
        }
        if self.hw.pin_state & 1 != 0 && self.hw.light_edge {
            self.hw.light_counter = self.hw.light_counter.wrapping_add(1);
        }
        self.hw.light_edge = self.hw.pin_state & 1 == 0;
        let send_bit = (self.hw.light_counter as i32) >= (self.hw.light_sample as i32);
        self.gpio_output_pins(((send_bit as u8) << 3) | (self.hw.pin_state & 0x7));
    }


    // --- Tilt ---
    pub fn tilt_write(&mut self, address: u32, value: u8) {
        match address & CART_SPACE_OFFSET {
            0x8000 => {
                if value == 0x55 {
                    self.hw.tilt_state = 1;
                } else {
                    mlog!(Level::GameError, rgba_core::log::GBA_HW, "Tilt sensor wrote wrong byte to {:04x}: {:02x}", address, value);
                }
            }
            0x8100 => {
                if value == 0xAA && self.hw.tilt_state == 1 {
                    self.hw.tilt_state = 0;
                    self.hw.tilt_x = 0x3A0;
                    self.hw.tilt_y = 0x3A0;
                } else {
                    mlog!(Level::GameError, rgba_core::log::GBA_HW, "Tilt sensor wrote wrong byte to {:04x}: {:02x}", address, value);
                }
            }
            _ => {
                mlog!(Level::GameError, rgba_core::log::GBA_HW, "Invalid tilt sensor write to {:04x}: {:02x}", address, value);
            }
        }
    }

    pub fn tilt_read(&self, address: u32) -> u8 {
        match address & CART_SPACE_OFFSET {
            0x8200 => self.hw.tilt_x as u8,
            0x8300 => ((self.hw.tilt_x >> 8) as u8 & 0xF) | 0x80,
            0x8400 => self.hw.tilt_y as u8,
            0x8500 => (self.hw.tilt_y >> 8) as u8 & 0xF,
            _ => 0xFF,
        }
    }

    pub fn hw_init_rtc(&mut self) {
        self.hw.devices |= HW_RTC;
        self.hw.rtc_bytes_remaining = 0;
        self.hw.rtc_bits_read = 0;
        self.hw.rtc_bits = 0;
        self.hw.rtc_command_active = false;
        self.hw.rtc_sck_edge = true;
        self.hw.rtc_sio_output = true;
        self.hw.rtc_command = 0;
        self.hw.rtc_control = 0x40;
        self.hw.rtc_time = [0; 7];
        self.hw.rtc_last_latch = 0;
        self.hw.rtc_offset = 0;
    }
    pub fn hw_init_gyro(&mut self) {
        self.hw.devices |= HW_GYRO;
        self.hw.gyro_sample = 0;
        self.hw.gyro_edge = false;
    }
    pub fn hw_init_rumble(&mut self) {
        self.hw.devices |= HW_RUMBLE;
    }
    pub fn hw_init_light(&mut self) {
        self.hw.devices |= HW_SOLAR_SENSOR;
        self.hw.light_counter = 0;
        self.hw.light_edge = false;
        self.hw.light_sample = 0xFF;
    }
    pub fn hw_init_tilt(&mut self) {
        self.hw.devices |= HW_TILT;
        self.hw.tilt_x = 0xFFF;
        self.hw.tilt_y = 0xFFF;
        self.hw.tilt_state = 0;
    }

    // e-Reader (cart/ereader.rs), Matrix (cart/matrix.rs) and unlicensed-cart
    // (cart/unlicensed.rs) handlers live in their own modules now.
}

// Remaining no-device stubs.
impl Gba {
    pub fn agb_print_write16(&mut self, _address: u32, _value: u16) {}
    pub fn agb_print_load(&mut self, _address: u32) -> u16 {
        0
    }
    pub fn agb_print_restore_by_phi(&mut self, _phi: u16) {}
}
