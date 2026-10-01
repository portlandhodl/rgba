// Copyright (c) 2013-2018 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gba/timer.c.

use rgba_core::timing::Timing;

use crate::gba::{EventId, Gba};
use crate::io::*;

pub const PRESCALE_TABLE: [u32; 4] = [0, 6, 8, 10];

// GBATimerFlags bit accessors
pub const TIMER_COUNT_UP: u32 = 0x10;
pub const TIMER_DO_IRQ: u32 = 0x20;
pub const TIMER_ENABLE: u32 = 0x40;
pub const TIMER_PRESCALE_MASK: u32 = 0xF;

pub struct Timer {
    pub reload: u16,
    pub last_event: i32,
    pub flags: u32,
}

impl Timer {
    pub fn new() -> Self {
        Timer { reload: 0, last_event: 0, flags: 0 }
    }
}

impl Default for Timer {
    fn default() -> Self { Self::new() }
}

impl Gba {
    /// GBATimerInit + Reset (both identical for state purposes)
    pub fn timers_reset(&mut self) {
        for t in self.timers.iter_mut() {
            t.reload = 0;
            t.last_event = 0;
            t.flags = 0;
        }
    }

    fn timer_event_id(id: i32) -> EventId {
        match id {
            0 => EventId::Timer0,
            1 => EventId::Timer1,
            2 => EventId::Timer2,
            _ => EventId::Timer3,
        }
    }

    /// GBATimerUpdate (event callback)
    pub fn timer_fired(&mut self, timer_id: i32, timing: &mut Timing, cycles_late: u32) {
        let t = timer_id as usize;
        if self.timers[t].flags & TIMER_COUNT_UP != 0 {
            self.memory.io[((GBA_REG_TM0CNT_LO as usize + (t << 2)) >> 1)] = self.timers[t].reload;
        } else {
            self.timer_update_register_late(timer_id, cycles_late);
        }

        if self.timers[t].flags & TIMER_DO_IRQ != 0 {
            self.raise_irq(3 + timer_id as u16);
        }

        if self.audio.enable && timer_id < 2 {
            if self.audio.ch_a_left_or_right() && self.audio.ch_a_timer() == timer_id as u32 {
                self.audio_sample_fifo(0, cycles_late);
            }
            if self.audio.ch_b_left_or_right() && self.audio.ch_b_timer() == timer_id as u32 {
                self.audio_sample_fifo(1, cycles_late);
            }
        }

        if timer_id < 3 {
            let n = timer_id as usize + 1;
            if self.timers[n].flags & TIMER_COUNT_UP != 0 && self.timers[n].flags & TIMER_ENABLE != 0 {
                let idx = ((GBA_REG_TM0CNT_LO as usize) + (n << 2)) >> 1;
                self.memory.io[idx] = self.memory.io[idx].wrapping_add(1);
                if self.memory.io[idx] == 0 {
                    self.timer_fired(n as i32, timing, cycles_late);
                }
            }
        }
    }

    /// GBATimerUpdateRegister
    pub fn timer_update_register(&mut self, timer: i32, cycles_late: i32) {
        self.timer_update_register_late(timer, cycles_late as u32);
    }

    fn timer_update_register_late(&mut self, timer: i32, cycles_late: u32) {
        let t = timer as usize;
        if self.timers[t].flags & TIMER_ENABLE == 0 || self.timers[t].flags & TIMER_COUNT_UP != 0 {
            return;
        }

        // Align timer
        let prescale_bits = self.timers[t].flags & TIMER_PRESCALE_MASK;
        let mut current_time = self.current_time() - cycles_late as i32;
        let tick_mask = (1i32 << prescale_bits) - 1;
        current_time &= !tick_mask;

        // Update register
        let mut tick_increment = current_time - self.timers[t].last_event;
        self.timers[t].last_event = current_time;
        tick_increment >>= prescale_bits;
        tick_increment += self.memory.io[((GBA_REG_TM0CNT_LO as usize) + (t << 2)) >> 1] as i32;
        while tick_increment >= 0x10000 {
            tick_increment -= 0x10000 - self.timers[t].reload as i32;
        }
        self.memory.io[((GBA_REG_TM0CNT_LO as usize) + (t << 2)) >> 1] = tick_increment as u16;

        // Schedule next update
        tick_increment = (0x10000 - tick_increment) << prescale_bits;
        current_time += tick_increment;
        current_time &= !tick_mask;
        let id = Self::timer_event_id(timer);
        self.deschedule(id);
        self.timing.set_relative_cycles(self.cpu.cycles);
        self.timing.schedule_absolute(id.into(), id.priority(), current_time);
    }

    /// GBATimerWriteTMCNT_LO
    pub fn timer_write_tmcnt_lo(&mut self, timer: i32, reload: u16) {
        self.timers[timer as usize].reload = reload;
    }

    /// GBATimerWriteTMCNT_HI
    pub fn timer_write_tmcnt_hi(&mut self, timer: i32, control: u16) {
        let t = timer as usize;
        self.timer_update_register_late(timer, 0);

        let prescale_bits = PRESCALE_TABLE[(control & 0x0003) as usize];
        let old_flags = self.timers[t].flags;
        let mut flags = self.timers[t].flags;
        flags = (flags & !TIMER_PRESCALE_MASK) | prescale_bits;
        flags = (flags & !TIMER_COUNT_UP) | (((timer > 0) && (control & 0x0004) != 0) as u32) << 4;
        flags = (flags & !TIMER_DO_IRQ) | ((control & 0x0040) as u32) >> 1;
        flags = (flags & !TIMER_ENABLE) | ((control & 0x0080) as u32) >> 1;
        self.timers[t].flags = flags;

        let mut reschedule = false;
        if old_flags & TIMER_ENABLE != flags & TIMER_ENABLE {
            reschedule = true;
            if flags & TIMER_ENABLE != 0 {
                self.memory.io[((GBA_REG_TM0CNT_LO as usize) + (t << 2)) >> 1] = self.timers[t].reload;
            }
        } else if old_flags & TIMER_COUNT_UP != flags & TIMER_COUNT_UP {
            reschedule = true;
        } else if old_flags & TIMER_PRESCALE_MASK != flags & TIMER_PRESCALE_MASK {
            reschedule = true;
        }

        if reschedule {
            let id = Self::timer_event_id(timer);
            self.deschedule(id);
            if flags & TIMER_ENABLE != 0 && flags & TIMER_COUNT_UP == 0 {
                let tick_mask = (1i32 << prescale_bits) - 1;
                self.timers[t].last_event = self.current_time() & !tick_mask;
                self.timer_update_register_late(timer, 0);
            }
        }
    }
}
