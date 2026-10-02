// "View I/O registers..." — mirrors mGBA Qt's IOViewer.cpp: pick a register
// by name (GBA_IO_REGISTER_NAMES / GB_IO_REGISTER_NAMES), see its value in
// hex with one checkbox per bit, plus a named bitfield breakdown for the
// commonly inspected registers (the Qt `registerDescriptions` table). Edits
// are staged and written back with Apply through the console's IO write
// path (GBAIOWrite / GBIOWrite, the Qt busWrite), then re-read.
//
// Reads come from the IO register array (no side effects); on the GBA that
// means TMxCNT_LO shows the last latched counter, not a live one.

use eframe::egui;

use super::{no_game, ToolCtx, ToolWindow};
use crate::emu::Console;

/// One bitfield (IOViewer::RegisterItem).
struct Field {
    name: &'static str,
    start: u32,
    len: u32,
    read_only: bool,
    /// Enumerated values; empty = plain number / checkbox.
    items: &'static [&'static str],
}

const fn bit(name: &'static str, start: u32) -> Field {
    Field { name, start, len: 1, read_only: false, items: &[] }
}
const fn ro_bit(name: &'static str, start: u32) -> Field {
    Field { name, start, len: 1, read_only: true, items: &[] }
}
const fn num(name: &'static str, start: u32, len: u32) -> Field {
    Field { name, start, len, read_only: false, items: &[] }
}
const fn ro_num(name: &'static str, start: u32, len: u32) -> Field {
    Field { name, start, len, read_only: true, items: &[] }
}
const fn en(name: &'static str, start: u32, len: u32, items: &'static [&'static str]) -> Field {
    Field { name, start, len, read_only: false, items }
}

/// A `'static` field table (const-promoted).
macro_rules! fields {
    ($($e:expr),* $(,)?) => {{
        const F: &[Field] = &[$($e),*];
        F
    }};
}

const BGCNT: &[Field] = &[
    num("Priority", 0, 2),
    num("Tile data base (* 16kB)", 2, 2),
    bit("Enable mosaic", 6),
    bit("Enable 256-color", 7),
    num("Tile map base (* 2kB)", 8, 5),
    bit("Overflow wraps", 13),
    num("Background dimensions", 14, 2),
];
const DMACNT_HI: &[Field] = &[
    en("Destination offset", 5, 2, &["Increment", "Decrement", "Fixed", "Increment and reload"]),
    en("Source offset", 7, 2, &["Increment", "Decrement", "Fixed", ""]),
    bit("Repeat", 9),
    bit("32-bit", 10),
    en("Start timing", 12, 2, &["Immediate", "VBlank", "HBlank", "Special"]),
    bit("IRQ", 14),
    bit("Enable", 15),
];
const TMCNT_HI: &[Field] = &[
    en("Scale", 0, 2, &["1", "1/64", "1/256", "1/1024"]),
    bit("Count up", 2),
    bit("IRQ", 6),
    bit("Enable", 7),
];
const IRQ_BITS: &[Field] = &[
    bit("VBlank", 0),
    bit("HBlank", 1),
    bit("VCounter", 2),
    bit("Timer 0", 3),
    bit("Timer 1", 4),
    bit("Timer 2", 5),
    bit("Timer 3", 6),
    bit("SIO", 7),
    bit("DMA 0", 8),
    bit("DMA 1", 9),
    bit("DMA 2", 10),
    bit("DMA 3", 11),
    bit("Keypad", 12),
    bit("Gamepak", 13),
];
const WAIT4: &[&str] = &["4", "3", "2", "8"];

/// IOViewer::registerDescriptions(mPLATFORM_GBA), the subset ported here.
fn gba_fields(offset: u32) -> &'static [Field] {
    match offset {
        0x000 => fields![
            en("Background mode", 0, 3, &[
                "Mode 0: 4 tile layers",
                "Mode 1: 2 tile layers + 1 rotated/scaled tile layer",
                "Mode 2: 2 rotated/scaled tile layers",
                "Mode 3: Full 15-bit bitmap",
                "Mode 4: Full 8-bit bitmap",
                "Mode 5: Small 15-bit bitmap",
                "",
                "",
            ]),
            ro_bit("CGB Mode", 3),
            bit("Frame select", 4),
            bit("Unlocked HBlank", 5),
            bit("Linear OBJ tile mapping", 6),
            bit("Force blank screen", 7),
            bit("Enable background 0", 8),
            bit("Enable background 1", 9),
            bit("Enable background 2", 10),
            bit("Enable background 3", 11),
            bit("Enable OBJ", 12),
            bit("Enable Window 0", 13),
            bit("Enable Window 1", 14),
            bit("Enable OBJ Window", 15),
        ],
        0x004 => fields![
            ro_bit("Currently in VBlank", 0),
            ro_bit("Currently in HBlank", 1),
            ro_bit("Currently in VCounter", 2),
            bit("Enable VBlank IRQ generation", 3),
            bit("Enable HBlank IRQ generation", 4),
            bit("Enable VCounter IRQ generation", 5),
            num("VCounter scanline", 8, 8),
        ],
        0x006 => fields![ro_num("Current scanline", 0, 8)],
        0x008 | 0x00A => &BGCNT[..5],
        0x00C | 0x00E => BGCNT,
        0x010..=0x01E => fields![num("Offset", 0, 9)],
        0x050 => fields![
            bit("BG 0 target 1", 0),
            bit("BG 1 target 1", 1),
            bit("BG 2 target 1", 2),
            bit("BG 3 target 1", 3),
            bit("OBJ target 1", 4),
            bit("Backdrop target 1", 5),
            en("Blend mode", 6, 2, &["Disabled", "Additive blending", "Brighten", "Darken"]),
            bit("BG 0 target 2", 8),
            bit("BG 1 target 2", 9),
            bit("BG 2 target 2", 10),
            bit("BG 3 target 2", 11),
            bit("OBJ target 2", 12),
            bit("Backdrop target 2", 13),
        ],
        0x052 => fields![num("Blend A (target 1)", 0, 5), num("Blend B (target 2)", 8, 5)],
        0x054 => fields![num("Blend Y", 0, 5)],
        0x080 => fields![
            num("PSG volume right", 0, 3),
            num("PSG volume left", 4, 3),
            bit("Enable channel 1 right", 8),
            bit("Enable channel 2 right", 9),
            bit("Enable channel 3 right", 10),
            bit("Enable channel 4 right", 11),
            bit("Enable channel 1 left", 12),
            bit("Enable channel 2 left", 13),
            bit("Enable channel 3 left", 14),
            bit("Enable channel 4 left", 15),
        ],
        0x082 => fields![
            en("PSG master volume", 0, 2, &["25%", "50%", "100%", ""]),
            bit("Loud channel A", 2),
            bit("Loud channel B", 3),
            bit("Enable channel A right", 8),
            bit("Enable channel A left", 9),
            en("Channel A timer", 10, 1, &["0", "1"]),
            bit("Channel A reset", 11),
            bit("Enable channel B right", 12),
            bit("Enable channel B left", 13),
            en("Channel B timer", 14, 1, &["0", "1"]),
            bit("Channel B reset", 15),
        ],
        0x084 => fields![
            ro_bit("Channel 1 playing", 0),
            ro_bit("Channel 2 playing", 1),
            ro_bit("Channel 3 playing", 2),
            ro_bit("Channel 4 playing", 3),
            bit("Enable audio", 7),
        ],
        0x0BA | 0x0C6 | 0x0D2 | 0x0DE => DMACNT_HI,
        0x102 | 0x106 | 0x10A | 0x10E => TMCNT_HI,
        0x130 => fields![
            ro_bit("A", 0),
            ro_bit("B", 1),
            ro_bit("Select", 2),
            ro_bit("Start", 3),
            ro_bit("Right", 4),
            ro_bit("Left", 5),
            ro_bit("Up", 6),
            ro_bit("Down", 7),
            ro_bit("R", 8),
            ro_bit("L", 9),
        ],
        0x132 => fields![
            bit("A", 0),
            bit("B", 1),
            bit("Select", 2),
            bit("Start", 3),
            bit("Right", 4),
            bit("Left", 5),
            bit("Up", 6),
            bit("Down", 7),
            bit("R", 8),
            bit("L", 9),
            bit("IRQ", 14),
            en("Condition", 15, 1, &["Any (OR)", "All (AND)"]),
        ],
        0x200 | 0x202 => IRQ_BITS,
        0x204 => fields![
            en("SRAM wait", 0, 2, WAIT4),
            en("Cart 0 non-sequential", 2, 2, WAIT4),
            en("Cart 0 sequential", 4, 1, &["2", "1"]),
            en("Cart 1 non-sequential", 5, 2, WAIT4),
            en("Cart 1 sequential", 7, 1, &["4", "1"]),
            en("Cart 2 non-sequential", 8, 2, WAIT4),
            en("Cart 2 sequential", 10, 1, &["8", "1"]),
            en("PHI terminal", 11, 2, &["Disable", "4.19MHz", "8.38MHz", "16.78MHz"]),
            bit("Enable prefetch", 14),
            ro_bit("Game Boy Color mode", 15),
        ],
        0x208 => fields![bit("Enable IRQs", 0)],
        _ => fields![],
    }
}

/// The GB IOViewer table, subset (LCDC/STAT/IE/IF/TAC/NR5x).
fn gb_fields(reg: u32) -> &'static [Field] {
    match reg {
        0x40 => fields![
            bit("Background enable/priority", 0),
            bit("Enable sprites", 1),
            en("Double-height sprites", 2, 1, &["8×8", "8×16"]),
            en("Background tile map", 3, 1, &["0x9800", "0x9C00"]),
            en("Background tile data", 4, 1, &["0x8800", "0x8000"]),
            bit("Enable window", 5),
            en("Window tile map", 6, 1, &["0x9800", "0x9C00"]),
            bit("Enable LCD", 7),
        ],
        0x41 => fields![
            en("Mode", 0, 2, &["HBlank", "VBlank", "OAM search", "Transfer"]),
            ro_bit("LYC", 2),
            bit("Enable HBlank IRQ", 3),
            bit("Enable VBlank IRQ", 4),
            bit("Enable OAM IRQ", 5),
            bit("Enable LYC IRQ", 6),
        ],
        0x07 => fields![
            en("Clock", 0, 2, &["4096Hz", "262144Hz", "65536Hz", "16384Hz"]),
            bit("Enable", 2),
        ],
        0x0F | 0xFF => fields![
            bit("VBlank", 0),
            bit("LCD STAT", 1),
            bit("Timer", 2),
            bit("Serial", 3),
            bit("Joypad", 4),
        ],
        0x26 => fields![
            ro_bit("Channel 1 playing", 0),
            ro_bit("Channel 2 playing", 1),
            ro_bit("Channel 3 playing", 2),
            ro_bit("Channel 4 playing", 3),
            bit("Enable audio", 7),
        ],
        _ => fields![],
    }
}

/// (offset, name) for every named register.
fn register_list(c: &Console) -> Vec<(u32, &'static str)> {
    match c {
        Console::Gba(_) => rgba_gba::io::GBA_IO_REGISTER_NAMES
            .iter()
            .enumerate()
            .filter_map(|(i, n)| n.map(|n| ((i as u32) << 1, n)))
            .collect(),
        Console::Gb(_) => rgba_gb::io::GB_IO_REGISTER_NAMES
            .iter()
            .enumerate()
            .filter(|(i, n)| !n.is_empty() && (*i < rgba_gb::memory::GB_SIZE_IO || *i == 0xFF))
            .map(|(i, n)| (i as u32, *n))
            .collect(),
    }
}

fn read_reg(c: &mut Console, off: u32) -> u32 {
    match c {
        Console::Gba(g) => g.memory.io.get((off >> 1) as usize).copied().unwrap_or(0) as u32,
        Console::Gb(g) => g.view8(0xFF00 | off as u16, -1) as u32,
    }
}

fn write_reg(c: &mut Console, off: u32, value: u32) {
    match c {
        Console::Gba(g) => g.io_write(off, value as u16),
        Console::Gb(g) => g.io_write(off as u16, value as u8),
    }
}

#[derive(Default)]
pub struct IoViewer {
    registers: Vec<(u32, &'static str)>,
    selected: usize,
    /// Staged value (None = follow the live register).
    staged: Option<u32>,
    filter: String,
}

impl ToolWindow for IoViewer {
    fn title(&self) -> &'static str {
        "I/O registers"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        egui::Window::new(self.title())
            .open(open)
            .default_size([460.0, 520.0])
            .show(ctx, |ui| {
                let Some(s) = tc.session.as_deref_mut() else {
                    no_game(ui);
                    return;
                };
                let c = &mut s.console;
                let gba = matches!(c, Console::Gba(_));
                if self.registers.is_empty() {
                    self.registers = register_list(c);
                }
                let base = if gba { 0x0400_0000 } else { 0xFF00 };
                let bits = if gba { 16 } else { 8 };

                ui.horizontal(|ui| {
                    let (off, name) = self.registers.get(self.selected).copied().unwrap_or((0, ""));
                    egui::ComboBox::from_id_salt("io_reg")
                        .selected_text(format!("{:08X}: {name}", base + off))
                        .width(260.0)
                        .height(400.0)
                        .show_ui(ui, |ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter"));
                            let f = self.filter.to_ascii_lowercase();
                            for (i, (off, name)) in self.registers.iter().enumerate() {
                                if !f.is_empty() && !name.to_ascii_lowercase().contains(&f) {
                                    continue;
                                }
                                if ui.selectable_label(i == self.selected, format!("{:08X}: {name}", base + off)).clicked() {
                                    self.selected = i;
                                    self.staged = None;
                                }
                            }
                        });
                });
                let Some(&(off, _)) = self.registers.get(self.selected) else { return };
                let live = read_reg(c, off);
                let mut value = self.staged.unwrap_or(live);
                let before = value;

                ui.horizontal(|ui| {
                    ui.label("Value:");
                    ui.monospace(format!("0x{:0w$X}", value, w = bits / 4));
                    if self.staged.is_some() {
                        ui.label(egui::RichText::new(format!("(live 0x{:0w$X})", live, w = bits / 4)).weak());
                    }
                });
                ui.separator();
                // One checkbox per bit, MSB first like the Qt grid.
                egui::Grid::new("io_bits").spacing([2.0, 2.0]).show(ui, |ui| {
                    for b in (0..bits).rev() {
                        ui.label(egui::RichText::new(format!("{b:X}")).monospace().weak());
                    }
                    ui.end_row();
                    for b in (0..bits).rev() {
                        let mut on = value & (1 << b) != 0;
                        if ui.checkbox(&mut on, "").changed() {
                            value ^= 1 << b;
                        }
                    }
                    ui.end_row();
                });
                ui.separator();

                let fields = if gba { gba_fields(off) } else { gb_fields(off) };
                if fields.is_empty() {
                    ui.label(egui::RichText::new("No field descriptions for this register.").weak());
                }
                egui::Grid::new("io_fields").num_columns(2).striped(true).show(ui, |ui| {
                    for f in fields {
                        let mask = ((1u32 << f.len) - 1) << f.start;
                        let mut v = (value & mask) >> f.start;
                        ui.label(f.name);
                        ui.add_enabled_ui(!f.read_only, |ui| {
                            if f.len == 1 && f.items.is_empty() {
                                let mut on = v != 0;
                                if ui.checkbox(&mut on, "").changed() {
                                    v = on as u32;
                                }
                            } else if !f.items.is_empty() {
                                let cur = f.items.get(v as usize).copied().filter(|s| !s.is_empty());
                                egui::ComboBox::from_id_salt(("io_field", f.start))
                                    .selected_text(cur.map(str::to_string).unwrap_or_else(|| v.to_string()))
                                    .show_ui(ui, |ui| {
                                        for (i, item) in f.items.iter().enumerate() {
                                            if !item.is_empty() {
                                                ui.selectable_value(&mut v, i as u32, *item);
                                            }
                                        }
                                    });
                            } else {
                                ui.add(egui::DragValue::new(&mut v).range(0..=(1u32 << f.len) - 1));
                            }
                        });
                        value = (value & !mask) | ((v << f.start) & mask);
                        ui.end_row();
                    }
                });

                if value != before {
                    self.staged = Some(value);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.add_enabled(self.staged.is_some(), egui::Button::new("Apply")).clicked() {
                        write_reg(c, off, value);
                        self.staged = None;
                    }
                    if ui.add_enabled(self.staged.is_some(), egui::Button::new("Reset")).clicked() {
                        self.staged = None;
                    }
                });
            });
    }

    fn on_session_changed(&mut self) {
        self.registers.clear();
        self.selected = 0;
        self.staged = None;
    }
}
