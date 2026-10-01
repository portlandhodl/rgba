// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gb/timer.c and include/mgba/internal/gb/timer.h.

use rgba_core::timing::Timing;

use crate::gb::{EventId, Gb};
use crate::io::{GB_IRQ_TIMER, GB_REG_DIV, GB_REG_IF, GB_REG_TIMA, GB_REG_TMA};

pub const GB_DMG_DIV_PERIOD: i32 = 16;

pub struct Timer {
    pub internal_div: u32,
    pub next_div: i32,
    pub tima_period: u32,
}

impl Timer {
    pub fn new() -> Self {
        Timer {
            internal_div: 0,
            next_div: GB_DMG_DIV_PERIOD * 2,
            tima_period: 1024 >> 4,
        }
    }
}

impl Default for Timer {
    fn default() -> Self {
        Self::new()
    }
}

impl Gb {
    /// _GBTimerIRQ
    pub fn timer_irq_event(&mut self, _timing: &mut Timing, _cycles_late: i32) {
        self.memory.io[GB_REG_TIMA as usize] = self.memory.io[GB_REG_TMA as usize];
        self.memory.io[GB_REG_IF as usize] |= 1 << GB_IRQ_TIMER;
        self.update_irqs();
    }

    fn timer_div_increment(&mut self, timing: &mut Timing, cycles_late: u32) {
        let t_multiplier = 2 - self.double_speed as i32;
        while self.timer.next_div >= GB_DMG_DIV_PERIOD * t_multiplier {
            self.timer.next_div -= GB_DMG_DIV_PERIOD * t_multiplier;

            // Make sure to trigger when the correct bit is a falling edge
            if self.timer.tima_period > 0
                && (self.timer.internal_div & (self.timer.tima_period - 1))
                    == self.timer.tima_period - 1
            {
                let tima = &mut self.memory.io[GB_REG_TIMA as usize];
                *tima = tima.wrapping_add(1);
                if self.memory.io[GB_REG_TIMA as usize] == 0 {
                    timing.schedule(
                        EventId::TimerIrq.into(),
                        EventId::TimerIrq.priority(),
                        7 * t_multiplier
                            - ((self.cpu.execution_state * t_multiplier - cycles_late as i32)
                                & (3 * t_multiplier)),
                    );
                }
            }
            let timing_factor = (0x200u32 << self.double_speed as u32) - 1;
            if (self.timer.internal_div & timing_factor) == timing_factor {
                self.audio_update_frame();
            }
            self.timer.internal_div = self.timer.internal_div.wrapping_add(1);
            self.memory.io[GB_REG_DIV as usize] = (self.timer.internal_div >> 4) as u8;
        }
    }

    /// _GBTimerUpdate (the timer event callback)
    pub fn timer_event(&mut self, timing: &mut Timing, cycles_late: i32) {
        self.timer.next_div += cycles_late;
        self.timer_div_increment(timing, cycles_late as u32);
        // Batch div increments
        let mut divs_to_go: i32 = 16 - (self.timer.internal_div & 15) as i32;
        let mut tima_to_go = i32::MAX;
        if self.timer.tima_period != 0 {
            tima_to_go =
                (self.timer.tima_period - (self.timer.internal_div & (self.timer.tima_period - 1)))
                    as i32;
        }
        if tima_to_go < divs_to_go {
            divs_to_go = tima_to_go;
        }
        self.timer.next_div = GB_DMG_DIV_PERIOD * divs_to_go * (2 - self.double_speed as i32);
        let next = self.timer.next_div;
        timing.schedule(
            EventId::Timer.into(),
            EventId::Timer.priority(),
            next - cycles_late,
        );
    }

    /// GBTimerReset
    pub fn timer_reset(&mut self) {
        self.timer.next_div = GB_DMG_DIV_PERIOD * 2;
        self.timer.tima_period = 1024 >> 4;
    }

    /// GBTimerDivReset
    pub fn timer_div_reset(&mut self) {
        let until = self.until(EventId::Timer);
        self.timer.next_div -= until;
        self.deschedule(EventId::Timer);
        self.with_timing(|gb, timing| {
            gb.timer_div_increment(timing, 0);
        });
        let t_multiplier = 2 - self.double_speed as i32;
        if (((self.timer.internal_div << 1)
            | (((self.timer.next_div >> (4 - self.double_speed as i32)) & 1) as u32))
            & self.timer.tima_period)
            != 0
        {
            let tima = &mut self.memory.io[GB_REG_TIMA as usize];
            *tima = tima.wrapping_add(1);
            if self.memory.io[GB_REG_TIMA as usize] == 0 {
                let t = (7 - (self.cpu.execution_state & 3)) * t_multiplier;
                self.schedule(EventId::TimerIrq, t);
            }
        }
        if self.timer.internal_div & (0x200 << self.double_speed as u32) != 0 {
            self.audio_update_frame();
        }
        self.memory.io[GB_REG_DIV as usize] = 0;
        self.timer.internal_div = 0;
        self.timer.next_div = GB_DMG_DIV_PERIOD * t_multiplier;
        let next = self.timer.next_div - ((self.cpu.execution_state + 1) & 3) * t_multiplier;
        self.schedule(EventId::Timer, next);
    }

    /// GBTimerUpdateTAC; returns the possibly-modified TAC value.
    pub fn timer_update_tac(&mut self, tac: u8) -> u8 {
        if tac & 0x4 != 0 {
            // GBRegisterTACIsRun
            let until = self.until(EventId::Timer);
            self.timer.next_div -= until;
            self.deschedule(EventId::Timer);
            let late = ((self.cpu.execution_state + 2) & 3) * (2 - self.double_speed as i32);
            self.with_timing(|gb, timing| {
                gb.timer_div_increment(timing, late as u32);
            });

            self.timer.tima_period = match tac & 3 {
                0 => 1024 >> 4,
                1 => 16 >> 4,
                2 => 64 >> 4,
                _ => 256 >> 4,
            };

            self.timer.next_div += GB_DMG_DIV_PERIOD * (2 - self.double_speed as i32);
            let next = self.timer.next_div;
            self.schedule(EventId::Timer, next);
        } else {
            self.timer.tima_period = 0;
        }
        tac
    }
}
