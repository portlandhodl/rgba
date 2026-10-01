// Copyright (c) 2013-2020 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gba/cart/ereader.c.

use rgba_core::{mlog, Level};

use crate::gba::{EventId, Gba, GBA_IRQ_GAMEPAK, HW_EREADER};
use crate::io::{GBA_REG_IE, GBA_REG_IF};

pub const EREADER_BLOCK_SIZE: usize = 40;

pub const EREADER_DOTCODE_STRIDE: usize = 1420;
pub const EREADER_DOTCODE_SIZE: usize = EREADER_DOTCODE_STRIDE * 40;
pub const EREADER_CARDS_MAX: usize = 16;

// EReaderControl0 / EReaderControl1 bit accessors (DECL_BIT in the C).
fn ereader_control0_is_data(v: u8) -> bool {
    v & 1 != 0
}
fn ereader_control0_get_data(v: u8) -> u8 {
    v & 1
}
fn ereader_control0_clear_data(v: u8) -> u8 {
    v & !1
}
fn ereader_control0_set_data(v: u8, e: u8) -> u8 {
    (v & !1) | (e & 1)
}
fn ereader_control0_is_clock(v: u8) -> bool {
    v & 2 != 0
}
fn ereader_control0_is_direction(v: u8) -> bool {
    v & 4 != 0
}
fn ereader_control0_is_led_enable(v: u8) -> bool {
    v & 8 != 0
}
fn ereader_control0_is_scan(v: u8) -> bool {
    v & 0x10 != 0
}
fn ereader_control1_is_scanline(v: u8) -> bool {
    v & 2 != 0
}
fn ereader_control1_fill_scanline(v: u8) -> u8 {
    v | 2
}

// enum EReaderStateMachine
pub const EREADER_SERIAL_INACTIVE: u8 = 0;
pub const EREADER_SERIAL_STARTING: u8 = 1;
pub const EREADER_SERIAL_BIT_0: u8 = 2;
pub const EREADER_SERIAL_END_BIT: u8 = 11;

// enum EReaderCommand
pub const EREADER_COMMAND_IDLE: u8 = 0; // TODO: Verify on hardware
pub const EREADER_COMMAND_WRITE_DATA: u8 = 1;
pub const EREADER_COMMAND_SET_INDEX: u8 = 0x22;
pub const EREADER_COMMAND_READ_DATA: u8 = 0x23;

pub const EREADER_NYBBLE_5BIT: [[u8; 5]; 16] = [
    [0, 0, 0, 0, 0],
    [0, 0, 0, 0, 1],
    [0, 0, 0, 1, 0],
    [1, 0, 0, 1, 0],
    [0, 0, 1, 0, 0],
    [0, 0, 1, 0, 1],
    [0, 0, 1, 1, 0],
    [1, 0, 1, 1, 0],
    [0, 1, 0, 0, 0],
    [0, 1, 0, 0, 1],
    [0, 1, 0, 1, 0],
    [1, 0, 1, 0, 0],
    [0, 1, 1, 0, 0],
    [0, 1, 1, 0, 1],
    [1, 0, 0, 0, 1],
    [1, 0, 0, 0, 0],
];

pub const EREADER_NYBBLE_LOOKUP: [i8; 32] = [
    0, 1, 2, -1, 4, 5, 6, -1, 8, 9, 10, -1, 12, 13, -1, -1, 15, 14, 3, -1, 11, -1, 7, -1, -1, -1,
    -1, -1, -1, -1, -1, -1,
];

pub const EREADER_CALIBRATION_TEMPLATE: [u8; 83] = [
    0x43, 0x61, 0x72, 0x64, 0x2d, 0x45, 0x20, 0x52, 0x65, 0x61, 0x64, 0x65, 0x72, 0x20, 0x32, 0x30,
    0x30, 0x31, 0x00, 0x00, 0xcf, 0x72, 0x2f, 0x37, 0x3a, 0x3a, 0x3a, 0x38, 0x33, 0x30, 0x30, 0x37,
    0x3a, 0x39, 0x37, 0x35, 0x33, 0x2f, 0x2f, 0x34, 0x36, 0x36, 0x37, 0x36, 0x34, 0x31, 0x2d, 0x30,
    0x32, 0x34, 0x35, 0x35, 0x34, 0x30, 0x2a, 0x2d, 0x2d, 0x2f, 0x31, 0x32, 0x31, 0x2f, 0x29, 0x2a,
    0x2c, 0x2b, 0x2c, 0x2e, 0x2e, 0x2d, 0x18, 0x2d, 0x8f, 0x03, 0x00, 0x00, 0xc0, 0xfd, 0x77, 0x00,
    0x00, 0x00, 0x01,
];

pub const EREADER_ADDRESS_CODES: [u16; 54] = [
    1023, 1174, 2628, 3373, 4233, 6112, 6450, 7771, 8826, 9491, 11201, 11432, 12556, 13925, 14519,
    16350, 16629, 18332, 18766, 20007, 21379, 21738, 23096, 23889, 24944, 26137, 26827, 28578,
    29190, 30063, 31677, 31956, 33410, 34283, 35641, 35920, 37364, 38557, 38991, 40742, 41735,
    42094, 43708, 44501, 45169, 46872, 47562, 48803, 49544, 50913, 51251, 53082, 54014, 54679,
];

const DUMMY_HEADER_STRIP: [[u8; 0x10]; 2] = [
    [
        0x00, 0x30, 0x01, 0x01, 0x00, 0x01, 0x05, 0x10, 0x00, 0x00, 0x10, 0x13, 0x00, 0x00, 0x02,
        0x00,
    ],
    [
        0x00, 0x30, 0x01, 0x02, 0x00, 0x01, 0x08, 0x10, 0x00, 0x00, 0x10, 0x12, 0x00, 0x00, 0x01,
        0x00,
    ],
];

const DUMMY_HEADER_FIXED: [u8; 0x16] = [
    0x00, 0x00, 0x10, 0x00, 0x00, 0x19, 0x00, 0x00, 0x00, 0x08, 0x4e, 0x49, 0x4e, 0x54, 0x45, 0x4e,
    0x44, 0x4f, 0x00, 0x22, 0x00, 0x09,
];

const BLOCK_HEADER: [[u8; 0x18]; 2] = [
    [
        0x00, 0x02, 0x00, 0x01, 0x40, 0x10, 0x00, 0x1c, 0x10, 0x6f, 0x40, 0xda, 0x39, 0x25, 0x8e,
        0xe0, 0x7b, 0xb5, 0x98, 0xb6, 0x5b, 0xcf, 0x7f, 0x72,
    ],
    [
        0x00, 0x03, 0x00, 0x19, 0x40, 0x10, 0x00, 0x2c, 0x0e, 0x88, 0xed, 0x82, 0x50, 0x67, 0xfb,
        0xd1, 0x43, 0xee, 0x03, 0xc6, 0xc6, 0x2b, 0x2c, 0x93,
    ],
];

const RS_POW: [u8; 256] = [
    0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x87, 0x89, 0x95, 0xad, 0xdd, 0x3d, 0x7a, 0xf4,
    0x6f, 0xde, 0x3b, 0x76, 0xec, 0x5f, 0xbe, 0xfb, 0x71, 0xe2, 0x43, 0x86, 0x8b, 0x91, 0xa5, 0xcd,
    0x1d, 0x3a, 0x74, 0xe8, 0x57, 0xae, 0xdb, 0x31, 0x62, 0xc4, 0x0f, 0x1e, 0x3c, 0x78, 0xf0, 0x67,
    0xce, 0x1b, 0x36, 0x6c, 0xd8, 0x37, 0x6e, 0xdc, 0x3f, 0x7e, 0xfc, 0x7f, 0xfe, 0x7b, 0xf6, 0x6b,
    0xd6, 0x2b, 0x56, 0xac, 0xdf, 0x39, 0x72, 0xe4, 0x4f, 0x9e, 0xbb, 0xf1, 0x65, 0xca, 0x13, 0x26,
    0x4c, 0x98, 0xb7, 0xe9, 0x55, 0xaa, 0xd3, 0x21, 0x42, 0x84, 0x8f, 0x99, 0xb5, 0xed, 0x5d, 0xba,
    0xf3, 0x61, 0xc2, 0x03, 0x06, 0x0c, 0x18, 0x30, 0x60, 0xc0, 0x07, 0x0e, 0x1c, 0x38, 0x70, 0xe0,
    0x47, 0x8e, 0x9b, 0xb1, 0xe5, 0x4d, 0x9a, 0xb3, 0xe1, 0x45, 0x8a, 0x93, 0xa1, 0xc5, 0x0d, 0x1a,
    0x34, 0x68, 0xd0, 0x27, 0x4e, 0x9c, 0xbf, 0xf9, 0x75, 0xea, 0x53, 0xa6, 0xcb, 0x11, 0x22, 0x44,
    0x88, 0x97, 0xa9, 0xd5, 0x2d, 0x5a, 0xb4, 0xef, 0x59, 0xb2, 0xe3, 0x41, 0x82, 0x83, 0x81, 0x85,
    0x8d, 0x9d, 0xbd, 0xfd, 0x7d, 0xfa, 0x73, 0xe6, 0x4b, 0x96, 0xab, 0xd1, 0x25, 0x4a, 0x94, 0xaf,
    0xd9, 0x35, 0x6a, 0xd4, 0x2f, 0x5e, 0xbc, 0xff, 0x79, 0xf2, 0x63, 0xc6, 0x0b, 0x16, 0x2c, 0x58,
    0xb0, 0xe7, 0x49, 0x92, 0xa3, 0xc1, 0x05, 0x0a, 0x14, 0x28, 0x50, 0xa0, 0xc7, 0x09, 0x12, 0x24,
    0x48, 0x90, 0xa7, 0xc9, 0x15, 0x2a, 0x54, 0xa8, 0xd7, 0x29, 0x52, 0xa4, 0xcf, 0x19, 0x32, 0x64,
    0xc8, 0x17, 0x2e, 0x5c, 0xb8, 0xf7, 0x69, 0xd2, 0x23, 0x46, 0x8c, 0x9f, 0xb9, 0xf5, 0x6d, 0xda,
    0x33, 0x66, 0xcc, 0x1f, 0x3e, 0x7c, 0xf8, 0x77, 0xee, 0x5b, 0xb6, 0xeb, 0x51, 0xa2, 0xc3, 0x00
];

const RS_REV: [u8; 256] = [
    0xff, 0x00, 0x01, 0x63, 0x02, 0xc6, 0x64, 0x6a, 0x03, 0xcd, 0xc7, 0xbc, 0x65, 0x7e, 0x6b, 0x2a,
    0x04, 0x8d, 0xce, 0x4e, 0xc8, 0xd4, 0xbd, 0xe1, 0x66, 0xdd, 0x7f, 0x31, 0x6c, 0x20, 0x2b, 0xf3,
    0x05, 0x57, 0x8e, 0xe8, 0xcf, 0xac, 0x4f, 0x83, 0xc9, 0xd9, 0xd5, 0x41, 0xbe, 0x94, 0xe2, 0xb4,
    0x67, 0x27, 0xde, 0xf0, 0x80, 0xb1, 0x32, 0x35, 0x6d, 0x45, 0x21, 0x12, 0x2c, 0x0d, 0xf4, 0x38,
    0x06, 0x9b, 0x58, 0x1a, 0x8f, 0x79, 0xe9, 0x70, 0xd0, 0xc2, 0xad, 0xa8, 0x50, 0x75, 0x84, 0x48,
    0xca, 0xfc, 0xda, 0x8a, 0xd6, 0x54, 0x42, 0x24, 0xbf, 0x98, 0x95, 0xf9, 0xe3, 0x5e, 0xb5, 0x15,
    0x68, 0x61, 0x28, 0xba, 0xdf, 0x4c, 0xf1, 0x2f, 0x81, 0xe6, 0xb2, 0x3f, 0x33, 0xee, 0x36, 0x10,
    0x6e, 0x18, 0x46, 0xa6, 0x22, 0x88, 0x13, 0xf7, 0x2d, 0xb8, 0x0e, 0x3d, 0xf5, 0xa4, 0x39, 0x3b,
    0x07, 0x9e, 0x9c, 0x9d, 0x59, 0x9f, 0x1b, 0x08, 0x90, 0x09, 0x7a, 0x1c, 0xea, 0xa0, 0x71, 0x5a,
    0xd1, 0x1d, 0xc3, 0x7b, 0xae, 0x0a, 0xa9, 0x91, 0x51, 0x5b, 0x76, 0x72, 0x85, 0xa1, 0x49, 0xeb,
    0xcb, 0x7c, 0xfd, 0xc4, 0xdb, 0x1e, 0x8b, 0xd2, 0xd7, 0x92, 0x55, 0xaa, 0x43, 0x0b, 0x25, 0xaf,
    0xc0, 0x73, 0x99, 0x77, 0x96, 0x5c, 0xfa, 0x52, 0xe4, 0xec, 0x5f, 0x4a, 0xb6, 0xa2, 0x16, 0x86,
    0x69, 0xc5, 0x62, 0xfe, 0x29, 0x7d, 0xbb, 0xcc, 0xe0, 0xd3, 0x4d, 0x8c, 0xf2, 0x1f, 0x30, 0xdc,
    0x82, 0xab, 0xe7, 0x56, 0xb3, 0x93, 0x40, 0xd8, 0x34, 0xb0, 0xef, 0x26, 0x37, 0x0c, 0x11, 0x44,
    0x6f, 0x78, 0x19, 0x9a, 0x47, 0x74, 0xa7, 0xc1, 0x23, 0x53, 0x89, 0xfb, 0x14, 0x5d, 0xf8, 0x97,
    0x2e, 0x4b, 0xb9, 0x60, 0x0f, 0xed, 0x3e, 0xe5, 0xf6, 0x87, 0xa5, 0x17, 0x3a, 0xa3, 0x3c, 0xb7,
];

const RS_GG: [u8; 16] = [
    0x00, 0x4b, 0xeb, 0xd5, 0xef, 0x4c, 0x71, 0x00, 0xf4, 0x00, 0x71, 0x4c, 0xef, 0xd5, 0xeb, 0x4b,
];

/// struct EReaderCard
pub struct EReaderCard {
    pub data: Vec<u8>,
}

/// struct GBACartEReader
pub struct EReader {
    /// C: uint16_t data[44]; kept as bytes, 16-bit accesses are little-endian.
    pub data: [u8; 88],
    pub serial: [u8; 92],
    pub register_unk: u16,
    pub register_reset: u16,
    pub register_control0: u8,
    pub register_control1: u8,
    pub register_led: u16,

    // TODO: Serialize these
    pub state: u8,
    pub command: u8,
    pub active_register: u8,
    pub byte: u8,
    pub scan_x: i32,
    pub scan_y: i32,
    /// Dotcode bitmap fed by GBACartEReaderScan. No frontend feeds images yet,
    /// so this stays None (the C's `dots == NULL` behavior).
    pub dots: Option<Box<[u8; EREADER_DOTCODE_SIZE]>>,
    pub cards: [Option<EReaderCard>; EREADER_CARDS_MAX],
}

impl EReader {
    pub fn new() -> Self {
        EReader {
            data: [0; 88],
            serial: [0; 92],
            register_unk: 0,
            register_reset: 0,
            register_control0: 0,
            register_control1: 0,
            register_led: 0,
            state: EREADER_SERIAL_INACTIVE,
            command: EREADER_COMMAND_IDLE,
            active_register: 0,
            byte: 0,
            scan_x: 0,
            scan_y: 0,
            dots: None,
            cards: std::array::from_fn(|_| None),
        }
    }
}

impl Default for EReader {
    fn default() -> Self {
        Self::new()
    }
}

impl Gba {
    /// GBACartEReaderInit
    pub fn ereader_init(&mut self) {
        self.hw.devices |= HW_EREADER;
        self.e_reader_reset();

        if self.savedata.data.len() >= 0xE000 && self.savedata.data[0xD000] == 0xFF {
            for b in &mut self.savedata.data[0xD000..0xE000] {
                *b = 0;
            }
            self.savedata.data[0xD000..0xD000 + EREADER_CALIBRATION_TEMPLATE.len()]
                .copy_from_slice(&EREADER_CALIBRATION_TEMPLATE);
        }
        if self.savedata.data.len() >= 0xF000 && self.savedata.data[0xE000] == 0xFF {
            for b in &mut self.savedata.data[0xE000..0xF000] {
                *b = 0;
            }
            self.savedata.data[0xE000..0xE000 + EREADER_CALIBRATION_TEMPLATE.len()]
                .copy_from_slice(&EREADER_CALIBRATION_TEMPLATE);
        }
    }

    /// GBACartEReaderWrite
    pub fn ereader_write(&mut self, address: u32, value: u16) {
        let address = address & 0x700FF;
        match address >> 17 {
            0 => {
                self.ereader.register_unk = value & 0xF;
            }
            1 => {
                self.ereader.register_reset = (value & 0x8A) | 4;
                if value & 2 != 0 {
                    self.e_reader_reset();
                }
            }
            2 => {
                mlog!(
                    Level::GameError,
                    rgba_core::log::GBA_HW,
                    "e-Reader write to read-only registers: {:05X}:{:04X}",
                    address,
                    value
                );
            }
            _ => {
                mlog!(
                    Level::Stub,
                    rgba_core::log::GBA_HW,
                    "Unimplemented e-Reader write: {:05X}:{:04X}",
                    address,
                    value
                );
            }
        }
    }

    /// GBACartEReaderWriteFlash
    pub fn ereader_write_flash(&mut self, address: u32, value: u8) {
        let address = address & 0xFFFF;
        match address {
            0xFFB0 => {
                self.e_reader_write_control0(value);
            }
            0xFFB1 => {
                self.e_reader_write_control1(value);
            }
            0xFFB2 => {
                self.ereader.register_led &= 0xFF00;
                self.ereader.register_led |= value as u16;
            }
            0xFFB3 => {
                self.ereader.register_led &= 0x00FF;
                self.ereader.register_led |= (value as u16) << 8;
            }
            _ => {
                mlog!(
                    Level::Stub,
                    rgba_core::log::GBA_HW,
                    "Unimplemented e-Reader write to flash: {:04X}:{:02X}",
                    address,
                    value
                );
            }
        }
    }

    /// GBACartEReaderRead
    pub fn ereader_read(&mut self, address: u32) -> u16 {
        let address = address & 0x700FF;
        match address >> 17 {
            0 => self.ereader.register_unk,
            1 => self.ereader.register_reset,
            2 => {
                if address > 0x40088 {
                    return 0;
                }
                // LOAD_16(value, address & 0xFE, ereader->data). At 0x40086.. the C reads past
                // the 88-byte buffer (into `serial`); out-of-buffer bytes read back as 0 here.
                let offset = (address & 0xFE) as usize;
                let lo = self.ereader.data.get(offset).copied().unwrap_or(0) as u16;
                let hi = self.ereader.data.get(offset + 1).copied().unwrap_or(0) as u16;
                lo | (hi << 8)
            }
            _ => {
                mlog!(
                    Level::Stub,
                    rgba_core::log::GBA_HW,
                    "Unimplemented e-Reader read: {:05X}",
                    address
                );
                0
            }
        }
    }

    /// GBACartEReaderReadFlash
    pub fn ereader_read_flash(&mut self, address: u32) -> u8 {
        let address = address & 0xFFFF;
        match address {
            0xFFB0 => self.ereader.register_control0,
            0xFFB1 => self.ereader.register_control1,
            _ => {
                mlog!(
                    Level::Stub,
                    rgba_core::log::GBA_HW,
                    "Unimplemented e-Reader read from flash: {:04X}",
                    address
                );
                0
            }
        }
    }

    /// GBACartEReaderScan: turn a raw/bitmap dotcode card into the dots image.
    pub fn ereader_scan(&mut self, data: &[u8]) {
        if self.ereader.dots.is_none() {
            let dots: Box<[u8; EREADER_DOTCODE_SIZE]> =
                vec![0u8; EREADER_DOTCODE_SIZE].try_into().unwrap();
            self.ereader.dots = Some(dots);
        }
        self.ereader.scan_x = -24;
        let dots = self.ereader.dots.as_deref_mut().unwrap();
        for b in dots.iter_mut() {
            *b = 0;
        }

        let mut block_rs = [[0u8; 0x10]; 44];
        let mut block0 = [0u8; 0x30];
        let mut parsed = false;
        let mut bitmap = false;
        let mut reduced_header = false;
        let blocks: usize;
        let base: i32;
        match data.len() {
            // Raw sizes
            2076 | 2112 | 2912 => {
                if data.len() == 2076 {
                    block0[..0x10].copy_from_slice(&DUMMY_HEADER_STRIP[1]);
                    reduced_header = true;
                }
                if data.len() != 2912 {
                    parsed = true;
                }
                base = 25;
                blocks = 28;
            }
            1308 | 1344 | 1872 => {
                if data.len() == 1308 {
                    block0[..0x10].copy_from_slice(&DUMMY_HEADER_STRIP[0]);
                    reduced_header = true;
                }
                if data.len() != 1872 {
                    parsed = true;
                }
                base = 1;
                blocks = 18;
            }
            // Bitmap sizes
            5456 => {
                bitmap = true;
                blocks = 124;
                base = 0;
            }
            3520 => {
                bitmap = true;
                blocks = 80;
                base = 0;
            }
            _ => return,
        }

        let cdata = data;
        if bitmap {
            for i in 0..40usize {
                let line = &cdata[(i + 2) * blocks..];
                let origin = EREADER_DOTCODE_STRIDE * i + 200;
                for x in 0..blocks {
                    let mut byte = line[x];
                    if x == 123 {
                        byte &= 0xE0;
                    }
                    dots[origin + x * 8 + 0] = (byte >> 7) & 1;
                    dots[origin + x * 8 + 1] = (byte >> 6) & 1;
                    dots[origin + x * 8 + 2] = (byte >> 5) & 1;
                    dots[origin + x * 8 + 3] = (byte >> 4) & 1;
                    dots[origin + x * 8 + 4] = (byte >> 3) & 1;
                    dots[origin + x * 8 + 5] = (byte >> 2) & 1;
                    dots[origin + x * 8 + 6] = (byte >> 1) & 1;
                    dots[origin + x * 8 + 7] = byte & 1;
                }
            }
            return;
        }

        for i in 0..blocks + 1 {
            let origin = 35 * i + 200;
            e_reader_anchor(dots, origin);
            e_reader_anchor(dots, origin + EREADER_DOTCODE_STRIDE * 35);
            e_reader_address(dots, origin, (base + i as i32) as usize);
        }
        if parsed {
            if reduced_header {
                block0[0x10..0x10 + 0x16].copy_from_slice(&DUMMY_HEADER_FIXED);
                block0[0x0D] = cdata[0x0];
                block0[0x0C] = cdata[0x1];
                block0[0x10] = cdata[0x2];
                block0[0x11] = cdata[0x3];
                block0[0x26] = cdata[0x4];
                block0[0x27] = cdata[0x5];
                block0[0x28] = cdata[0x6];
                block0[0x29] = cdata[0x7];
                block0[0x2A] = cdata[0x8];
                block0[0x2B] = cdata[0x9];
                block0[0x2C] = cdata[0xA];
                block0[0x2D] = cdata[0xB];
                for i in 0..12 {
                    block0[0x2E] ^= cdata[i];
                }
                let mut data_checksum: u32 = 0;
                for i in 1..(data.len() + 36) / 48 {
                    let block = &cdata[i * 48 - 36..];
                    e_reader_reed_solomon(block, &mut block_rs[i]);
                    let mut fragment_checksum: u32 = 0;
                    for j in (0..0x30usize).step_by(2) {
                        fragment_checksum ^= block[j] as u32;
                        fragment_checksum ^= block[j + 1] as u32;
                        let halfword = u16::from_be_bytes([block[j], block[j + 1]]);
                        data_checksum = data_checksum.wrapping_add(halfword as u32);
                    }
                    block0[0x2F] = block0[0x2F].wrapping_add(fragment_checksum as u8);
                }
                block0[0x13] = ((!data_checksum) >> 8) as u8;
                block0[0x14] = !data_checksum as u8;
                for i in 0..0x2F {
                    block0[0x2F] = block0[0x2F].wrapping_add(block0[i]);
                }
                block0[0x2F] = !block0[0x2F];
                e_reader_reed_solomon(&block0, &mut block_rs[0]);
            } else {
                for i in 0..data.len() / 48 {
                    e_reader_reed_solomon(&cdata[i * 48..], &mut block_rs[i]);
                }
            }
        }
        let mut block_id: usize = 0;
        let mut byte_offset: usize = 0;
        for i in 0..blocks {
            let mut block = [0u8; 1040];
            let origin = 35 * i + 200;
            e_reader_alignment(dots, origin + EREADER_DOTCODE_STRIDE * 2);
            e_reader_alignment(dots, origin + EREADER_DOTCODE_STRIDE * 37);

            let mut parsed_block_data = [0u8; 104];
            let block_data: &[u8] = if parsed {
                let header = &BLOCK_HEADER[if data.len() == 1344 { 0 } else { 1 }];
                parsed_block_data[0] = header[(2 * i) % 0x18];
                parsed_block_data[1] = header[(2 * i) % 0x18 + 1];
                for j in 2..104usize {
                    if byte_offset >= 0x40 {
                        break;
                    }
                    if byte_offset >= 0x30 {
                        parsed_block_data[j] = block_rs[block_id][byte_offset - 0x30];
                    } else if !reduced_header {
                        parsed_block_data[j] = cdata[block_id * 0x30 + byte_offset];
                    } else if block_id > 0 {
                        parsed_block_data[j] = cdata[block_id * 0x30 + byte_offset - 36];
                    } else {
                        parsed_block_data[j] = block0[byte_offset];
                    }
                    block_id += 1;
                    if block_id * 0x30 >= data.len() {
                        block_id = 0;
                        byte_offset += 1;
                    }
                }
                &parsed_block_data[..]
            } else {
                &cdata[i * 104..i * 104 + 104]
            };
            for b in 0..104usize {
                let nybble5 = &EREADER_NYBBLE_5BIT[(block_data[b] >> 4) as usize];
                block[b * 10 + 0] = nybble5[0];
                block[b * 10 + 1] = nybble5[1];
                block[b * 10 + 2] = nybble5[2];
                block[b * 10 + 3] = nybble5[3];
                block[b * 10 + 4] = nybble5[4];
                let nybble5 = &EREADER_NYBBLE_5BIT[(block_data[b] & 0xF) as usize];
                block[b * 10 + 5] = nybble5[0];
                block[b * 10 + 6] = nybble5[1];
                block[b * 10 + 7] = nybble5[2];
                block[b * 10 + 8] = nybble5[3];
                block[b * 10 + 9] = nybble5[4];
            }

            let mut b = 0usize;
            for y in 0..3 {
                dots[origin + EREADER_DOTCODE_STRIDE * (4 + y) + 7..][..26]
                    .copy_from_slice(&block[b..b + 26]);
                b += 26;
            }
            for y in 0..26 {
                dots[origin + EREADER_DOTCODE_STRIDE * (7 + y) + 3..][..34]
                    .copy_from_slice(&block[b..b + 34]);
                b += 34;
            }
            for y in 0..3 {
                dots[origin + EREADER_DOTCODE_STRIDE * (33 + y) + 7..][..26]
                    .copy_from_slice(&block[b..b + 26]);
                b += 26;
            }
        }
    }

    /// GBACartEReaderQueueCard
    pub fn ereader_queue_card(&mut self, data: &[u8]) {
        for slot in self.ereader.cards.iter_mut() {
            if slot.is_none() {
                *slot = Some(EReaderCard {
                    data: data.to_vec(),
                });
                return;
            }
        }
    }

    /// _eReaderReset
    fn e_reader_reset(&mut self) {
        self.ereader.data = [0; 88];
        self.ereader.register_unk = 0;
        self.ereader.register_reset = 4;
        self.ereader.register_control0 = 0;
        self.ereader.register_control1 = 0x80;
        self.ereader.register_led = 0;
        self.ereader.state = EREADER_SERIAL_INACTIVE;
        self.ereader.active_register = 0;
    }

    /// _eReaderWriteControl0
    fn e_reader_write_control0(&mut self, value: u8) {
        let mut control = value & 0x7F;
        let old_control = self.ereader.register_control0;
        if self.ereader.state == EREADER_SERIAL_INACTIVE {
            if ereader_control0_is_clock(old_control)
                && ereader_control0_is_data(old_control)
                && !ereader_control0_is_data(control)
            {
                self.ereader.state = EREADER_SERIAL_STARTING;
            }
        } else if ereader_control0_is_clock(old_control)
            && !ereader_control0_is_data(old_control)
            && ereader_control0_is_data(control)
        {
            self.ereader.state = EREADER_SERIAL_INACTIVE;
        } else if self.ereader.state == EREADER_SERIAL_STARTING {
            if ereader_control0_is_clock(old_control)
                && !ereader_control0_is_data(old_control)
                && !ereader_control0_is_clock(control)
            {
                self.ereader.state = EREADER_SERIAL_BIT_0;
                self.ereader.command = EREADER_COMMAND_IDLE;
            }
        } else if ereader_control0_is_clock(old_control) && !ereader_control0_is_clock(control) {
            mlog!(
                Level::Debug,
                rgba_core::log::GBA_HW,
                "[e-Reader] Serial falling edge: {} {}",
                if ereader_control0_is_direction(control) {
                    '>'
                } else {
                    '<'
                },
                ereader_control0_get_data(control)
            );
            // TODO: Improve direction control
            if ereader_control0_is_direction(control) {
                self.ereader.byte |= ereader_control0_get_data(control)
                    << (7 - (self.ereader.state - EREADER_SERIAL_BIT_0));
                self.ereader.state += 1;
                if self.ereader.state == EREADER_SERIAL_END_BIT {
                    mlog!(
                        Level::Debug,
                        rgba_core::log::GBA_HW,
                        "[e-Reader] Wrote serial byte: {:02x}",
                        self.ereader.byte
                    );
                    match self.ereader.command {
                        EREADER_COMMAND_IDLE => {
                            self.ereader.command = self.ereader.byte;
                        }
                        EREADER_COMMAND_SET_INDEX => {
                            self.ereader.active_register = self.ereader.byte;
                            self.ereader.command = EREADER_COMMAND_WRITE_DATA;
                        }
                        EREADER_COMMAND_WRITE_DATA => {
                            match self.ereader.active_register & 0x7F {
                                0 | 0x57 | 0x58 | 0x59 | 0x5A => {
                                    // Read-only
                                    mlog!(
                                        Level::GameError,
                                        rgba_core::log::GBA_HW,
                                        "Writing to read-only e-Reader serial register: {:02X}",
                                        self.ereader.active_register
                                    );
                                }
                                _ => {
                                    if (self.ereader.active_register & 0x7F) > 0x5A {
                                        mlog!(Level::GameError, rgba_core::log::GBA_HW, "Writing to non-existent e-Reader serial register: {:02X}", self.ereader.active_register);
                                    } else {
                                        self.ereader.serial
                                            [(self.ereader.active_register & 0x7F) as usize] =
                                            self.ereader.byte;
                                    }
                                }
                            }
                            self.ereader.active_register =
                                self.ereader.active_register.wrapping_add(1);
                        }
                        _ => {
                            mlog!(
                                Level::Error,
                                rgba_core::log::GBA_HW,
                                "Hit undefined state {:02X} in e-Reader state machine",
                                self.ereader.command
                            );
                        }
                    }
                    self.ereader.state = EREADER_SERIAL_BIT_0;
                    self.ereader.byte = 0;
                }
            } else if self.ereader.command == EREADER_COMMAND_READ_DATA {
                let mut bit = 0u8;
                if (self.ereader.active_register & 0x7F) < 0x5A {
                    bit = self.ereader.serial[(self.ereader.active_register & 0x7F) as usize]
                        >> (7 - (self.ereader.state - EREADER_SERIAL_BIT_0));
                }
                control = ereader_control0_set_data(control, bit & 1);
                self.ereader.state += 1;
                if self.ereader.state == EREADER_SERIAL_END_BIT {
                    self.ereader.active_register = self.ereader.active_register.wrapping_add(1);
                    if (self.ereader.active_register & 0x7F) < 0x5A {
                        mlog!(
                            Level::Debug,
                            rgba_core::log::GBA_HW,
                            "[e-Reader] Read serial byte: {:02x}",
                            self.ereader.serial[(self.ereader.active_register & 0x7F) as usize]
                        );
                    } else {
                        mlog!(
                            Level::GameError,
                            rgba_core::log::GBA_HW,
                            "[e-Reader] Read out of bounds serial byte {:02x}",
                            self.ereader.active_register & 0x7F
                        );
                    }
                }
            }
        } else if !ereader_control0_is_direction(control) {
            // Clear the error bit
            control = ereader_control0_clear_data(control);
        }
        self.ereader.register_control0 = control;
        if !ereader_control0_is_scan(old_control) && ereader_control0_is_scan(control) {
            if self.ereader.scan_x > 0 {
                self.e_reader_scan_card();
            }
            self.ereader.scan_x = 0;
            self.ereader.scan_y = 0;
        } else if ereader_control0_is_led_enable(control)
            && ereader_control0_is_scan(control)
            && !ereader_control1_is_scanline(self.ereader.register_control1)
        {
            self.e_reader_read_data();
        }
        mlog!(
            Level::Stub,
            rgba_core::log::GBA_HW,
            "Unimplemented e-Reader Control0 write: {:02X}",
            value
        );
    }

    /// _eReaderWriteControl1
    fn e_reader_write_control1(&mut self, value: u8) {
        let control = (value & 0x32) | 0x80;
        self.ereader.register_control1 = control;
        if ereader_control0_is_scan(self.ereader.register_control0)
            && !ereader_control1_is_scanline(control)
        {
            self.ereader.scan_y += 1;
            let lines =
                self.ereader.serial[0x15] as u16 | ((self.ereader.serial[0x14] as u16) << 8);
            if self.ereader.scan_y == lines as i32 {
                self.ereader.scan_y = 0;
                if self.ereader.scan_x < 4050 {
                    self.ereader.scan_x += 210;
                }
            }
            self.e_reader_read_data();
        }
        mlog!(
            Level::Stub,
            rgba_core::log::GBA_HW,
            "Unimplemented e-Reader Control1 write: {:02X}",
            value
        );
    }

    /// _eReaderReadData
    fn e_reader_read_data(&mut self) {
        for b in &mut self.ereader.data[..EREADER_BLOCK_SIZE] {
            *b = 0;
        }
        if self.ereader.dots.is_none() {
            self.e_reader_scan_card();
        }
        if let Some(dots) = self.ereader.dots.as_deref() {
            let y = self.ereader.scan_y - 10;
            if y < 0 || y >= 120 {
                for b in &mut self.ereader.data[..EREADER_BLOCK_SIZE] {
                    *b = 0;
                }
            } else {
                let origin = EREADER_DOTCODE_STRIDE * (y as usize / 3) + 16;
                for i in 0..20i32 {
                    let mut word: u16 = 0;
                    let x = self.ereader.scan_x + i * 16;
                    // The C reads the dots buffer directly; guard the (large
                    // scan_x) tail reads instead of doing C's out-of-bounds read.
                    let dot = |k: i32| -> u16 {
                        dots.get(origin + ((x + k) as usize) / 3)
                            .copied()
                            .unwrap_or(0) as u16
                    };
                    word |= dot(0) << 8;
                    word |= dot(1) << 9;
                    word |= dot(2) << 10;
                    word |= dot(3) << 11;
                    word |= dot(4) << 12;
                    word |= dot(5) << 13;
                    word |= dot(6) << 14;
                    word |= dot(7) << 15;
                    word |= dot(8);
                    word |= dot(9) << 1;
                    word |= dot(10) << 2;
                    word |= dot(11) << 3;
                    word |= dot(12) << 4;
                    word |= dot(13) << 5;
                    word |= dot(14) << 6;
                    word |= dot(15) << 7;
                    let offset = (19 - i) as usize * 2;
                    self.ereader.data[offset..offset + 2].copy_from_slice(&word.to_le_bytes());
                }
            }
        }
        self.ereader.register_control1 =
            ereader_control1_fill_scanline(self.ereader.register_control1);
        if ereader_control0_is_led_enable(self.ereader.register_control0) {
            // uint16_t led = ereader->registerLed * 2;
            let mut led = self.ereader.register_led.wrapping_mul(2);
            if led > 0x4000 {
                led = 0x4000;
            }
            // GBARaiseIRQ(ereader->p, GBA_IRQ_GAMEPAK, -led): the IF bit is set
            // and the IRQ event scheduled for GBA_IRQ_DELAY - cyclesLate, i.e.
            // 7 + led with the wrapping -led cyclesLate.
            self.memory.io[(GBA_REG_IF >> 1) as usize] |= 1 << GBA_IRQ_GAMEPAK;
            let cycles_late = 0u32.wrapping_sub(led as u32);
            if self.memory.io[(GBA_REG_IE >> 1) as usize]
                & self.memory.io[(GBA_REG_IF >> 1) as usize]
                != 0
                && !self.is_scheduled(EventId::IrqEvent)
            {
                self.schedule(EventId::IrqEvent, 7u32.wrapping_sub(cycles_late) as i32);
            }
        }
    }

    /// _eReaderScanCard
    fn e_reader_scan_card(&mut self) {
        if let Some(dots) = self.ereader.dots.as_mut() {
            for b in dots.iter_mut() {
                *b = 0;
            }
        }
        for i in 0..EREADER_CARDS_MAX {
            let card = match self.ereader.cards[i].take() {
                Some(card) => card,
                None => continue,
            };
            self.ereader_scan(&card.data);
            break;
        }
    }
}

fn e_reader_anchor(dots: &mut [u8], origin: usize) {
    dots[origin + EREADER_DOTCODE_STRIDE * 0 + 1] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 0 + 2] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 0 + 3] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 1 + 0] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 1 + 1] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 1 + 2] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 1 + 3] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 1 + 4] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 2 + 0] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 2 + 1] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 2 + 2] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 2 + 3] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 2 + 4] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 3 + 0] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 3 + 1] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 3 + 2] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 3 + 3] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 3 + 4] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 4 + 1] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 4 + 2] = 1;
    dots[origin + EREADER_DOTCODE_STRIDE * 4 + 3] = 1;
}

fn e_reader_alignment(dots: &mut [u8], origin: usize) {
    dots[origin + 8] = 1;
    dots[origin + 10] = 1;
    dots[origin + 12] = 1;
    dots[origin + 14] = 1;
    dots[origin + 16] = 1;
    dots[origin + 18] = 1;
    dots[origin + 21] = 1;
    dots[origin + 23] = 1;
    dots[origin + 25] = 1;
    dots[origin + 27] = 1;
    dots[origin + 29] = 1;
    dots[origin + 31] = 1;
}

fn e_reader_address(dots: &mut [u8], origin: usize, a: usize) {
    dots[origin + EREADER_DOTCODE_STRIDE * 7 + 2] = 1;
    let addr = EREADER_ADDRESS_CODES[a];
    for i in 0..16usize {
        dots[origin + EREADER_DOTCODE_STRIDE * (16 + i) + 2] = ((addr >> (15 - i)) & 1) as u8;
    }
}

/// _eReaderReedSolomon
fn e_reader_reed_solomon(input: &[u8], output: &mut [u8]) {
    let mut rs_buffer = [0u8; 64];
    for i in 0..48 {
        rs_buffer[63 - i] = input[i];
    }
    for i in 0..48 {
        let z = RS_REV[(rs_buffer[63 - i] ^ rs_buffer[15]) as usize] as u32;
        for j in (0..16usize).rev() {
            let mut x: u32 = 0;
            if j != 0 {
                x = rs_buffer[j - 1] as u32;
            }
            if z != 0xFF {
                let mut y = RS_GG[j] as u32;
                if y != 0xFF {
                    y += z;
                    if y >= 0xFF {
                        y -= 0xFF;
                    }
                    x ^= RS_POW[y as usize] as u32;
                }
            }
            rs_buffer[j] = x as u8;
        }
    }
    for i in 0..16 {
        output[15 - i] = !rs_buffer[i];
    }
}
