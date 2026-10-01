// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gba/video.c and include/mgba/internal/gba/video.h.
// The software renderer lives in renderers.rs.

pub mod renderers;

use rgba_core::timing::Timing;
use rgba_core::{mlog, Level};

use crate::gba::{EventId, Gba};
use crate::io::*;
use crate::memory::GBA_SIZE_VRAM;

pub const GBA_SIZE_OAM: usize = 0x400;

pub const VIDEO_HBLANK_PIXELS: i32 = 68;
pub const VIDEO_HDRAW_LENGTH: i32 = 1008;
pub const VIDEO_HBLANK_LENGTH: i32 = 224;
pub const VIDEO_HORIZONTAL_LENGTH: i32 = 1232;
pub const VIDEO_VERTICAL_TOTAL_PIXELS: i32 = 228;
pub const VIDEO_VBLANK_PIXELS: i32 = 68;
pub const VIDEO_TOTAL_LENGTH: i32 = 280896;
pub const VIDEO_HORIZONTAL_PIXELS: i32 = 240;
pub const VIDEO_VERTICAL_PIXELS: i32 = 160;
pub const VIDEO_OAM_BLANK_PIXELS: i32 = 954;
pub const VIDEO_OBJ_LENGTH: i32 = 1210;
pub const BASE_TILE: u32 = 0x00010000;

pub const GBA_VSTALL_T4_0: u32 = 0x011;
pub const GBA_VSTALL_T4_1: u32 = 0x022;
pub const GBA_VSTALL_T4_2: u32 = 0x044;
pub const GBA_VSTALL_T4_3: u32 = 0x088;
pub const GBA_VSTALL_T8_0: u32 = 0x010;
pub const GBA_VSTALL_T8_1: u32 = 0x020;
pub const GBA_VSTALL_T8_2: u32 = 0x040;
pub const GBA_VSTALL_T8_3: u32 = 0x080;
pub const GBA_VSTALL_A2: u32 = 0x100;
pub const GBA_VSTALL_A3: u32 = 0x200;
pub const GBA_VSTALL_B: u32 = 0x400;

use renderers::SwVideo;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VideoEventKind {

    StartHdraw,
    StartHblank,
}

pub struct Video {
    pub palette: Vec<u8>, // 0x400 bytes
    pub vram: Vec<u8>,    // GBA_SIZE_VRAM
    pub oam: Vec<u8>,     // 0x400
    pub vcount: i32,
    pub stall_mask: u32,
    pub frame_counter: u32,
    pub frameskip: i32,
    pub frameskip_counter: i32,
    pub event_kind: VideoEventKind,

    pub sw: SwVideo,
}

impl Video {
    pub fn new() -> Self {
        Video {
            palette: vec![0; 0x400],
            vram: vec![0; GBA_SIZE_VRAM],
            oam: vec![0; GBA_SIZE_OAM],
            vcount: 0,
            stall_mask: 0,
            frame_counter: 0,
            frameskip: 0,
            frameskip_counter: 0,
            event_kind: VideoEventKind::StartHblank,
            sw: SwVideo::new(),
        }
    }
}

impl Default for Video {
    fn default() -> Self {
        Self::new()
    }
}

// DISPCNT bitfield accessors
#[inline]
pub fn dispcnt_mode(v: u16) -> u16 {
    v & 7
}
#[inline]
pub fn dispcnt_forced_blank(v: u16) -> bool {
    v & 0x80 != 0
}
#[inline]
pub fn dispcnt_obj_char_mapping(v: u16) -> bool {
    v & 0x40 != 0
}
#[inline]
pub fn dispcnt_hblank_free(v: u16) -> bool {
    v & 0x20 != 0
}
#[inline]
pub fn dispcnt_frame_select(v: u16) -> bool {
    v & 0x10 != 0
}
#[inline]
pub fn dispcnt_cgb(v: u16) -> bool {
    v & 0x8 != 0
}

fn dispcnt_bg_enable(v: u16, bg: i32) -> bool {
    v & (0x100 << bg) != 0
}
fn dispcnt_obj_enable(v: u16) -> bool {
    v & 0x1000 != 0
}
pub fn dispcnt_win0_enable(v: u16) -> bool {
    v & 0x2000 != 0
}
pub fn dispcnt_win1_enable(v: u16) -> bool {
    v & 0x4000 != 0
}
pub fn dispcnt_objwin_enable(v: u16) -> bool {
    v & 0x8000 != 0
}

// BGCNT bitfield accessors
#[inline]
pub fn bgcnt_is_256color(v: u16) -> bool {
    v & 0x80 != 0
}
#[inline]
pub fn bgcnt_priority(v: u16) -> u16 {
    v & 3
}
#[inline]
pub fn bgcnt_char_base(v: u16) -> u16 {
    (v >> 2) & 3
}
#[inline]
pub fn bgcnt_mosaic(v: u16) -> bool {
    v & 0x40 != 0
}
#[inline]
pub fn bgcnt_screen_base(v: u16) -> u16 {
    (v >> 8) & 0x1F
}
#[inline]
pub fn bgcnt_overflow(v: u16) -> bool {
    v & 0x2000 != 0
}
#[inline]
pub fn bgcnt_size(v: u16) -> u16 {
    (v >> 14) & 3
}

// DISPSTAT bitfield accessors
#[inline]
pub fn dispstat_in_vblank(v: u16) -> bool {
    v & 1 != 0
}
#[inline]
pub fn dispstat_in_hblank(v: u16) -> bool {
    v & 2 != 0
}
#[inline]
pub fn dispstat_vcounter(v: u16) -> bool {
    v & 4 != 0
}
#[inline]
pub fn dispstat_vblank_irq(v: u16) -> bool {
    v & 8 != 0
}
#[inline]
pub fn dispstat_hblank_irq(v: u16) -> bool {
    v & 0x10 != 0
}
#[inline]
pub fn dispstat_vcounter_irq(v: u16) -> bool {
    v & 0x20 != 0
}
#[inline]
pub fn dispstat_vcount_setting(v: u16) -> u16 {
    (v >> 8) & 0xFF
}

fn dispstat_set_in_vblank(v: u16, b: bool) -> u16 {
    (v & !1) | b as u16
}
fn dispstat_set_in_hblank(v: u16, b: bool) -> u16 {
    (v & !2) | ((b as u16) << 1)
}
fn dispstat_set_vcounter(v: u16, b: bool) -> u16 {
    (v & !4) | ((b as u16) << 2)
}

impl Gba {
    pub fn video_reset(&mut self) {
        if !self.has_bios {
            self.video.vcount = 0x7E;
        } else {
            self.video.vcount = 0;
        }
        self.memory.io[(GBA_REG_VCOUNT >> 1) as usize] = self.video.vcount as u16;
        self.video.event_kind = VideoEventKind::StartHblank;
        self.video.frame_counter = 0;
        self.video.frameskip_counter = 0;
        self.video.stall_mask = 0;

        for b in self.video.palette.iter_mut() {
            *b = 0;
        }
        for b in self.video.oam.iter_mut() {
            *b = 0;
        }
        for b in self.video.vram.iter_mut() {
            *b = 0;
        }
        self.renderer_reset();

        let next_event = if self.has_bios {
            VIDEO_HDRAW_LENGTH
        } else {
            120
        };
        self.deschedule(EventId::Video);
        self.schedule(EventId::Video, next_event);
    }

    /// The video frame event (video.c's `event` callback).
    pub fn video_event(&mut self, timing: &mut Timing, cycles_late: u32) {
        match self.video.event_kind {
            VideoEventKind::StartHdraw => self.video_start_hdraw(timing, cycles_late),
            VideoEventKind::StartHblank => self.video_start_hblank(timing, cycles_late),
        }
    }

    fn video_start_hdraw(&mut self, timing: &mut Timing, cycles_late: u32) {
        self.video.event_kind = VideoEventKind::StartHblank;
        let when = VIDEO_HDRAW_LENGTH - cycles_late as i32;
        timing.schedule(
            EventId::Video.into(),
            EventId::Video.priority(),
            when,
        );

        self.video.vcount += 1;
        if self.video.vcount == VIDEO_VERTICAL_TOTAL_PIXELS {
            self.video.vcount = 0;
        }
        self.memory.io[(GBA_REG_VCOUNT >> 1) as usize] = self.video.vcount as u16;

        if self.video.vcount < VIDEO_VERTICAL_PIXELS {
            let dispcnt = self.memory.io[(GBA_REG_DISPCNT >> 1) as usize];
            self.video.stall_mask = self.calculate_stall_mask(dispcnt);
        }

        let mut dispstat = self.memory.io[(GBA_REG_DISPSTAT >> 1) as usize];
        dispstat = dispstat_set_in_hblank(dispstat, false);
        if self.video.vcount == dispstat_vcount_setting(dispstat) as i32 {
            dispstat = dispstat_set_vcounter(dispstat, true);
            if dispstat_vcounter_irq(dispstat) {
                self.raise_irq(2);
            }
        } else {
            dispstat = dispstat_set_vcounter(dispstat, false);
        }
        self.memory.io[(GBA_REG_DISPSTAT >> 1) as usize] = dispstat;

        match self.video.vcount {
            0 => {
                self.frame_started();
            }
            x if x == VIDEO_VERTICAL_PIXELS => {
                self.memory.io[(GBA_REG_DISPSTAT >> 1) as usize] =
                    dispstat_set_in_vblank(dispstat, true);
                if self.video.frameskip_counter <= 0 {
                    self.renderer_finish_frame();
                }
                self.dma_run_vblank_all(cycles_late);
                if dispstat_vblank_irq(dispstat) {
                    self.raise_irq(0);
                }
                self.frame_ended();
                self.video.frameskip_counter -= 1;
                if self.video.frameskip_counter < 0 {
                    self.video.frameskip_counter = self.video.frameskip;
                }
                self.video.frame_counter += 1;
                self.gb_interrupt();
            }
            x if x == VIDEO_VERTICAL_TOTAL_PIXELS - 1 => {
                self.memory.io[(GBA_REG_DISPSTAT >> 1) as usize] =
                    dispstat_set_in_vblank(dispstat, false);
            }
            _ => {}
        }
    }

    fn video_start_hblank(&mut self, timing: &mut Timing, cycles_late: u32) {
        self.video.event_kind = VideoEventKind::StartHdraw;
        let when = VIDEO_HBLANK_LENGTH - cycles_late as i32;
        timing.schedule(
            EventId::Video.into(),
            EventId::Video.priority(),
            when,
        );

        // Begin Hblank
        let mut dispstat = self.memory.io[(GBA_REG_DISPSTAT >> 1) as usize];
        dispstat = dispstat_set_in_hblank(dispstat, true);
        if self.video.vcount < VIDEO_VERTICAL_PIXELS && self.video.frameskip_counter <= 0 {
            self.renderer_draw_scanline(self.video.vcount);
        }
        if self.video.vcount < VIDEO_VERTICAL_PIXELS {
            self.dma_run_hblank_all(cycles_late);
        }
        if self.video.vcount >= 2 && self.video.vcount < VIDEO_VERTICAL_PIXELS + 2 {
            self.dma_run_display_start_all(cycles_late);
        }
        if dispstat_hblank_irq(dispstat) {
            // TODO: Where does this fudge factor come from?
            self.raise_irq(1);
        }
        self.video.stall_mask = 0;
        self.memory.io[(GBA_REG_DISPSTAT >> 1) as usize] = dispstat;
    }

    /// GBAVideoWriteDISPSTAT
    pub fn video_write_dispstat(&mut self, value: u16) -> u16 {
        let mut dispstat = self.memory.io[(GBA_REG_DISPSTAT >> 1) as usize] & 0x7;
        dispstat |= value & 0xFFF8;

        if self.video.vcount == dispstat_vcount_setting(dispstat) as i32 {
            // Edge trigger only
            if dispstat_vcounter_irq(dispstat) && !dispstat_vcounter(dispstat) {
                self.raise_irq(2);
            }
            dispstat = dispstat_set_vcounter(dispstat, true);
        } else {
            dispstat = dispstat_set_vcounter(dispstat, false);
        }
        dispstat
    }

    fn frame_started(&mut self) {}
    fn frame_ended(&mut self) {
        self.sync_savedata();
        self.cheat_apply();
    }
    fn gb_interrupt(&mut self) {
        self.gba_interrupt();
    }

    fn calculate_stall_mask(&self, dispcnt: u16) -> u32 {
        let mut mask = 0u32;
        if dispcnt_forced_blank(dispcnt) {
            return 0;
        }
        match dispcnt_mode(dispcnt) {
            0 => {
                for bg in 0..4 {
                    if dispcnt_bg_enable(dispcnt, bg) {
                        let cnt = self.memory.io
                            [((GBA_REG_BG0CNT as usize + bg as usize * 2) >> 1) as usize];
                        if bgcnt_is_256color(cnt) {
                            mask |= 0x010 << bg;
                        } else {
                            mask |= 0x011 << bg;
                        }
                    }
                }
            }
            1 => {
                for bg in 0..2 {
                    if dispcnt_bg_enable(dispcnt, bg) {
                        let cnt = self.memory.io
                            [((GBA_REG_BG0CNT as usize + bg as usize * 2) >> 1) as usize];
                        if bgcnt_is_256color(cnt) {
                            mask |= 0x010 << bg;
                        } else {
                            mask |= 0x011 << bg;
                        }
                    }
                }
                if dispcnt_bg_enable(dispcnt, 2) {
                    mask |= GBA_VSTALL_A2;
                }
            }
            2 => {
                if dispcnt_bg_enable(dispcnt, 2) {
                    mask |= GBA_VSTALL_A2;
                }
                if dispcnt_bg_enable(dispcnt, 3) {
                    mask |= GBA_VSTALL_A3;
                }
            }
            3..=5 => {
                if dispcnt_bg_enable(dispcnt, 2) {
                    mask |= GBA_VSTALL_B;
                }
            }
            _ => {}
        }
        mask
    }
}
