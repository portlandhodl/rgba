// Copyright (c) 2013-2019 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gba/renderers/{common.c,video-software.c,software-bg.c,
// software-mode0.c,software-obj.c}, plus include/mgba/internal/gba/renderers/
// {common.h,video-software.h} and software-private.h.
//
// The C's mCacheSet layer is not ported (pure perf cache; identical output).

use rgba_core::{mlog, Level};

use crate::gba::*;
use crate::io::*;
use crate::video::*;

// Flag bits for the internal scanline rows (software-private.h).
pub const FLAG_PRIORITY: u32 = 0xC0000000;
pub const FLAG_INDEX: u32 = 0x30000000;
pub const FLAG_IS_BACKGROUND: u32 = 0x08000000;
pub const FLAG_UNWRITTEN: u32 = 0xFC000000;
pub const FLAG_REBLEND: u32 = 0x04000000;
pub const FLAG_TARGET_1: u32 = 0x02000000;
pub const FLAG_TARGET_2: u32 = 0x01000000;
pub const FLAG_OBJWIN: u32 = 0x01000000;
pub const FLAG_ORDER_MASK: u32 = 0xF8000000;

pub const OFFSET_PRIORITY: u32 = 30;
pub const OFFSET_INDEX: u32 = 28;

pub const MAX_WINDOW: usize = 5;
pub const ENABLED_MAX: i32 = 4;

// mColor: mGBA stores BGR u32 (0x00BBGGRR).
#[inline]
pub fn color_from_555(value: u16) -> u32 {
    let c = ((value as u32 & 0x1F) << 3)
        | ((value as u32 & 0x3E0) << 6)
        | ((value as u32 & 0x7C00) << 9);
    c | (c >> 5) & 0x070707
}

/// _brighten
#[inline]
pub fn brighten(color: u32, y: u16) -> u32 {
    let y = y as u32;
    let mut c = 0u32;
    let a = color & 0xFF;
    c |= (a + ((0xFF - a) * y) / 16) & 0xFF;
    let a = color & 0xFF00;
    c |= (a + ((0xFF00 - a) * y) / 16) & 0xFF00;
    let a = color & 0xFF0000;
    c |= (a + ((0xFF0000 - a) * y) / 16) & 0xFF0000;
    c
}

/// _darken
#[inline]
pub fn darken(color: u32, y: u16) -> u32 {
    let y = y as u32;
    let mut c = 0u32;
    let a = color & 0xFF;
    c |= (a - (a * y) / 16) & 0xFF;
    let a = color & 0xFF00;
    c |= (a - (a * y) / 16) & 0xFF00;
    let a = color & 0xFF0000;
    c |= (a - (a * y) / 16) & 0xFF0000;
    c
}

/// mColorMix5Bit
#[inline]
pub fn color_mix_5bit(weight_a: i32, color_a: u32, weight_b: i32, color_b: u32) -> u32 {
    let mut c: u32 = 0;
    let a = color_a & 0xFF;
    let b = color_b & 0xFF;
    c |= ((a * weight_a as u32 + b * weight_b as u32) / 16) & 0x1FF;
    if c & 0x100 != 0 {
        c = 0xFF;
    }
    let a = color_a & 0xFF00;
    let b = color_b & 0xFF00;
    c |= ((a * weight_a as u32 + b * weight_b as u32) / 16) & 0x1FF00;
    if c & 0x10000 != 0 {
        c = (c & 0xFF) | 0xFF00;
    }
    let a = color_a & 0xFF0000;
    let b = color_b & 0xFF0000;
    c |= ((a * weight_a as u32 + b * weight_b as u32) / 16) & 0x1FF0000;
    if c & 0x1000000 != 0 {
        c = (c & 0xFFFF) | 0xFF0000;
    }
    c
}

/// mColor → framebuffer u32 (0xFFRRGGBB).
#[inline]
pub fn out_pixel(color: u32) -> u32 {
    0xFF00_0000 | (color & 0xFF) << 16 | (color & 0xFF00) | ((color >> 16) & 0xFF)
}

#[inline]
fn is_writable(pixel: u32) -> bool {
    pixel & 0xFE000000 != 0
}

// GBA obj attribute accessors
#[inline]
pub fn obj_a_y(a: u16) -> i32 {
    (a & 0xFF) as i32
}
#[inline]
pub fn obj_a_transformed(a: u16) -> bool {
    a & 0x100 != 0
}
#[inline]
pub fn obj_a_disable(a: u16) -> bool {
    a & 0x200 != 0
}
#[inline]
pub fn obj_a_double_size(a: u16) -> bool {
    a & 0x200 != 0
}
#[inline]
pub fn obj_a_mode(a: u16) -> i32 {
    ((a >> 10) & 3) as i32
}
#[inline]
pub fn obj_a_mosaic(a: u16) -> bool {
    a & 0x1000 != 0
}
#[inline]
pub fn obj_a_256color(a: u16) -> bool {
    a & 0x2000 != 0
}
#[inline]
pub fn obj_a_shape(a: u16) -> i32 {
    ((a >> 14) & 3) as i32
}
#[inline]
pub fn obj_b_x(b: u16) -> i32 {
    (b & 0x1FF) as i32
}
#[inline]
pub fn obj_b_mat_index(b: u16) -> i32 {
    ((b >> 9) & 0x1F) as i32
}
#[inline]
pub fn obj_b_hflip(b: u16) -> bool {
    b & 0x1000 != 0
}
#[inline]
pub fn obj_b_vflip(b: u16) -> bool {
    b & 0x2000 != 0
}
#[inline]
pub fn obj_b_size(b: u16) -> i32 {
    ((b >> 14) & 3) as i32
}
#[inline]
pub fn obj_c_tile(c: u16) -> i32 {
    (c & 0x3FF) as i32
}
#[inline]
pub fn obj_c_priority(c: u16) -> i32 {
    ((c >> 10) & 3) as i32
}
#[inline]
pub fn obj_c_palette(c: u16) -> i32 {
    ((c >> 12) & 0xF) as i32
}

pub const OBJ_MODE_NORMAL: i32 = 0;
pub const OBJ_MODE_SEMITRANSPARENT: i32 = 1;
pub const OBJ_MODE_OBJWIN: i32 = 2;

pub const BLEND_NONE: i32 = 0;
pub const BLEND_ALPHA: i32 = 1;
pub const BLEND_BRIGHTEN: i32 = 2;
pub const BLEND_DARKEN: i32 = 3;

// GBAWindowControl bits
pub const WIN_BG0_ENABLE: u8 = 0x01;
pub const WIN_BG1_ENABLE: u8 = 0x02;
pub const WIN_BG2_ENABLE: u8 = 0x04;
pub const WIN_BG3_ENABLE: u8 = 0x08;
pub const WIN_OBJ_ENABLE: u8 = 0x10;
pub const WIN_BLEND_ENABLE: u8 = 0x20;

#[inline]
pub fn win_bg_enable(packed: u8, bg: usize) -> bool {
    packed & (1 << bg) != 0
}

// Mosaic packed u16 accessors
#[inline]
pub fn mosaic_bg_h(m: u16) -> i32 {
    (m & 0xF) as i32
}
#[inline]
pub fn mosaic_bg_v(m: u16) -> i32 {
    ((m >> 4) & 0xF) as i32
}
#[inline]
pub fn mosaic_obj_h(m: u16) -> i32 {
    ((m >> 8) & 0xF) as i32
}
#[inline]
pub fn mosaic_obj_v(m: u16) -> i32 {
    ((m >> 12) & 0xF) as i32
}

/// OBJ sizes (GBAVideoObjSizes: [shape*4+size][w,h])
pub const OBJ_SIZES_TABLE: [[i32; 2]; 12] = [
    [8, 8],
    [16, 8],
    [8, 16],
    [0, 0],
    [16, 16],
    [32, 8],
    [8, 32],
    [0, 0],
    [32, 32],
    [32, 16],
    [16, 32],
    [0, 0],
];

// ---- Structs (exactly the C's fields) ------------------------------------

#[derive(Clone, Copy)]
pub struct SoftwareBackground {
    pub index: u32,
    pub enabled: i32,
    pub priority: u32,
    pub char_base: u32,
    pub mosaic: bool,
    pub multipalette: bool,
    pub screen_base: u32,
    pub overflow: bool,
    pub size: i32,
    pub target1: i32,
    pub target2: i32,
    pub x: u16,
    pub y: u16,
    pub ref_x: i32,
    pub ref_y: i32,
    pub dx: i16,
    pub dmx: i16,
    pub dy: i16,
    pub dmy: i16,
    pub sx: i32,
    pub sy: i32,
    pub y_cache: i32,
    pub map_cache: [u16; 64],
    pub flags: u32,
    pub objwin_flags: u32,
    pub objwin_force_enable: i32,
    pub objwin_only: bool,
    pub variant: bool,
    pub offset_x: i32,
    pub offset_y: i32,
    pub highlight: bool,
}

impl std::default::Default for SoftwareBackground {
    fn default() -> Self {
        let mut v = unsafe { std::mem::zeroed::<SoftwareBackground>() };
        v.y_cache = -1;
        v
    }
}

#[derive(Clone, Copy, Default)]
pub struct WindowControl {
    pub packed: u8,
    pub priority: i8,
}

#[derive(Clone, Copy, Default)]
pub struct Window {
    pub end_x: u8,
    pub control: WindowControl,
}

#[derive(Clone, Copy, Default)]
pub struct WinRegion {
    pub end: u8,
    pub start: u8,
}

#[derive(Clone, Copy)]
pub struct WindowN {
    pub h: WinRegion,
    pub v: WinRegion,
    pub control: WindowControl,
    pub offset_x: i16,
    pub offset_y: i16,
    pub on: bool,
}

impl Default for WindowN {
    fn default() -> Self {
        WindowN {
            h: WinRegion::default(),
            v: WinRegion::default(),
            control: WindowControl::default(),
            offset_x: 0,
            offset_y: 0,
            on: false,
        }
    }
}

impl WindowN {
    pub fn new(priority: i8) -> Self {
        let mut w = WindowN::default();
        w.control.priority = priority;
        w
    }
}

#[derive(Clone, Copy)]
pub struct ScanlineCache {
    pub io: [u16; 0x30], // nextIo layout (0x30 = GBA_REG(SOUND1CNT_LO) >> 1 = 0x30)
    pub scale: [[i32; 2]; 2],
    pub window_on: [bool; 2],
}

impl ScanlineCache {
    pub fn new() -> Self {
        ScanlineCache {
            io: [0; 0x30],
            scale: [[0; 2]; 2],
            window_on: [false; 2],
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct RendererSprite {
    pub obj: GbaObj,
    pub y: i16,
    pub end_y: i16,
    pub cycles: i16,
    pub index: i8,
}

#[derive(Clone, Copy, Default)]
pub struct GbaObj {
    pub a: u16,
    pub b: u16,
    pub c: u16,
}

/// The software renderer (GBAVideoSoftwareRenderer minus the vtable).
pub struct SwVideo {
    pub output: Vec<u32>,
    pub output_stride: usize,

    pub dispcnt: u16,

    pub row: [u32; 240],
    pub sprite_layer: [u32; 240],
    pub sprite_cycles_remaining: i32,

    pub target1_obj: u32,
    pub target1_bd: u32,
    pub target2_obj: u32,
    pub target2_bd: u32,
    pub blend_dirty: bool,
    pub blend_effect: i32,

    pub normal_palette: Vec<u32>,
    pub variant_palette: Vec<u32>,
    pub highlight_palette: Vec<u32>,
    pub highlight_variant_palette: Vec<u32>,

    pub blda: u16,
    pub bldb: u16,
    pub bldy: u16,
    pub mosaic: u16,
    pub stereo: bool,

    pub win_n: [WindowN; 2],
    pub winout: WindowControl,
    pub objwin: WindowControl,
    pub current_window: WindowControl,

    pub n_windows: i32,
    pub windows: [Window; MAX_WINDOW],

    pub bg: [SoftwareBackground; 4],

    pub force_target1: bool,
    pub oam_dirty: bool,
    pub oam_max: i32,
    pub sprites: Vec<RendererSprite>,
    pub obj_offset_x: i16,
    pub obj_offset_y: i16,

    pub scanline_dirty: [u32; 5],
    pub next_io: Vec<u16>,         // GBA_REG(SOUND1CNT_LO) >> 1 elements
    pub cache: Vec<ScanlineCache>, // 160
    pub next_y: i32,

    pub start: i32,
    pub end: i32,

    pub last_highlight_amount: u8,
    pub disable_bg: [bool; 4],
    pub disable_obj: bool,
    pub disable_win: [bool; 2],
    pub disable_objwin: bool,
    pub highlight_bg: [bool; 4],
    pub highlight_obj: [bool; 128],
    pub highlight_color: u32,
    pub highlight_amount: u8,
}

impl SwVideo {
    pub fn new() -> Self {
        SwVideo {
            output: vec![0; 240 * 160],
            output_stride: 240,
            dispcnt: 0x0080,
            row: [0; 240],
            sprite_layer: [0; 240],
            sprite_cycles_remaining: 0,
            target1_obj: 0,
            target1_bd: 0,
            target2_obj: 0,
            target2_bd: 0,
            blend_dirty: false,
            blend_effect: BLEND_NONE,
            normal_palette: vec![0; 512],
            variant_palette: vec![0; 512],
            highlight_palette: vec![0; 512],
            highlight_variant_palette: vec![0; 512],
            blda: 0,
            bldb: 0,
            bldy: 0,
            mosaic: 0,
            stereo: false,
            win_n: [WindowN::new(0), WindowN::new(1)],
            winout: WindowControl {
                packed: 0,
                priority: 3,
            },
            objwin: WindowControl {
                packed: 0,
                priority: 2,
            },
            current_window: WindowControl::default(),
            n_windows: 0,
            windows: [Window::default(); MAX_WINDOW],
            bg: [SoftwareBackground::default(); 4],
            force_target1: false,
            oam_dirty: true,
            oam_max: 0,
            sprites: vec![RendererSprite::default(); 128],
            obj_offset_x: 0,
            obj_offset_y: 0,
            scanline_dirty: [0xFFFFFFFF; 5],
            next_io: vec![0; 0x30],
            cache: vec![ScanlineCache::new(); 160],
            next_y: 0,
            start: 0,
            end: 0,
            last_highlight_amount: 0,
            disable_bg: [false; 4],
            disable_obj: false,
            disable_win: [false; 2],
            disable_objwin: false,
            highlight_bg: [false; 4],
            highlight_obj: [false; 128],
            highlight_color: 0x00FFFFFF,
            highlight_amount: 0,
        }
    }
}

impl Default for SwVideo {
    fn default() -> Self {
        Self::new()
    }
}

/// GBAVideoRendererCleanOAM (common.c)
pub fn renderer_clean_oam(oam: &[u8], sprites: &mut [RendererSprite], offset_y: i16) -> i32 {
    let mut oam_max = 0usize;
    for i in 0..128usize {
        let o = &oam[i * 8..i * 8 + 8];
        let a = u16::from_le_bytes([o[0], o[1]]);
        let b = u16::from_le_bytes([o[2], o[3]]);
        let c = u16::from_le_bytes([o[4], o[5]]);
        if obj_a_transformed(a) || !obj_a_disable(a) {
            let widths = OBJ_SIZES_TABLE[(obj_a_shape(a) * 4 + obj_b_size(b)) as usize];
            let mut width = widths[0];
            let mut height = widths[1];
            let mut x = obj_b_x(b) << 23 >> 23;
            let mut cycles;
            if obj_a_transformed(a) {
                height <<= obj_a_double_size(a) as u32;
                width <<= obj_a_double_size(a) as u32;
                cycles = 8 + width * 2;
                if x < 0 {
                    cycles += x;
                }
            } else {
                cycles = width - 2;
                if x < 0 {
                    if x + width < 0 {
                        continue;
                    }
                    cycles += x >> 1;
                }
            }
            if obj_a_y(a) >= VIDEO_VERTICAL_PIXELS
                && obj_a_y(a) + height < VIDEO_VERTICAL_TOTAL_PIXELS
            {
                continue;
            }
            if obj_b_x(b) >= VIDEO_HORIZONTAL_PIXELS && obj_b_x(b) + width < 512 {
                continue;
            }
            let mut y = obj_a_y(a) + offset_y as i32;
            if y + height > 256 {
                y -= 256;
            }
            sprites[oam_max].y = y as i16;
            sprites[oam_max].end_y = (y + height) as i16;
            sprites[oam_max].cycles = cycles as i16;
            sprites[oam_max].obj = GbaObj { a, b, c };
            sprites[oam_max].index = i as i8;
            oam_max += 1;
        }
    }
    oam_max as i32
}

// ---------------------------------------------------------------------------
// Scanline pipeline (video-software.c)

impl Gba {
    /// GBAVideoSoftwareRendererReset
    pub fn renderer_reset(&mut self) {
        let sw = &mut self.video.sw;
        sw.dispcnt = 0x0080;
        sw.target1_obj = 0;
        sw.target1_bd = 0;
        sw.target2_obj = 0;
        sw.target2_bd = 0;
        sw.blend_effect = BLEND_NONE;
        for i in (0..1024).step_by(2) {
            let entry = self.video.palette[i] as u16 | ((self.video.palette[i + 1] as u16) << 8);
            self.renderer_write_palette(i as u32, entry);
        }
        let sw = &mut self.video.sw;
        sw.blend_dirty = false;
        self.update_palettes();
        let sw = &mut self.video.sw;
        sw.blda = 0;
        sw.bldb = 0;
        sw.bldy = 0;
        sw.win_n[0] = WindowN::new(0);
        sw.win_n[1] = WindowN::new(1);
        sw.objwin = WindowControl {
            packed: 0,
            priority: 2,
        };
        sw.winout = WindowControl {
            packed: 0,
            priority: 3,
        };
        sw.oam_dirty = true;
        sw.oam_max = 0;
        sw.mosaic = 0;
        sw.stereo = false;
        sw.next_y = 0;
        sw.obj_offset_x = 0;
        sw.obj_offset_y = 0;
        sw.scanline_dirty = [0xFFFFFFFF; 5];
        for c in sw.cache.iter_mut() {
            *c = ScanlineCache::new();
        }
        for n in sw.next_io.iter_mut() {
            *n = 0;
        }
        sw.last_highlight_amount = 0;
        for bg in sw.bg.iter_mut() {
            *bg = SoftwareBackground {
                index: 0,
                ..SoftwareBackground::default()
            };
        }
        for i in 0..4 {
            sw.bg[i].index = i as u32;
            sw.bg[i].dx = 256;
            sw.bg[i].dmy = 256;
            sw.bg[i].y_cache = -1;
        }
    }

    /// _updatePalettes
    pub fn update_palettes(&mut self) {
        let sw = &mut self.video.sw;
        let blend = sw.blend_effect;
        if blend == BLEND_BRIGHTEN {
            for i in 0..512 {
                sw.variant_palette[i] = brighten(sw.normal_palette[i], sw.bldy);
            }
        } else if blend == BLEND_DARKEN {
            for i in 0..512 {
                sw.variant_palette[i] = darken(sw.normal_palette[i], sw.bldy);
            }
        } else {
            for i in 0..512 {
                sw.variant_palette[i] = sw.normal_palette[i];
            }
        }
        let highlight_amount = (sw.highlight_amount >> 4) as i32;
        if highlight_amount != 0 {
            for i in 0..512 {
                sw.highlight_palette[i] = color_mix_5bit(
                    0x10 - highlight_amount,
                    sw.normal_palette[i],
                    highlight_amount,
                    sw.highlight_color,
                );
                sw.highlight_variant_palette[i] = color_mix_5bit(
                    0x10 - highlight_amount,
                    sw.variant_palette[i],
                    highlight_amount,
                    sw.highlight_color,
                );
            }
        }
    }

    /// _updateFlags
    fn update_flags(&mut self, bgidx: usize) {
        let sw = &mut self.video.sw;
        let bg = &mut sw.bg[bgidx];
        let mut flags =
            (bg.priority << OFFSET_PRIORITY) | (bg.index << OFFSET_INDEX) | FLAG_IS_BACKGROUND;
        if bg.target2 != 0 {
            flags |= FLAG_TARGET_2;
        }
        let mut objwin_flags = flags;
        if sw.blend_effect == BLEND_ALPHA {
            if sw.blda == 0x10 && sw.bldb == 0 {
                flags &= !FLAG_TARGET_2;
                objwin_flags &= !FLAG_TARGET_2;
            } else if bg.target1 != 0 {
                if sw.current_window.packed & WIN_BLEND_ENABLE != 0 {
                    flags |= FLAG_TARGET_1;
                }
                if sw.objwin.packed & WIN_BLEND_ENABLE != 0 {
                    objwin_flags |= FLAG_TARGET_1;
                }
            }
        }
        bg.flags = flags;
        bg.objwin_flags = objwin_flags;
        bg.variant = bg.target1 != 0
            && sw.current_window.packed & WIN_BLEND_ENABLE != 0
            && (sw.blend_effect == BLEND_BRIGHTEN || sw.blend_effect == BLEND_DARKEN);
    }

    /// GBAVideoSoftwareRendererWriteVideoRegister
    pub fn renderer_write_video_register(&mut self, address: u32, value: u16) -> u16 {
        let sw = &mut self.video.sw;
        let mut value = value;
        match address {
            GBA_REG_DISPCNT => {
                value &= 0xFFF7;
                sw.dispcnt = value;
                // UpdateDISPCNT below
            }
            GBA_REG_STEREOCNT => {
                sw.stereo = value & 1 != 0;
            }
            GBA_REG_BG0CNT => {
                value &= 0xDFFF;
                self.write_bgcnt(0, value);
            }
            GBA_REG_BG1CNT => {
                value &= 0xDFFF;
                self.write_bgcnt(1, value);
            }
            GBA_REG_BG2CNT => {
                self.write_bgcnt(2, value);
            }
            GBA_REG_BG3CNT => {
                self.write_bgcnt(3, value);
            }
            GBA_REG_BG0HOFS => {
                value &= 0x01FF;
                self.video.sw.bg[0].x = value;
            }
            GBA_REG_BG0VOFS => {
                value &= 0x01FF;
                self.video.sw.bg[0].y = value;
            }
            GBA_REG_BG1HOFS => {
                value &= 0x01FF;
                self.video.sw.bg[1].x = value;
            }
            GBA_REG_BG1VOFS => {
                value &= 0x01FF;
                self.video.sw.bg[1].y = value;
            }
            GBA_REG_BG2HOFS => {
                value &= 0x01FF;
                self.video.sw.bg[2].x = value;
            }
            GBA_REG_BG2VOFS => {
                value &= 0x01FF;
                self.video.sw.bg[2].y = value;
            }
            GBA_REG_BG3HOFS => {
                value &= 0x01FF;
                self.video.sw.bg[3].x = value;
            }
            GBA_REG_BG3VOFS => {
                value &= 0x01FF;
                self.video.sw.bg[3].y = value;
            }
            GBA_REG_BG2PA => self.video.sw.bg[2].dx = value as i16,
            GBA_REG_BG2PB => self.video.sw.bg[2].dmx = value as i16,
            GBA_REG_BG2PC => self.video.sw.bg[2].dy = value as i16,
            GBA_REG_BG2PD => self.video.sw.bg[2].dmy = value as i16,
            GBA_REG_BG2X_LO => {
                let mut bg = self.video.sw.bg[2];
                Self::write_bgx_lo(&mut bg, value);
                self.video.sw.bg[2] = bg;
                let next_y = self.video.sw.next_y as usize;
                if self.video.sw.bg[2].sx != self.video.sw.cache[next_y].scale[0][0] {
                    self.video.sw.scanline_dirty[next_y >> 5] |= 1 << (next_y & 0x1F);
                }
            }
            GBA_REG_BG2X_HI => {
                let mut bg = self.video.sw.bg[2];
                Self::write_bgx_hi(&mut bg, value);
                self.video.sw.bg[2] = bg;
                let next_y = self.video.sw.next_y as usize;
                if self.video.sw.bg[2].sx != self.video.sw.cache[next_y].scale[0][0] {
                    self.video.sw.scanline_dirty[next_y >> 5] |= 1 << (next_y & 0x1F);
                }
            }
            GBA_REG_BG2Y_LO => {
                let mut bg = self.video.sw.bg[2];
                Self::write_bgy_lo(&mut bg, value);
                self.video.sw.bg[2] = bg;
                let next_y = self.video.sw.next_y as usize;
                if self.video.sw.bg[2].sy != self.video.sw.cache[next_y].scale[0][1] {
                    self.video.sw.scanline_dirty[next_y >> 5] |= 1 << (next_y & 0x1F);
                }
            }
            GBA_REG_BG2Y_HI => {
                let mut bg = self.video.sw.bg[2];
                Self::write_bgy_hi(&mut bg, value);
                self.video.sw.bg[2] = bg;
                let next_y = self.video.sw.next_y as usize;
                if self.video.sw.bg[2].sy != self.video.sw.cache[next_y].scale[0][1] {
                    self.video.sw.scanline_dirty[next_y >> 5] |= 1 << (next_y & 0x1F);
                }
            }
            GBA_REG_BG3PA => self.video.sw.bg[3].dx = value as i16,
            GBA_REG_BG3PB => self.video.sw.bg[3].dmx = value as i16,
            GBA_REG_BG3PC => self.video.sw.bg[3].dy = value as i16,
            GBA_REG_BG3PD => self.video.sw.bg[3].dmy = value as i16,
            GBA_REG_BG3X_LO => {
                let mut bg = self.video.sw.bg[3];
                Self::write_bgx_lo(&mut bg, value);
                self.video.sw.bg[3] = bg;
                let next_y = self.video.sw.next_y as usize;
                if self.video.sw.bg[3].sx != self.video.sw.cache[next_y].scale[1][0] {
                    self.video.sw.scanline_dirty[next_y >> 5] |= 1 << (next_y & 0x1F);
                }
            }
            GBA_REG_BG3X_HI => {
                let mut bg = self.video.sw.bg[3];
                Self::write_bgx_hi(&mut bg, value);
                self.video.sw.bg[3] = bg;
                let next_y = self.video.sw.next_y as usize;
                if self.video.sw.bg[3].sx != self.video.sw.cache[next_y].scale[1][0] {
                    self.video.sw.scanline_dirty[next_y >> 5] |= 1 << (next_y & 0x1F);
                }
            }
            GBA_REG_BG3Y_LO => {
                let mut bg = self.video.sw.bg[3];
                Self::write_bgy_lo(&mut bg, value);
                self.video.sw.bg[3] = bg;
                let next_y = self.video.sw.next_y as usize;
                if self.video.sw.bg[3].sy != self.video.sw.cache[next_y].scale[1][1] {
                    self.video.sw.scanline_dirty[next_y >> 5] |= 1 << (next_y & 0x1F);
                }
            }
            GBA_REG_BG3Y_HI => {
                let mut bg = self.video.sw.bg[3];
                Self::write_bgy_hi(&mut bg, value);
                self.video.sw.bg[3] = bg;
                let next_y = self.video.sw.next_y as usize;
                if self.video.sw.bg[3].sy != self.video.sw.cache[next_y].scale[1][1] {
                    self.video.sw.scanline_dirty[next_y >> 5] |= 1 << (next_y & 0x1F);
                }
            }
            GBA_REG_BLDCNT => {
                self.write_bldcnt(value);
                value &= 0x3FFF;
            }
            GBA_REG_BLDALPHA => {
                self.video.sw.blda = value & 0x1F;
                if self.video.sw.blda > 0x10 {
                    self.video.sw.blda = 0x10;
                }
                self.video.sw.bldb = (value >> 8) & 0x1F;
                if self.video.sw.bldb > 0x10 {
                    self.video.sw.bldb = 0x10;
                }
                value &= 0x1F1F;
            }
            GBA_REG_BLDY => {
                value &= 0x1F;
                if value > 0x10 {
                    value = 0x10;
                }
                if self.video.sw.bldy != value {
                    self.video.sw.bldy = value;
                    self.video.sw.blend_dirty = true;
                }
            }
            GBA_REG_WIN0H => {
                let w = &mut self.video.sw.win_n[0];
                w.h.end = (value & 0xFF) as u8;
                w.h.start = (value >> 8) as u8;
            }
            GBA_REG_WIN1H => {
                let w = &mut self.video.sw.win_n[1];
                w.h.end = (value & 0xFF) as u8;
                w.h.start = (value >> 8) as u8;
            }
            GBA_REG_WIN0V => {
                let w = &mut self.video.sw.win_n[0];
                w.v.end = (value & 0xFF) as u8;
                w.v.start = (value >> 8) as u8;
            }
            GBA_REG_WIN1V => {
                let w = &mut self.video.sw.win_n[1];
                w.v.end = (value & 0xFF) as u8;
                w.v.start = (value >> 8) as u8;
            }
            GBA_REG_WININ => {
                value &= 0x3F3F;
                self.video.sw.win_n[0].control.packed = (value & 0xFF) as u8;
                self.video.sw.win_n[1].control.packed = ((value >> 8) & 0xFF) as u8;
            }
            GBA_REG_WINOUT => {
                value &= 0x3F3F;
                self.video.sw.winout.packed = (value & 0xFF) as u8;
                self.video.sw.objwin.packed = ((value >> 8) & 0xFF) as u8;
            }
            GBA_REG_MOSAIC => {
                self.video.sw.mosaic = value;
            }
            _ => {
                mlog!(
                    Level::GameError,
                    rgba_core::log::GBA_VIDEO,
                    "Invalid video register: 0x{:03X}",
                    address
                );
            }
        }
        if (address >> 1) < self.video.sw.next_io.len() as u32 {
            self.video.sw.next_io[(address >> 1) as usize] = value;
            if self.video.sw.cache[self.video.sw.next_y as usize].io[(address >> 1) as usize]
                != value
            {
                self.video.sw.cache[self.video.sw.next_y as usize].io[(address >> 1) as usize] =
                    value;
                let next_y = self.video.sw.next_y as usize;
                self.video.sw.scanline_dirty[next_y >> 5] |= 1 << (next_y & 0x1F);
            }
        }
        if address == GBA_REG_DISPCNT {
            self.update_dispcnt();
        }
        value
    }

    fn write_bgcnt(&mut self, bg: usize, value: u16) {
        let b = &mut self.video.sw.bg[bg];
        b.priority = (value & 3) as u32;
        b.char_base = (((value >> 2) & 3) as u32) << 14;
        b.mosaic = value & 0x40 != 0;
        b.multipalette = value & 0x80 != 0;
        b.screen_base = (((value >> 8) & 0x1F) as u32) << 11;
        b.overflow = value & 0x2000 != 0;
        b.size = ((value >> 14) & 3) as i32;
        b.y_cache = -1;
        self.update_flags(bg);
    }

    fn write_bgx_lo(bg: &mut SoftwareBackground, value: u16) {
        bg.ref_x = (bg.ref_x & 0xFFFF0000u32 as i32) | value as i32;
        bg.sx = bg.ref_x;
    }
    fn write_bgx_hi(bg: &mut SoftwareBackground, value: u16) {
        bg.ref_x = ((bg.ref_x & 0x0000FFFF) | ((value as i32) << 16)) as i32;
        bg.ref_x = (bg.ref_x << 4) >> 4;
        bg.sx = bg.ref_x;
    }
    fn write_bgy_lo(bg: &mut SoftwareBackground, value: u16) {
        bg.ref_y = (bg.ref_y & (0xFFFF0000u32 as i32)) | value as i32;
        bg.sy = bg.ref_y;
    }
    fn write_bgy_hi(bg: &mut SoftwareBackground, value: u16) {
        bg.ref_y = ((bg.ref_y & 0x0000FFFF) | ((value as i32) << 16)) as i32;
        bg.ref_y = (bg.ref_y << 4) >> 4;
        bg.sy = bg.ref_y;
    }

    fn update_dispcnt(&mut self) {
        for bg in 0..4usize {
            let enable = self.video.sw.dispcnt & (0x100 << bg) != 0;
            self.enable_bg(bg, enable);
        }
    }

    fn enable_bg(&mut self, bg: usize, active: bool) {
        let was_active = self.video.sw.bg[bg].enabled;
        if !active {
            if self.video.sw.next_y == 0 || (was_active > 0 && was_active < ENABLED_MAX) {
                self.video.sw.bg[bg].enabled = 0;
            } else if was_active == ENABLED_MAX {
                self.video.sw.bg[bg].enabled = -2;
            }
        } else if was_active == 0 && active {
            if self.video.sw.next_y == 0 {
                self.video.sw.bg[bg].enabled = ENABLED_MAX;
            } else if dispcnt_mode(self.video.sw.dispcnt) > 2 {
                self.video.sw.bg[bg].enabled = 2;
            } else {
                self.video.sw.bg[bg].enabled = 1;
            }
        } else if was_active < 0 && active {
            self.video.sw.bg[bg].enabled = ENABLED_MAX;
        }
    }

    pub fn renderer_write_vram(&mut self, _address: u32) {
        self.video.sw.scanline_dirty = [0xFFFFFFFF; 5];
        for bg in self.video.sw.bg.iter_mut() {
            bg.y_cache = -1;
        }
    }

    pub fn renderer_write_oam(&mut self, _oam: u32) {
        self.video.sw.oam_dirty = true;
        self.video.sw.scanline_dirty = [0xFFFFFFFF; 5];
    }

    /// GBAVideoSoftwareRendererWriteBLDCNT
    fn write_bldcnt(&mut self, value: u16) {
        let old_effect = self.video.sw.blend_effect;
        self.video.sw.bg[0].target1 = (value & 0x1) as i32;
        self.video.sw.bg[1].target1 = ((value >> 1) & 1) as i32;
        self.video.sw.bg[2].target1 = ((value >> 2) & 1) as i32;
        self.video.sw.bg[3].target1 = ((value >> 3) & 1) as i32;
        self.video.sw.bg[0].target2 = ((value >> 8) & 1) as i32;
        self.video.sw.bg[1].target2 = ((value >> 9) & 1) as i32;
        self.video.sw.bg[2].target2 = ((value >> 10) & 1) as i32;
        self.video.sw.bg[3].target2 = ((value >> 11) & 1) as i32;
        self.video.sw.blend_effect = ((value >> 6) & 3) as i32;
        self.video.sw.target1_obj = ((value >> 4) & 1) as u32;
        self.video.sw.target1_bd = ((value >> 5) & 1) as u32;
        self.video.sw.target2_obj = ((value >> 12) & 1) as u32;
        self.video.sw.target2_bd = ((value >> 13) & 1) as u32;
        if old_effect != self.video.sw.blend_effect {
            self.video.sw.blend_dirty = true;
        }
    }

    /// GBAVideoSoftwareRendererFinishFrame
    pub fn renderer_finish_frame(&mut self) {
        let sw = &mut self.video.sw;
        sw.next_y = 0;
        sw.bg[2].sx = sw.bg[2].ref_x;
        sw.bg[2].sy = sw.bg[2].ref_y;
        sw.bg[3].sx = sw.bg[3].ref_x;
        sw.bg[3].sy = sw.bg[3].ref_y;
        for i in 0..4 {
            if sw.bg[i].enabled > 0 {
                sw.bg[i].enabled = ENABLED_MAX;
            }
        }
        for i in 0..2 {
            let win = &mut self.video.sw.win_n[i];
            if win.v.end >= VIDEO_VERTICAL_PIXELS as u8
                && win.v.end < VIDEO_VERTICAL_TOTAL_PIXELS as u8
            {
                win.on = false;
            }
            if win.v.start >= VIDEO_VERTICAL_PIXELS as u8
                && win.v.start < VIDEO_VERTICAL_TOTAL_PIXELS as u8
                && win.v.start > win.v.end
            {
                win.on = true;
            }
        }
    }

    pub fn renderer_write_palette(&mut self, address: u32, value: u16) {
        let sw = &mut self.video.sw;
        let color = color_from_555(value);
        let palette_idx = (address >> 1) as usize;
        sw.normal_palette[palette_idx] = color;
        if sw.blend_effect == BLEND_BRIGHTEN {
            sw.variant_palette[palette_idx] = brighten(color, sw.bldy);
        } else if sw.blend_effect == BLEND_DARKEN {
            sw.variant_palette[palette_idx] = darken(color, sw.bldy);
        }
        let highlight_amount = (sw.highlight_amount >> 4) as i32;
        if highlight_amount != 0 {
            sw.highlight_palette[palette_idx] = color_mix_5bit(
                0x10 - highlight_amount,
                sw.normal_palette[palette_idx],
                highlight_amount,
                sw.highlight_color,
            );
            sw.highlight_variant_palette[palette_idx] = color_mix_5bit(
                0x10 - highlight_amount,
                sw.variant_palette[palette_idx],
                highlight_amount,
                sw.highlight_color,
            );
        } else {
            sw.highlight_palette[palette_idx] = sw.normal_palette[palette_idx];
            sw.highlight_variant_palette[palette_idx] = sw.variant_palette[palette_idx];
        }
        for b in sw.scanline_dirty.iter_mut() {
            *b = 0xFFFFFFFF;
        }
    }
}

// Rest of the renderer machinery.
// GBAVideoSoftwareRendererDrawScanline's top-level, ScanlineStep calls.

impl Gba {
    fn renderer_scale_dirty_next_y(&mut self) {
        // Nop — the renderer writes to `scanline_dirty` on vram/register writes.
    }

    /// GBAVideoSoftwareRendererStepWindow
    fn step_window(&mut self, y: i32) {
        let win0y = self.video.sw.win_n[0];
        let win1y = self.video.sw.win_n[1];
        if y == win0y.v.start as i32 + win0y.offset_y as i32 {
            self.video.sw.win_n[0].on = true;
        }
        if y == win0y.v.end as i32 + win0y.offset_y as i32 {
            self.video.sw.win_n[0].on = false;
        }
        if y == win1y.v.start as i32 + win1y.offset_y as i32 {
            self.video.sw.win_n[1].on = true;
        }
        if y == win1y.v.end as i32 + win1y.offset_y as i32 {
            self.video.sw.win_n[1].on = false;
        }
    }

    /// _breakWindow/_breakWindowInner
    fn break_window(&mut self, win: WindowN) {
        if win.h.end > VIDEO_HORIZONTAL_PIXELS as u8 || win.h.end < win.h.start {
            let mut splits = [win, win];
            splits[0].h.start = 0;
            splits[1].h.end = VIDEO_HORIZONTAL_PIXELS as u8;
            self.break_window_inner(&splits[0]);
            self.break_window_inner(&splits[1]);
        } else {
            self.break_window_inner(&win);
        }
    }

    fn break_window_inner(&mut self, win: &WindowN) {
        let mut start_x: i32 = 0;
        if win.h.end > 0 {
            let mut active = 0usize;
            while active < self.video.sw.n_windows as usize {
                if win.h.start < self.video.sw.windows[active].end_x {
                    let old_window = self.video.sw.windows[active];
                    if win.h.start as i32 > start_x {
                        let mut next = self.video.sw.n_windows as usize;
                        self.video.sw.n_windows += 1;
                        while next > active {
                            self.video.sw.windows[next] = self.video.sw.windows[next - 1];
                            next -= 1;
                        }
                        self.video.sw.windows[active].end_x = win.h.start;
                        active += 1;
                    }
                    self.video.sw.windows[active].control = win.control;
                    self.video.sw.windows[active].end_x = win.h.end;
                    if win.h.end >= old_window.end_x {
                        // Trim off extra windows we've overwritten
                        active += 1;
                        while (self.video.sw.n_windows as usize) > active + 1
                            && win.h.end >= self.video.sw.windows[active].end_x
                        {
                            self.video.sw.windows[active] = self.video.sw.windows[active + 1];
                            self.video.sw.n_windows -= 1;
                            active += 1;
                        }
                    } else {
                        active += 1;
                        let mut next = self.video.sw.n_windows as usize;
                        self.video.sw.n_windows += 1;
                        while next > active {
                            self.video.sw.windows[next] = self.video.sw.windows[next - 1];
                            next -= 1;
                        }
                        self.video.sw.windows[active] = old_window;
                    }
                    break;
                }
                start_x = self.video.sw.windows[active].end_x as i32;
                active += 1;
            }
        }
    }

    /// GBAVideoSoftwareRendererPrepareWindow
    fn prepare_window(&mut self) {
        let objwin_slow_path = dispcnt_objwin_enable(self.video.sw.dispcnt);
        if objwin_slow_path {
            for bg in 0..4usize {
                let bg_bit = 1 << bg;
                let on_obj = self.video.sw.objwin.packed & bg_bit != 0;
                let on_cur = self.video.sw.current_window.packed & bg_bit != 0;
                self.video.sw.bg[bg].objwin_force_enable = (on_obj && on_cur) as i32;
                self.video.sw.bg[bg].objwin_only = !on_obj;
            }
        }
        match dispcnt_mode(self.video.sw.dispcnt) {
            0 => {
                self.try_update_flags(0);
                self.try_update_flags(1);
                self.try_update_flags(3);
                self.try_update_flags(2);
            }
            1 => {
                self.try_update_flags(0);
                self.try_update_flags(1);
                self.try_update_flags(2);
            }
            2 => {
                self.try_update_flags(3);
                self.try_update_flags(2);
            }
            3 | 4 | 5 => {
                self.try_update_flags(2);
            }
            _ => {}
        }
    }

    fn try_update_flags(&mut self, bgidx: usize) {
        if self.video.sw.bg[bgidx].enabled != ENABLED_MAX {
            return;
        }
        self.update_flags(bgidx);
    }
}

// ---------------------------------------------------------------------------
// The scanline composer (DrawScanline) + preprocessing and per-mode drawing.

impl Gba {
    /// GBAVideoSoftwareRendererDrawScanline
    pub fn renderer_draw_scanline(&mut self, y: i32) {
        self.video.sw.next_y = if y == VIDEO_VERTICAL_PIXELS - 1 {
            0
        } else {
            y + 1
        };
        let ny = self.video.sw.next_y;

        let mut dirty = self.video.sw.scanline_dirty[(y >> 5) as usize] & (1u32 << (y & 0x1F)) != 0;
        if self.video.sw.next_io[..] != self.video.sw.cache[y as usize].io[..] {
            self.video.sw.cache[y as usize]
                .io
                .copy_from_slice(&self.video.sw.next_io);
            dirty = true;
        }

        if dispcnt_mode(self.video.sw.dispcnt) != 0 {
            if self.video.sw.cache[y as usize].scale[0][0] != self.video.sw.bg[2].sx
                || self.video.sw.cache[y as usize].scale[0][1] != self.video.sw.bg[2].sy
                || self.video.sw.cache[y as usize].scale[1][0] != self.video.sw.bg[3].sx
                || self.video.sw.cache[y as usize].scale[1][1] != self.video.sw.bg[3].sy
            {
                dirty = true;
            }
        }
        self.video.sw.cache[y as usize].scale[0][0] = self.video.sw.bg[2].sx;
        self.video.sw.cache[y as usize].scale[0][1] = self.video.sw.bg[2].sy;
        self.video.sw.cache[y as usize].scale[1][0] = self.video.sw.bg[3].sx;
        self.video.sw.cache[y as usize].scale[1][1] = self.video.sw.bg[3].sy;

        self.step_window(y);
        if self.video.sw.cache[y as usize].window_on[0] != self.video.sw.win_n[0].on
            || self.video.sw.cache[y as usize].window_on[1] != self.video.sw.win_n[1].on
        {
            dirty = true;
        }
        self.video.sw.cache[y as usize].window_on[0] = self.video.sw.win_n[0].on;
        self.video.sw.cache[y as usize].window_on[1] = self.video.sw.win_n[1].on;

        if !dirty {
            if dispcnt_mode(self.video.sw.dispcnt) != 0 {
                if self.video.sw.bg[2].enabled == ENABLED_MAX {
                    self.video.sw.bg[2].sx = self.video.sw.bg[2]
                        .sx
                        .wrapping_add(self.video.sw.bg[2].dmx as i32);
                    self.video.sw.bg[2].sy = self.video.sw.bg[2]
                        .sy
                        .wrapping_add(self.video.sw.bg[2].dmy as i32);
                }
                if self.video.sw.bg[3].enabled == ENABLED_MAX {
                    self.video.sw.bg[3].sx = self.video.sw.bg[3]
                        .sx
                        .wrapping_add(self.video.sw.bg[3].dmx as i32);
                    self.video.sw.bg[3].sy = self.video.sw.bg[3]
                        .sy
                        .wrapping_add(self.video.sw.bg[3].dmy as i32);
                }
            }
            return;
        }

        self.video.sw.scanline_dirty[(y >> 5) as usize] &= !(1u32 << (y & 0x1F));

        let out_y = y as usize;
        let out_stride = self.video.sw.output_stride;

        if dispcnt_forced_blank(self.video.sw.dispcnt) {
            let row_base = out_y * out_stride;
            for x in 0..240usize {
                self.video.sw.output[row_base + x] = 0xFFFFFFFF;
            }
            return;
        }

        self.preprocess_buffer();
        self.video.sw.sprite_cycles_remaining = if dispcnt_hblank_free(self.video.sw.dispcnt) {
            VIDEO_OAM_BLANK_PIXELS
        } else {
            VIDEO_OBJ_LENGTH
        };
        let mut sprite_layers = self.preprocess_sprite_layer(y);

        let mut w = 0usize;
        self.video.sw.end = 0;
        while w < self.video.sw.n_windows as usize {
            self.video.sw.start = self.video.sw.end;
            self.video.sw.end = self.video.sw.windows[w].end_x as i32;
            self.video.sw.current_window = self.video.sw.windows[w].control;
            self.prepare_window();
            let mut priority = 0u32;
            while priority < 4 {
                if sprite_layers & (1 << priority) != 0 {
                    self.postprocess_sprite(priority);
                }
                if self.layer_enabled(0, priority) {
                    self.draw_background_mode0(0, y);
                }
                if self.layer_enabled(1, priority) {
                    self.draw_background_mode0(1, y);
                }
                if self.layer_enabled(2, priority) {
                    match dispcnt_mode(self.video.sw.dispcnt) {
                        0 => self.draw_background_mode0(2, y),
                        1 => self.draw_background_mode2(2, y),
                        2 => self.draw_background_mode2(2, y),
                        3 => self.draw_background_mode3(y),
                        4 => self.draw_background_mode4(y),
                        5 => self.draw_background_mode5(y),
                        _ => {}
                    }
                }
                if self.layer_enabled(3, priority) {
                    match dispcnt_mode(self.video.sw.dispcnt) {
                        0 => self.draw_background_mode0(3, y),
                        2 => self.draw_background_mode2(3, y),
                        _ => {}
                    }
                }
                priority += 1;
            }
            w += 1;
        }

        self.postprocess_buffer();

        if dispcnt_mode(self.video.sw.dispcnt) != 0 {
            if self.video.sw.bg[2].enabled == ENABLED_MAX {
                self.video.sw.bg[2].sx = self.video.sw.bg[2]
                    .sx
                    .wrapping_add(self.video.sw.bg[2].dmx as i32);
                self.video.sw.bg[2].sy = self.video.sw.bg[2]
                    .sy
                    .wrapping_add(self.video.sw.bg[2].dmy as i32);
            }
            if self.video.sw.bg[3].enabled == ENABLED_MAX {
                self.video.sw.bg[3].sx = self.video.sw.bg[3]
                    .sx
                    .wrapping_add(self.video.sw.bg[3].dmx as i32);
                self.video.sw.bg[3].sy = self.video.sw.bg[3]
                    .sy
                    .wrapping_add(self.video.sw.bg[3].dmy as i32);
            }
        }

        for bg in 0..4 {
            if self.video.sw.bg[bg].enabled != 0 && self.video.sw.bg[bg].enabled < ENABLED_MAX {
                self.video.sw.bg[bg].enabled += 1;
                self.video.sw.scanline_dirty[(y >> 5) as usize] |= 1u32 << (y & 0x1F);
            }
        }

        // blit to output
        let row_base = out_y * out_stride;
        if self.video.sw.stereo {
            for x in (0..240usize).step_by(4) {
                let v0 = self.video.sw.row[x] & (0x0000FF | 0xFF0000);
                self.video.sw.output[row_base + x] = v0;
                self.video.sw.output[row_base + x + 1] =
                    self.video.sw.row[x + 1] & (0x0000FF | 0xFF0000);
                self.video.sw.output[row_base + x + 2] =
                    self.video.sw.row[x + 2] & (0x0000FF | 0xFF0000);
                self.video.sw.output[row_base + x + 3] =
                    self.video.sw.row[x + 3] & (0x0000FF | 0xFF0000);
                self.video.sw.output[row_base + x + 1] |= self.video.sw.row[x] & 0x00FF00;
                self.video.sw.output[row_base + x + 2] |= self.video.sw.row[x + 3] & 0x00FF00;
                self.video.sw.output[row_base + x + 3] |= self.video.sw.row[x + 2] & 0x00FF00;
                self.video.sw.output[row_base + x] = self.video.sw.row[x] & (0x0000FF | 0xFF0000)
                    | (self.video.sw.output[row_base + x + 1] & 0x00FF00);
            }
        } else {
            for x in 0..240usize {
                let px = self.video.sw.row[x];
                self.video.sw.output[row_base + x] = out_pixel(px);
            }
        }
        sprite_layers = 0;
        let _ = sprite_layers;
    }

    fn layer_enabled(&self, x: usize, priority: u32) -> bool {
        let sw = &self.video.sw;
        if sw.disable_bg[x] {
            return false;
        }
        if sw.bg[x].enabled != ENABLED_MAX {
            return false;
        }
        // TEST_LAYER_ENABLED
        let win_ok = win_bg_enable(sw.current_window.packed, x)
            || (dispcnt_objwin_enable(sw.dispcnt) && win_bg_enable(sw.objwin.packed, x));
        if !win_ok {
            return false;
        }
        sw.bg[x].priority == priority
    }

    /// GBAVideoSoftwareRendererPreprocessBuffer
    fn preprocess_buffer(&mut self) {
        let sw = &mut self.video.sw;
        for x in (0..240usize).step_by(4) {
            sw.sprite_layer[x] = FLAG_UNWRITTEN;
            sw.sprite_layer[x + 1] = FLAG_UNWRITTEN;
            sw.sprite_layer[x + 2] = FLAG_UNWRITTEN;
            sw.sprite_layer[x + 3] = FLAG_UNWRITTEN;
        }

        let d = self.video.sw.dispcnt;
        let mut win1_set = false;
        let mut win0_set = false;
        if dispcnt_win1_enable(d) && !self.video.sw.disable_win[1] && self.video.sw.win_n[1].on {
            self.video.sw.win_n[1].on.clone_from(&true);
            win1_set = true;
        }
        if dispcnt_win0_enable(d) && !self.video.sw.disable_win[0] && self.video.sw.win_n[0].on {
            win0_set = true;
        }
        self.video.sw.windows[0].end_x = 240;
        self.video.sw.n_windows = 1;
        if dispcnt_win0_enable(d) || dispcnt_win1_enable(d) || dispcnt_objwin_enable(d) {
            self.video.sw.windows[0].control = self.video.sw.winout;
            if win1_set {
                let win = self.video.sw.win_n[1];
                self.break_window(win);
            }
            if win0_set {
                let win = self.video.sw.win_n[0];
                self.break_window(win);
            }
        } else {
            self.video.sw.windows[0].control.packed = 0xFF;
        }

        self.update_dispcnt();

        let sw = &mut self.video.sw;
        if sw.last_highlight_amount != sw.highlight_amount {
            sw.last_highlight_amount = sw.highlight_amount;
            if sw.last_highlight_amount != 0 {
                sw.blend_dirty = true;
            }
        }
        if sw.blend_dirty {
            self.update_palettes();
            self.video.sw.blend_dirty = false;
        }
        self.video.sw.force_target1 = false;

        let mut x = 0usize;
        for w in 0..self.video.sw.n_windows as usize {
            let mut backdrop = FLAG_UNWRITTEN | FLAG_PRIORITY | FLAG_IS_BACKGROUND;
            if self.video.sw.target1_bd == 0
                || self.video.sw.blend_effect == BLEND_NONE
                || self.video.sw.blend_effect == BLEND_ALPHA
                || self.video.sw.windows[w].control.packed & WIN_BLEND_ENABLE == 0
            {
                backdrop |= self.video.sw.normal_palette[0];
            } else {
                backdrop |= self.video.sw.variant_palette[0];
            }
            let end = self.video.sw.windows[w].end_x as usize;
            while x & 3 != 0 && x < end {
                self.video.sw.row[x] = backdrop;
                x += 1;
            }
            while x + 3 < end {
                for i in 0..4 {
                    self.video.sw.row[x + i] = backdrop;
                }
                x += 4;
            }
            while x < end {
                self.video.sw.row[x] = backdrop;
                x += 1;
            }
        }

        for bg in 0..4 {
            self.video.sw.bg[bg].highlight = self.video.sw.highlight_bg[bg];
        }
    }

    /// GBAVideoSoftwareRendererPostprocessBuffer
    fn postprocess_buffer(&mut self) {
        let sw = &mut self.video.sw;
        if (sw.force_target1
            || sw.bg[0].target1 != 0
            || sw.bg[1].target1 != 0
            || sw.bg[2].target1 != 0
            || sw.bg[3].target1 != 0)
            && sw.target2_bd != 0
        {
            let mut x = 0usize;
            for w in 0..self.video.sw.n_windows as usize {
                let mut backdrop = 0u32;
                if self.video.sw.target1_bd == 0
                    || self.video.sw.blend_effect == BLEND_NONE
                    || self.video.sw.blend_effect == BLEND_ALPHA
                    || self.video.sw.windows[w].control.packed & WIN_BLEND_ENABLE == 0
                {
                    backdrop |= self.video.sw.normal_palette[0];
                } else {
                    backdrop |= self.video.sw.variant_palette[0];
                }
                let end = self.video.sw.windows[w].end_x as usize;
                while x < end {
                    let color = self.video.sw.row[x];
                    if color & FLAG_TARGET_1 != 0 {
                        let mixed = color_mix_5bit(
                            self.video.sw.bldb as i32,
                            backdrop,
                            self.video.sw.blda as i32,
                            color,
                        );
                        self.video.sw.row[x] = mixed;
                    }
                    x += 1;
                }
            }
        }
        if self.video.sw.force_target1
            && (self.video.sw.blend_effect == BLEND_DARKEN
                || self.video.sw.blend_effect == BLEND_BRIGHTEN)
        {
            let mut x = 0usize;
            for w in 0..self.video.sw.n_windows as usize {
                let end = self.video.sw.windows[w].end_x as usize;
                let mut mask = FLAG_REBLEND | FLAG_IS_BACKGROUND;
                let mut match_ = FLAG_REBLEND;
                let obj_blend = self.video.sw.objwin.packed & WIN_BLEND_ENABLE != 0;
                let win_blend = self.video.sw.windows[w].control.packed & WIN_BLEND_ENABLE != 0;
                if dispcnt_objwin_enable(self.video.sw.dispcnt) && obj_blend != win_blend {
                    mask |= FLAG_OBJWIN;
                    if obj_blend {
                        match_ |= FLAG_OBJWIN;
                    }
                } else if !win_blend {
                    x = end;
                    continue;
                }
                if self.video.sw.blend_effect == BLEND_DARKEN {
                    while x < end {
                        let color = self.video.sw.row[x];
                        if color & mask == match_ {
                            self.video.sw.row[x] = darken(color, self.video.sw.bldy);
                        }
                        x += 1;
                    }
                } else if self.video.sw.blend_effect == BLEND_BRIGHTEN {
                    while x < end {
                        let color = self.video.sw.row[x];
                        if color & mask == match_ {
                            self.video.sw.row[x] = brighten(color, self.video.sw.bldy);
                        }
                        x += 1;
                    }
                }
            }
        }
    }

    /// GBAVideoSoftwareRendererPreprocessSpriteLayer
    fn preprocess_sprite_layer(&mut self, y: i32) -> i32 {
        let mut sprite_layers = 0i32;
        let obj_enabled = self.video.sw.dispcnt & 0x1000 != 0;
        if !obj_enabled || self.video.sw.disable_obj {
            return 0;
        }

        if self.video.sw.oam_dirty {
            self.video.sw.oam_max = renderer_clean_oam(
                &self.video.oam,
                &mut self.video.sw.sprites,
                self.video.sw.obj_offset_y,
            );
            self.video.sw.oam_dirty = false;
        }
        let mosaic_v = mosaic_obj_v(self.video.sw.mosaic) + 1;
        let mosaic_y = y - (y % mosaic_v);
        let mut i = 0usize;
        let mut last_index = 0i32;
        while i < self.video.sw.oam_max as usize {
            let sprite = self.video.sw.sprites[i];
            let mut local_y = y;
            self.video.sw.end = 0;
            let idx_delta = sprite.index as i32 - last_index;
            last_index = sprite.index as i32;
            self.video.sw.sprite_cycles_remaining -= 2 * idx_delta.max(0);
            if self.video.sw.sprite_cycles_remaining <= 0 {
                break;
            }
            if y < sprite.y as i32 || y >= sprite.end_y as i32 {
                i += 1;
                continue;
            }
            if obj_a_mosaic(sprite.obj.a) && mosaic_v > 1 {
                local_y = mosaic_y;
                if local_y < sprite.y as i32 && (sprite.y as i32) < VIDEO_VERTICAL_PIXELS {
                    local_y = sprite.y as i32;
                }
                if local_y >= (sprite.end_y & 0xFF) as i32 {
                    local_y = sprite.end_y as i32 - 1;
                }
            }
            let mut w = 0usize;
            while w < self.video.sw.n_windows as usize {
                self.video.sw.current_window = self.video.sw.windows[w].control;
                self.video.sw.start = self.video.sw.end;
                self.video.sw.end = self.video.sw.windows[w].end_x as i32;
                if self.video.sw.current_window.packed & WIN_OBJ_ENABLE == 0
                    && !dispcnt_objwin_enable(self.video.sw.dispcnt)
                {
                    w += 1;
                    continue;
                }
                let drawn = self.preprocess_sprite(sprite.obj, sprite.index as i8 as i32, local_y);
                sprite_layers |= drawn << obj_c_priority(sprite.obj.c);
                w += 1;
            }
            self.video.sw.sprite_cycles_remaining -= sprite.cycles as i32;
            i += 1;
        }
        sprite_layers
    }
}

// ---------------------------------------------------------------------------
// software-mode0.c / -bg.c / -obj.c

impl Gba {
    #[inline]
    fn draw_scanline_end_pad(&mut self, _y: i32) {}

    /// GBAVideoSoftwareRendererDrawBackgroundMode0 (per-pixel walk;
    /// tile-row map cache identical to the C).
    fn draw_background_mode0(&mut self, bg: usize, y: i32) {
        let mut y = y;
        let sw_start = self.video.sw.start;
        let sw_end = self.video.sw.end;
        let background = self.video.sw.bg[bg];
        let in_x0 = (sw_start + background.x as i32 - background.offset_x) & 0x1FF;
        if background.mosaic {
            let mosaic_v = mosaic_bg_v(self.video.sw.mosaic) + 1;
            y -= y % mosaic_v;
        }
        let in_y = y + background.y as i32 - background.offset_y;

        println!(
            "mode0 bg={} enabled={} y={} sw={} end={} inx={:x} iny={:x} sel_idx...",
            bg, self.video.sw.bg[bg].enabled, y, sw_start, sw_end, in_x0, in_y
        );
        let mut y_base = in_y & 0xF8;
        if background.size == 2 {
            y_base += in_y & 0x100;
        } else if background.size == 3 {
            y_base += (in_y & 0x100) << 1;
        }
        y_base = ((background.screen_base >> 1) as i32 + (y_base << 2)) as i32;

        let flags = background.flags;
        let objwin_flags = background.objwin_flags;
        let variant = background.variant;
        let slow_path = dispcnt_objwin_enable(self.video.sw.dispcnt);

        if self.video.sw.bg[bg].y_cache != (in_y >> 3) {
            let mut lx = 0i32;
            let mut entries = self.video.sw.bg[bg].map_cache;
            for t in 0..64usize {
                let mut x_base = lx & 0xF8;
                if background.size & 1 != 0 {
                    x_base += (lx & 0x100) << 5;
                }
                let screen_base = y_base + (x_base >> 3);
                let p = (screen_base << 1) as usize;
                entries[t] = u16::from_le_bytes([self.video.vram[p], self.video.vram[p + 1]]);
                lx += 8;
            }
            self.video.sw.bg[bg].map_cache = entries;
            self.video.sw.bg[bg].y_cache = in_y >> 3;
        }

        let mut mosaic_wait = 0i32;
        let mosaic_h = mosaic_bg_h(self.video.sw.mosaic) + 1;
        if background.mosaic && mosaic_h > 1 {
            mosaic_wait = (mosaic_h - sw_start + VIDEO_HORIZONTAL_PIXELS * mosaic_h) % mosaic_h;
        }

        let mut carry_color: u32 = 0;
        let mut have_carry = false;
        let mut out_x = sw_start;
        let mut local_x = in_x0 & 0x1FF;
        while out_x < sw_end {
            let map_data = self.video.sw.bg[bg].map_cache[((local_x >> 3) & 0x3F) as usize];
            let mut local_y = (in_y & 0x7) as usize;
            if map_data & 0x0800 != 0 {
                local_y = 7 - local_y;
            }
            let mut color: u32 = 0;
            let mut fresh = true;
            if mosaic_h > 1 && mosaic_wait != 0 {
                fresh = false;
            } else if mosaic_h > 1 {
                mosaic_wait = mosaic_h;
            }

            if fresh {
                let tile_off = if !background.multipalette {
                    ((map_data & 0x3FF) as usize) << 5
                } else {
                    ((map_data & 0x3FF) as usize) << 6
                };
                let char_base = background.char_base as usize
                    + tile_off
                    + (local_y * if background.multipalette { 8 } else { 4 });

                if !background.multipalette {
                    let shift_bytes = (local_x & 7) as usize;
                    let byte_off = char_base + ((shift_bytes >> 1) * 4) + (shift_bytes & 1);
                    let _ = byte_off;
                    // load 4-byte tile row word: fetch 4 bytes at once
                    if char_base < 0x10000 {
                        let td32 = u32::from_le_bytes([
                            self.video.vram[char_base],
                            self.video.vram[char_base + 1],
                            self.video.vram[char_base + 2],
                            self.video.vram[char_base + 3],
                        ]);
                        let mut td = td32;
                        let mut shift = local_x & 7;
                        if map_data & 0x0400 != 0 {
                            shift = 7 - shift;
                        }
                        td >>= (shift * 4) as u32;
                        let _ = td32;
                        color = td & 0xF;
                        if color != 0 {
                            carry_color = self.choose_palette(
                                bg,
                                16,
                                color,
                                (((map_data >> 12) & 0xF) << 4) as u32,
                                variant,
                            );
                            have_carry = true;
                        } else {
                            have_carry = false;
                        }
                    } else {
                        have_carry = false;
                    }
                } else {
                    if char_base < 0x10000 {
                        let byte_idx = char_base + ((local_x & 7) as usize);
                        let mut bx = byte_idx;
                        let _ = bx;
                        let v = &self.video.vram;
                        let flip_h = map_data & 0x0400 != 0;
                        let idx_x = if flip_h {
                            7 - (local_x & 7)
                        } else {
                            local_x & 7
                        };
                        let _ = idx_x;
                        let px_off = char_base + idx_x as usize;
                        color = v[px_off] as u32;
                        if color != 0 {
                            carry_color = self.choose_palette(bg, 256, color, 0, variant);
                            have_carry = true;
                        } else {
                            have_carry = false;
                        }
                    } else {
                        have_carry = false;
                    }
                }
                if mosaic_h > 1 {
                    mosaic_wait -= 0; // handled at loop head
                }
            }
            if mosaic_h > 1 {
                mosaic_wait -= 1;
            }
            let current = self.video.sw.row[out_x as usize];
            if carry_color != 0 && is_writable(current) {
                self.mode0_compose(
                    out_x as usize,
                    carry_color,
                    flags,
                    objwin_flags,
                    slow_path,
                    current,
                );
            }
            out_x += 1;
            local_x = (local_x + 1) & 0x1FF;
        }
    }

    fn choose_palette(
        &mut self,
        bg: usize,
        _bpp: i32,
        color_idx: u32,
        palette_data: u32,
        variant: bool,
    ) -> u32 {
        let sw = &self.video.sw;
        let idx = (palette_data | color_idx) as usize & 0x1FF;
        let highlight = sw.highlight_amount != 0 && sw.bg[bg].highlight;
        if variant && highlight {
            sw.highlight_variant_palette[idx]
        } else if variant {
            sw.variant_palette[idx]
        } else if highlight {
            sw.highlight_palette[idx]
        } else {
            sw.normal_palette[idx]
        }
    }

    /// One-mode 0 pixel composite (COMPOSITE_* dispatch without the macro).
    fn mode0_compose(
        &mut self,
        idx: usize,
        color: u32,
        flags: u32,
        objwin_flags: u32,
        slow_path: bool,
        current: u32,
    ) {
        if !slow_path {
            let mut c = color | flags;
            if c >= current {
                if current & FLAG_TARGET_1 != 0 && c & FLAG_TARGET_2 != 0 {
                    c = color_mix_5bit(
                        self.video.sw.blda as i32,
                        current,
                        self.video.sw.bldb as i32,
                        c,
                    );
                    self.video.sw.row[idx] = c;
                } else {
                    self.video.sw.row[idx] = current & (0x00FFFFFF | FLAG_REBLEND | FLAG_OBJWIN);
                }
            } else {
                c = c & !FLAG_TARGET_2;
                self.video.sw.row[idx] = c;
            }
        } else {
            let bg_idx = ((flags >> OFFSET_INDEX) & 3) as usize;
            let objwin_only = self.video.sw.bg[bg_idx].objwin_only;
            let force = self.video.sw.bg[bg_idx].objwin_force_enable;
            if force != 0 || (!(current & FLAG_OBJWIN != 0)) == objwin_only {
                let mut merged_flags = flags;
                let mut c = color;
                if current & FLAG_OBJWIN != 0 {
                    merged_flags = objwin_flags;
                    c = self.video.sw.normal_palette[(color as usize) & 0x1FF];
                }
                let mut co = c | merged_flags;
                if co >= current {
                    if current & FLAG_TARGET_1 != 0 && co & FLAG_TARGET_2 != 0 {
                        co = color_mix_5bit(
                            self.video.sw.blda as i32,
                            current,
                            self.video.sw.bldb as i32,
                            co,
                        );
                        self.video.sw.row[idx] = co;
                    } else {
                        self.video.sw.row[idx] =
                            current & (0x00FFFFFF | FLAG_REBLEND | FLAG_OBJWIN);
                    }
                } else {
                    self.video.sw.row[idx] = (co & !FLAG_TARGET_2) | (current & FLAG_OBJWIN);
                }
            }
        }
    }

    fn draw_background_mode2(&mut self, bg: usize, in_y: i32) {
        let background = self.video.sw.bg[bg];
        let size_adjusted = 0x8000i32 << background.size;
        let sw_start = self.video.sw.start;
        let sw_end = self.video.sw.end;

        let mut x = background.sx + (sw_start - 1) * background.dx as i32;
        let mut y = background.sy + (sw_start - 1) * background.dy as i32;
        let mut mosaic_h = 0;
        let mut mosaic_wait = 0i32;
        let mut local_x: i32;
        let mut local_y: i32;
        if background.mosaic {
            let mosaic_v = mosaic_bg_v(self.video.sw.mosaic) + 1;
            mosaic_h = mosaic_bg_h(self.video.sw.mosaic) + 1;
            mosaic_wait = (mosaic_h - sw_start + VIDEO_HORIZONTAL_PIXELS * mosaic_h) % mosaic_h;
            let start_x = sw_start - (sw_start % mosaic_h);
            local_x = -(in_y % mosaic_v) * background.dmx as i32;
            local_y = -(in_y % mosaic_v) * background.dmy as i32;
            x = x.wrapping_add(local_x);
            y = y.wrapping_add(local_y);
            x = x.wrapping_add(background.sx + start_x * background.dx as i32 - local_x);
            y = y.wrapping_add(background.sy + start_x * background.dy as i32 - local_y);
            x = background.sx + start_x * background.dx as i32 + local_x;
            y = background.sy + start_x * background.dy as i32 + local_y;
        }

        let slow_path = dispcnt_objwin_enable(self.video.sw.dispcnt);
        let flags = background.flags;
        let objwin_flags = background.objwin_flags;
        let variant = background.variant;
        let highlight = self.video.sw.highlight_amount != 0 && background.highlight;

        let mut pixel_data: u32 = 0;
        let mut out_x = sw_start;
        while out_x < sw_end {
            x = x.wrapping_add(background.dx as i32);
            y = y.wrapping_add(background.dy as i32);

            if !background.overflow {
                if (x | y) & !(size_adjusted - 1) != 0 {
                    out_x += 1;
                    continue;
                }
                local_x = x;
                local_y = y;
            } else {
                local_x = x & (size_adjusted - 1);
                local_y = y & (size_adjusted - 1);
            }

            if mosaic_wait == 0 {
                let map_idx = (local_x >> 11) as usize
                    + ((((local_y >> 7) & 0x7F0) as usize) << background.size);
                let map_data = self.video.vram[background.screen_base as usize + map_idx];
                let char_off = (background.char_base + ((map_data as u32) << 6)) as usize
                    + (((local_y & 0x700) >> 5) as usize)
                    + ((local_x & 0x700) >> 8) as usize;
                pixel_data = self.video.vram[char_off] as u32;
                mosaic_wait = mosaic_h;
            } else {
                mosaic_wait -= 1;
            }

            let current = self.video.sw.row[out_x as usize];
            if pixel_data != 0 && is_writable(current) {
                self.bg_mode2_composite(
                    bg,
                    pixel_data,
                    flags,
                    objwin_flags,
                    variant,
                    slow_path,
                    out_x as usize,
                    current,
                );
            }
            out_x += 1;
        }
    }

    fn draw_background_mode3(&mut self, in_y: i32) {
        let background = self.video.sw.bg[2];
        let sw_start = self.video.sw.start;
        let sw_end = self.video.sw.end;
        let mut x = background.sx + (sw_start - 1) * background.dx as i32;
        let mut y = background.sy + (sw_start - 1) * background.dy as i32;
        let mut mosaic_wait = 0i32;
        let mut mosaic_h = 1i32;
        if background.mosaic {
            let mosaic_v = mosaic_bg_v(self.video.sw.mosaic) + 1;
            mosaic_h = mosaic_bg_h(self.video.sw.mosaic) + 1;
            mosaic_wait = (mosaic_h - sw_start + VIDEO_HORIZONTAL_PIXELS * mosaic_h) % mosaic_h;
            let start_x = sw_start - (sw_start % mosaic_h);
            x = x.wrapping_add(-(in_y % mosaic_v) * background.dmx as i32);
            y = y.wrapping_add(-(in_y % mosaic_v) * background.dmy as i32);
            x = x.wrapping_add(background.sx + start_x * background.dx as i32 - x);
            y = y.wrapping_add(background.sy + start_x * background.dy as i32 - y);
        }
        let slow_path = dispcnt_objwin_enable(self.video.sw.dispcnt);
        let flags = background.flags;
        let objwin_flags = background.objwin_flags;
        let variant = background.variant;

        // First-sample shortcut
        let mut color = self.video.sw.normal_palette[0];
        if mosaic_wait != 0
            && x >= 0
            && y >= 0
            && (x >> 8) < VIDEO_HORIZONTAL_PIXELS
            && (y >> 8) < VIDEO_VERTICAL_PIXELS
        {
            let px = ((((x >> 8) as usize)
                + ((y >> 8) as usize) * VIDEO_HORIZONTAL_PIXELS as usize)
                << 1) as usize;
            color = color_from_555(u16::from_le_bytes([
                self.video.vram[px],
                self.video.vram[px + 1],
            ]));
        }

        let mut out_x = sw_start;
        while out_x < sw_end {
            x = x.wrapping_add(background.dx as i32);
            y = y.wrapping_add(background.dy as i32);

            if !(x >= 0
                && y >= 0
                && (x >> 8) < VIDEO_HORIZONTAL_PIXELS
                && (y >> 8) < VIDEO_VERTICAL_PIXELS)
                && mosaic_wait == 0
            {
                out_x += 1;
                continue;
            }
            if mosaic_wait == 0 {
                let px = ((((x >> 8) as usize)
                    + ((y >> 8) as usize) * VIDEO_HORIZONTAL_PIXELS as usize)
                    << 1) as usize;
                color = color_from_555(u16::from_le_bytes([
                    self.video.vram[px],
                    self.video.vram[px + 1],
                ]));
                mosaic_wait = mosaic_h;
            } else {
                mosaic_wait -= 1;
            }

            let current = self.video.sw.row[out_x as usize];
            println!(
                "mode3 x={} current={:08x} color={:08x}",
                out_x, current, color
            );
            if !slow_path || (!(current & FLAG_OBJWIN != 0)) != background.objwin_only {
                let mut merged_flags = flags;
                if current & FLAG_OBJWIN != 0 {
                    merged_flags = objwin_flags;
                }
                let pix_color = if !variant {
                    color
                } else if self.video.sw.blend_effect == BLEND_BRIGHTEN {
                    brighten(color, self.video.sw.bldy)
                } else if self.video.sw.blend_effect == BLEND_DARKEN {
                    darken(color, self.video.sw.bldy)
                } else {
                    color
                };
                self.composite_objwin(out_x as usize, pix_color | merged_flags, current);
            }
            out_x += 1;
        }
    }

    fn draw_background_mode4(&mut self, in_y: i32) {
        let background = self.video.sw.bg[2];
        let sw_start = self.video.sw.start;
        let sw_end = self.video.sw.end;
        let mut x = background.sx + (sw_start - 1) * background.dx as i32;
        let mut y = background.sy + (sw_start - 1) * background.dy as i32;
        let mut mosaic_wait = 0i32;
        let mut mosaic_h = 1i32;
        if background.mosaic {
            let mosaic_v = mosaic_bg_v(self.video.sw.mosaic) + 1;
            mosaic_h = mosaic_bg_h(self.video.sw.mosaic) + 1;
            mosaic_wait = (mosaic_h - sw_start + VIDEO_HORIZONTAL_PIXELS * mosaic_h) % mosaic_h;
            let start_x = sw_start - (sw_start % mosaic_h);
            x = x.wrapping_add(-(in_y % mosaic_v) * background.dmx as i32);
            y = y.wrapping_add(-(in_y % mosaic_v) * background.dmy as i32);
            x = x.wrapping_add(background.sx + start_x * background.dx as i32 - x);
            y = y.wrapping_add(background.sy + start_x * background.dy as i32 - y);
        }
        let slow_path = dispcnt_objwin_enable(self.video.sw.dispcnt);
        let flags = background.flags;
        let objwin_flags = background.objwin_flags;
        let variant = background.variant;
        let highlight = self.video.sw.highlight_amount != 0 && background.highlight;

        let offset = if dispcnt_frame_select(self.video.sw.dispcnt) {
            0xA000usize
        } else {
            0
        };

        let mut color_byte = 0u32;
        let mut out_x = sw_start;
        while out_x < sw_end {
            x = x.wrapping_add(background.dx as i32);
            y = y.wrapping_add(background.dy as i32);

            if !(x >= 0
                && y >= 0
                && (x >> 8) < VIDEO_HORIZONTAL_PIXELS
                && (y >> 8) < VIDEO_VERTICAL_PIXELS)
                && mosaic_wait == 0
            {
                out_x += 1;
                continue;
            }
            if mosaic_wait == 0 {
                color_byte = self.video.vram[offset
                    + ((x >> 8) as usize)
                    + ((y >> 8) as usize) * VIDEO_HORIZONTAL_PIXELS as usize]
                    as u32;
                mosaic_wait = mosaic_h;
            } else {
                mosaic_wait -= 1;
            }

            let current = self.video.sw.row[out_x as usize];
            if color_byte != 0 && is_writable(current) {
                if !slow_path {
                    let c = self.bg_palette256(color_byte, variant, highlight);
                    self.composite_noobjwin(out_x as usize, c | flags, current);
                } else if background.objwin_force_enable != 0
                    || (!(current & FLAG_OBJWIN != 0)) == background.objwin_only
                {
                    let mut merged_flags = flags;
                    if current & FLAG_OBJWIN != 0 {
                        merged_flags = objwin_flags;
                    }
                    let palette = if highlight {
                        self.video.sw.highlight_palette[color_byte as usize]
                    } else {
                        self.video.sw.normal_palette[color_byte as usize]
                    };
                    self.composite_objwin(out_x as usize, palette | merged_flags, current);
                }
            }
            out_x += 1;
        }
    }

    fn draw_background_mode5(&mut self, in_y: i32) {
        let background = self.video.sw.bg[2];
        let sw_start = self.video.sw.start;
        let sw_end = self.video.sw.end;
        let mut x = background.sx + (sw_start - 1) * background.dx as i32;
        let mut y = background.sy + (sw_start - 1) * background.dy as i32;
        let mut mosaic_wait = 0i32;
        let mut mosaic_h = 1i32;
        if background.mosaic {
            let mosaic_v = mosaic_bg_v(self.video.sw.mosaic) + 1;
            mosaic_h = mosaic_bg_h(self.video.sw.mosaic) + 1;
            mosaic_wait = (mosaic_h - sw_start + VIDEO_HORIZONTAL_PIXELS * mosaic_h) % mosaic_h;
            let start_x = sw_start - (sw_start % mosaic_h);
            x = x.wrapping_add(-(in_y % mosaic_v) * background.dmx as i32);
            y = y.wrapping_add(-(in_y % mosaic_v) * background.dmy as i32);
            x = x.wrapping_add(background.sx + start_x * background.dx as i32 - x);
            y = y.wrapping_add(background.sy + start_x * background.dy as i32 - y);
        }
        let slow_path = dispcnt_objwin_enable(self.video.sw.dispcnt);
        let flags = background.flags;
        let objwin_flags = background.objwin_flags;
        let variant = background.variant;

        let offset = if dispcnt_frame_select(self.video.sw.dispcnt) {
            0xA000usize
        } else {
            0
        };

        let mut color = self.video.sw.normal_palette[0];
        if mosaic_wait != 0 && x >= 0 && y >= 0 && (x >> 8) < 160 && (y >> 8) < 128 {
            let px = offset + (((x >> 8) as usize) * 2) + ((y >> 8) as usize) * 320;
            color = color_from_555(u16::from_le_bytes([
                self.video.vram[px],
                self.video.vram[px + 1],
            ]));
        }

        let mut out_x = sw_start;
        while out_x < sw_end {
            x = x.wrapping_add(background.dx as i32);
            y = y.wrapping_add(background.dy as i32);

            if !(x >= 0 && y >= 0 && (x >> 8) < 160 && (y >> 8) < 128) && mosaic_wait == 0 {
                out_x += 1;
                continue;
            }
            if mosaic_wait == 0 {
                let px = offset + (((x >> 8) as usize) * 2) + ((y >> 8) as usize) * 320;
                color = color_from_555(u16::from_le_bytes([
                    self.video.vram[px],
                    self.video.vram[px + 1],
                ]));
                mosaic_wait = mosaic_h;
            } else {
                mosaic_wait -= 1;
            }

            let current = self.video.sw.row[out_x as usize];
            if !slow_path || (!(current & FLAG_OBJWIN != 0)) != background.objwin_only {
                let mut merged_flags = flags;
                if current & FLAG_OBJWIN != 0 {
                    merged_flags = objwin_flags;
                }
                let pix_color = if !variant {
                    color
                } else if self.video.sw.blend_effect == BLEND_BRIGHTEN {
                    brighten(color, self.video.sw.bldy)
                } else if self.video.sw.blend_effect == BLEND_DARKEN {
                    darken(color, self.video.sw.bldy)
                } else {
                    color
                };
                self.composite_objwin(out_x as usize, pix_color | merged_flags, current);
            }
            out_x += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// software-obj.c: sprite preprocess/postprocess

impl Gba {
    /// GBAVideoSoftwareRendererPreprocessSprite — runs per line per window.
    fn preprocess_sprite(&mut self, sprite: GbaObj, index: i32, y: i32) -> i32 {
        let width = OBJ_SIZES_TABLE[(obj_a_shape(sprite.a) * 4 + obj_b_size(sprite.b)) as usize][0];
        let height =
            OBJ_SIZES_TABLE[(obj_a_shape(sprite.a) * 4 + obj_b_size(sprite.b)) as usize][1];
        let start = self.video.sw.start;
        let end = self.video.sw.end;

        let mut flags = (obj_c_priority(sprite.c) as u32) << OFFSET_PRIORITY;
        let current_window_blend = self.video.sw.current_window.packed & WIN_BLEND_ENABLE != 0;
        if (current_window_blend
            && self.video.sw.target1_obj != 0
            && self.video.sw.blend_effect == BLEND_ALPHA)
            || obj_a_mode(sprite.a) == OBJ_MODE_SEMITRANSPARENT
        {
            flags |= FLAG_TARGET_1;
        }
        if obj_a_mode(sprite.a) == OBJ_MODE_OBJWIN {
            flags |= FLAG_OBJWIN;
        }
        if flags & FLAG_OBJWIN != 0
            && (self.video.sw.current_window.priority < self.video.sw.objwin.priority
                || self.video.sw.disable_objwin)
        {
            return 0;
        }

        let obj_character_mapping = dispcnt_obj_char_mapping(self.video.sw.dispcnt);
        let align = obj_a_256color(sprite.a) && !obj_character_mapping;
        let char_base = ((obj_c_tile(sprite.c) & !(align as i32)) as u32) * 0x20;
        let mask_lo = if obj_character_mapping { 0x7FFE } else { 0x3FE } as u32;
        let mask_hi = if obj_character_mapping {
            0
        } else {
            char_base & 0x7C00
        };
        if dispcnt_mode(self.video.sw.dispcnt) >= 3 && obj_c_tile(sprite.c) < 512 {
            return 0;
        }

        let mut variant = self.video.sw.target1_obj != 0
            && current_window_blend
            && (self.video.sw.blend_effect == BLEND_BRIGHTEN
                || self.video.sw.blend_effect == BLEND_DARKEN);
        let objwin_slow_path = dispcnt_objwin_enable(self.video.sw.dispcnt)
            && ((self.video.sw.objwin.packed & WIN_BLEND_ENABLE != 0)
                != (self.video.sw.current_window.packed & WIN_BLEND_ENABLE != 0));

        if obj_a_mode(sprite.a) == OBJ_MODE_SEMITRANSPARENT
            || (self.video.sw.target1_obj != 0 && self.video.sw.blend_effect == BLEND_ALPHA)
            || objwin_slow_path
        {
            let mut target2 = self.video.sw.target2_bd as i32;
            for bg in 0..4 {
                target2 |=
                    (self.video.sw.bg[bg].target2 != 0 && self.video.sw.bg[bg].enabled != 0) as i32;
            }
            if target2 != 0 {
                self.video.sw.force_target1 = true;
                flags |= FLAG_REBLEND;
                variant = false;
            } else {
                flags &= !FLAG_TARGET_1;
            }
        }

        let obj_x = obj_b_x(sprite.b) + self.video.sw.obj_offset_x as i32;
        let in_y_base = y - (obj_a_y(sprite.a) + self.video.sw.obj_offset_y as i32);
        let in_y_base = in_y_base;
        let is_256 = obj_a_256color(sprite.a);
        let is_mosaic = obj_a_mosaic(sprite.a);
        let mosaic_h = if is_mosaic {
            mosaic_obj_h(self.video.sw.mosaic) + 1
        } else {
            1
        };

        let palette_offset = obj_c_palette(sprite.c) as usize * 16;

        let mat_pair = if obj_a_transformed(sprite.a) {
            let mi = obj_b_mat_index(sprite.b) as usize;
            let oam = &self.video.oam;
            let pos = mi * 16 + 6; // matrix A at +6 within each 16-byte matrix record? Actually matrix struct: {padding0[3], a, padding1[3], b, padding2[3], c, padding3[3], d} = each 16 bytes stride 16: mat.a is offset 6 bytes; Wait: GBAOAMMatrix = { int16_t padding0[3]; int16_t a; int16_t padding1[3]; int16_t b; int16_t padding2[3]; int16_t c; int16_t padding3[3]; int16_t d; }
            let d_ot = pos;
            let _ = d_ot;
            // resolve: mat.a at offset 6
            let _ = oam;
            0
        } else {
            0
        };
        let _ = mat_pair;

        // Pass to the kernel that mirrors SPRITE_*_LOOP semantics.
        self.draw_sprite_kernel(
            sprite,
            width,
            height,
            start,
            end,
            obj_x,
            in_y_base,
            flags,
            is_256,
            objwin_slow_path,
            variant,
            char_base,
            mask_lo,
            mask_hi,
            mosaic_h,
            palette_offset,
            index,
            y,
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn draw_sprite_kernel(
        &mut self,
        sprite: GbaObj,
        width: i32,
        height: i32,
        start: i32,
        end: i32,
        x: i32,
        in_y_base: i32,
        flags: u32,
        is_256: bool,
        objwin_slow_path: bool,
        variant: bool,
        char_base: u32,
        mask_lo: u32,
        mask_hi: u32,
        mosaic_h: i32,
        palette_offset: usize,
        index: i32,
        _y: i32,
    ) -> i32 {
        let _ = variant;
        let _ = index;

        let obj_transformed = obj_a_transformed(sprite.a);
        let obj_double = obj_a_double_size(sprite.a);
        let obj_mode = obj_a_mode(sprite.a);
        let obj_mosaic = obj_a_mosaic(sprite.a);
        let h_flip = obj_b_hflip(sprite.b);
        let v_flip = obj_b_vflip(sprite.b);

        let obj_enabled_this_window = self.video.sw.current_window.packed & WIN_OBJ_ENABLE != 0;

        // Palette bases (the C bakes the variant selection in via `palette`):
        // spriteLayer stores palette|flags; the palette pointer picks the set.
        let highlight_on =
            self.video.sw.highlight_amount != 0 && self.video.sw.highlight_obj[index as usize];
        // Choose a palette handle: 0 = normal, 1 = variant, 2 = highlight, 3 = highlight-variant.
        // The objwin "palette" tracks wins against the blended-target state.
        let objwin_blend_sel = variant && self.video.sw.objwin.packed & WIN_BLEND_ENABLE != 0;
        let palette_kind: u8 = if variant && highlight_on {
            3
        } else if variant {
            1
        } else if highlight_on {
            2
        } else {
            0
        };
        let objwin_palette_kind: u8 = if objwin_blend_sel && variant {
            palette_kind
        } else if variant {
            0 // normal
        } else {
            palette_kind
        };
        let _ = objwin_palette_kind;

        if obj_transformed {
            let total_width = width << obj_double as i32;
            let total_height = height << obj_double as i32;
            let mat_idx = obj_b_mat_index(sprite.b) as usize;
            let oam = &self.video.oam;
            // GBAOAMMatrix: each entry = 16 bytes at (i*0x10): {pad[3], a, pad[3], b, pad[3], c, pad[3], d}
            let mi = mat_idx * 16;
            let mat_a = u16::from_le_bytes([oam[mi + 6], oam[mi + 7]]) as i16 as i32;
            let mat_b = u16::from_le_bytes([oam[mi + 22], oam[mi + 23]]) as i16 as i32;
            let mat_c = u16::from_le_bytes([oam[mi + 38], oam[mi + 39]]) as i16 as i32;
            let mat_d = u16::from_le_bytes([oam[mi + 46], oam[mi + 47]]) as i16 as i32;

            let mut in_y = in_y_base;
            if in_y < 0 {
                in_y += 256;
            }

            let mut out_x = if x >= start { x } else { start };
            let mut condition = x + total_width;
            let mut in_x = out_x - x;
            if end < condition {
                condition = end;
            }
            let mut mosaic_h_t = 1;
            if obj_mosaic {
                mosaic_h_t = mosaic_h;
                if condition != end && condition % mosaic_h_t != 0 {
                    condition += mosaic_h_t - (condition % mosaic_h_t);
                }
            }

            let mut x_accum = mat_a * (in_x - 1 - (total_width >> 1))
                + mat_b * (in_y - (total_height >> 1))
                + (width << 7);
            let mut y_accum = mat_c * (in_x - 1 - (total_width >> 1))
                + mat_d * (in_y - (total_height >> 1))
                + (height << 7);

            // Clip off early pixels
            // TODO: Transform end coordinates too
            if mat_a != 0 {
                if (x_accum >> 8) < 0 {
                    let diff_x = -x_accum - 1;
                    let xx = if mat_a != 0 { diff_x / mat_a } else { 0 };
                    x_accum += mat_a * xx;
                    y_accum += mat_c * xx;
                    out_x += xx;
                    in_x += xx;
                } else if (x_accum >> 8) >= width {
                    let diff_x = (width << 8) - x_accum;
                    let xx = if mat_a != 0 { diff_x / mat_a } else { 0 };
                    x_accum += mat_a * xx;
                    y_accum += mat_c * xx;
                    out_x += xx;
                    in_x += xx;
                }
            }
            if mat_c != 0 {
                if (y_accum >> 8) < 0 {
                    let diff_y = -y_accum - 1;
                    let yy = if mat_c != 0 { diff_y / mat_c } else { 0 };
                    x_accum += mat_a * yy;
                    y_accum += mat_c * yy;
                    out_x += yy;
                    in_x += yy;
                } else if (y_accum >> 8) >= height {
                    let diff_y = (height << 8) - y_accum;
                    let yy = if mat_c != 0 { diff_y / mat_c } else { 0 };
                    x_accum += mat_a * yy;
                    y_accum += mat_c * yy;
                    out_x += yy;
                    in_x += yy;
                }
            }

            if out_x < start || out_x >= condition {
                return 0;
            }

            let obj_mosaic_a = obj_mosaic && mosaic_h_t > 1;
            while out_x < condition {
                let before_x = x_accum >> 8;
                let before_y = y_accum >> 8;
                if obj_mosaic_a && (out_x % mosaic_h_t) != 0 {
                    // keep previous accumulations
                } else {
                    x_accum = mat_a * (in_x - 1 - (total_width >> 1))
                        + mat_b * (in_y - (total_height >> 1))
                        + (width << 7);
                    y_accum = mat_c * (in_x - 1 - (total_width >> 1))
                        + mat_d * (in_y - (total_height >> 1))
                        + (height << 7);
                }
                let local_x = x_accum >> 8;
                let local_y = y_accum >> 8;
                if local_x < 0 || local_y < 0 || local_x >= width || local_y >= height {
                    break;
                }
                self.sprite_draw_pixel(
                    sprite,
                    local_x,
                    local_y,
                    flags,
                    char_base,
                    mask_lo,
                    mask_hi,
                    palette_offset,
                    out_x as usize,
                    palette_kind,
                    objwin_palette_kind,
                    end,
                );
                if obj_mosaic_a {
                    // don't advance the accumulators per-pixel; the loop in C
                    // increments inX with the MOSAIC step on transformed objs
                }
                in_x += 1;
                out_x += 1;
            }
        } else {
            let mut out_x = if x >= start { x } else { start };
            let mut condition = x + width;
            if obj_mosaic && condition % mosaic_h != 0 {
                condition += mosaic_h - (condition % mosaic_h);
            }
            if end < condition {
                condition = end;
            }
            let mut in_x = out_x - x;
            let mut x_offset = 1i32;
            if h_flip {
                in_x = width - in_x - 1;
                x_offset = -1;
            }
            let mut in_y = in_y_base;
            if obj_a_y(sprite.a) + height - 256 >= 0 {
                in_y += 256;
            }
            if v_flip {
                in_y = height - in_y - 1;
            }
            while out_x < condition {
                let mut local_x = in_x;
                if mosaic_h > 1 {
                    local_x = in_x - x_offset * ((out_x) % mosaic_h);
                    if local_x < 0 {
                        local_x = 0;
                    } else if local_x > width - 1 {
                        local_x = width - 1;
                    }
                }
                self.sprite_draw_pixel(
                    sprite,
                    local_x,
                    in_y,
                    flags,
                    char_base,
                    mask_lo,
                    mask_hi,
                    palette_offset,
                    out_x as usize,
                    palette_kind,
                    objwin_palette_kind,
                    end,
                );
                in_x += x_offset;
                out_x += 1;
            }
        }
        1
    }

    /// GBAVideoSoftwareRendererPostprocessSprite
    fn postprocess_sprite(&mut self, priority: u32) {
        let sw = &self.video.sw;
        let flags = FLAG_TARGET_2 * sw.target2_obj;
        let flags = FLAG_TARGET_2 * sw.target2_obj;

        let objwin_slow_path = dispcnt_objwin_enable(sw.dispcnt);
        let mut objwin_disable = false;
        let mut objwin_only = false;
        if objwin_slow_path {
            objwin_disable = sw.objwin.packed & WIN_OBJ_ENABLE == 0;
            objwin_only = !objwin_disable && sw.current_window.packed & WIN_OBJ_ENABLE == 0;
            if objwin_disable && sw.current_window.packed & WIN_OBJ_ENABLE == 0 {
                return;
            }
            if objwin_disable {
                for x in sw.start as usize..sw.end as usize {
                    let color = self.video.sw.sprite_layer[x] & !FLAG_OBJWIN;
                    let current = self.video.sw.row[x];
                    if (color & FLAG_UNWRITTEN) != FLAG_UNWRITTEN
                        && current & FLAG_OBJWIN == 0
                        && (color & FLAG_PRIORITY) >> OFFSET_PRIORITY == priority
                    {
                        self.composite_sprite_pixel(x, color | flags, current);
                    }
                }
                return;
            } else if objwin_only {
                for x in sw.start as usize..sw.end as usize {
                    let color = self.video.sw.sprite_layer[x] & !FLAG_OBJWIN;
                    let current = self.video.sw.row[x];
                    if (color & FLAG_UNWRITTEN) != FLAG_UNWRITTEN
                        && current & FLAG_OBJWIN != 0
                        && (color & FLAG_PRIORITY) >> OFFSET_PRIORITY == priority
                    {
                        self.composite_sprite_pixel(x, color | flags, current);
                    }
                }
                return;
            } else {
                for x in sw.start as usize..sw.end as usize {
                    let color = self.video.sw.sprite_layer[x] & !FLAG_OBJWIN;
                    let current = self.video.sw.row[x];
                    if (color & FLAG_UNWRITTEN) != FLAG_UNWRITTEN
                        && (color & FLAG_PRIORITY) >> OFFSET_PRIORITY == priority
                    {
                        self.composite_sprite_pixel(x, color | flags, current);
                    }
                }
                return;
            }
        } else if sw.current_window.packed & WIN_OBJ_ENABLE == 0 {
            return;
        }
        let start = self.video.sw.start;
        let end = self.video.sw.end;
        let mut x = start;
        while x < end {
            let color = self.video.sw.sprite_layer[x as usize] & !FLAG_OBJWIN;
            let current = self.video.sw.row[x as usize];
            if (color & FLAG_UNWRITTEN) != FLAG_UNWRITTEN
                && (color & FLAG_PRIORITY) >> OFFSET_PRIORITY == priority
            {
                self.composite_sprite_pixel(x as usize, color | flags, current);
            }
            x += 1;
        }
    }

    fn composite_sprite_pixel(&mut self, idx: usize, color_in: u32, current: u32) {
        let sw = &mut self.video.sw;
        let mut color = color_in;
        if color >= current {
            if current & FLAG_TARGET_1 != 0 && color & FLAG_TARGET_2 != 0 {
                color = color_mix_5bit(sw.blda as i32, current, sw.bldb as i32, color);
            } else {
                color = current & (0x00FFFFFF | FLAG_REBLEND | FLAG_OBJWIN);
            }
        } else {
            color = (color & !FLAG_TARGET_2) | (current & FLAG_OBJWIN);
        }
        sw.row[idx] = color;
    }

    fn composite_blend_slow(&mut self, idx: usize, color_in: u32, current: u32) {
        self.composite_sprite_pixel(idx, color_in, current);
    }
}

impl SwVideo {}

impl Gba {
    #[inline]
    fn composite_objwin(&mut self, idx: usize, color_in: u32, current: u32) {
        let sw = &mut self.video.sw;
        let mut color = color_in;
        if color >= current {
            if current & FLAG_TARGET_1 != 0 && color & FLAG_TARGET_2 != 0 {
                color = color_mix_5bit(sw.blda as i32, current, sw.bldb as i32, color);
            } else {
                color = current & (0x00FFFFFF | FLAG_REBLEND | FLAG_OBJWIN);
            }
        } else {
            color = (color & !FLAG_TARGET_2) | (current & FLAG_OBJWIN);
        }
        sw.row[idx] = color;
    }

    #[inline]
    fn composite_noobjwin(&mut self, idx: usize, color_in: u32, current: u32) {
        let sw = &mut self.video.sw;
        let mut color = color_in;
        if color >= current {
            if current & FLAG_TARGET_1 != 0 && color & FLAG_TARGET_2 != 0 {
                color = color_mix_5bit(sw.blda as i32, current, sw.bldb as i32, color);
            } else {
                color = current & (0x00FFFFFF | FLAG_REBLEND | FLAG_OBJWIN);
            }
        } else {
            color = color & !FLAG_TARGET_2;
        }
        sw.row[idx] = color;
    }

    fn bg_mode2_composite(
        &mut self,
        _bg: usize,
        pixel_data: u32,
        flags: u32,
        objwin_flags: u32,
        _variant: bool,
        slow_path: bool,
        idx: usize,
        current: u32,
    ) {
        if !slow_path {
            let color = self.video.sw.normal_palette[pixel_data as usize];
            self.composite_noobjwin(idx, color | flags, current);
        } else {
            let bg = &self.video.sw.bg[_bg];
            let force = bg.objwin_force_enable;
            let only = bg.objwin_only;
            if force != 0 || (!(current & FLAG_OBJWIN != 0)) == only {
                let mut merged_flags = flags;
                if current & FLAG_OBJWIN != 0 {
                    merged_flags = objwin_flags;
                }
                let color = self.video.sw.normal_palette[pixel_data as usize];
                self.composite_objwin(idx, color | merged_flags, current);
            }
        }
    }

    // SPRITE_DRAW_PIXEL_* kernels
    #[allow(clippy::too_many_arguments)]
    fn sprite_draw_pixel(
        &mut self,
        sprite: GbaObj,
        in_x: i32,
        in_y: i32,
        flags: u32,
        char_base: u32,
        mask_lo: u32,
        mask_hi: u32,
        palette_offset: usize,
        out_x: usize,
        palette_kind: u8,
        objwin_palette_kind: u8,
        _end: i32,
    ) {
        let _ = objwin_palette_kind;
        let _ = mask_hi;
        let is_256 = obj_a_256color(sprite.a);
        let obj_mapping = dispcnt_obj_char_mapping(self.video.sw.dispcnt);
        let width_px =
            OBJ_SIZES_TABLE[(obj_a_shape(sprite.a) * 4 + obj_b_size(sprite.b)) as usize][0];
        let stride = if obj_mapping {
            if is_256 {
                width_px as usize
            } else {
                (width_px >> 1) as usize
            }
        } else {
            0x80
        };
        // vram base tile
        let vram_base = 0x10000usize;
        let tile_data: u32;
        if !is_256 {
            let x_base = (in_x & !0x7) as usize * 4 + ((in_x >> 1) & 2) as usize;
            let y_base =
                (in_y & !0x7) as usize * stride + (in_y & 7) as usize * 4 + mask_hi as usize;
            let off = y_base + ((x_base + char_base as usize) & mask_lo as usize);
            let off = off & 0x7FFE;
            let td = u16::from_le_bytes([
                self.video.vram[vram_base + off],
                self.video.vram[vram_base + off + 1],
            ]);
            tile_data = ((td as u32) >> ((in_x & 3) << 2)) & 0xF;
        } else {
            let x_base = (in_x & !0x7) as usize * 8 + (in_x & 6) as usize;
            let y_base =
                (in_y & !0x7) as usize * stride + (in_y & 7) as usize * 8 + mask_hi as usize;
            let off = y_base + ((x_base + char_base as usize) & mask_lo as usize);
            let off = off & 0x7FFE;
            let td = u16::from_le_bytes([
                self.video.vram[vram_base + off],
                self.video.vram[vram_base + off + 1],
            ]);
            tile_data = ((td as u32) >> ((in_x & 1) << 3)) & 0xFF;
        }

        let current = self.video.sw.sprite_layer[out_x];
        if (current & FLAG_ORDER_MASK) > flags {
            if tile_data != 0 {
                let palette = match palette_kind {
                    1 => self.video.sw.variant_palette[0x100 + palette_offset + tile_data as usize],
                    2 => {
                        self.video.sw.highlight_palette[0x100 + palette_offset + tile_data as usize]
                    }
                    3 => {
                        self.video.sw.highlight_variant_palette
                            [0x100 + palette_offset + tile_data as usize]
                    }
                    _ => self.video.sw.normal_palette[0x100 + palette_offset + tile_data as usize],
                };
                self.video.sw.sprite_layer[out_x] = palette | flags;
            } else if current != FLAG_UNWRITTEN {
                self.video.sw.sprite_layer[out_x] = (current
                    & !(FLAG_ORDER_MASK | FLAG_REBLEND | FLAG_TARGET_1))
                    | (flags & (FLAG_ORDER_MASK | FLAG_REBLEND | FLAG_TARGET_1));
            }
        }
    }

    fn pick_palette_mode2(&mut self, _highlight: bool, _variant: bool) -> u32 {
        self.video.sw.normal_palette.len() as u32 - 512
    }
}

impl Gba {
    /// Palette read dispatch honoring variant/highlight (256-color index path).
    fn bg_palette256(&self, idx: u32, variant: bool, highlight: bool) -> u32 {
        let i = idx as usize & 0x1FF;
        if variant && highlight {
            self.video.sw.highlight_variant_palette[i]
        } else if variant {
            self.video.sw.variant_palette[i]
        } else if highlight {
            self.video.sw.highlight_palette[i]
        } else {
            self.video.sw.normal_palette[i]
        }
    }
}
