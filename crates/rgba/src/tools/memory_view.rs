// "View memory..." — mirrors mGBA Qt's MemoryView.cpp / MemoryModel.cpp:
// region selector (the core's memory block list), a 16-bytes-per-row hex
// grid with an ASCII column, 8/16/32-bit grouping, goto-address, segment
// (bank) selection for banked regions, and a selection inspector that shows
// the value as signed/unsigned and lets you edit it.
//
// Reads go through `DebugConsole::dbg_raw_read` (GBAView8/16/32, GBView8),
// which have no side effects; writes through `dbg_raw_write`
// (GBAPatch8/16/32, GBPatch8), exactly like MemoryModel's rawRead/rawWrite.

use eframe::egui;
use rgba_debugger::access_logger::core_memory_flags as mf;
use rgba_debugger::debugger::DebugConsole;

use super::{no_game, ToolCtx, ToolWindow};
use crate::emu::Console;

/// One selectable region (mCoreMemoryBlock subset).
#[derive(Clone, Debug)]
pub struct Region {
    pub name: String,
    pub start: u32,
    pub end: u32,
    pub flags: u32,
    /// Highest valid segment (0 = unsegmented).
    pub max_segments: u32,
}

pub fn dbg(c: &mut Console) -> &mut dyn DebugConsole {
    match c {
        Console::Gba(g) => g.as_mut(),
        Console::Gb(g) => g.as_mut(),
    }
}

/// Friendly names like the Qt region combo box (core.c `longName`s).
pub fn long_name(internal: &str, gba: bool) -> &'static str {
    match (internal, gba) {
        ("mem", _) => "All",
        ("bios", true) => "BIOS (16kiB)",
        ("wram", true) => "EWRAM (256kiB)",
        ("iwram", true) => "IWRAM (32kiB)",
        ("io", true) => "Memory-mapped I/O",
        ("palette", true) => "Palette RAM (1kiB)",
        ("vram", true) => "Video RAM (96kiB)",
        ("oam", true) => "OBJ Attribute Memory (1kiB)",
        ("cart0", true) => "Game Pak (32MiB)",
        ("cart1", true) => "Game Pak (Waitstate 1)",
        ("cart2", true) => "Game Pak (Waitstate 2)",
        ("sram", true) => "Static RAM / Flash (64kiB)",
        ("cart0", false) => "Game Pak (ROM bank)",
        ("vram", false) => "Video RAM",
        ("sram", false) => "External RAM",
        ("wram", false) => "Working RAM",
        ("oam", false) => "OBJ Attribute Memory",
        ("io", false) => "Memory-mapped I/O",
        ("hram", false) => "High RAM",
        _ => "",
    }
}

/// The console's memory blocks (mCore::listMemoryBlocks).
pub fn regions(c: &mut Console) -> Vec<Region> {
    let gba = matches!(c, Console::Gba(_));
    let mut out: Vec<Region> = dbg(c)
        .dbg_list_memory_blocks_full()
        .into_iter()
        .map(|b| {
            let ln = long_name(b.internal_name, gba);
            Region {
                name: if ln.is_empty() { b.internal_name.to_string() } else { ln.to_string() },
                start: b.start,
                end: b.end,
                flags: b.flags,
                max_segments: b.max_segments,
            }
        })
        .collect();
    if gba {
        // The GBA block list omits the savedata block (its shape depends on
        // the chip); GBAView8 reads SRAM/Flash space directly.
        out.push(Region {
            name: long_name("sram", true).to_string(),
            start: 0x0E00_0000,
            end: 0x0E01_0000,
            flags: mf::RW | mf::MAPPED,
            max_segments: 0,
        });
    }
    // Put the "All" virtual block last, like the Qt combo puts the mapped
    // regions first.
    out.sort_by_key(|r| r.flags & mf::VIRTUAL != 0);
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Width {
    W8 = 1,
    W16 = 2,
    W32 = 4,
}

pub struct MemoryView {
    regions: Vec<Region>,
    region: usize,
    segment: i32,
    width: Width,
    goto_text: String,
    /// Selected address (aligned to `width`).
    selection: Option<u32>,
    edit_text: String,
    scroll_to: Option<u32>,
    status: String,
}

impl Default for MemoryView {
    fn default() -> Self {
        MemoryView {
            regions: Vec::new(),
            region: 0,
            segment: -1,
            width: Width::W8,
            goto_text: String::new(),
            selection: None,
            edit_text: String::new(),
            scroll_to: None,
            status: String::new(),
        }
    }
}

impl MemoryView {
    fn read(c: &mut Console, addr: u32, seg: i32, width: u32) -> u32 {
        dbg(c).dbg_raw_read(addr, seg, width)
    }

    fn jump(&mut self, addr: u32) {
        // Pick the region containing the address (prefer mapped ones).
        if let Some(i) = self
            .regions
            .iter()
            .position(|r| r.flags & mf::VIRTUAL == 0 && addr >= r.start && addr < r.end)
            .or_else(|| self.regions.iter().position(|r| addr >= r.start && addr < r.end))
        {
            self.region = i;
        }
        let w = self.width as u32;
        self.selection = Some(addr & !(w - 1));
        self.scroll_to = Some(addr);
        self.edit_text.clear();
    }

    fn grid(&mut self, ui: &mut egui::Ui, c: &mut Console) {
        let Some(r) = self.regions.get(self.region).cloned() else { return };
        let rows = (r.end.saturating_sub(r.start) as usize).div_ceil(16);
        let w = self.width as u32;
        let seg = if r.max_segments > 0 { self.segment } else { -1 };
        let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
        let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if let Some(a) = self.scroll_to.take() {
            let row = (a.saturating_sub(r.start) / 16) as f32;
            area = area.vertical_scroll_offset((row * (row_h + ui.spacing().item_spacing.y)).max(0.0));
        }
        let mono = egui::TextStyle::Monospace;
        area.show_rows(ui, row_h, rows, |ui, range| {
            for row in range {
                let base = r.start + (row as u32) * 16;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(egui::RichText::new(format!("{base:08X}")).text_style(mono.clone()).weak());
                    let mut bytes = [0u8; 16];
                    for (i, b) in bytes.iter_mut().enumerate() {
                        *b = Self::read(c, base + i as u32, seg, 1) as u8;
                    }
                    let mut off = 0u32;
                    while off < 16 {
                        let addr = base + off;
                        let mut v = 0u32;
                        for k in 0..w {
                            v |= (bytes[(off + k) as usize] as u32) << (8 * k);
                        }
                        let text = match self.width {
                            Width::W8 => format!("{v:02X}"),
                            Width::W16 => format!("{v:04X}"),
                            Width::W32 => format!("{v:08X}"),
                        };
                        let selected = self.selection == Some(addr);
                        let resp = ui.add(egui::Button::selectable(
                            selected,
                            egui::RichText::new(text).text_style(mono.clone()),
                        ));
                        if resp.clicked() {
                            self.selection = Some(addr);
                            self.edit_text = match self.width {
                                Width::W8 => format!("{v:02X}"),
                                Width::W16 => format!("{v:04X}"),
                                Width::W32 => format!("{v:08X}"),
                            };
                        }
                        off += w;
                    }
                    let ascii: String = bytes
                        .iter()
                        .map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '.' })
                        .collect();
                    ui.label(egui::RichText::new(ascii).text_style(mono.clone()));
                });
            }
        });
    }

    fn inspector(&mut self, ui: &mut egui::Ui, c: &mut Console) {
        let Some(addr) = self.selection else {
            ui.label("Click a value to select it.");
            return;
        };
        let seg = self
            .regions
            .get(self.region)
            .map_or(-1, |r| if r.max_segments > 0 { self.segment } else { -1 });
        let w = self.width as u32;
        let v = Self::read(c, addr, seg, w);
        ui.horizontal(|ui| {
            ui.label(format!("Selected: 0x{addr:08X}"));
            ui.separator();
            ui.label(format!("u8 {}  s8 {}", v as u8, v as u8 as i8));
            if w >= 2 {
                ui.label(format!("u16 {}  s16 {}", v as u16, v as u16 as i16));
            }
            if w >= 4 {
                ui.label(format!("u32 {}  s32 {}", v, v as i32));
            }
        });
        ui.horizontal(|ui| {
            ui.label("Set value (hex):");
            let resp = ui.add(egui::TextEdit::singleline(&mut self.edit_text).desired_width(90.0).font(egui::TextStyle::Monospace));
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Write").clicked() || enter {
                match u32::from_str_radix(self.edit_text.trim().trim_start_matches("0x"), 16) {
                    Ok(nv) => {
                        dbg(c).dbg_raw_write(addr, seg, w, nv);
                        self.status = format!("Wrote 0x{nv:X} to 0x{addr:08X}");
                    }
                    Err(_) => self.status = "Invalid hex value".into(),
                }
            }
        });
    }
}

impl ToolWindow for MemoryView {
    fn title(&self) -> &'static str {
        "Memory"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        egui::Window::new(self.title())
            .open(open)
            .default_size([640.0, 480.0])
            .show(ctx, |ui| {
                let Some(s) = tc.session.as_deref_mut() else {
                    no_game(ui);
                    return;
                };
                let c = &mut s.console;
                if self.regions.is_empty() {
                    self.regions = regions(c);
                }
                ui.horizontal(|ui| {
                    let cur = self.regions.get(self.region).map(|r| r.name.clone()).unwrap_or_default();
                    egui::ComboBox::from_id_salt("memview_region")
                        .selected_text(cur)
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            for (i, r) in self.regions.iter().enumerate() {
                                if ui.selectable_label(self.region == i, &r.name).clicked() {
                                    self.region = i;
                                    self.selection = None;
                                    self.segment = -1;
                                }
                            }
                        });
                    if let Some(r) = self.regions.get(self.region) {
                        if r.max_segments > 0 {
                            ui.label("Segment:");
                            let max = r.max_segments as i32;
                            ui.add(egui::DragValue::new(&mut self.segment).range(-1..=max))
                                .on_hover_text("-1 = currently mapped bank");
                        }
                    }
                    ui.separator();
                    ui.radio_value(&mut self.width, Width::W8, "8-bit");
                    ui.radio_value(&mut self.width, Width::W16, "16-bit");
                    ui.radio_value(&mut self.width, Width::W32, "32-bit");
                    ui.separator();
                    ui.label("Go to:");
                    let resp = ui.add(egui::TextEdit::singleline(&mut self.goto_text).desired_width(90.0).font(egui::TextStyle::Monospace));
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        match u32::from_str_radix(self.goto_text.trim().trim_start_matches("0x"), 16) {
                            Ok(a) => self.jump(a),
                            Err(_) => self.status = "Invalid address".into(),
                        }
                    }
                });
                if let Some(sel) = self.selection {
                    let w = self.width as u32;
                    self.selection = Some(sel & !(w - 1));
                }
                ui.separator();
                egui::Panel::bottom("memview_inspector").show(ui, |ui| {
                    self.inspector(ui, c);
                    if !self.status.is_empty() {
                        ui.label(egui::RichText::new(&self.status).weak());
                    }
                });
                self.grid(ui, c);
            });
    }

    fn on_session_changed(&mut self) {
        *self = MemoryView::default();
    }
}
