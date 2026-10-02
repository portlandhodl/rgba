// Copyright (c) 2013-2015 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gba/dma.c and include/mgba/internal/gba/dma.h.

use rgba_core::{mlog, Level};

use crate::gba::{EventId, Gba};
use crate::io::*;
use crate::memory::*;
use crate::arm::{ExecutionMode, ARM_PC, WORD_SIZE_ARM, WORD_SIZE_THUMB};
use crate::savedata::SavedataType;
use crate::video::VIDEO_VERTICAL_PIXELS;

pub const GBA_DMA_INCREMENT: i32 = 0;
pub const GBA_DMA_DECREMENT: i32 = 1;
pub const GBA_DMA_FIXED: i32 = 2;
pub const GBA_DMA_INCREMENT_RELOAD: i32 = 3;

pub const GBA_DMA_TIMING_NOW: i32 = 0;
pub const GBA_DMA_TIMING_VBLANK: i32 = 1;
pub const GBA_DMA_TIMING_HBLANK: i32 = 2;
pub const GBA_DMA_TIMING_CUSTOM: i32 = 3;

pub const DMA_OFFSET: [i32; 4] = [1, -1, 0, 1];

pub const DMA_SRC_MASK: [u32; 4] = [0x07FFFFFE, 0x0FFFFFFE, 0x0FFFFFFE, 0x0FFFFFFE];
pub const DMA_DST_MASK: [u32; 4] = [0x07FFFFFE, 0x07FFFFFE, 0x07FFFFFE, 0x0FFFFFFE];

// GBADMARegister bit accesses
#[inline]
pub fn dma_dest_control(reg: u16) -> i32 {
    ((reg >> 5) & 3) as i32
}
#[inline]
pub fn dma_src_control(reg: u16) -> i32 {
    ((reg >> 7) & 3) as i32
}
#[inline]
pub fn dma_repeat(reg: u16) -> bool {
    reg & 0x200 != 0
}
#[inline]
pub fn dma_width(reg: u16) -> i32 {
    ((reg >> 10) & 1) as i32
}
#[inline]
pub fn dma_drq(reg: u16) -> bool {
    reg & 0x800 != 0
}
#[inline]
pub fn dma_timing(reg: u16) -> i32 {
    ((reg >> 12) & 3) as i32
}
#[inline]
pub fn dma_do_irq(reg: u16) -> bool {
    reg & 0x4000 != 0
}
#[inline]
pub fn dma_enable(reg: u16) -> bool {
    reg & 0x8000 != 0
}


pub struct Dma {
    pub reg: u16,
    pub source: u32,
    pub dest: u32,
    pub count: i32,
    pub next_source: u32,
    pub next_dest: u32,
    pub next_count: i32,
    pub when: i32,
    pub cycles: i32,
    pub latch: u32,
    pub source_offset: i32,
    pub dest_offset: i32,
}
impl Dma {
    pub fn new() -> Self {
        Dma { reg: 0, source: 0, dest: 0, count: 0, next_source: 0, next_dest: 0, next_count: 0, when: 0, cycles: 0, latch: 0, source_offset: 0, dest_offset: 0 }
    }
}
impl Default for Dma { fn default() -> Self { Self::new() } }

impl Gba {
    /// GBADMAReset
    pub fn dma_reset(&mut self) {
        for d in self.dma.iter_mut() {
            *d = Dma::new();
        }
        for i in 0..4 {
            self.dma[i].count = 0x4000;
            self.dma[i].latch = 0;
        }
        self.dma[3].count = 0x10000;
        self.active_dma = -1;
    }

    fn dma_is_valid_sad(dma: i32, address: u32) -> bool {
        if dma == 0 && address >= GBA_BASE_ROM0 && address < GBA_BASE_SRAM {
            return false;
        }
        address >= GBA_BASE_EWRAM
    }

    fn dma_is_valid_dad(dma: i32, address: u32) -> bool {
        dma == 3 || address < GBA_BASE_ROM0
    }

    pub fn dma_write_sad(&mut self, dma: i32, address: u32) -> u32 {
        if !Gba::dma_is_valid_sad(dma, address) {
            mlog!(Level::GameError, rgba_core::log::GBA_DMA, "Invalid DMA source address: 0x{:08X}", address);
        }
        self.dma[dma as usize].source = address & DMA_SRC_MASK[dma as usize];
        self.dma[dma as usize].source
    }

    pub fn dma_write_dad(&mut self, dma: i32, address: u32) -> u32 {
        if !Gba::dma_is_valid_dad(dma, address) {
            mlog!(Level::GameError, rgba_core::log::GBA_DMA, "Invalid DMA destination address: 0x{:08X}", address);
        }
        self.dma[dma as usize].dest = address & DMA_DST_MASK[dma as usize];
        self.dma[dma as usize].dest
    }

    fn dma_reload(&mut self, dma: usize) {
        let reg_lo = (GBA_REG_DMA0CNT_LO as usize + dma * (GBA_REG_DMA1CNT_LO - GBA_REG_DMA0CNT_LO) as usize) >> 1;
        self.dma[dma].count = self.memory.io[reg_lo] as i32;
        if dma == 3 {
            if self.dma[dma].count == 0 {
                self.dma[dma].count = 0x10000;
            }
        } else {
            self.dma[dma].count &= 0x3FFF;
            if self.dma[dma].count == 0 {
                self.dma[dma].count = 0x4000;
            }
        }
    }

    pub fn dma_write_cnt_hi(&mut self, dma: i32, control: u16) -> u16 {
        let d = dma as usize;
        let was_enabled = dma_enable(self.dma[d].reg);
        let control = if dma < 3 { control & 0xF7E0 } else { control & 0xFFE0 };
        self.dma[d].reg = control;

        let width = 2i64 << dma_width(self.dma[d].reg) as u32;
        let width = width as i32;
        if self.dma[d].source >= GBA_BASE_ROM0 && self.dma[d].source < GBA_BASE_SRAM {
            self.dma[d].source_offset = width;
        } else {
            self.dma[d].source_offset = DMA_OFFSET[dma_src_control(self.dma[d].reg) as usize] * width;
        }
        self.dma[d].dest_offset = DMA_OFFSET[dma_dest_control(self.dma[d].reg) as usize] * width;

        if dma_drq(self.dma[d].reg) {
            mlog!(Level::Stub, rgba_core::log::GBA_DMA, "DRQ not implemented");
        }

        if !was_enabled && dma_enable(self.dma[d].reg) {
            self.dma[d].next_source = self.dma[d].source;
            self.dma[d].next_dest = self.dma[d].dest;
            self.dma_reload(d);

            if self.dma[d].next_source as i32 & (width - 1) != 0 {
                mlog!(Level::GameError, rgba_core::log::GBA_DMA, "Misaligned DMA source address: 0x{:08X}", self.dma[d].next_source);
            }
            if self.dma[d].next_dest as i32 & (width - 1) != 0 {
                mlog!(Level::GameError, rgba_core::log::GBA_DMA, "Misaligned DMA destination address: 0x{:08X}", self.dma[d].next_dest);
            }
            mlog!(Level::Info, rgba_core::log::GBA_DMA, "Starting DMA {} 0x{:08X} -> 0x{:08X} ({:04X}:{:04X})",
                dma, self.dma[d].next_source, self.dma[d].next_dest, self.dma[d].reg, (self.dma[d].count & 0xFFFF) as u16);

            self.dma[d].next_source &= !(width as u32 - 1);
            self.dma[d].next_dest &= !(width as u32 - 1);

            if dma_timing(self.dma[d].reg) == GBA_DMA_TIMING_NOW {
                self.dma[d].next_count = self.dma[d].count;
                self.dma[d].when = self.current_time() + 3; // DMAs take 3 cycles to start
                self.dma_update();
            }
        }
        self.dma[d].reg
    }

    /// GBADMARunHblank
    pub fn dma_run_hblank_all(&mut self, cycles_late: u32) {
        let mut found = false;
        let now = self.current_time();
        for i in 0..4 {
            if dma_enable(self.dma[i].reg) && dma_timing(self.dma[i].reg) == GBA_DMA_TIMING_HBLANK {
                self.dma[i].when = now + 3 + cycles_late as i32;
                if self.dma[i].next_count == 0 {
                    self.dma[i].next_count = self.dma[i].count;
                }
                found = true;
            }
        }
        if found {
            self.dma_update();
        }
    }

    /// GBADMARunVblank
    pub fn dma_run_vblank_all(&mut self, cycles_late: u32) {
        let mut found = false;
        let now = self.current_time();
        for i in 0..4 {
            if dma_enable(self.dma[i].reg) && dma_timing(self.dma[i].reg) == GBA_DMA_TIMING_VBLANK {
                self.dma[i].when = now + 3 + cycles_late as i32;
                if self.dma[i].next_count == 0 {
                    self.dma[i].next_count = self.dma[i].count;
                }
                found = true;
            }
        }
        if found {
            self.dma_update();
        }
    }

    /// GBADMARunDisplayStart
    pub fn dma_run_display_start_all(&mut self, cycles_late: u32) {
        let now = self.current_time();
        if dma_enable(self.dma[3].reg) && dma_timing(self.dma[3].reg) == GBA_DMA_TIMING_CUSTOM {
            self.dma[3].when = now + 3 + cycles_late as i32;
            if self.dma[3].next_count == 0 {
                self.dma[3].next_count = self.dma[3].count;
            }
            self.dma_update();
        }
    }

    /// The actual DMA granularity event
    pub fn dma_event(&mut self, _dma: usize, _cycles_late: i32) {
        let active = self.active_dma;
        if active < 0 {
            return;
        }
        let now = self.current_time();
        {
            let dma = &mut self.dma[active as usize];
            if dma.next_count == dma.count {
                dma.when = now;
            }
        }
        self.active_dma = active;
        if self.dma[active as usize].next_count & 0xFFFFF != 0 {
            self.dma_service(active as usize);
        } else {
            self.dma[active as usize].next_count = 0;
            let mut no_repeat = !dma_repeat(self.dma[active as usize].reg);
            no_repeat |= dma_timing(self.dma[active as usize].reg) == GBA_DMA_TIMING_NOW;
            no_repeat |= active == 3
                && dma_timing(self.dma[active as usize].reg) == GBA_DMA_TIMING_CUSTOM
                && self.video.vcount == VIDEO_VERTICAL_PIXELS + 1;
            if no_repeat {
                self.dma[active as usize].reg &= !0x8000;

                let reg_hi = (GBA_REG_DMA0CNT_HI as usize + active as usize * (GBA_REG_DMA1CNT_HI - GBA_REG_DMA0CNT_HI) as usize) >> 1;
                self.memory.io[reg_hi] &= 0x7FE0;
            } else {
                self.dma_reload(active as usize);
            }
            if dma_dest_control(self.dma[active as usize].reg) == GBA_DMA_INCREMENT_RELOAD {
                self.dma[active as usize].next_dest = self.dma[active as usize].dest;
            }
            if dma_do_irq(self.dma[active as usize].reg) {
                self.raise_irq(8 + active as u16);
            }
            self.dma_update();
        }
    }

    /// GBADMAUpdate
    pub fn dma_update(&mut self) {
        let current_time = self.current_time();
        let mut least_time = i32::MAX;
        self.active_dma = -1;
        for i in 0..4 {
            if dma_enable(self.dma[i].reg) && self.dma[i].next_count != 0 {
                let time = self.dma[i].when.wrapping_sub(current_time);
                if self.active_dma == -1 || time < least_time {
                    least_time = time;
                    self.active_dma = i as i32;
                }
            }
        }
        if self.active_dma >= 0 {
            self.dma_pc = self.cpu.gprs[ARM_PC] as u32;
            self.deschedule(EventId::Dma);
            let d_when = self.dma[self.active_dma as usize].when;
            self.dma_event_id = self.active_dma as u32;
            self.schedule(EventId::Dma, d_when - current_time);
        } else {
            self.cpu_blocked = false;
        }
    }

    /// GBADMAService
    pub fn dma_service(&mut self, number: usize) {
        let width = 2i32 << dma_width(self.dma[number].reg) as u32;
        let width = width;
        let source = self.dma[number].next_source;
        let dest = self.dma[number].next_dest;
        let source_region = source >> BASE_OFFSET;
        let dest_region = dest >> BASE_OFFSET;
        let mut cycles: i32 = 2;

        self.cpu_blocked = true;
        self.performing_dma = 1 | ((number as i32) << 1);

        if self.dma[number].count == self.dma[number].next_count {
            if width == 4 {
                let sr = source_region as usize;
                let dr = dest_region as usize;
                cycles += self.memory.waitstates_nonseq32[sr] + self.memory.waitstates_nonseq32[dr];
                let c = self.memory.waitstates_seq32[sr] + self.memory.waitstates_seq32[dr];
                self.dma[number].cycles = c;
            } else {
                if source >= GBA_BASE_EWRAM {
                    let mut cc = 0;
                    self.dma[number].latch = self.load32(source, &mut cc);
                }
                let sr = source_region as usize;
                let dr = dest_region as usize;
                cycles += self.memory.waitstates_nonseq16[sr] + self.memory.waitstates_nonseq16[dr];
                let c = self.memory.waitstates_seq16[sr] + self.memory.waitstates_seq16[dr];
                self.dma[number].cycles = c;
            }
        } else {
            cycles += self.dma[number].cycles;
        }
        self.dma[number].when += cycles;

        if width == 4 {
            if source >= GBA_BASE_EWRAM {
                let mut cc = 0;
                self.dma[number].latch = self.load32(source, &mut cc);
            }
            let latch = self.dma[number].latch as i32;
            self.store32(dest, latch, &mut 0);
            self.bus = latch as u32;
        } else {
            if source_region == GBA_REGION_ROM2_EX
                && (self.savedata.savedata_type == SavedataType::Eeprom
                    || self.savedata.savedata_type == SavedataType::Eeprom512)
            {
                let v = self.savedata_read_eeprom() as u32;
                self.dma[number].latch = v | (v << 16);
            } else if source >= GBA_BASE_EWRAM {
                let mut cc = 0;
                let v = self.load16(source, &mut cc);
                self.dma[number].latch = v | (v << 16);
            }
            if dest_region == GBA_REGION_ROM2_EX {
                if self.savedata.savedata_type == SavedataType::Autodetect {
                    mlog!(Level::Info, rgba_core::log::GBA_MEM, "Detected EEPROM savegame");
                    self.savedata_init_eeprom();
                }
                if self.savedata.savedata_type == SavedataType::Eeprom512
                    || self.savedata.savedata_type == SavedataType::Eeprom
                {
                    let latch = (self.dma[number].latch as u16) as u16;
                    self.savedata_write_eeprom(latch, self.dma[number].next_count as u32);
                }
            } else {
                let v = (self.dma[number].latch >> (8 * (dest & 2))) as i32;
                self.store16(dest, v, &mut 0);
            }
            let latch = self.dma[number].latch;
            self.bus = (latch & 0xFFFF) | (latch << 16);
        }

        self.dma[number].next_source = self.dma[number].next_source.wrapping_add(self.dma[number].source_offset as u32);
        self.dma[number].next_dest = self.dma[number].next_dest.wrapping_add(self.dma[number].dest_offset as u32);
        if source_region != (self.dma[number].next_source >> BASE_OFFSET)
            || dest_region != (self.dma[number].next_dest >> BASE_OFFSET)
        {
            // Crossed region boundary
            if self.dma[number].next_source >= GBA_BASE_ROM0
                && self.dma[number].next_source < GBA_BASE_SRAM
            {
                self.dma[number].source_offset = width;
            } else {
                self.dma[number].source_offset =
                    DMA_OFFSET[dma_src_control(self.dma[number].reg) as usize] * width;
            }

            // Recalculate cached cycles
            if width == 4 {
                let sr = (self.dma[number].next_source >> BASE_OFFSET) as usize;
                let dr = (self.dma[number].next_dest >> BASE_OFFSET) as usize;
                self.dma[number].cycles =
                    self.memory.waitstates_seq32[sr] + self.memory.waitstates_seq32[dr];
            } else {
                let sr = (self.dma[number].next_source >> BASE_OFFSET) as usize;
                let dr = (self.dma[number].next_dest >> BASE_OFFSET) as usize;
                self.dma[number].cycles =
                    self.memory.waitstates_seq16[sr] + self.memory.waitstates_seq16[dr];
            }
        }
        self.dma[number].next_count -= 1;

        self.performing_dma = 0;

        for i in 0..4usize {
            if dma_enable(self.dma[i].reg) && self.dma[i].next_count != 0 {
                let time = self.dma[i].when.wrapping_sub(self.dma[number].when);
                if time < 0 {
                    self.dma[i].when = self.dma[number].when;
                }
            }
        }

        if self.dma[number].next_count == 0 {
            self.dma[number].next_count |= 0x80000000u32 as i32;
            if source_region < GBA_REGION_ROM0 || dest_region < GBA_REGION_ROM0 {
                self.dma[number].when += 2;
            }
        }
        self.dma_update();
    }

    /// GBADMARecalculateCycles
    pub fn dma_recalculate_cycles(&mut self) {
        for i in 0..4usize {
            if !dma_enable(self.dma[i].reg) {
                continue;
            }
            let width = dma_width(self.dma[i].reg);
            let sr = (self.dma[i].next_source >> BASE_OFFSET) as usize;
            let dr = (self.dma[i].next_dest >> BASE_OFFSET) as usize;
            if width != 0 {
                self.dma[i].cycles =
                    self.memory.waitstates_seq32[sr] + self.memory.waitstates_seq32[dr];
            } else {
                self.dma[i].cycles =
                    self.memory.waitstates_seq16[sr] + self.memory.waitstates_seq16[dr];
            }
        }
    }
}
