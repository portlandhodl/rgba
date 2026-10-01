// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gb/mbc/pocket-cam.c (Pocket Camera), with no camera
// image source (the C's memory->cam == NULL behavior).

use rgba_core::mlog;
use rgba_core::Level;

use crate::gb::Gb;
use crate::memory::{GB_SIZE_EXTERNAL_RAM, M_SAVEDATA_DIRT_NEW};

pub const GBCAM_WIDTH: usize = 128;
pub const GBCAM_HEIGHT: usize = 112;

#[derive(Clone)]
pub struct PocketCamState {
    pub registers_active: bool,
    pub registers: [u8; 0x36],
}

/// _GBPocketCam
pub fn pocket_cam(gb: &mut Gb, address: u16, value: u8) {
    let bank = (value & 0x3F) as i32;
    match address >> 13 {
        0x0 => match value {
            0 => {
                gb.memory.sram_access = false;
            }
            0xA => {
                gb.memory.sram_access = true;
                let b = gb.memory.sram_current_bank;
                gb.mbc_switch_sram_bank(b);
            }
            _ => {
                // TODO
                mlog!(
                    Level::Stub,
                    rgba_core::log::GB_MBC,
                    "Pocket Cam unknown value {:02X}",
                    value
                );
            }
        },
        0x1 => {
            gb.mbc_switch_bank(bank);
        }
        0x2 => {
            if value < 0x10 {
                gb.mbc_switch_sram_bank(value as i32);
                gb.memory.mbc_state.pocket_cam.registers_active = false;
                gb.memory.direct_sram_access = true;
            } else {
                gb.memory.mbc_state.pocket_cam.registers_active = true;
                gb.memory.direct_sram_access = false;
            }
        }
        0x5 => {
            if !gb.memory.mbc_state.pocket_cam.registers_active {
                return;
            }
            let mut value = value;
            let address = address & 0x7F;
            if address == 0 && value & 1 != 0 {
                value &= 6; // TODO: Timing
                gb.sram_dirty |= M_SAVEDATA_DIRT_NEW;
                pocket_cam_capture(gb);
            }
            if (address as usize) < 0x36 {
                gb.memory.mbc_state.pocket_cam.registers[address as usize] = value;
            }
        }
        _ => {
            mlog!(
                Level::Stub,
                rgba_core::log::GB_MBC,
                "Pocket Cam unknown address: {:04X}:{:02X}",
                address,
                value
            );
        }
    }
}

/// _GBPocketCamRead
pub fn pocket_cam_read(gb: &mut Gb, address: u16) -> u8 {
    if gb.memory.mbc_state.pocket_cam.registers_active {
        if address & 0x7F == 0 {
            return gb.memory.mbc_state.pocket_cam.registers[0];
        }
        return 0;
    }
    gb.memory
        .sram_byte(gb.memory.sram_bank_off + (address as usize & (GB_SIZE_EXTERNAL_RAM - 1)))
}

/// _GBPocketCamCapture with no camera: the C checks `memory->cam` first and
/// returns immediately when there is no image source, leaving SRAM untouched.
fn pocket_cam_capture(_gb: &mut Gb) {}

impl Default for PocketCamState {
    fn default() -> Self {
        PocketCamState { registers_active: false, registers: [0; 0x36] }
    }
}
