// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gb/video.c, mgba/src/gb/renderers/software.c,
// mgba/src/gb/renderers/cache-set.c and their headers.
//
// NOTE: the C's mCacheSet/mTileCache layer (core/cache-set.c) is a pure
// caching indirection with no cycle or pixel effect; this port renders
// directly from VRAM in draw_range and omits it. Behavior is identical.

use rgba_core::timing::Timing;
use rgba_core::{mlog, Level};

use crate::gb::{EventId, Gb, GbModel};
use crate::io::*;

pub const GB_VIDEO_HORIZONTAL_PIXELS: i32 = 160;
pub const GB_VIDEO_VERTICAL_PIXELS: i32 = 144;
pub const GB_VIDEO_VBLANK_PIXELS: i32 = 10;
pub const GB_VIDEO_VERTICAL_TOTAL_PIXELS: i32 = 154;
pub const SGB_VIDEO_HORIZONTAL_PIXELS: i32 = 256;
pub const SGB_VIDEO_VERTICAL_PIXELS: i32 = 224;
pub const GB_VIDEO_MODE_2_LENGTH: i32 = 80;
pub const GB_VIDEO_MODE_3_LENGTH_BASE: i32 = 172;
pub const GB_VIDEO_MODE_0_LENGTH_BASE: i32 = 204;
pub const GB_VIDEO_HORIZONTAL_LENGTH: i32 = 456;
pub const GB_VIDEO_TOTAL_LENGTH: i32 = 70224;
pub const GB_VIDEO_MAX_OBJ: usize = 40;
pub const GB_VIDEO_MAX_LINE_OBJ: usize = 10;
pub const GB_SIZE_VRAM: usize = 0x4000;
pub const GB_SIZE_VRAM_BANK0: usize = 0x2000;
pub const GB_SIZE_OAM: usize = 0xA0;
pub const GB_BASE_MAP: usize = 0x1800;
pub const GB_SIZE_MAP: usize = 0x400;
pub const SGB_SIZE_CHAR_RAM: usize = 0x2000;
pub const SGB_SIZE_MAP_RAM: usize = 0x1000;
pub const SGB_SIZE_PAL_RAM: usize = 0x1000;
pub const SGB_SIZE_ATF_RAM: usize = 0x1000;

// GBObjAttributes bits
pub const OBJ_ATTR_CGB_PALETTE_MASK: u8 = 0x7;
pub const OBJ_ATTR_BANK: u8 = 0x8;
pub const OBJ_ATTR_PALETTE: u8 = 0x10;
pub const OBJ_ATTR_XFLIP: u8 = 0x20;
pub const OBJ_ATTR_YFLIP: u8 = 0x40;
pub const OBJ_ATTR_PRIORITY: u8 = 0x80;

// SGBBgAttributes bits (in the u16 map entry)
pub const SGB_BG_ATTR_TILE_MASK: u16 = 0x3FF;
pub const SGB_BG_ATTR_PALETTE_MASK: u16 = 0x3800;
pub const SGB_BG_ATTR_PALETTE_SHIFT: u16 = 10;
pub const SGB_BG_ATTR_PRIORITY: u16 = 0x2000;
pub const SGB_BG_ATTR_XFLIP: u16 = 0x4000;
pub const SGB_BG_ATTR_YFLIP: u16 = 0x8000;

// Renderer palette layout
const PAL_BG: u32 = 0;
const PAL_OBJ: u32 = 0x20;
const PAL_HIGHLIGHT: u32 = 0x80;
const PAL_HIGHLIGHT_BG: u32 = PAL_HIGHLIGHT | PAL_BG;
const PAL_HIGHLIGHT_OBJ: u32 = PAL_HIGHLIGHT | PAL_OBJ;
const PAL_SGB_BORDER: u32 = 0x40;
const OBJ_PRIORITY: u32 = 0x100;
const OBJ_PRIO_MASK: u32 = 0x0FF;

// Renderer lookup mirror size
const RENDERER_PALETTE_LEN: usize = 192;

// mColor on 32-bit mGBA is BGR in a u32 (0x00BBGGRR). Our output buffer is
// 0xFFRRGGBB `u32`; palette stores are converted at write time, but all the
// mixing happens in mColor space first.
#[inline]
fn m_color_from_555(value: u16) -> u32 {
    // M_RGB5_TO_BGR8: (M_R5 << 3) | (M_G5 << 11) | (M_B5 << 19)
    let color = (((value as u32) & 0x1F) << 3)
        | ((((value as u32) >> 5) & 0x1F) << 11)
        | ((((value as u32) >> 10) & 0x1F) << 19);
    color | ((color >> 5) & 0x070707)
}

#[inline]
fn m_color_to_rgba32(color: u32) -> u32 {
    // mColor is 0x00BBGGRR; target is 0xFFRRGGBB
    let r = color & 0xFF;
    let g = (color >> 8) & 0xFF;
    let b = (color >> 16) & 0xFF;
    0xFF00_0000 | (r << 16) | (g << 8) | b
}

#[inline]
fn m_color_mix_5bit(weight_a: i32, color_a: u32, weight_b: i32, color_b: u32) -> u32 {
    // 32-bit branch of mColorMix5Bit
    let mut c: u32 = 0;
    let a = color_a & 0xFF;
    let b = color_b & 0xFF;
    c |= ((a * weight_a as u32 + b * weight_b as u32) / 16) & 0x1FF;
    if c & 0x00000100 != 0 {
        c = 0x000000FF;
    }
    let a = color_a & 0xFF00;
    let b = color_b & 0xFF00;
    c |= ((a * weight_a as u32 + b * weight_b as u32) / 16) & 0x1FF00;
    if c & 0x00010000 != 0 {
        c = (c & 0x000000FF) | 0x0000FF00;
    }
    let a = color_a & 0xFF0000;
    let b = color_b & 0xFF0000;
    c |= ((a * weight_a as u32 + b * weight_b as u32) / 16) & 0x1FF0000;
    if c & 0x01000000 != 0 {
        c = (c & 0x0000FFFF) | 0x00FF0000;
    }
    c
}

// STAT packed accessors (GBRegisterSTAT)
#[inline]
pub fn stat_mode(stat: u8) -> u8 {
    stat & 0x3
}
#[inline]
pub fn stat_set_mode(stat: u8, mode: i32) -> u8 {
    (stat & !0x3) | (mode as u8 & 0x3)
}
#[inline]
pub fn stat_lyc(stat: u8) -> bool {
    stat & 0x4 != 0
}
#[inline]
pub fn stat_set_lyc(stat: u8, v: bool) -> u8 {
    (stat & !0x4) | (v as u8) << 2
}
#[inline]
pub fn stat_clear_lyc(stat: u8) -> u8 {
    stat & !0x4
}
#[inline]
pub fn stat_hblank_irq(stat: u8) -> bool {
    stat & 0x8 != 0
}
#[inline]
pub fn stat_vblank_irq(stat: u8) -> bool {
    stat & 0x10 != 0
}
#[inline]
pub fn stat_oam_irq(stat: u8) -> bool {
    stat & 0x20 != 0
}
#[inline]
pub fn stat_lyc_irq(stat: u8) -> bool {
    stat & 0x40 != 0
}

// LCDC packed accessors (GBRegisterLCDC)
#[inline]
pub fn lcdc_bg_enable(v: u8) -> bool {
    v & 0x01 != 0
}
#[inline]
pub fn lcdc_obj_enable(v: u8) -> bool {
    v & 0x02 != 0
}
#[inline]
pub fn lcdc_obj_size(v: u8) -> bool {
    v & 0x04 != 0
}
#[inline]
pub fn lcdc_tile_map(v: u8) -> bool {
    v & 0x08 != 0
}
#[inline]
pub fn lcdc_tile_data(v: u8) -> bool {
    v & 0x10 != 0
}
#[inline]
pub fn lcdc_window(v: u8) -> bool {
    v & 0x20 != 0
}
#[inline]
pub fn lcdc_window_tile_map(v: u8) -> bool {
    v & 0x40 != 0
}
#[inline]
pub fn lcdc_enable(v: u8) -> bool {
    v & 0x80 != 0
}

/// The mode the modeEvent will complete next (replaces the C's switched
/// function pointer).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ModeEventKind {
    EndMode0,
    EndMode1,
    EndMode2,
    EndMode3,
}

#[derive(Clone, Copy, Default)]
pub struct GbObj {
    pub y: u8,
    pub x: u8,
    pub tile: u8,
    pub attr: u8,
}

#[derive(Clone, Copy, Default)]
pub struct RendererSprite {
    pub obj: GbObj,
    pub index: i8,
}

pub struct Video {
    // GBVideo
    pub x: i32,
    pub ly: i32,
    pub stat: u8,
    pub mode: i32,
    pub dot_clock: i32,
    pub vram: Vec<u8>,
    pub vram_current_bank: i32,
    pub oam: [u8; GB_SIZE_OAM],
    pub obj_max: i32,
    pub bcp_index: i32,
    pub bcp_increment: bool,
    pub ocp_index: i32,
    pub ocp_increment: bool,
    pub sgb_command_header: u8,
    pub sgb_buffer_index: i32,
    pub sgb_packet_buffer: [u8; 128],
    pub dmg_palette: [u16; 12],
    pub palette: [u16; 64],
    pub sgb_borders: bool,
    pub frame_counter: u32,
    pub frameskip: i32,
    pub frameskip_counter: i32,

    // event bookkeeping
    pub mode_event: ModeEventKind,

    // GBVideoSoftwareRenderer
    pub output: Vec<u32>,
    pub output_stride: usize,
    pub row: [u16; GB_VIDEO_HORIZONTAL_PIXELS as usize + 8],
    pub renderer_palette: [u32; RENDERER_PALETTE_LEN],
    pub lookup: [u8; RENDERER_PALETTE_LEN],
    pub scy: u8,
    pub scx: u8,
    pub wy: u8,
    pub wx: u8,
    pub current_wy: u8,
    pub current_wx: u8,
    pub last_y: i32,
    pub last_x: i32,
    pub has_window: bool,
    pub lcdc: u8,
    pub model: GbModel,
    pub obj: [RendererSprite; GB_VIDEO_MAX_LINE_OBJ],
    pub renderer_obj_max: i32,
    pub obj_offset_x: i16,
    pub obj_offset_y: i16,
    pub offset_scx: i16,
    pub offset_scy: i16,
    pub offset_wx: i16,
    pub offset_wy: i16,
    pub sgb_transfer: i32,
    pub sgb_packet: [u8; 128],
    pub renderer_sgb_command_header: u8,
    pub renderer_sgb_borders: bool,
    pub sgb_border_mask: [u32; 18],
    pub last_highlight_amount: u8,

    pub disable_bg: bool,
    pub disable_obj: bool,
    pub disable_win: bool,
    pub highlight_bg: bool,
    pub highlight_obj: [bool; GB_VIDEO_MAX_OBJ],
    pub highlight_win: bool,
    pub highlight_color: u32,
    pub highlight_amount: u8,

    pub sgb_render_mode: i32,
    pub sgb_char_ram: Option<Box<[u8; SGB_SIZE_CHAR_RAM]>>,
    pub sgb_map_ram: Option<Box<[u8; SGB_SIZE_MAP_RAM]>>,
    pub sgb_pal_ram: Option<Box<[u8; SGB_SIZE_PAL_RAM]>>,
    pub sgb_attribute_files: Option<Box<[u8; SGB_SIZE_ATF_RAM]>>,
    pub sgb_attributes: Option<Vec<u8>>, // 90*45
}

impl Video {
    pub fn new() -> Box<Video> {
        let mut v = Box::new(Video {
            x: 0,
            ly: 0,
            stat: 0,
            mode: 0,
            dot_clock: 0,
            vram: vec![0; GB_SIZE_VRAM],
            vram_current_bank: 0,
            oam: [0; GB_SIZE_OAM],
            obj_max: 0,
            bcp_index: 0,
            bcp_increment: false,
            ocp_index: 0,
            ocp_increment: false,
            sgb_command_header: 0,
            sgb_buffer_index: 0,
            sgb_packet_buffer: [0; 128],
            dmg_palette: [0; 12],
            palette: [0; 64],
            sgb_borders: true,
            frame_counter: 0,
            frameskip: 0,
            frameskip_counter: 0,
            mode_event: ModeEventKind::EndMode0,
            output: vec![0; (SGB_VIDEO_HORIZONTAL_PIXELS * SGB_VIDEO_VERTICAL_PIXELS) as usize],
            output_stride: GB_VIDEO_HORIZONTAL_PIXELS as usize,
            row: [0; GB_VIDEO_HORIZONTAL_PIXELS as usize + 8],
            renderer_palette: [0; RENDERER_PALETTE_LEN],
            lookup: [0; RENDERER_PALETTE_LEN],
            scy: 0,
            scx: 0,
            wy: 0,
            wx: 0,
            current_wy: 0,
            current_wx: 0,
            last_y: GB_VIDEO_VERTICAL_PIXELS,
            last_x: 0,
            has_window: false,
            lcdc: 0,
            model: GbModel::Autodetect,
            obj: [RendererSprite::default(); GB_VIDEO_MAX_LINE_OBJ],
            renderer_obj_max: 0,
            obj_offset_x: 0,
            obj_offset_y: 0,
            offset_scx: 0,
            offset_scy: 0,
            offset_wx: 0,
            offset_wy: 0,
            sgb_transfer: 0,
            sgb_packet: [0; 128],
            renderer_sgb_command_header: 0,
            renderer_sgb_borders: false,
            sgb_border_mask: [0; 18],
            last_highlight_amount: 0,
            disable_bg: false,
            disable_obj: false,
            disable_win: false,
            highlight_bg: false,
            highlight_obj: [false; GB_VIDEO_MAX_OBJ],
            highlight_win: false,
            highlight_color: 0x00FFFFFF, // M_COLOR_WHITE
            highlight_amount: 0,
            sgb_render_mode: 0,
            sgb_char_ram: None,
            sgb_map_ram: None,
            sgb_pal_ram: None,
            sgb_attribute_files: None,
            sgb_attributes: None,
        });
        v.dmg_palette = [
            0x7FFF, 0x56B5, 0x294A, 0x0000, 0x7FFF, 0x56B5, 0x294A, 0x0000, 0x7FFF, 0x56B5, 0x294A,
            0x0000,
        ];
        v.renderer_init(GbModel::Autodetect, true);
        v
    }

    #[inline]
    pub fn obj(&self, i: usize) -> GbObj {
        let b = &self.oam[i * 4..i * 4 + 4];
        GbObj {
            y: b[0],
            x: b[1],
            tile: b[2],
            attr: b[3],
        }
    }

    /// GBVideoSoftwareRendererInit (model/borders)
    pub(crate) fn renderer_init(&mut self, model: GbModel, sgb_borders: bool) {
        self.lcdc = 0;
        self.scy = 0;
        self.scx = 0;
        self.wy = 0;
        self.current_wy = 0;
        self.current_wx = 0;
        self.last_y = GB_VIDEO_VERTICAL_PIXELS;
        self.last_x = 0;
        self.has_window = false;
        self.wx = 0;
        self.model = model;
        self.sgb_transfer = 0;
        self.renderer_sgb_command_header = 0;
        self.renderer_sgb_borders = sgb_borders;
        self.obj_offset_x = 0;
        self.obj_offset_y = 0;
        self.offset_scx = 0;
        self.offset_scy = 0;
        self.offset_wx = 0;
        self.offset_wy = 0;
        for i in 0..RENDERER_PALETTE_LEN {
            self.lookup[i] = i as u8;
        }
        self.renderer_palette = [0; RENDERER_PALETTE_LEN];
        self.sgb_border_mask = [0; 18];
        self.last_highlight_amount = 0;
        self.output_stride = if (model as i32) & (GbModel::Sgb as i32) != 0 && sgb_borders {
            SGB_VIDEO_HORIZONTAL_PIXELS as usize
        } else {
            GB_VIDEO_HORIZONTAL_PIXELS as usize
        };
    }
}

impl Default for Box<Video> {
    fn default() -> Self {
        Video::new()
    }
}

fn stat_irq_asserted(stat: u8) -> bool {
    // TODO: variable for the IRQ line value?
    if stat_lyc_irq(stat) && stat_lyc(stat) {
        return true;
    }
    match stat_mode(stat) {
        0 => stat_hblank_irq(stat),
        1 => stat_vblank_irq(stat),
        2 => stat_oam_irq(stat),
        3 => false,
        _ => false,
    }
}

impl Gb {
    /// GBVideoReset
    pub fn video_reset(&mut self) {
        self.video.ly = 0;
        self.video.x = 0;
        self.video.mode = 0;
        self.video.stat = 0;

        self.video.frame_counter = 0;
        self.video.frameskip_counter = 0;

        self.video_switch_bank(0);
        for b in self.video.vram.iter_mut() {
            *b = 0;
        }
        self.video.oam = [0; GB_SIZE_OAM];
        self.video.palette = [0; 64];

        if self.model_has_sgb() {
            if self.video.sgb_char_ram.is_none() {
                self.video.sgb_char_ram = Some(Box::new([0; SGB_SIZE_CHAR_RAM]));
            }
            self.video
                .sgb_char_ram
                .as_mut()
                .unwrap()
                .copy_from_slice(&DEFAULT_BORDER_CHARDATA);

            if self.video.sgb_map_ram.is_none() {
                self.video.sgb_map_ram = Some(Box::new([0; SGB_SIZE_MAP_RAM]));
            }
            self.video
                .sgb_map_ram
                .as_mut()
                .unwrap()
                .copy_from_slice(&DEFAULT_BORDER_TILEMAP);
            let map = self.video.sgb_map_ram.as_mut().unwrap();
            for i in 0..16 {
                let v = DEFAULT_BORDER_PALETTE[i];
                map[0x800 + i * 2] = v as u8;
                map[0x800 + i * 2 + 1] = (v >> 8) as u8;
            }

            if self.video.sgb_pal_ram.is_none() {
                self.video.sgb_pal_ram = Some(Box::new([0; SGB_SIZE_PAL_RAM]));
            } else {
                self.video.sgb_pal_ram.as_mut().unwrap().fill(0);
            }
            if self.video.sgb_attribute_files.is_none() {
                self.video.sgb_attribute_files = Some(Box::new([0; SGB_SIZE_ATF_RAM]));
            } else {
                self.video.sgb_attribute_files.as_mut().unwrap().fill(0);
            }
            let attrs = self
                .video
                .sgb_attributes
                .get_or_insert_with(|| vec![0; 90 * 45]);
            for b in attrs.iter_mut() {
                *b = 0;
            }
            self.video.sgb_command_header = 0;
            self.video.sgb_buffer_index = 0;
        } else {
            self.video.sgb_char_ram = None;
            self.video.sgb_map_ram = None;
            self.video.sgb_pal_ram = None;
            self.video.sgb_attribute_files = None;
            self.video.sgb_attributes = None;
        }

        self.video.palette[0] = self.video.dmg_palette[0];
        self.video.palette[1] = self.video.dmg_palette[1];
        self.video.palette[2] = self.video.dmg_palette[2];
        self.video.palette[3] = self.video.dmg_palette[3];
        self.video.palette[8 * 4] = self.video.dmg_palette[4];
        self.video.palette[8 * 4 + 1] = self.video.dmg_palette[5];
        self.video.palette[8 * 4 + 2] = self.video.dmg_palette[6];
        self.video.palette[8 * 4 + 3] = self.video.dmg_palette[7];
        self.video.palette[9 * 4] = self.video.dmg_palette[8];
        self.video.palette[9 * 4 + 1] = self.video.dmg_palette[9];
        self.video.palette[9 * 4 + 2] = self.video.dmg_palette[10];
        self.video.palette[9 * 4 + 3] = self.video.dmg_palette[11];

        self.video.renderer_init(self.model, self.video.sgb_borders);
        // GBVideoProxyRendererInit (renderer->init through the shim):
        // mVideoLoggerRendererInit re-creates the dirty bitmaps.
        if let Some(vl) = self.video_logger.as_mut() {
            vl.logger.renderer_init();
        }

        let pal = self.video.palette;
        self.renderer_write_palette(0, pal[0]);
        self.renderer_write_palette(1, pal[1]);
        self.renderer_write_palette(2, pal[2]);
        self.renderer_write_palette(3, pal[3]);
        self.renderer_write_palette(8 * 4, pal[8 * 4]);
        self.renderer_write_palette(8 * 4 + 1, pal[8 * 4 + 1]);
        self.renderer_write_palette(8 * 4 + 2, pal[8 * 4 + 2]);
        self.renderer_write_palette(8 * 4 + 3, pal[8 * 4 + 3]);
        self.renderer_write_palette(9 * 4, pal[9 * 4]);
        self.renderer_write_palette(9 * 4 + 1, pal[9 * 4 + 1]);
        self.renderer_write_palette(9 * 4 + 2, pal[9 * 4 + 2]);
        self.renderer_write_palette(9 * 4 + 3, pal[9 * 4 + 3]);

        self.schedule(EventId::VideoFrame, GB_VIDEO_TOTAL_LENGTH << 1);
    }

    /// GBVideoSkipBIOS
    pub fn video_skip_bios(&mut self) {
        self.video.mode = 1;
        self.video.mode_event = ModeEventKind::EndMode1;

        let next: i32;
        if self.model.is_cgb() {
            for i in 0..0x40usize {
                self.video.palette[i] = 0x7FFF;
                self.renderer_write_palette(i as i32, 0x7FFF);
            }
            self.video.ly = GB_VIDEO_VERTICAL_PIXELS;
            self.memory.io[GB_REG_LY as usize] = self.video.ly as u8;
            self.video.stat = stat_clear_lyc(self.video.stat);
            next = 20;
        } else {
            self.video.ly = GB_VIDEO_VERTICAL_TOTAL_PIXELS;
            self.memory.io[GB_REG_LY as usize] = 0;
            next = 56;
        }
        self.video.stat = stat_set_mode(self.video.stat, self.video.mode);

        self.memory.io[GB_REG_IF as usize] |= 1 << GB_IRQ_VBLANK;
        self.update_irqs();
        self.memory.io[GB_REG_STAT as usize] = self.video.stat;
        self.deschedule(EventId::VideoMode);
        self.schedule(EventId::VideoMode, next << 1);
    }

    fn end_mode0(&mut self, timing: &mut Timing, cycles_late: u32) {
        if self.video.frameskip_counter <= 0 {
            self.renderer_finish_scanline(self.video.ly);
            // GBVideoProxyRendererFinishScanline: backend first, then log.
            self.vl_finish_scanline(self.video.ly);
        }
        let lyc = self.memory.io[GB_REG_LYC as usize] as i32;
        let next: i32;
        self.video.ly += 1;
        self.memory.io[GB_REG_LY as usize] = self.video.ly as u8;
        let old_stat = self.video.stat;
        if self.video.ly < GB_VIDEO_VERTICAL_PIXELS {
            next = GB_VIDEO_MODE_2_LENGTH;
            self.video.mode = 2;
            self.video.mode_event = ModeEventKind::EndMode2;
        } else {
            next = GB_VIDEO_HORIZONTAL_LENGTH;
            self.video.mode = 1;
            self.video.mode_event = ModeEventKind::EndMode1;

            timing.deschedule(EventId::VideoFrame.into());
            timing.schedule(
                EventId::VideoFrame.into(),
                EventId::VideoFrame.priority(),
                -(cycles_late as i32),
            );

            if !stat_irq_asserted(old_stat) && stat_oam_irq(self.video.stat) {
                self.memory.io[GB_REG_IF as usize] |= 1 << GB_IRQ_LCDSTAT;
            }
            self.memory.io[GB_REG_IF as usize] |= 1 << GB_IRQ_VBLANK;
        }
        self.video.stat = stat_set_mode(self.video.stat, self.video.mode);
        if !stat_irq_asserted(old_stat) && stat_irq_asserted(self.video.stat) {
            self.memory.io[GB_REG_IF as usize] |= 1 << GB_IRQ_LCDSTAT;
        }

        // LYC stat is delayed 1 T-cycle
        let old_stat = self.video.stat;
        self.video.stat = stat_set_lyc(self.video.stat, lyc == self.video.ly);
        if !stat_irq_asserted(old_stat) && stat_irq_asserted(self.video.stat) {
            self.memory.io[GB_REG_IF as usize] |= 1 << GB_IRQ_LCDSTAT;
        }

        self.update_irqs();
        self.memory.io[GB_REG_STAT as usize] = self.video.stat;
        timing.schedule(
            EventId::VideoMode.into(),
            EventId::VideoMode.priority(),
            (next << 1) - cycles_late as i32,
        );
    }

    fn end_mode1(&mut self, timing: &mut Timing, cycles_late: u32) {
        if !lcdc_enable(self.memory.io[GB_REG_LCDC as usize]) {
            return;
        }
        let lyc = self.memory.io[GB_REG_LYC as usize] as i32;
        // TODO: One M-cycle delay
        self.video.ly += 1;
        let next: i32;
        if self.video.ly == GB_VIDEO_VERTICAL_TOTAL_PIXELS + 1 {
            self.video.ly = 0;
            self.memory.io[GB_REG_LY as usize] = self.video.ly as u8;
            next = GB_VIDEO_MODE_2_LENGTH;
            self.video.mode = 2;
            self.video.mode_event = ModeEventKind::EndMode2;
        } else if self.video.ly == GB_VIDEO_VERTICAL_TOTAL_PIXELS {
            self.memory.io[GB_REG_LY as usize] = 0;
            next = GB_VIDEO_HORIZONTAL_LENGTH - 8;
        } else if self.video.ly == GB_VIDEO_VERTICAL_TOTAL_PIXELS - 1 {
            self.memory.io[GB_REG_LY as usize] = self.video.ly as u8;
            next = 8;
        } else {
            self.memory.io[GB_REG_LY as usize] = self.video.ly as u8;
            next = GB_VIDEO_HORIZONTAL_LENGTH;
        }

        let old_stat = self.video.stat;
        self.video.stat = stat_set_mode(self.video.stat, self.video.mode);
        let lyc_eq = lyc == self.memory.io[GB_REG_LY as usize] as i32;
        self.video.stat = stat_set_lyc(self.video.stat, lyc_eq);
        if !stat_irq_asserted(old_stat) && stat_irq_asserted(self.video.stat) {
            self.memory.io[GB_REG_IF as usize] |= 1 << GB_IRQ_LCDSTAT;
            self.update_irqs();
        }
        self.memory.io[GB_REG_STAT as usize] = self.video.stat;
        timing.schedule(
            EventId::VideoMode.into(),
            EventId::VideoMode.priority(),
            (next << 1) - cycles_late as i32,
        );
    }

    pub(crate) fn clean_oam(&mut self, y: i32) {
        let mut sprite_height = 8;
        if lcdc_obj_size(self.memory.io[GB_REG_LCDC as usize]) {
            sprite_height = 16;
        }
        let mut o = 0;
        for i in 0..GB_VIDEO_MAX_OBJ {
            if o >= GB_VIDEO_MAX_LINE_OBJ as i32 {
                break;
            }
            let oy = self.video.oam[i * 4] as i32;
            if y < oy - 16 || y >= oy - 16 + sprite_height {
                continue;
            }
            o += 1;
        }
        self.video.obj_max = o;
    }

    fn end_mode2(&mut self, timing: &mut Timing, cycles_late: u32) {
        self.clean_oam(self.video.ly);
        self.video.x = -((self.memory.io[GB_REG_SCX as usize] & 7) as i32);
        self.video.dot_clock = timing.current_time() - cycles_late as i32 + 10 - (self.video.x << 1);
        let next = GB_VIDEO_MODE_3_LENGTH_BASE + self.video.obj_max * 6 - self.video.x;
        self.video.mode = 3;
        self.video.mode_event = ModeEventKind::EndMode3;
        let old_stat = self.video.stat;
        self.video.stat = stat_set_mode(self.video.stat, self.video.mode);
        if !stat_irq_asserted(old_stat) && stat_irq_asserted(self.video.stat) {
            self.memory.io[GB_REG_IF as usize] |= 1 << GB_IRQ_LCDSTAT;
            self.update_irqs();
        }
        self.memory.io[GB_REG_STAT as usize] = self.video.stat;
        timing.schedule(
            EventId::VideoMode.into(),
            EventId::VideoMode.priority(),
            (next << 1) - cycles_late as i32,
        );
    }

    fn end_mode3(&mut self, timing: &mut Timing, cycles_late: u32) {
        self.video_process_dots(timing, cycles_late);
        if self.video.ly < GB_VIDEO_VERTICAL_PIXELS
            && self.memory.is_hdma
            && self.memory.io[GB_REG_HDMA5 as usize] != 0xFF
        {
            self.memory.hdma_remaining = 0x10;
            self.cpu_blocked = true;
            timing.deschedule(EventId::Hdma.into());
            timing.schedule(EventId::Hdma.into(), EventId::Hdma.priority(), 0);
        }
        self.video.mode = 0;
        self.video.mode_event = ModeEventKind::EndMode0;
        let old_stat = self.video.stat;
        self.video.stat = stat_set_mode(self.video.stat, self.video.mode);
        if !stat_irq_asserted(old_stat) && stat_irq_asserted(self.video.stat) {
            self.memory.io[GB_REG_IF as usize] |= 1 << GB_IRQ_LCDSTAT;
            self.update_irqs();
        }
        self.memory.io[GB_REG_STAT as usize] = self.video.stat;
        // TODO: Cache SCX & 7 in case it changes
        let next = GB_VIDEO_MODE_0_LENGTH_BASE - self.video.obj_max * 6
            - (self.memory.io[GB_REG_SCX as usize] & 7) as i32;
        timing.schedule(
            EventId::VideoMode.into(),
            EventId::VideoMode.priority(),
            (next << 1) - cycles_late as i32,
        );
    }

    /// GBVideoProcessDots (also the modeEvent dispatch end point)
    pub fn video_process_dots(&mut self, timing: &mut Timing, cycles_late: u32) {
        if self.video.mode != 3 {
            return;
        }
        let mut old_x = self.video.x;
        self.video.x = ((timing.current_time() - cycles_late as i32 - self.video.dot_clock)
            >> 1) as i32;
        if self.video.x > GB_VIDEO_HORIZONTAL_PIXELS {
            self.video.x = GB_VIDEO_HORIZONTAL_PIXELS;
        } else if self.video.x < 0 {
            return;
        }
        if old_x < 0 {
            old_x = 0;
        }
        if self.video.frameskip_counter <= 0 {
            self.renderer_draw_range(old_x, self.video.x, self.video.ly);
            // GBVideoProxyRendererDrawRange: backend first, then the log.
            self.vl_draw_range(old_x, self.video.x, self.video.ly);
        }
    }

    /// Dispatch of the modeEvent (the C swaps its callback pointer).
    pub fn video_mode_event(&mut self, timing: &mut Timing, cycles_late: u32) {
        match self.video.mode_event {
            ModeEventKind::EndMode0 => self.end_mode0(timing, cycles_late),
            ModeEventKind::EndMode1 => self.end_mode1(timing, cycles_late),
            ModeEventKind::EndMode2 => self.end_mode2(timing, cycles_late),
            ModeEventKind::EndMode3 => self.end_mode3(timing, cycles_late),
        }
    }

    /// _updateFrameCount (frameEvent callback)
    pub fn video_frame_ended_event(&mut self, timing: &mut Timing) {
        if self.cpu.execution_state != crate::cpu::SM83_CORE_FETCH {
            timing.schedule(
                EventId::VideoFrame.into(),
                EventId::VideoFrame.priority(),
                (4 - ((self.cpu.execution_state + 1) & 3)) * (2 - self.double_speed as i32),
            );
            return;
        }
        if !lcdc_enable(self.memory.io[GB_REG_LCDC as usize]) {
            timing.schedule(
                EventId::VideoFrame.into(),
                EventId::VideoFrame.priority(),
                GB_VIDEO_TOTAL_LENGTH << 1,
            );
        }

        self.video.frameskip_counter -= 1;
        if self.video.frameskip_counter < 0 {
            self.renderer_finish_frame();
            // GBVideoProxyRendererFinishFrame: backend, then frame packet
            // + flush.
            self.vl_finish_frame();
            self.video.frameskip_counter = self.video.frameskip;
        }
        self.frame_ended();
        self.video.frame_counter += 1;
        self.gb_interrupt();
        self.frame_started();
    }

    /// GBVideoWriteLCDC
    pub fn video_write_lcdc(&mut self, value: u8) {
        if !lcdc_enable(self.memory.io[GB_REG_LCDC as usize]) && lcdc_enable(value) {
            self.video.mode = 2;
            self.video.mode_event = ModeEventKind::EndMode2;
            let next = GB_VIDEO_MODE_2_LENGTH - 5; // TODO: Why is this fudge factor needed?
            self.deschedule(EventId::VideoMode);
            self.schedule(EventId::VideoMode, next << 1);

            self.video.ly = 0;
            self.memory.io[GB_REG_LY as usize] = 0;
            let old_stat = self.video.stat;
            self.video.stat = stat_set_mode(self.video.stat, 0);
            let lyc_eq = self.video.ly == self.memory.io[GB_REG_LYC as usize] as i32;
            self.video.stat = stat_set_lyc(self.video.stat, lyc_eq);
            if !stat_irq_asserted(old_stat) && stat_irq_asserted(self.video.stat) {
                self.memory.io[GB_REG_IF as usize] |= 1 << GB_IRQ_LCDSTAT;
                self.update_irqs();
            }
            self.memory.io[GB_REG_STAT as usize] = self.video.stat;
            let p0 = self.video.palette[0];
            self.renderer_write_palette(0, p0);

            self.deschedule(EventId::VideoFrame);
        }
        if lcdc_enable(self.memory.io[GB_REG_LCDC as usize]) && !lcdc_enable(value) {
            // TODO: Fix serialization; this gets internal and visible modes out of sync
            self.video.mode = 0;
            self.video.stat = stat_set_mode(self.video.stat, 0);
            self.memory.io[GB_REG_STAT as usize] = self.video.stat;
            self.video.ly = 0;
            self.memory.io[GB_REG_LY as usize] = 0;
            let p = self.video.dmg_palette[0];
            self.renderer_write_palette(0, p);

            self.deschedule(EventId::VideoMode);
            self.deschedule(EventId::VideoFrame);
            self.schedule(EventId::VideoFrame, GB_VIDEO_TOTAL_LENGTH << 1);
        }
        self.memory.io[GB_REG_STAT as usize] = self.video.stat;
    }

    /// GBVideoWriteSTAT
    pub fn video_write_stat(&mut self, value: u8) {
        let old_stat = self.video.stat;
        self.video.stat = (self.video.stat & 0x7) | (value & 0x78);
        if !lcdc_enable(self.memory.io[GB_REG_LCDC as usize]) || self.model.is_cgb() {
            return;
        }
        // Writing to STAT on a DMG selects all STAT IRQ types for one cycle.
        // However, the signal that the mode 2 IRQ relies on is only high for
        // one cycle, which we don't handle yet. TODO: Handle it.
        if !stat_irq_asserted(old_stat) && (self.video.mode < 2 || stat_lyc(self.video.stat)) {
            // TODO: variable for the IRQ line value?
            self.memory.io[GB_REG_IF as usize] |= 1 << GB_IRQ_LCDSTAT;
            self.update_irqs();
        }
    }

    /// GBVideoWriteLYC
    pub fn video_write_lyc(&mut self, value: u8) {
        let old_stat = self.video.stat;
        if lcdc_enable(self.memory.io[GB_REG_LCDC as usize]) {
            self.video.stat = stat_set_lyc(self.video.stat, value as i32 == self.video.ly);
            if !stat_irq_asserted(old_stat) && stat_irq_asserted(self.video.stat) {
                self.memory.io[GB_REG_IF as usize] |= 1 << GB_IRQ_LCDSTAT;
                self.update_irqs();
            }
        }
        self.memory.io[GB_REG_STAT as usize] = self.video.stat;
    }

    /// GBVideoWritePalette
    pub fn video_write_palette(&mut self, address: u16, value: u8) {
        if !self.model_has_sgb() && !self.model.is_cgb() {
            match address {
                GB_REG_BGP => {
                    self.video.palette[0] = self.video.dmg_palette[(value & 3) as usize];
                    self.video.palette[1] = self.video.dmg_palette[((value >> 2) & 3) as usize];
                    self.video.palette[2] = self.video.dmg_palette[((value >> 4) & 3) as usize];
                    self.video.palette[3] = self.video.dmg_palette[((value >> 6) & 3) as usize];
                    let p = self.video.palette;
                    self.renderer_write_palette(0, p[0]);
                    self.renderer_write_palette(1, p[1]);
                    self.renderer_write_palette(2, p[2]);
                    self.renderer_write_palette(3, p[3]);
                }
                GB_REG_OBP0 => {
                    self.video.palette[8 * 4] = self.video.dmg_palette[(value & 3) as usize + 4];
                    self.video.palette[8 * 4 + 1] =
                        self.video.dmg_palette[((value >> 2) & 3) as usize + 4];
                    self.video.palette[8 * 4 + 2] =
                        self.video.dmg_palette[((value >> 4) & 3) as usize + 4];
                    self.video.palette[8 * 4 + 3] =
                        self.video.dmg_palette[((value >> 6) & 3) as usize + 4];
                    let p = self.video.palette;
                    for i in 0..4 {
                        self.renderer_write_palette(8 * 4 + i, p[8 * 4 + i as usize]);
                    }
                }
                GB_REG_OBP1 => {
                    self.video.palette[9 * 4] = self.video.dmg_palette[(value & 3) as usize + 8];
                    self.video.palette[9 * 4 + 1] =
                        self.video.dmg_palette[((value >> 2) & 3) as usize + 8];
                    self.video.palette[9 * 4 + 2] =
                        self.video.dmg_palette[((value >> 4) & 3) as usize + 8];
                    self.video.palette[9 * 4 + 3] =
                        self.video.dmg_palette[((value >> 6) & 3) as usize + 8];
                    let p = self.video.palette;
                    for i in 0..4 {
                        self.renderer_write_palette(9 * 4 + i, p[9 * 4 + i as usize]);
                    }
                }
                _ => {}
            }
        } else if self.model.is_cgb() {
            match address {
                GB_REG_BCPD => {
                    if self.video.mode != 3 {
                        if self.video.bcp_index & 1 != 0 {
                            let e = &mut self.video.palette[(self.video.bcp_index >> 1) as usize];
                            *e &= 0x00FF;
                            *e |= (value as u16) << 8;
                        } else {
                            let e = &mut self.video.palette[(self.video.bcp_index >> 1) as usize];
                            *e &= 0xFF00;
                            *e |= value as u16;
                        }
                        let v = self.video.palette[(self.video.bcp_index >> 1) as usize];
                        self.renderer_write_palette(self.video.bcp_index >> 1, v);
                    }
                    if self.video.bcp_increment {
                        self.video.bcp_index += 1;
                        self.video.bcp_index &= 0x3F;
                        self.memory.io[GB_REG_BCPS as usize] &= 0x80;
                        self.memory.io[GB_REG_BCPS as usize] |= self.video.bcp_index as u8;
                    }
                    self.memory.io[GB_REG_BCPD as usize] = (self.video.palette
                        [(self.video.bcp_index >> 1) as usize]
                        >> (8 * (self.video.bcp_index & 1)))
                        as u8;
                }
                GB_REG_OCPD => {
                    if self.video.mode != 3 {
                        let idx = 8 * 4 + (self.video.ocp_index >> 1) as usize;
                        if self.video.ocp_index & 1 != 0 {
                            self.video.palette[idx] &= 0x00FF;
                            self.video.palette[idx] |= (value as u16) << 8;
                        } else {
                            self.video.palette[idx] &= 0xFF00;
                            self.video.palette[idx] |= value as u16;
                        }
                        let v = self.video.palette[idx];
                        self.renderer_write_palette(idx as i32, v);
                    }
                    if self.video.ocp_increment {
                        self.video.ocp_index += 1;
                        self.video.ocp_index &= 0x3F;
                        self.memory.io[GB_REG_OCPS as usize] &= 0x80;
                        self.memory.io[GB_REG_OCPS as usize] |= self.video.ocp_index as u8;
                    }
                    self.memory.io[GB_REG_OCPD as usize] = (self.video.palette
                        [8 * 4 + (self.video.ocp_index >> 1) as usize]
                        >> (8 * (self.video.ocp_index & 1)))
                        as u8;
                }
                _ => {}
            }
        } else {
            self.renderer_write_video_register(address, value);
        }
    }

    /// GBVideoSwitchBank
    pub fn video_switch_bank(&mut self, value: u8) {
        let value = (value & 1) as i32;
        self.video.vram_current_bank = value;
    }

    /// GBVideoSetPalette (custom DMG palette from the frontend)
    pub fn video_set_palette(&mut self, index: u32, color: u32) {
        if index >= 12 {
            return;
        }
        self.video.dmg_palette[index as usize] =
            ((color & 0xF8) << 7 | (color & 0xF800) >> 6 | (color & 0xF80000) >> 19) as u16;
    }

    /// GBVideoDisableCGB
    pub fn video_disable_cgb(&mut self) {
        self.video.dmg_palette[0] = self.video.palette[0];
        self.video.dmg_palette[1] = self.video.palette[1];
        self.video.dmg_palette[2] = self.video.palette[2];
        self.video.dmg_palette[3] = self.video.palette[3];
        self.video.dmg_palette[4] = self.video.palette[8 * 4];
        self.video.dmg_palette[5] = self.video.palette[8 * 4 + 1];
        self.video.dmg_palette[6] = self.video.palette[8 * 4 + 2];
        self.video.dmg_palette[7] = self.video.palette[8 * 4 + 3];
        self.video.dmg_palette[8] = self.video.palette[9 * 4];
        self.video.dmg_palette[9] = self.video.palette[9 * 4 + 1];
        self.video.dmg_palette[10] = self.video.palette[9 * 4 + 2];
        self.video.dmg_palette[11] = self.video.palette[9 * 4 + 3];
        self.video.renderer_init(self.model, self.video.sgb_borders);
        // GBVideoProxyRendererInit parity (see video_reset).
        if let Some(vl) = self.video_logger.as_mut() {
            vl.logger.renderer_init();
        }
    }

    /// GBVideoWriteSGBPacket
    pub fn video_write_sgb_packet(&mut self, data: &[u8; 16]) {
        if self.video.sgb_command_header & 7 == 0 {
            self.video.sgb_buffer_index = 0;
            if (data[0] >> 3) > SGB_OBJ_TRN {
                self.video.sgb_command_header = 0;
                return;
            }
            self.video.sgb_command_header = data[0];
        }
        self.video.sgb_command_header -= 1;
        self.video.sgb_packet_buffer[((self.video.sgb_buffer_index as usize) << 4)..][..16]
            .copy_from_slice(data);
        self.video.sgb_buffer_index += 1;
        if self.video.sgb_command_header & 7 != 0 {
            return;
        }
        let packet = self.video.sgb_packet_buffer;
        match self.video.sgb_command_header >> 3 {
            SGB_PAL01 => {
                self.video.palette[0] = packet[1] as u16 | (packet[2] as u16) << 8;
                self.video.palette[1] = packet[3] as u16 | (packet[4] as u16) << 8;
                self.video.palette[2] = packet[5] as u16 | (packet[6] as u16) << 8;
                self.video.palette[3] = packet[7] as u16 | (packet[8] as u16) << 8;

                self.video.palette[4] = packet[1] as u16 | (packet[2] as u16) << 8;
                self.video.palette[5] = packet[9] as u16 | (packet[10] as u16) << 8;
                self.video.palette[6] = packet[11] as u16 | (packet[12] as u16) << 8;
                self.video.palette[7] = packet[13] as u16 | (packet[14] as u16) << 8;

                self.video.palette[8] = packet[1] as u16 | (packet[2] as u16) << 8;
                self.video.palette[12] = packet[1] as u16 | (packet[2] as u16) << 8;

                let idxs = [0usize, 1, 2, 3, 4, 5, 6, 7, 8, 12];
                let p = self.video.palette;
                for i in idxs {
                    self.renderer_write_palette(i as i32, p[i]);
                }
            }
            SGB_PAL23 => {
                self.video.palette[9] = packet[3] as u16 | (packet[4] as u16) << 8;
                self.video.palette[10] = packet[5] as u16 | (packet[6] as u16) << 8;
                self.video.palette[11] = packet[7] as u16 | (packet[8] as u16) << 8;

                self.video.palette[13] = packet[9] as u16 | (packet[10] as u16) << 8;
                self.video.palette[14] = packet[11] as u16 | (packet[12] as u16) << 8;
                self.video.palette[15] = packet[13] as u16 | (packet[14] as u16) << 8;
                let p = self.video.palette;
                for i in [9usize, 10, 11, 13, 14, 15] {
                    self.renderer_write_palette(i as i32, p[i]);
                }
            }
            SGB_PAL03 => {
                self.video.palette[0] = packet[1] as u16 | (packet[2] as u16) << 8;
                self.video.palette[1] = packet[3] as u16 | (packet[4] as u16) << 8;
                self.video.palette[2] = packet[5] as u16 | (packet[6] as u16) << 8;
                self.video.palette[3] = packet[7] as u16 | (packet[8] as u16) << 8;

                self.video.palette[4] = packet[1] as u16 | (packet[2] as u16) << 8;
                self.video.palette[8] = packet[1] as u16 | (packet[2] as u16) << 8;

                self.video.palette[12] = packet[1] as u16 | (packet[2] as u16) << 8;
                self.video.palette[13] = packet[9] as u16 | (packet[10] as u16) << 8;
                self.video.palette[14] = packet[11] as u16 | (packet[12] as u16) << 8;
                self.video.palette[15] = packet[13] as u16 | (packet[14] as u16) << 8;
                let p = self.video.palette;
                for i in [0usize, 1, 2, 3, 4, 8, 12, 13, 14, 15] {
                    self.renderer_write_palette(i as i32, p[i]);
                }
            }
            SGB_PAL12 => {
                self.video.palette[5] = packet[3] as u16 | (packet[4] as u16) << 8;
                self.video.palette[6] = packet[5] as u16 | (packet[6] as u16) << 8;
                self.video.palette[7] = packet[7] as u16 | (packet[8] as u16) << 8;

                self.video.palette[9] = packet[9] as u16 | (packet[10] as u16) << 8;
                self.video.palette[10] = packet[11] as u16 | (packet[12] as u16) << 8;
                self.video.palette[11] = packet[13] as u16 | (packet[14] as u16) << 8;
                let p = self.video.palette;
                for i in [5usize, 6, 7, 9, 10, 11] {
                    self.renderer_write_palette(i as i32, p[i]);
                }
            }
            SGB_PAL_SET => {
                for i in 0..4 {
                    let entry =
                        ((packet[2 + i * 2] as u16) << 8) | packet[1 + i * 2] as u16;
                    if entry >= 0x200 {
                        mlog!(
                            Level::Stub,
                            rgba_core::log::GB,
                            "Unimplemented SGB palette overflow: {:03X}",
                            entry
                        );
                        continue;
                    }
                    if let Some(pal_ram) = &self.video.sgb_pal_ram {
                        let base = entry as usize * 8;
                        for j in 0..4 {
                            let lo = pal_ram[base + j * 2] as u16;
                            let hi = pal_ram[base + j * 2 + 1] as u16;
                            self.video.palette[i * 4 + j] = lo | hi << 8;
                        }
                    }
                    let p = self.video.palette;
                    for j in 0..4 {
                        self.renderer_write_palette((i * 4 + j) as i32, p[i * 4 + j]);
                    }
                }
            }
            SGB_ATTR_BLK | SGB_ATTR_DIV | SGB_ATTR_CHR | SGB_ATTR_LIN | SGB_PAL_TRN
            | SGB_ATRC_EN | SGB_CHR_TRN | SGB_PCT_TRN | SGB_ATTR_TRN | SGB_ATTR_SET => {}
            SGB_MLT_REQ => {
                if (packet[1] & 0x3) == 2 {
                    // XXX: This unmasked increment appears to be an SGB hardware bug
                    self.sgb_current_controller += 1;
                }
                self.sgb_controllers = packet[1] & 0x3;
                self.sgb_current_controller &= self.sgb_controllers;
                return;
            }
            SGB_MASK_EN => {
                self.video.sgb_render_mode = (packet[1] & 0x3) as i32;
            }
            _ => {
                mlog!(
                    Level::Stub,
                    rgba_core::log::GB,
                    "Unimplemented SGB command: {:02X}",
                    self.video.sgb_packet_buffer[0] >> 3
                );
                return;
            }
        }
        self.renderer_write_sgb_packet();
        // GBVideoProxyRendererWriteSGBPacket: backend first, then the log.
        self.vl_write_sgb_packet();
    }
}

// SGB command ids (enum GBSGBCommand)
pub const SGB_PAL01: u8 = 0;
pub const SGB_PAL23: u8 = 1;
pub const SGB_PAL03: u8 = 2;
pub const SGB_PAL12: u8 = 3;
pub const SGB_ATTR_BLK: u8 = 4;
pub const SGB_ATTR_LIN: u8 = 5;
pub const SGB_ATTR_DIV: u8 = 6;
pub const SGB_ATTR_CHR: u8 = 7;
pub const SGB_SOUND: u8 = 8;
pub const SGB_SOU_TRN: u8 = 9;
pub const SGB_PAL_SET: u8 = 10;
pub const SGB_PAL_TRN: u8 = 11;
pub const SGB_ATRC_EN: u8 = 12;
pub const SGB_TEST_EN: u8 = 13;
pub const SGB_PICON_EN: u8 = 14;
pub const SGB_DATA_SND: u8 = 15;
pub const SGB_DATA_TRN: u8 = 16;
pub const SGB_MLT_REQ: u8 = 17;
pub const SGB_JUMP: u8 = 18;
pub const SGB_CHR_TRN: u8 = 19;
pub const SGB_PCT_TRN: u8 = 20;
pub const SGB_ATTR_TRN: u8 = 21;
pub const SGB_ATTR_SET: u8 = 22;
pub const SGB_MASK_EN: u8 = 23;
pub const SGB_OBJ_TRN: u8 = 24;

// ---------------------------------------------------------------------------
// Software renderer (renderers/software.c), merged into `impl Gb`.
// The C's renderer->d.vram points at video->vram, d.oam at video->oam.

impl Gb {
    fn in_window(&self) -> bool {
        lcdc_window(self.video.lcdc) && GB_VIDEO_HORIZONTAL_PIXELS + 7 > self.video.wx as i32
    }

    fn update_window(&mut self, before: bool, after: bool, old_wy: u8) {
        if self.video.last_y >= GB_VIDEO_VERTICAL_PIXELS || !(after || before) {
            return;
        }
        if !self.video.has_window
            && self.video.last_x == GB_VIDEO_HORIZONTAL_PIXELS
            && self.video.last_y != old_wy as i32
        {
            return;
        }
        if self.video.last_y >= old_wy as i32 {
            if !after {
                self.video.current_wy = self.video.current_wy.wrapping_sub(self.video.last_y as u8);
                self.video.has_window = true;
            } else if !before {
                if !self.video.has_window {
                    self.video.current_wy = (self.video.last_y - self.video.wy as i32) as u8;
                    if self.video.last_y >= self.video.wy as i32
                        && self.video.last_x > self.video.wx as i32
                    {
                        self.video.current_wy = self.video.current_wy.wrapping_add(1);
                    }
                } else {
                    self.video.current_wy =
                        self.video.current_wy.wrapping_add(self.video.last_y as u8);
                }
            } else if self.video.wy != old_wy {
                self.video.current_wy =
                    self.video.current_wy.wrapping_add(old_wy.wrapping_sub(self.video.wy));
                self.video.has_window = true;
            }
        }
    }

    /// GBVideoSoftwareRendererWriteVideoRegister
    pub fn renderer_write_video_register(&mut self, address: u16, value: u8) -> u8 {
        // GBVideoProxyRendererWriteVideoRegister: log before the backend.
        self.vl_write_video_register(address, value);
        let was_window = self.in_window();
        let wy = self.video.wy;
        match address {
            GB_REG_LCDC => {
                self.video.lcdc = value;
                let now_window = self.in_window();
                self.update_window(was_window, now_window, wy);
            }
            GB_REG_SCY => self.video.scy = value,
            GB_REG_SCX => self.video.scx = value,
            GB_REG_WY => {
                self.video.wy = value;
                let now_window = self.in_window();
                self.update_window(was_window, now_window, wy);
            }
            GB_REG_WX => {
                self.video.wx = value;
                let now_window = self.in_window();
                self.update_window(was_window, now_window, wy);
            }
            GB_REG_BGP => {
                let v = &mut self.video;
                v.lookup[0] = value & 3;
                v.lookup[1] = (value >> 2) & 3;
                v.lookup[2] = (value >> 4) & 3;
                v.lookup[3] = (value >> 6) & 3;
                v.lookup[(PAL_HIGHLIGHT_BG + 0) as usize] =
                    (PAL_HIGHLIGHT + (value & 3) as u32) as u8;
                v.lookup[(PAL_HIGHLIGHT_BG + 1) as usize] =
                    (PAL_HIGHLIGHT + ((value >> 2) & 3) as u32) as u8;
                v.lookup[(PAL_HIGHLIGHT_BG + 2) as usize] =
                    (PAL_HIGHLIGHT + ((value >> 4) & 3) as u32) as u8;
                v.lookup[(PAL_HIGHLIGHT_BG + 3) as usize] =
                    (PAL_HIGHLIGHT + ((value >> 6) & 3) as u32) as u8;
            }
            GB_REG_OBP0 => {
                let v = &mut self.video;
                v.lookup[(PAL_OBJ + 0) as usize] = value & 3;
                v.lookup[(PAL_OBJ + 1) as usize] = (value >> 2) & 3;
                v.lookup[(PAL_OBJ + 2) as usize] = (value >> 4) & 3;
                v.lookup[(PAL_OBJ + 3) as usize] = (value >> 6) & 3;
                for i in 0..4u32 {
                    v.lookup[(PAL_HIGHLIGHT_OBJ + i) as usize] =
                        (PAL_HIGHLIGHT + ((value >> (i * 2)) & 3) as u32) as u8;
                }
            }
            GB_REG_OBP1 => {
                let v = &mut self.video;
                v.lookup[(PAL_OBJ + 4) as usize] = value & 3;
                v.lookup[(PAL_OBJ + 5) as usize] = (value >> 2) & 3;
                v.lookup[(PAL_OBJ + 6) as usize] = (value >> 4) & 3;
                v.lookup[(PAL_OBJ + 7) as usize] = (value >> 6) & 3;
                for i in 0..4u32 {
                    v.lookup[(PAL_HIGHLIGHT_OBJ + 4 + i) as usize] =
                        (PAL_HIGHLIGHT + ((value >> (i * 2)) & 3) as u32) as u8;
                }
            }
            _ => {}
        }
        value
    }

    /// GBVideoSoftwareRendererWriteSGBPacket
    pub fn renderer_write_sgb_packet(&mut self) {
        let data = self.video.sgb_packet_buffer;
        self.video.sgb_packet = data;
        self.video.renderer_sgb_command_header = data[0];
        self.video.sgb_transfer = 0;
        match self.video.renderer_sgb_command_header >> 3 {
            SGB_PAL_SET => {
                self.video.sgb_packet[1] = data[9];
                if data[9] & 0x80 == 0 {
                    // break out of the switch
                } else {
                    self.sgb_attr_set_apply();
                }
            }
            SGB_ATTR_SET => self.sgb_attr_set_apply(),
            SGB_ATTR_BLK => {
                let mut sets = self.video.sgb_packet[1] as i32;
                let mut i = 2usize;
                while i < ((self.video.renderer_sgb_command_header as usize & 7) << 4) && sets > 0 {
                    self.parse_attr_block(i as i32);
                    i += 6;
                    sets -= 1;
                }
            }
            SGB_ATTR_LIN => {
                let mut sets = self.video.sgb_packet[1] as i32;
                let mut i = 2usize;
                while i < ((self.video.renderer_sgb_command_header as usize & 7) << 4) && sets > 0 {
                    self.parse_attr_line(i as i32);
                    i += 1;
                    sets -= 1;
                }
            }
            SGB_ATTR_DIV => {
                let p_after = (self.video.sgb_packet[1] & 3) as i32;
                let p_before = ((self.video.sgb_packet[1] >> 2) & 3) as i32;
                let p_div = ((self.video.sgb_packet[1] >> 4) & 3) as i32;
                let mut attr_x = self.video.sgb_packet[2] as i32;
                let attr_width = GB_VIDEO_HORIZONTAL_PIXELS / 8;
                let attr_height = GB_VIDEO_VERTICAL_PIXELS / 8;
                if self.video.sgb_packet[1] & 0x40 != 0 {
                    if attr_x > attr_height {
                        attr_x = attr_height;
                    }
                    let mut j = 0;
                    while j < attr_x {
                        for i in 0..attr_width {
                            self.set_attribute(i, j, p_before);
                        }
                        j += 1;
                    }
                    if attr_x < attr_height {
                        for i in 0..attr_width {
                            self.set_attribute(i, attr_x, p_div);
                        }
                    }
                    while j < attr_height {
                        for i in 0..attr_width {
                            self.set_attribute(i, j, p_after);
                        }
                        j += 1;
                    }
                } else {
                    if attr_x > attr_width {
                        attr_x = attr_width;
                    }
                    let mut j = 0;
                    while j < attr_x {
                        for i in 0..attr_width {
                            self.set_attribute(j, i, p_before);
                        }
                        j += 1;
                    }
                    if attr_x < attr_width {
                        for i in 0..attr_height {
                            self.set_attribute(attr_x, i, p_div);
                        }
                    }
                    while j < attr_width {
                        for i in 0..attr_height {
                            self.set_attribute(j, i, p_after);
                        }
                        j += 1;
                    }
                }
            }
            SGB_ATTR_CHR => {
                let mut attr_x = self.video.sgb_packet[1] as i32;
                let mut attr_y = self.video.sgb_packet[2] as i32;
                let attr_width = GB_VIDEO_HORIZONTAL_PIXELS / 8;
                let attr_height = GB_VIDEO_VERTICAL_PIXELS / 8;
                if attr_x >= attr_width {
                    attr_x = 0;
                }
                if attr_y >= attr_height {
                    attr_y = 0;
                }
                let mut sets =
                    self.video.sgb_packet[3] as i32 | ((self.video.sgb_packet[4] as i32) << 8);
                let attr_direction = self.video.sgb_packet[5] as i32;
                let mut i = 6usize;
                while i < ((self.video.renderer_sgb_command_header as usize & 7) << 4) && sets > 0 {
                    let mut j = 0;
                    while j < 4 && sets > 0 {
                        let p = self.video.sgb_packet[i] >> (6 - j * 2);
                        self.set_attribute(attr_x, attr_y, (p & 3) as i32);
                        if attr_direction != 0 {
                            attr_y += 1;
                            if attr_y >= attr_height {
                                attr_y = 0;
                                attr_x += 1;
                            }
                            if attr_x >= attr_width {
                                attr_x = 0;
                            }
                        } else {
                            attr_x += 1;
                            if attr_x >= attr_width {
                                attr_x = 0;
                                attr_y += 1;
                            }
                            if attr_y >= attr_height {
                                attr_y = 0;
                            }
                        }
                        sets -= 1;
                        j += 1;
                    }
                    i += 1;
                }
            }
            SGB_ATRC_EN | SGB_MASK_EN => {
                if self.video.renderer_sgb_borders && self.video.sgb_render_mode == 0 {
                    self.regenerate_sgb_border();
                }
            }
            _ => {}
        }
    }

    fn sgb_attr_set_apply(&mut self) {
        let set = (self.video.sgb_packet[1] & 0x3F) as usize;
        if set <= 0x2C {
            if let (Some(attrs), Some(files)) = (
                self.video.sgb_attributes.as_mut(),
                self.video.sgb_attribute_files.as_ref(),
            ) {
                attrs[..90].copy_from_slice(&files[set * 90..set * 90 + 90]);
            }
        }
    }

    fn set_attribute(&mut self, x: i32, y: i32, palette: i32) {
        if let Some(attrs) = self.video.sgb_attributes.as_mut() {
            let idx = ((x >> 2) + 5 * y) as usize;
            if idx >= attrs.len() {
                return;
            }
            let mut p = attrs[idx];
            p &= !(3 << (2 * (3 - (x & 3)))) as u8;
            p |= (palette as u8) << (2 * (3 - (x & 3)));
            attrs[idx] = p;
        }
    }

    fn parse_attr_block(&mut self, start: i32) {
        let block: [u8; 6] = self.video.sgb_packet[start as usize..start as usize + 6]
            .try_into()
            .unwrap();
        let x0 = block[2] as i32;
        let x1 = block[4] as i32;
        let y0 = block[3] as i32;
        let y1 = block[5] as i32;
        let p_in = (block[1] & 3) as i32;
        let p_perim = ((block[1] >> 2) & 3) as i32;
        let p_out = ((block[1] >> 4) & 3) as i32;

        for y in 0..GB_VIDEO_VERTICAL_PIXELS / 8 {
            for x in 0..GB_VIDEO_HORIZONTAL_PIXELS / 8 {
                if y > y0 && y < y1 && x > x0 && x < x1 {
                    if block[0] & 1 != 0 {
                        self.set_attribute(x, y, p_in);
                    }
                } else if y < y0 || y > y1 || x < x0 || x > x1 {
                    if block[0] & 4 != 0 {
                        self.set_attribute(x, y, p_out);
                    }
                } else if block[0] & 2 != 0 {
                    self.set_attribute(x, y, p_perim);
                } else if block[0] & 1 != 0 {
                    self.set_attribute(x, y, p_in);
                } else if block[0] & 4 != 0 {
                    self.set_attribute(x, y, p_out);
                }
            }
        }
    }

    fn parse_attr_line(&mut self, start: i32) {
        let byte = self.video.sgb_packet[start as usize];
        let line = (byte & 0x1F) as i32;
        let pal = ((byte >> 5) & 3) as i32;

        if byte & 0x80 != 0 {
            if line > GB_VIDEO_VERTICAL_PIXELS / 8 {
                return;
            }
            for x in 0..GB_VIDEO_HORIZONTAL_PIXELS / 8 {
                self.set_attribute(x, line, pal);
            }
        } else {
            if line > GB_VIDEO_HORIZONTAL_PIXELS / 8 {
                return;
            }
            for y in 0..GB_VIDEO_VERTICAL_PIXELS / 8 {
                self.set_attribute(line, y, pal);
            }
        }
    }

    /// GBVideoSoftwareRendererWriteVRAM (tile-cache invalidate; no-op here)
    pub fn renderer_write_vram(&mut self, addr: u16) {
        // GBVideoProxyRendererWriteVRAM.
        self.vl_write_vram(addr);
    }

    /// GBVideoSoftwareRendererWriteOAM
    pub fn renderer_write_oam(&mut self, addr: u16) {
        // GBVideoProxyRendererWriteOAM (backend had nothing to do; the C
        // logs after the no-op backend call, reading live OAM either way).
        self.vl_write_oam(addr);
    }

    /// GBVideoSoftwareRendererWritePalette (index is in [0,192) into the C
    /// renderer's palette array)
    pub fn renderer_write_palette(&mut self, index: i32, value: u16) {
        // GBVideoProxyRendererWritePalette: log before the backend.
        self.vl_write_palette(index, value);
        let mut color = m_color_from_555(value);
        if self.model_has_sgb() {
            if index as u32 >= PAL_SGB_BORDER && (index & 0xF) == 0 {
                color = self.video.renderer_palette[0];
            } else if !self.model.is_cgb() {
                if index < 0x10 && index != 0 && (index & 3) == 0 {
                    color = self.video.renderer_palette[0];
                } else if index as u32 > PAL_HIGHLIGHT
                    && (index as u32) < PAL_HIGHLIGHT_OBJ
                    && (index & 3) == 0
                {
                    color = self.video.renderer_palette[PAL_HIGHLIGHT_BG as usize];
                }
            }
        }
        if self.model == GbModel::Agb {
            let mut r = (value & 0x1F) as u32;
            let mut g = ((value >> 5) & 0x1F) as u32;
            let mut b = ((value >> 10) & 0x1F) as u32;
            r *= r;
            g *= g;
            b *= b;
            // 32-bit color path
            r >>= 2;
            r += r >> 4;
            g >>= 2;
            g += g >> 4;
            b >>= 2;
            b += b >> 4;
            // NOTE: C packs these directly as an RGB8-out color
            color = ((b & 0xFF) << 16) | ((g & 0xFF) << 8) | (r & 0xFF);
        }
        self.video.renderer_palette[index as usize] = color;
        if (index as u32) < PAL_SGB_BORDER && ((index as u32) < PAL_OBJ || (index & 3) != 0) {
            self.video.renderer_palette[index as usize + PAL_HIGHLIGHT as usize] = m_color_mix_5bit(
                0x10 - self.video.last_highlight_amount as i32,
                color,
                self.video.last_highlight_amount as i32,
                self.video.highlight_color,
            );
        }

        if self.model_has_sgb() && index == 0 && lcdc_enable(self.video.lcdc) {
            if !self.model.is_cgb() {
                self.renderer_write_palette(0x04, value);
                self.renderer_write_palette(0x08, value);
                self.renderer_write_palette(0x0C, value);
                self.renderer_write_palette(0x40, value);
                self.renderer_write_palette(0x50, value);
                self.renderer_write_palette(0x60, value);
                self.renderer_write_palette(0x70, value);
            }
            if self.video.renderer_sgb_borders && self.video.sgb_render_mode == 0 {
                self.regenerate_sgb_border();
            }
        }
    }

    fn regenerate_sgb_border(&mut self) {
        if self.video.sgb_map_ram.is_none() || self.video.sgb_char_ram.is_none() {
            return;
        }
        for i in 0..0x40u32 {
            let lo = self.video.sgb_map_ram.as_ref().unwrap()[0x800 + i as usize * 2] as u16;
            let hi = self.video.sgb_map_ram.as_ref().unwrap()[0x800 + i as usize * 2 + 1] as u16;
            let color = lo | (hi << 8);
            self.renderer_write_palette((i + PAL_SGB_BORDER) as i32, color);
        }
        let stride = self.video.output_stride;
        for y in 0..224i32 {
            let local_y = (y & 0x7) as usize;
            if local_y == 0 && (40..184).contains(&y) {
                self.video.sgb_border_mask[((y - 40) >> 3) as usize] = 0;
            }
            let mut x = 0i32;
            while x < 256 {
                let off = ((x >> 2) + (y & !7) * 8) as usize;
                let map_data = self.video.sgb_map_ram.as_ref().unwrap()[off] as u16
                    | (self.video.sgb_map_ram.as_ref().unwrap()[off + 1] as u16) << 8;
                if map_data & SGB_BG_ATTR_TILE_MASK >= 0x100 {
                    x += 8;
                    continue;
                }
                if (48..208).contains(&x) && (40..184).contains(&y) {
                    if local_y == 0 {
                        let tile_base = ((map_data & SGB_BG_ATTR_TILE_MASK) * 8) as usize;
                        let chars = self.video.sgb_char_ram.as_ref().unwrap();
                        let mut bits: u32 = 0;
                        for w in 0..8 {
                            bits |= u32::from_le_bytes([
                                chars[tile_base * 4 + w * 4],
                                chars[tile_base * 4 + w * 4 + 1],
                                chars[tile_base * 4 + w * 4 + 2],
                                chars[tile_base * 4 + w * 4 + 3],
                            ]);
                        }
                        if bits != 0 {
                            self.video.sgb_border_mask[((y - 40) >> 3) as usize] |=
                                1 << ((x - 48) >> 3);
                        }
                    }
                    x += 8;
                    continue;
                }

                let mut y_flip = 0usize;
                if map_data & SGB_BG_ATTR_YFLIP != 0 {
                    y_flip = 7;
                }
                let tile_base =
                    (((map_data & SGB_BG_ATTR_TILE_MASK) as usize * 16) + (local_y ^ y_flip)) * 2;
                let chars = self.video.sgb_char_ram.as_ref().unwrap();
                let tile_data = [
                    chars[tile_base],
                    chars[tile_base + 1],
                    chars[tile_base + 0x10],
                    chars[tile_base + 0x11],
                ];
                let base = y as usize * stride + x as usize;
                let palette_base = ((map_data & SGB_BG_ATTR_PALETTE_MASK)
                    >> SGB_BG_ATTR_PALETTE_SHIFT) as usize
                    * 0x10;

                let mut x_flip = 0usize;
                if map_data & SGB_BG_ATTR_XFLIP != 0 {
                    x_flip = 7;
                }
                for i in (0..8usize).rev() {
                    let color_selector = ((tile_data[0] >> i) & 1) as usize
                        | (((tile_data[1] >> i) & 1) as usize) << 1
                        | (((tile_data[2] >> i) & 1) as usize) << 2
                        | (((tile_data[3] >> i) & 1) as usize) << 3;
                    self.video.output[(base + 7 - i) ^ x_flip] =
                        m_color_to_rgba32(self.video.renderer_palette[palette_base | color_selector]);
                }
                x += 8;
            }
        }
    }

    fn renderer_clean_oam(&mut self, y: i32) {
        // TODO: GBC differences
        // TODO: Optimize
        let mut sprite_height = 8;
        if lcdc_obj_size(self.video.lcdc) {
            sprite_height = 16;
        }
        let mut o = 0usize;
        let mut ids = [0i16; GB_VIDEO_MAX_LINE_OBJ];
        for i in 0..GB_VIDEO_MAX_OBJ {
            if o >= GB_VIDEO_MAX_LINE_OBJ {
                break;
            }
            let oy = self.video.oam[i * 4] as i32; // obj.y
            if y < oy - 16 || y >= oy - 16 + sprite_height {
                continue;
            }
            ids[o] = (((self.video.oam[i * 4 + 1] as i32) << 7) | i as i32) as i16;
            o += 1;
        }
        self.video.renderer_obj_max = o as i32;
        if !self.model.is_cgb() {
            // Terrible n^2 sort, but it's only 10 elements so it shouldn't be that bad
            let mut ids2 = [0i16; GB_VIDEO_MAX_LINE_OBJ];
            let mut min = -1i32;
            for i in 0..o {
                let mut min2 = 0xFFFFi32;
                for j in 0..o {
                    if ids[j] as i32 > min && (ids[j] as i32) < min2 {
                        min2 = ids[j] as i32;
                    }
                }
                min = min2;
                ids2[i] = min as i16;
            }
            ids[..o].copy_from_slice(&ids2[..o]);
        }
        for i in 0..o {
            let id = (ids[i] & 0x7F) as usize;
            self.video.obj[i] = RendererSprite {
                obj: self.video.obj(id),
                index: id as i8,
            };
        }
    }

    /// GBVideoSoftwareRendererDrawBackground
    fn draw_background(&mut self, map_off: usize, start_x: i32, end_x: i32, sx: i32, sy: i32, highlight: bool) {
        let mut start_x = start_x;
        let data_off = if lcdc_tile_data(self.video.lcdc) { 0 } else { 0x1000 };
        let top_y = ((sy >> 3) & 0x1F) as usize * 0x20;
        let bottom_y = (sy & 7) as usize;
        if start_x < 0 {
            start_x = 0;
        }
        if (start_x + sx) & 7 != 0 {
            let start_x2 = start_x + 8 - ((start_x + sx) & 7);
            let mut x = start_x;
            while x < start_x2 && x < end_x {
                let mut local_data = data_off;
                let mut local_y = bottom_y;
                let top_x = (((x + sx) >> 3) & 0x1F) as usize;
                let mut bottom_x = 7 - ((x + sx) & 7);
                let bg_tile = if lcdc_tile_data(self.video.lcdc) {
                    self.video.vram[map_off + top_x + top_y] as i32
                } else {
                    self.video.vram[map_off + top_x + top_y] as i8 as i32
                };
                let mut p: u32 = if highlight { PAL_HIGHLIGHT_BG } else { PAL_BG };
                if self.model.is_cgb() {
                    let attrs = self.video.vram[map_off + GB_SIZE_VRAM_BANK0 + top_x + top_y];
                    p |= ((attrs & OBJ_ATTR_CGB_PALETTE_MASK) as u32) * 4;
                    if attrs & OBJ_ATTR_PRIORITY != 0 && lcdc_bg_enable(self.video.lcdc) {
                        p |= OBJ_PRIORITY;
                    }
                    if attrs & OBJ_ATTR_BANK != 0 {
                        local_data += GB_SIZE_VRAM_BANK0;
                    }
                    if attrs & OBJ_ATTR_YFLIP != 0 {
                        local_y = 7 - bottom_y;
                    }
                    if attrs & OBJ_ATTR_XFLIP != 0 {
                        bottom_x = 7 - bottom_x;
                    }
                }
                let mut tile_data_lower = self.video.vram[local_data + (bg_tile as usize * 8 + local_y) * 2];
                let mut tile_data_upper = self.video.vram[local_data + (bg_tile as usize * 8 + local_y) * 2 + 1];
                tile_data_upper >>= bottom_x;
                tile_data_lower >>= bottom_x;
                self.video.row[x as usize] =
                    (p | (((tile_data_upper & 1) as u32) << 1) | (tile_data_lower & 1) as u32) as u16;
                x += 1;
            }
            start_x = start_x2;
        }
        let mut x = start_x;
        while x < end_x {
            let mut local_data = data_off;
            let mut local_y = bottom_y;
            let top_x = (((x + sx) >> 3) & 0x1F) as usize;
            let bg_tile = if lcdc_tile_data(self.video.lcdc) {
                self.video.vram[map_off + top_x + top_y] as i32
            } else {
                self.video.vram[map_off + top_x + top_y] as i8 as i32
            };
            let mut p: u32 = if highlight { PAL_HIGHLIGHT_BG } else { PAL_BG };
            if self.model.is_cgb() {
                let attrs = self.video.vram[map_off + GB_SIZE_VRAM_BANK0 + top_x + top_y];
                p |= ((attrs & OBJ_ATTR_CGB_PALETTE_MASK) as u32) * 4;
                if attrs & OBJ_ATTR_PRIORITY != 0 && lcdc_bg_enable(self.video.lcdc) {
                    p |= OBJ_PRIORITY;
                }
                if attrs & OBJ_ATTR_BANK != 0 {
                    local_data += GB_SIZE_VRAM_BANK0;
                }
                if attrs & OBJ_ATTR_YFLIP != 0 {
                    local_y = 7 - bottom_y;
                }
                if attrs & OBJ_ATTR_XFLIP != 0 {
                    let tl = self.video.vram[local_data + (bg_tile as usize * 8 + local_y) * 2] as u32;
                    let tu = self.video.vram[local_data + (bg_tile as usize * 8 + local_y) * 2 + 1] as u32;
                    self.video.row[x as usize] = (p | ((tu & 1) << 1) | (tl & 1)) as u16;
                    self.video.row[x as usize + 1] = (p | (tu & 2) | ((tl & 2) >> 1)) as u16;
                    self.video.row[x as usize + 2] = (p | ((tu & 4) >> 1) | ((tl & 4) >> 2)) as u16;
                    self.video.row[x as usize + 3] = (p | ((tu & 8) >> 2) | ((tl & 8) >> 3)) as u16;
                    self.video.row[x as usize + 4] = (p | ((tu & 16) >> 3) | ((tl & 16) >> 4)) as u16;
                    self.video.row[x as usize + 5] = (p | ((tu & 32) >> 4) | ((tl & 32) >> 5)) as u16;
                    self.video.row[x as usize + 6] = (p | ((tu & 64) >> 5) | ((tl & 64) >> 6)) as u16;
                    self.video.row[x as usize + 7] = (p | ((tu & 128) >> 6) | ((tl & 128) >> 7)) as u16;
                    x += 8;
                    continue;
                }
            }
            let tl = self.video.vram[local_data + (bg_tile as usize * 8 + local_y) * 2] as u32;
            let tu = self.video.vram[local_data + (bg_tile as usize * 8 + local_y) * 2 + 1] as u32;
            self.video.row[x as usize + 7] = (p | ((tu & 1) << 1) | (tl & 1)) as u16;
            self.video.row[x as usize + 6] = (p | (tu & 2) | ((tl & 2) >> 1)) as u16;
            self.video.row[x as usize + 5] = (p | ((tu & 4) >> 1) | ((tl & 4) >> 2)) as u16;
            self.video.row[x as usize + 4] = (p | ((tu & 8) >> 2) | ((tl & 8) >> 3)) as u16;
            self.video.row[x as usize + 3] = (p | ((tu & 16) >> 3) | ((tl & 16) >> 4)) as u16;
            self.video.row[x as usize + 2] = (p | ((tu & 32) >> 4) | ((tl & 32) >> 5)) as u16;
            self.video.row[x as usize + 1] = (p | ((tu & 64) >> 5) | ((tl & 64) >> 6)) as u16;
            self.video.row[x as usize] = (p | ((tu & 128) >> 6) | ((tl & 128) >> 7)) as u16;
            x += 8;
        }
    }

    /// GBVideoSoftwareRendererDrawObj
    fn draw_obj(&mut self, obj: RendererSprite, start_x: i32, end_x: i32, y: i32) {
        let mut start_x = start_x;
        let mut end_x = end_x;
        let obj_x = obj.obj.x as i32 + self.video.obj_offset_x as i32;
        let ix = obj_x - 8;
        if end_x < ix || start_x >= ix + 8 {
            return;
        }
        if obj_x < end_x {
            end_x = obj_x;
        }
        if obj_x - 8 > start_x {
            start_x = obj_x - 8;
        }
        if start_x < 0 {
            start_x = 0;
        }
        let mut data_off = 0usize;
        let mut tile_offset = 0i32;
        let obj_y = obj.obj.y as i32 + self.video.obj_offset_y as i32;
        let bottom_y: usize;
        if obj.obj.attr & OBJ_ATTR_YFLIP != 0 {
            bottom_y = (7 - ((y - obj_y - 16) & 7)) as usize;
            if lcdc_obj_size(self.video.lcdc) && y - obj_y < -8 {
                tile_offset += 1;
            }
        } else {
            bottom_y = ((y - obj_y - 16) & 7) as usize;
            if lcdc_obj_size(self.video.lcdc) && y - obj_y >= -8 {
                tile_offset += 1;
            }
        }
        if lcdc_obj_size(self.video.lcdc) && obj.obj.tile & 1 != 0 {
            tile_offset -= 1;
        }
        let mut mask: u32 = if obj.obj.attr & OBJ_ATTR_PRIORITY != 0 { 0x63 } else { 0x60 };
        let mut mask2: u32 = if obj.obj.attr & OBJ_ATTR_PRIORITY != 0 {
            0
        } else {
            OBJ_PRIORITY | 3
        };
        let mut p: u32 = if self.video.highlight_obj[obj.index as usize] {
            PAL_HIGHLIGHT_OBJ
        } else {
            PAL_OBJ
        };
        if self.model.is_cgb() {
            p |= ((obj.obj.attr & OBJ_ATTR_CGB_PALETTE_MASK) as u32) * 4;
            if obj.obj.attr & OBJ_ATTR_BANK != 0 {
                data_off += GB_SIZE_VRAM_BANK0;
            }
            if !lcdc_bg_enable(self.video.lcdc) {
                mask = 0x60;
                mask2 = OBJ_PRIORITY | 3;
            }
        } else {
            p |= (((obj.obj.attr & OBJ_ATTR_PALETTE) >> 4) as u32 + 8) * 4;
        }
        let obj_tile = obj.obj.tile as i32 + tile_offset;
        let mut x = start_x;
        if (x - obj_x) & 7 != 0 {
            while x < end_x {
                let bottom_x = if obj.obj.attr & OBJ_ATTR_XFLIP != 0 {
                    (x - obj_x) & 7
                } else {
                    7 - ((x - obj_x) & 7)
                };
                let mut tile_data_lower = self.video.vram[data_off + (obj_tile as usize * 8 + bottom_y) * 2];
                let mut tile_data_upper =
                    self.video.vram[data_off + (obj_tile as usize * 8 + bottom_y) * 2 + 1];
                tile_data_upper >>= bottom_x;
                tile_data_lower >>= bottom_x;
                let current = self.video.row[x as usize] as u32;
                if (tile_data_upper | tile_data_lower) & 1 != 0
                    && current & mask == 0
                    && current & mask2 <= OBJ_PRIORITY
                {
                    self.video.row[x as usize] =
                        (p | (((tile_data_upper & 1) as u32) << 1) | (tile_data_lower & 1) as u32)
                            as u16;
                }
                x += 1;
            }
        } else if obj.obj.attr & OBJ_ATTR_XFLIP != 0 {
            let tile_data_lower = self.video.vram[data_off + (obj_tile as usize * 8 + bottom_y) * 2] as u32;
            let tile_data_upper = self.video.vram[data_off + (obj_tile as usize * 8 + bottom_y) * 2 + 1] as u32;
            let row_base = x as usize;
            {
                let current = self.video.row[row_base] as u32;
                if (tile_data_upper | tile_data_lower) & 1 != 0 && current & mask == 0 && current & mask2 <= OBJ_PRIORITY {
                    self.video.row[row_base] = (p | ((tile_data_upper & 1) << 1) | (tile_data_lower & 1)) as u16;
                }
                let current = self.video.row[row_base + 1] as u32;
                if (tile_data_upper | tile_data_lower) & 2 != 0 && current & mask == 0 && current & mask2 <= OBJ_PRIORITY {
                    self.video.row[row_base + 1] = (p | (tile_data_upper & 2) | ((tile_data_lower & 2) >> 1)) as u16;
                }
                let current = self.video.row[row_base + 2] as u32;
                if (tile_data_upper | tile_data_lower) & 4 != 0 && current & mask == 0 && current & mask2 <= OBJ_PRIORITY {
                    self.video.row[row_base + 2] = (p | ((tile_data_upper & 4) >> 1) | ((tile_data_lower & 4) >> 2)) as u16;
                }
                let current = self.video.row[row_base + 3] as u32;
                if (tile_data_upper | tile_data_lower) & 8 != 0 && current & mask == 0 && current & mask2 <= OBJ_PRIORITY {
                    self.video.row[row_base + 3] = (p | ((tile_data_upper & 8) >> 2) | ((tile_data_lower & 8) >> 3)) as u16;
                }
                let current = self.video.row[row_base + 4] as u32;
                if (tile_data_upper | tile_data_lower) & 16 != 0 && current & mask == 0 && current & mask2 <= OBJ_PRIORITY {
                    self.video.row[row_base + 4] = (p | ((tile_data_upper & 16) >> 3) | ((tile_data_lower & 16) >> 4)) as u16;
                }
                let current = self.video.row[row_base + 5] as u32;
                if (tile_data_upper | tile_data_lower) & 32 != 0 && current & mask == 0 && current & mask2 <= OBJ_PRIORITY {
                    self.video.row[row_base + 5] = (p | ((tile_data_upper & 32) >> 4) | ((tile_data_lower & 32) >> 5)) as u16;
                }
                let current = self.video.row[row_base + 6] as u32;
                if (tile_data_upper | tile_data_lower) & 64 != 0 && current & mask == 0 && current & mask2 <= OBJ_PRIORITY {
                    self.video.row[row_base + 6] = (p | ((tile_data_upper & 64) >> 5) | ((tile_data_lower & 64) >> 6)) as u16;
                }
                let current = self.video.row[row_base + 7] as u32;
                if (tile_data_upper | tile_data_lower) & 128 != 0 && current & mask == 0 && current & mask2 <= OBJ_PRIORITY {
                    self.video.row[row_base + 7] = (p | ((tile_data_upper & 128) >> 6) | ((tile_data_lower & 128) >> 7)) as u16;
                }
            }
        } else {
            let tile_data_lower = self.video.vram[data_off + (obj_tile as usize * 8 + bottom_y) * 2] as u32;
            let tile_data_upper = self.video.vram[data_off + (obj_tile as usize * 8 + bottom_y) * 2 + 1] as u32;
            let row_base = x as usize;
            {
                let current = self.video.row[row_base + 7] as u32;
                if (tile_data_upper | tile_data_lower) & 1 != 0 && current & mask == 0 && current & mask2 <= OBJ_PRIORITY {
                    self.video.row[row_base + 7] = (p | ((tile_data_upper & 1) << 1) | (tile_data_lower & 1)) as u16;
                }
                let current = self.video.row[row_base + 6] as u32;
                if (tile_data_upper | tile_data_lower) & 2 != 0 && current & mask == 0 && current & mask2 <= OBJ_PRIORITY {
                    self.video.row[row_base + 6] = (p | (tile_data_upper & 2) | ((tile_data_lower & 2) >> 1)) as u16;
                }
                let current = self.video.row[row_base + 5] as u32;
                if (tile_data_upper | tile_data_lower) & 4 != 0 && current & mask == 0 && current & mask2 <= OBJ_PRIORITY {
                    self.video.row[row_base + 5] = (p | ((tile_data_upper & 4) >> 1) | ((tile_data_lower & 4) >> 2)) as u16;
                }
                let current = self.video.row[row_base + 4] as u32;
                if (tile_data_upper | tile_data_lower) & 8 != 0 && current & mask == 0 && current & mask2 <= OBJ_PRIORITY {
                    self.video.row[row_base + 4] = (p | ((tile_data_upper & 8) >> 2) | ((tile_data_lower & 8) >> 3)) as u16;
                }
                let current = self.video.row[row_base + 3] as u32;
                if (tile_data_upper | tile_data_lower) & 16 != 0 && current & mask == 0 && current & mask2 <= OBJ_PRIORITY {
                    self.video.row[row_base + 3] = (p | ((tile_data_upper & 16) >> 3) | ((tile_data_lower & 16) >> 4)) as u16;
                }
                let current = self.video.row[row_base + 2] as u32;
                if (tile_data_upper | tile_data_lower) & 32 != 0 && current & mask == 0 && current & mask2 <= OBJ_PRIORITY {
                    self.video.row[row_base + 2] = (p | ((tile_data_upper & 32) >> 4) | ((tile_data_lower & 32) >> 5)) as u16;
                }
                let current = self.video.row[row_base + 1] as u32;
                if (tile_data_upper | tile_data_lower) & 64 != 0 && current & mask == 0 && current & mask2 <= OBJ_PRIORITY {
                    self.video.row[row_base + 1] = (p | ((tile_data_upper & 64) >> 5) | ((tile_data_lower & 64) >> 6)) as u16;
                }
                let current = self.video.row[row_base] as u32;
                if (tile_data_upper | tile_data_lower) & 128 != 0 && current & mask == 0 && current & mask2 <= OBJ_PRIORITY {
                    self.video.row[row_base] = (p | ((tile_data_upper & 128) >> 6) | ((tile_data_lower & 128) >> 7)) as u16;
                }
            }
        }
    }

    /// GBVideoSoftwareRendererDrawRange
    pub fn renderer_draw_range(&mut self, start_x: i32, end_x: i32, y: i32) {
        self.video.last_y = y;
        self.video.last_x = end_x;
        if start_x >= end_x {
            return;
        }
        let mut map_off = GB_BASE_MAP;
        if lcdc_tile_map(self.video.lcdc) {
            map_off += GB_SIZE_MAP;
        }
        if self.video.disable_bg {
            for i in start_x..end_x {
                self.video.row[i as usize] = 0;
            }
        }
        if lcdc_bg_enable(self.video.lcdc) || self.model.is_cgb() {
            let wy = self.video.wy as i32 + self.video.current_wy as i8 as i32;
            let wx = self.video.wx as i32 + self.video.current_wx as i8 as i32 - 7;
            if lcdc_window(self.video.lcdc) && wy == y && wx <= end_x {
                self.video.has_window = true;
            }
            if lcdc_window(self.video.lcdc) && self.video.has_window && wx <= end_x && !self.video.disable_win
            {
                if wx > 0 && !self.video.disable_bg {
                    self.draw_background(
                        map_off,
                        start_x,
                        wx,
                        self.video.scx as i32 - self.video.offset_scx as i32,
                        self.video.scy as i32 + y - self.video.offset_scy as i32,
                        self.video.highlight_bg,
                    );
                }

                let mut wmap_off = GB_BASE_MAP;
                if lcdc_window_tile_map(self.video.lcdc) {
                    wmap_off += GB_SIZE_MAP;
                }
                self.draw_background(
                    wmap_off,
                    wx,
                    end_x,
                    -wx - self.video.offset_wx as i32,
                    y - wy - self.video.offset_wy as i32,
                    self.video.highlight_win,
                );
            } else if !self.video.disable_bg {
                self.draw_background(
                    map_off,
                    start_x,
                    end_x,
                    self.video.scx as i32 - self.video.offset_scx as i32,
                    self.video.scy as i32 + y - self.video.offset_scy as i32,
                    self.video.highlight_bg,
                );
            }
        } else if !self.video.disable_bg {
            for i in start_x..end_x {
                self.video.row[i as usize] = 0;
            }
        }

        if start_x == 0 {
            self.renderer_clean_oam(y);
        }
        if lcdc_obj_enable(self.video.lcdc) && !self.video.disable_obj {
            for i in 0..self.video.renderer_obj_max {
                let sprite = self.video.obj[i as usize];
                self.draw_obj(sprite, start_x, end_x, y);
            }
        }

        let highlight_amount = ((self.video.highlight_amount as u32 + 6) >> 4) as u8;
        if self.video.last_highlight_amount != highlight_amount {
            self.video.last_highlight_amount = highlight_amount;
            for i in 0..PAL_SGB_BORDER as usize {
                if i >= PAL_OBJ as usize && (i & 3) == 0 {
                    continue;
                }
                self.video.renderer_palette[i + PAL_HIGHLIGHT as usize] = m_color_mix_5bit(
                    0x10 - highlight_amount as i32,
                    self.video.renderer_palette[i],
                    highlight_amount as i32,
                    self.video.highlight_color,
                );
            }
        }

        let mut sgb_offset = 0usize;
        if self.model_has_sgb() && self.video.renderer_sgb_borders {
            sgb_offset = self.video.output_stride * 40 + 48;
        }
        let stride = self.video.output_stride;
        let row_base = stride * y as usize + sgb_offset;
        let mut x = start_x;
        let mut p: u32 = 0;
        match self.video.sgb_render_mode {
            0 => {
                if self.model_has_sgb() && !self.model.is_cgb() {
                    p = self.video.sgb_attributes.as_ref().map(|a| {
                        a[((start_x >> 5) + 5 * (y >> 3)) as usize]
                    }).unwrap_or(0) as u32;
                    p >>= 6 - (x / 4 & 6) as u32;
                    p &= 3;
                    p <<= 2;
                }
                while x < (start_x + 7) & !7 && x < end_x {
                    self.video.output[row_base + x as usize] = m_color_to_rgba32(
                        self.video.renderer_palette
                            [(p | self.video.lookup[(self.video.row[x as usize] as u32 & OBJ_PRIO_MASK) as usize] as u32) as usize],
                    );
                    x += 1;
                }
                while x + 7 < end_x & !7 {
                    if self.model_has_sgb() && !self.model.is_cgb() {
                        p = self.video.sgb_attributes.as_ref().map(|a| {
                            a[((x >> 5) + 5 * (y >> 3)) as usize]
                        }).unwrap_or(0) as u32;
                        p >>= 6 - (x / 4 & 6) as u32;
                        p &= 3;
                        p <<= 2;
                    }
                    for i in 0..8 {
                        self.video.output[row_base + x as usize + i] = m_color_to_rgba32(
                            self.video.renderer_palette[(p
                                | self.video.lookup[(self.video.row[x as usize + i] as u32
                                    & OBJ_PRIO_MASK)
                                    as usize] as u32)
                                as usize],
                        );
                    }
                    x += 8;
                }
                if self.model_has_sgb() && !self.model.is_cgb() {
                    p = self.video.sgb_attributes.as_ref().map(|a| {
                        a[((x >> 5) + 5 * (y >> 3)) as usize]
                    }).unwrap_or(0) as u32;
                    p >>= 6 - (x / 4 & 6) as u32;
                    p &= 3;
                    p <<= 2;
                }
                while x < end_x {
                    self.video.output[row_base + x as usize] = m_color_to_rgba32(
                        self.video.renderer_palette
                            [(p | self.video.lookup[(self.video.row[x as usize] as u32 & OBJ_PRIO_MASK) as usize] as u32) as usize],
                    );
                    x += 1;
                }
                if self.video.sgb_border_mask[(y >> 3) as usize] != 0 {
                    let border_mask = self.video.sgb_border_mask[(y >> 3) as usize];
                    let local_y = (y & 0x7) as usize;
                    let mut x = start_x;
                    while x < end_x {
                        if border_mask & (1 << (x >> 3)) == 0 {
                            x += 8;
                            continue;
                        }
                        let off = ((x >> 2) + 12 + (y & !7) * 8 + 320) as usize;
                        let map_ram = self.video.sgb_map_ram.as_ref().unwrap();
                        let map_data = map_ram[off] as u16 | (map_ram[off + 1] as u16) << 8;
                        if map_data & SGB_BG_ATTR_TILE_MASK >= 0x100 {
                            x += 8;
                            continue;
                        }

                        let mut y_flip = 0usize;
                        if map_data & SGB_BG_ATTR_YFLIP != 0 {
                            y_flip = 7;
                        }
                        let tile_base =
                            (((map_data & SGB_BG_ATTR_TILE_MASK) as usize) * 16 + (local_y ^ y_flip)) * 2;
                        let chars = self.video.sgb_char_ram.as_ref().unwrap();
                        let tile_data = [
                            chars[tile_base],
                            chars[tile_base + 1],
                            chars[tile_base + 0x10],
                            chars[tile_base + 0x11],
                        ];
                        let palette_base = ((map_data & SGB_BG_ATTR_PALETTE_MASK) >> 10) as usize * 0x10;

                        let mut flip = 0usize;
                        if map_data & SGB_BG_ATTR_XFLIP != 0 {
                            flip = 7;
                        }
                        for i in (0..8usize).rev() {
                            let color_selector = ((tile_data[0] >> i) & 1) as usize
                                | ((tile_data[1] >> i) & 1) as usize
                                | ((((tile_data[2] >> i) & 1) as usize) << 2)
                                | ((((tile_data[3] >> i) & 1) as usize) << 3);
                            if color_selector != 0 {
                                self.video.output[row_base + ((x as usize + 7 - i) ^ flip)] =
                                    m_color_to_rgba32(self.video.renderer_palette
                                        [palette_base | color_selector]);
                            }
                        }
                        x += 8;
                    }
                }
            }
            1 => {}
            2 => {
                while x < (start_x + 7) & !7 && x < end_x {
                    self.video.output[row_base + x as usize] = 0;
                    x += 1;
                }
                while x + 7 < end_x & !7 {
                    for i in 0..8 {
                        self.video.output[row_base + x as usize + i] = 0;
                    }
                    x += 8;
                }
                while x < end_x {
                    self.video.output[row_base + x as usize] = 0;
                    x += 1;
                }
            }
            3 => {
                let pal0 = m_color_to_rgba32(self.video.renderer_palette[0]);
                while x < (start_x + 7) & !7 && x < end_x {
                    self.video.output[row_base + x as usize] = pal0;
                    x += 1;
                }
                while x + 7 < end_x & !7 {
                    for i in 0..8 {
                        self.video.output[row_base + x as usize + i] = pal0;
                    }
                    x += 8;
                }
                while x < end_x {
                    self.video.output[row_base + x as usize] = pal0;
                    x += 1;
                }
            }
            _ => {}
        }
    }

    /// GBVideoSoftwareRendererFinishScanline
    pub fn renderer_finish_scanline(&mut self, y: i32) {
        self.video.last_x = 0;
        self.video.current_wx = 0;

        if self.video.sgb_transfer == 1 {
            let offset = 2 * ((y & 7) + (y >> 3) * GB_VIDEO_HORIZONTAL_PIXELS) as usize;
            if offset >= 0x1000 {
                return;
            }
            let header = self.video.renderer_sgb_command_header >> 3;
            // choose target buffer without holding borrows
            match header {
                SGB_PAL_TRN | SGB_CHR_TRN | SGB_PCT_TRN | SGB_ATTR_TRN => {}
                _ => {}
            }
            let target: Option<usize> = match header {
                SGB_PAL_TRN => Some(0),
                SGB_CHR_TRN => Some(1),
                SGB_PCT_TRN => Some(2),
                SGB_ATTR_TRN => Some(3),
                _ => None,
            };
            if let Some(which) = target {
                let mut i = 0usize;
                while i < GB_VIDEO_HORIZONTAL_PIXELS as usize {
                    if offset + (i << 1) + 1 >= 0x1000 {
                        break;
                    }
                    let row = &self.video.row;
                    let mut hi: u8 = 0;
                    let mut lo: u8 = 0;
                    hi |= ((row[i] & 0x2) as u8) << 6;
                    lo |= ((row[i] & 0x1) as u8) << 7;
                    hi |= ((row[i + 1] & 0x2) as u8) << 5;
                    lo |= ((row[i + 1] & 0x1) as u8) << 6;
                    hi |= ((row[i + 2] & 0x2) as u8) << 4;
                    lo |= ((row[i + 2] & 0x1) as u8) << 5;
                    hi |= ((row[i + 3] & 0x2) as u8) << 3;
                    lo |= ((row[i + 3] & 0x1) as u8) << 4;
                    hi |= ((row[i + 4] & 0x2) as u8) << 2;
                    lo |= ((row[i + 4] & 0x1) as u8) << 3;
                    hi |= ((row[i + 5] & 0x2) as u8) << 1;
                    lo |= ((row[i + 5] & 0x1) as u8) << 2;
                    hi |= (row[i + 6] & 0x2) as u8;
                    lo |= ((row[i + 6] & 0x1) as u8) << 1;
                    hi |= ((row[i + 7] & 0x2) as u8) >> 1;
                    lo |= (row[i + 7] & 0x1) as u8;
                    let v = &mut self.video;
                    let (lo_b, hi_b) = (lo, hi);
                    match which {
                        0 => {
                            if let Some(buf) = v.sgb_pal_ram.as_mut() {
                                buf[offset + (i << 1)] = lo_b;
                                buf[offset + (i << 1) + 1] = hi_b;
                            }
                        }
                        1 => {
                            let base = SGB_SIZE_CHAR_RAM / 2
                                * (v.sgb_packet[1] as usize & 1);
                            if let Some(buf) = v.sgb_char_ram.as_mut() {
                                buf[base + offset + (i << 1)] = lo_b;
                                buf[base + offset + (i << 1) + 1] = hi_b;
                            }
                        }
                        2 => {
                            if let Some(buf) = v.sgb_map_ram.as_mut() {
                                buf[offset + (i << 1)] = lo_b;
                                buf[offset + (i << 1) + 1] = hi_b;
                            }
                        }
                        _ => {
                            if let Some(buf) = v.sgb_attribute_files.as_mut() {
                                buf[offset + (i << 1)] = lo_b;
                                buf[offset + (i << 1) + 1] = hi_b;
                            }
                        }
                    }
                    i += 8;
                }
            }
        }
    }

    /// GBVideoSoftwareRendererFinishFrame
    pub fn renderer_finish_frame(&mut self) {
        if !lcdc_enable(self.video.lcdc) {
            self.clear_screen();
        }
        if self.model_has_sgb() {
            match self.video.renderer_sgb_command_header >> 3 {
                SGB_PAL_SET | SGB_ATTR_SET => {
                    if self.video.sgb_packet[1] & 0x40 != 0 {
                        self.video.sgb_render_mode = 0;
                        if self.video.renderer_sgb_borders {
                            self.regenerate_sgb_border();
                        }
                    }
                }
                SGB_PAL_TRN | SGB_CHR_TRN | SGB_PCT_TRN | SGB_ATRC_EN | SGB_MASK_EN => {
                    if self.video.renderer_sgb_borders && self.video.sgb_render_mode == 0 {
                        self.regenerate_sgb_border();
                    }
                    self.video.sgb_transfer += 1;
                    if self.video.sgb_transfer == 5 {
                        self.video.renderer_sgb_command_header = 0;
                    }
                }
                SGB_ATTR_TRN => {
                    self.video.sgb_transfer += 1;
                    if self.video.sgb_transfer == 5 {
                        self.video.renderer_sgb_command_header = 0;
                    }
                }
                _ => {}
            }
        }
        self.video.last_y = GB_VIDEO_VERTICAL_PIXELS;
        self.video.last_x = 0;
        self.video.current_wy = 0;
        self.video.current_wx = 0;
        self.video.has_window = false;
    }

    /// _clearScreen
    pub fn clear_screen(&mut self) {
        if self.model_has_sgb() {
            return;
        }
        let stride = self.video.output_stride;
        let pal0 = m_color_to_rgba32(self.video.renderer_palette[0]);
        for y in 0..GB_VIDEO_VERTICAL_PIXELS {
            let base = stride * y as usize;
            for x in 0..GB_VIDEO_HORIZONTAL_PIXELS {
                self.video.output[base + x as usize] = pal0;
            }
        }
    }

    /// Renderer::enableSGBBorder
    pub fn renderer_enable_sgb_border(&mut self, enable: bool) {
        if !self.model_has_sgb() {
            return;
        }
        if enable == self.video.renderer_sgb_borders {
            return;
        }
        self.video.renderer_sgb_borders = enable;
        if enable && self.video.sgb_render_mode == 0 {
            self.regenerate_sgb_border();
        }
        self.video.output_stride = if self.model_has_sgb() && self.video.renderer_sgb_borders {
            SGB_VIDEO_HORIZONTAL_PIXELS as usize
        } else {
            GB_VIDEO_HORIZONTAL_PIXELS as usize
        };
    }

    /// getPixels
    pub fn video_current_framebuffer(&self) -> (&[u32], usize) {
        (&self.video.output, self.video.output_stride)
    }

    pub fn video_framebuffer_size(&self) -> (u32, u32) {
        if self.model_has_sgb() && self.video.renderer_sgb_borders {
            (
                SGB_VIDEO_HORIZONTAL_PIXELS as u32,
                SGB_VIDEO_VERTICAL_PIXELS as u32,
            )
        } else {
            (
                GB_VIDEO_HORIZONTAL_PIXELS as u32,
                GB_VIDEO_VERTICAL_PIXELS as u32,
            )
        }
    }
}
const DEFAULT_BORDER_PALETTE: [u16; 16] = [
    0x0000, 0x7FDE, 0x7FFF, 0x739A, 0x2929, 0x24E7, 0x1CC6, 0x0400, 0x514A, 0x3907, 0x28C5, 0, 0, 0, 0, 0,
];

const DEFAULT_BORDER_TILEMAP: [u8; 1792] = [
    0x01, 0x10, 0x01, 0x10, 0x01, 0x10, 0x01, 0x10, 0x01, 0x10, 0x01, 0x10, 0x01, 0x10, 0x01, 0x10,
    0x12, 0x10, 0x14, 0x10, 0x16, 0x10, 0x17, 0x10, 0x17, 0x10, 0x17, 0x10, 0x17, 0x10, 0x17, 0x10,
    0x17, 0x10, 0x17, 0x10, 0x17, 0x10, 0x17, 0x10, 0x17, 0x10, 0x16, 0x50, 0x14, 0x10, 0x12, 0x50,
    0x01, 0x10, 0x01, 0x10, 0x01, 0x10, 0x01, 0x10, 0x01, 0x10, 0x01, 0x10, 0x01, 0x10, 0x01, 0x10,
    0x02, 0x10, 0x06, 0x10, 0x0A, 0x10, 0x0D, 0x10, 0x0E, 0x10, 0x0F, 0x10, 0x10, 0x10, 0x11, 0x10,
    0x13, 0x10, 0x15, 0x10, 0x15, 0x10, 0x18, 0x10, 0x1B, 0x10, 0x1B, 0x10, 0x1B, 0x10, 0x1B, 0x10,
    0x1B, 0x10, 0x1B, 0x10, 0x1B, 0x10, 0x1B, 0x10, 0x18, 0x50, 0x15, 0x10, 0x15, 0x10, 0x13, 0x10,
    0x11, 0x10, 0x10, 0x50, 0x0F, 0x50, 0x0E, 0x50, 0x0D, 0x50, 0x0A, 0x50, 0x06, 0x50, 0x02, 0x50,
    0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10,
    0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10,
    0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10,
    0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10,
    0x04, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10,
    0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10,
    0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10,
    0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x07, 0x10, 0x04, 0x50,
    0x05, 0x10, 0x08, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10,
    0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10,
    0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10,
    0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x0B, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10, 0x00, 0x10,
    0x00, 0x10, 0x00, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x05, 0x10, 0x09, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10,
    0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10,
    0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10,
    0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x0C, 0x10, 0x05, 0x50,
    0x04, 0x90, 0x07, 0x90, 0x07, 0x90, 0x07, 0x90, 0x07, 0x90, 0x07, 0x90, 0x07, 0x90, 0x07, 0x90,
    0x07, 0x90, 0x07, 0x90, 0x07, 0x90, 0x07, 0x90, 0x07, 0x90, 0x1F, 0x10, 0x23, 0x10, 0x23, 0x10,
    0x23, 0x10, 0x23, 0x10, 0x23, 0x10, 0x23, 0x10, 0x34, 0x10, 0x07, 0x90, 0x07, 0x90, 0x07, 0x90,
    0x07, 0x90, 0x07, 0x90, 0x07, 0x90, 0x07, 0x90, 0x07, 0x90, 0x07, 0x90, 0x07, 0x90, 0x04, 0xD0,
    0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10,
    0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x19, 0x10, 0x1C, 0x10, 0x20, 0x10, 0x24, 0x10, 0x26, 0x10,
    0x29, 0x10, 0x2B, 0x10, 0x2E, 0x10, 0x31, 0x10, 0x35, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10,
    0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10,
    0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10,
    0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x1A, 0x10, 0x1D, 0x10, 0x21, 0x10, 0x25, 0x10, 0x27, 0x10,
    0x2A, 0x10, 0x2C, 0x10, 0x2F, 0x10, 0x32, 0x10, 0x1A, 0x50, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10,
    0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10,
    0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10,
    0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x19, 0x90, 0x1E, 0x10, 0x22, 0x10, 0x1C, 0x90, 0x28, 0x10,
    0x1C, 0x90, 0x2D, 0x10, 0x30, 0x10, 0x33, 0x10, 0x19, 0xD0, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10,
    0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10, 0x03, 0x10,
];

const DEFAULT_BORDER_CHARDATA: [u8; 1728] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0x00, 0x00, 0x00, 0x00, 0x03, 0xFC, 0x00, 0x00, 0xF0, 0x0F, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0x00, 0xFF, 0x00, 0xFF, 0x03, 0xFC, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x01, 0xFF, 0x03, 0xFF,
    0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0x03, 0xFF, 0x03, 0xFF, 0x03, 0xFF, 0x03, 0xFF, 0x03, 0xFF, 0x03, 0xFF, 0x03, 0xFF, 0x03, 0xFF,
    0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0x00, 0x00, 0x3F, 0x00, 0xF0, 0x00, 0x0F, 0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0x00, 0xFF, 0x00, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xF0, 0xFF, 0xF0, 0xFF,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xF0, 0xFF, 0xF0, 0xFF, 0xF0, 0xFF, 0xF0, 0xFF, 0xF0, 0xFF, 0xF0, 0xFF, 0xF0, 0xFF, 0xF0, 0xFF,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0xC0, 0x3F, 0x00, 0x00, 0xFF, 0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0x00, 0xFF, 0x00, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0xFF, 0x00, 0x00, 0xE0, 0x1F, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0x00, 0xFF, 0x7F, 0x80, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0x1F, 0x00, 0x7E, 0x80, 0x01, 0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0x00, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0xFC, 0x03, 0x00, 0x00, 0xFF, 0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0x00, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0x00, 0xFF, 0x00, 0x00, 0xF8, 0x07, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0x07, 0xF8, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0x00, 0xFF, 0x00, 0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00,
    0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0xFF, 0x00, 0x00, 0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0x00,
    0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0x00, 0x00, 0xFF, 0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x3F,
    0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF,
    0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0x00, 0x00, 0xC0, 0x3F, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x07, 0xFF, 0x0F, 0xFF, 0x0F, 0xFF, 0x0F, 0xFE,
    0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFE, 0x00,
    0x0F, 0xFE, 0x0F, 0xFE, 0x0F, 0xFE, 0x0F, 0xFE, 0x0F, 0xFE, 0x0F, 0xFE, 0x0F, 0xFE, 0x0F, 0xFE,
    0xFE, 0x00, 0xFE, 0x00, 0xFE, 0x00, 0xFE, 0x00, 0xFE, 0x00, 0xFE, 0x00, 0xFE, 0x00, 0xFE, 0x00,
    0x00, 0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00,
    0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0x00, 0x00,
    0xFF, 0x00, 0xFF, 0x19, 0xFF, 0x19, 0xFF, 0x19, 0xFF, 0x19, 0xFF, 0x19, 0xFF, 0x19, 0xFF, 0x19,
    0x00, 0x00, 0x19, 0x00, 0x19, 0x00, 0x19, 0x00, 0x19, 0x00, 0x19, 0x00, 0x19, 0x00, 0x19, 0x00,
    0xFF, 0x19, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0x19, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x0F, 0xFF,
    0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0x1F, 0xFF, 0x1F, 0xFF, 0x1F, 0xFC, 0x1F, 0xFC, 0xFF, 0xFC, 0xFF, 0xFC, 0xFF, 0xFC, 0xFF, 0x1C,
    0xFF, 0x00, 0xFF, 0x00, 0xFC, 0x00, 0xFC, 0x00, 0xFC, 0x00, 0xFC, 0x00, 0xFC, 0x00, 0x1C, 0x00,
    0xFF, 0x1C, 0xFF, 0x9C, 0xFF, 0x9C, 0xFF, 0x9C, 0xFF, 0x9C, 0xFF, 0x9C, 0xFF, 0x9C, 0xFF, 0x9C,
    0x1C, 0x00, 0x9C, 0x00, 0x9C, 0x00, 0x9C, 0x00, 0x9C, 0x00, 0x9C, 0x00, 0x9C, 0x00, 0x9C, 0x00,
    0xFF, 0x9C, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0x9C, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0xFF, 0xFF,
    0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x0F, 0xFF, 0x0F, 0xFF, 0x0F,
    0xFF, 0x00, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0F, 0x00, 0x0F, 0x00, 0x0F, 0x00,
    0xFF, 0x0E, 0xFF, 0x0E, 0xFF, 0x0E, 0xFF, 0x0F, 0xFF, 0x0F, 0xFF, 0x0F, 0xFF, 0x00, 0xFF, 0x00,
    0x0E, 0x00, 0x0E, 0x00, 0x0E, 0x00, 0x0F, 0x00, 0x0F, 0x00, 0x0F, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x07, 0xFF, 0x07, 0xFF, 0x07, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0x00, 0xFF, 0x00, 0x07, 0x00, 0x07, 0x00, 0x07, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0xFF, 0x07, 0xFF, 0x07, 0xFF, 0x07, 0xFF, 0xC7, 0xFF, 0xC7, 0xFF, 0xC7, 0xFF, 0x07, 0xFF, 0x07,
    0x07, 0x00, 0x07, 0x00, 0x07, 0x00, 0xC7, 0x00, 0xC7, 0x00, 0xC7, 0x00, 0x07, 0x00, 0x07, 0x00,
    0xFF, 0x07, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0x07, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x03, 0xFF, 0x03, 0xFF, 0x03,
    0xFF, 0x00, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x03, 0x00, 0x03, 0x00,
    0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x03, 0xFF, 0x03, 0xFF, 0x03, 0xFF, 0x00, 0xFF, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x03, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01, 0xFF, 0x01, 0xFF, 0x01, 0xFF, 0xF1, 0xFF, 0xF1, 0xFF, 0xF1,
    0xFF, 0x00, 0xFF, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0xF1, 0x00, 0xF1, 0x00, 0xF1, 0x00,
    0xFF, 0x03, 0xFF, 0x07, 0xFF, 0x03, 0xFF, 0xF1, 0xFF, 0xF1, 0xFF, 0xF1, 0xFF, 0x01, 0xFF, 0x01,
    0x03, 0x00, 0x07, 0x00, 0x03, 0x00, 0xF1, 0x00, 0xF1, 0x00, 0xF1, 0x00, 0x01, 0x00, 0x01, 0x00,
    0xFF, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0x01, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xC0, 0xFF, 0xC0, 0xFF, 0xC0, 0xFF, 0xC0, 0xFF, 0xC0, 0xFF, 0xC0,
    0xFF, 0x00, 0xFF, 0x00, 0xC0, 0x00, 0xC0, 0x00, 0xC0, 0x00, 0xC0, 0x00, 0xC0, 0x00, 0xC0, 0x00,
    0xFF, 0xC0, 0xFF, 0xC0, 0xFF, 0xC0, 0xFF, 0xC0, 0xFF, 0xC0, 0xFF, 0xC0, 0xFF, 0xC0, 0xFF, 0xC0,
    0xC0, 0x00, 0xC0, 0x00, 0xC0, 0x00, 0xC0, 0x00, 0xC0, 0x00, 0xC0, 0x00, 0xC0, 0x00, 0xC0, 0x00,
    0xFF, 0xC0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0xC0, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0xFC, 0xFF, 0xFC, 0xFF, 0xFC,
    0xFF, 0x00, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFC, 0x00, 0xFC, 0x00, 0xFC, 0x00,
    0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0xFC, 0xFF, 0xFC, 0xFF, 0xFC, 0xFF, 0xFC, 0xFF, 0xFC,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFC, 0x00, 0xFC, 0x00, 0xFC, 0x00, 0xFC, 0x00, 0xFC, 0x00,
    0xFF, 0xFC, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
    0xFC, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0xE0, 0xFF,
    0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
    0xF0, 0xFF, 0xF0, 0xFF, 0xF0, 0x7F, 0xF0, 0x7F, 0xF0, 0x7F, 0xF0, 0x7F, 0xF0, 0x7F, 0xF0, 0x7F,
    0xFF, 0x00, 0xFF, 0x00, 0x7F, 0x00, 0x7F, 0x00, 0x7F, 0x00, 0x7F, 0x00, 0x7F, 0x00, 0x7F, 0x00,
];
