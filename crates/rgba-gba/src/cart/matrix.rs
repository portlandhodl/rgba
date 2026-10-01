// Copyright (c) 2013-2018 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gba/cart/matrix.c.

use rgba_core::{mlog, Level};

use crate::gba::Gba;
use crate::memory::GBA_SIZE_ROM0;

pub const GBA_MATRIX_MAPPINGS_MAX: usize = 16;
const MAPPING_MASK: u32 = (GBA_MATRIX_MAPPINGS_MAX - 1) as u32;

/// struct GBAMatrix
pub struct Matrix {
    pub cmd: u32,
    pub paddr: u32,
    pub vaddr: u32,
    pub size: u32,

    pub mappings: [u32; GBA_MATRIX_MAPPINGS_MAX],

    /// The full ROM image for 'M'-ident >32MiB dumps; C streams this through
    /// romVf in _remapMatrix. Empty for non-Matrix carts (matrix.size == 0
    /// keeps the register writes inactive).
    pub rom: Vec<u8>,
}

impl Matrix {
    pub fn new() -> Self {
        Matrix {
            cmd: 0,
            paddr: 0,
            vaddr: 0,
            size: 0,
            mappings: [0; GBA_MATRIX_MAPPINGS_MAX],
            rom: Vec::new(),
        }
    }

    /// True once a Matrix ROM image was attached at load time (C: romVf size
    /// > GBA_SIZE_ROM0 && ident == 'M', checked per reset).
    pub fn has_rom(&self) -> bool {
        self.rom.len() > GBA_SIZE_ROM0
    }
}

impl Default for Matrix {
    fn default() -> Self {
        Self::new()
    }
}

impl Gba {
    /// matrix.size != 0 gates the cart register writes in memory.c.
    pub fn matrix_active(&self) -> bool {
        self.matrix.size != 0
    }

    /// Attach the full backing image (gba.c GBALoadROM's 'M'-ident branch).
    pub fn matrix_attach_rom(&mut self, rom: Vec<u8>) {
        self.matrix.rom = rom;
    }

    /// GBAMatrixReset
    pub fn matrix_reset(&mut self) {
        self.matrix.mappings = [0; GBA_MATRIX_MAPPINGS_MAX];
        self.matrix.size = 0x1000;

        self.matrix.paddr = 0;
        self.matrix.vaddr = 0;
        self.remap_matrix();
        self.matrix.paddr = 0x200;
        self.matrix.vaddr = 0x1000;
        self.remap_matrix();
    }

    /// _remapMatrix
    fn remap_matrix(&mut self) {
        if self.matrix.vaddr & 0xFFFFE1FF != 0 {
            mlog!(
                Level::Error,
                rgba_core::log::GBA_MEM,
                "Invalid Matrix mapping: {:08X}",
                self.matrix.vaddr
            );
            return;
        }
        if self.matrix.size & 0xFFFFE1FF != 0 {
            mlog!(
                Level::Error,
                rgba_core::log::GBA_MEM,
                "Invalid Matrix size: {:08X}",
                self.matrix.size
            );
            return;
        }
        if self
            .matrix
            .vaddr
            .wrapping_add(self.matrix.size)
            .wrapping_sub(1)
            & 0xFFFFE000
            != 0
        {
            mlog!(
                Level::Error,
                rgba_core::log::GBA_MEM,
                "Invalid Matrix mapping end: {:08X}",
                self.matrix.vaddr.wrapping_add(self.matrix.size)
            );
            return;
        }
        let start = self.matrix.vaddr >> 9;
        let size = (self.matrix.size >> 9) & MAPPING_MASK;
        for i in 0..size {
            self.matrix.mappings[((start + i) & MAPPING_MASK) as usize] =
                self.matrix.paddr.wrapping_add(i << 9);
        }

        // gba->romVf->read(&gba->memory.rom[gba->memory.matrix.vaddr >> 2],
        // gba->memory.matrix.size) — memory.rom is uint32_t* in the C, so the
        // destination byte offset is vaddr.
        let src = self.matrix.paddr as usize;
        let dst = self.matrix.vaddr as usize;
        let mut n = self.matrix.size as usize;
        n = n.min(self.matrix.rom.len().saturating_sub(src));
        n = n.min(self.memory.rom.len().saturating_sub(dst));
        if n > 0 {
            let data = self.matrix.rom[src..src + n].to_vec();
            self.memory.rom[dst..dst + n].copy_from_slice(&data);
        }
    }

    /// GBAMatrixWrite
    pub fn matrix_write(&mut self, address: u32, value: u32) {
        match address {
            0x0 => {
                self.matrix.cmd = value;
                match value {
                    0x01 | 0x11 => {
                        self.remap_matrix();
                    }
                    _ => {
                        mlog!(
                            Level::Stub,
                            rgba_core::log::GBA_MEM,
                            "Unknown Matrix command: {:08X}",
                            value
                        );
                    }
                }
            }
            0x4 => {
                self.matrix.paddr = value & 0x03FFFFFF;
            }
            0x8 => {
                self.matrix.vaddr = value & 0x007FFFFF;
            }
            0xC => {
                if value == 0 {
                    mlog!(
                        Level::Error,
                        rgba_core::log::GBA_MEM,
                        "Rejecting Matrix write for size 0"
                    );
                    return;
                }
                self.matrix.size = value << 9;
            }
            _ => {
                mlog!(
                    Level::Stub,
                    rgba_core::log::GBA_MEM,
                    "Unknown Matrix write: {:08X}:{:04X}",
                    address,
                    value
                );
            }
        }
    }

    /// GBAMatrixWrite16
    pub fn matrix_write16(&mut self, address: u32, value: u16) {
        match address {
            0x0 => {
                self.matrix_write(address, value as u32 | (self.matrix.cmd & 0xFFFF0000));
            }
            0x4 => {
                self.matrix_write(address, value as u32 | (self.matrix.paddr & 0xFFFF0000));
            }
            0x8 => {
                self.matrix_write(address, value as u32 | (self.matrix.vaddr & 0xFFFF0000));
            }
            0xC => {
                self.matrix_write(address, value as u32 | (self.matrix.size & 0xFFFF0000));
            }
            _ => {}
        }
    }
}
