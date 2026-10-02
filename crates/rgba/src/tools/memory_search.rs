// "Search memory..." — mirrors mGBA Qt's MemorySearch.cpp over
// rgba_core::mem_search (mCoreMemorySearch / mCoreMemorySearchRepeat):
// numeric (1/2/4 bytes or guessed width; decimal, hex or "guess" scaling) or
// text searches, the same operator list, New Search / Search Within /
// Refresh, a 10000-result limit, and in-place editing of a result.

use eframe::egui;
use rgba_core::mem_search::{
    core_memory_flags as mf, search, search_repeat, MemSearchOps, SearchBlock, SearchOp,
    SearchParams, SearchResult, SearchType,
};
use rgba_debugger::debugger::DebugConsole;

use super::memory_view::dbg;
use super::{no_game, ToolCtx, ToolWindow};
use crate::emu::Console;

/// MemorySearch::LIMIT
const LIMIT: usize = 10000;

/// `MemSearchOps` over a console: the block table and raw (side-effect
/// free) reads from `DebugConsole`. `memory_block` materializes the block by
/// raw-reading it, standing in for the C's direct `getMemoryBlock` pointer.
pub struct ConsoleSearchOps<'a> {
    console: &'a mut dyn DebugConsole,
    buf: Vec<u8>,
    blocks: Vec<SearchBlock>,
}

impl<'a> ConsoleSearchOps<'a> {
    pub fn new(c: &'a mut Console) -> Self {
        let gba = matches!(c, Console::Gba(_));
        let console = dbg(c);
        let blocks = console
            .dbg_list_memory_blocks_full()
            .into_iter()
            // The virtual "All" block aliases everything; mapped blocks only.
            .filter(|b| b.flags & mf::VIRTUAL == 0)
            // ROM mirrors (cart1/cart2) would only triplicate cart0 hits.
            .filter(|b| !(gba && (b.internal_name == "cart1" || b.internal_name == "cart2")))
            .map(|b| SearchBlock { id: b.id, start: b.start, end: b.end, flags: b.flags })
            .collect();
        ConsoleSearchOps { console, buf: Vec::new(), blocks }
    }
}

impl MemSearchOps for ConsoleSearchOps<'_> {
    fn memory_blocks(&self) -> Vec<SearchBlock> {
        self.blocks.clone()
    }

    fn memory_block(&mut self, id: i32) -> Option<&[u8]> {
        let b = *self.blocks.iter().find(|b| b.id == id)?;
        let len = b.end.saturating_sub(b.start) as usize;
        self.buf.clear();
        self.buf.reserve(len);
        let mut a = b.start;
        while (a as u64) + 4 <= b.end as u64 {
            self.buf.extend_from_slice(&self.console.dbg_raw_read(a, -1, 4).to_le_bytes());
            a += 4;
        }
        while a < b.end {
            self.buf.push(self.console.dbg_raw_read(a, -1, 1) as u8);
            a += 1;
        }
        Some(&self.buf)
    }

    fn raw_read(&mut self, address: u32, segment: i32, width: u32) -> u32 {
        self.console.dbg_raw_read(address, segment, width)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ValueType {
    Numeric,
    Text,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NumWidth {
    Guess = -1,
    B1 = 1,
    B2 = 2,
    B4 = 4,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NumBase {
    Guess,
    Dec,
    Hex,
}

/// The Qt radio buttons (opEqual, opGreater, ...).
#[derive(Clone, Copy, PartialEq, Eq)]
enum UiOp {
    Equal,
    Greater,
    Less,
    Unknown,
    Delta,
    Unchanged,
    Increased,
    Decreased,
}

impl UiOp {
    const ALL: [(UiOp, &'static str); 8] = [
        (UiOp::Equal, "Equal to value"),
        (UiOp::Greater, "Greater than value"),
        (UiOp::Less, "Less than value"),
        (UiOp::Unknown, "Unknown/changed"),
        (UiOp::Delta, "Changed by value"),
        (UiOp::Unchanged, "Unchanged"),
        (UiOp::Increased, "Increased"),
        (UiOp::Decreased, "Decreased"),
    ];
    fn needs_results(self) -> bool {
        matches!(self, UiOp::Delta | UiOp::Unchanged | UiOp::Increased | UiOp::Decreased)
    }
}

pub struct MemorySearch {
    value: String,
    typ: ValueType,
    width: NumWidth,
    base: NumBase,
    op: UiOp,
    search_rom: bool,
    results: Vec<SearchResult>,
    status: String,
    editing: Option<(usize, String)>,
}

impl Default for MemorySearch {
    fn default() -> Self {
        MemorySearch {
            value: String::new(),
            typ: ValueType::Numeric,
            width: NumWidth::B1,
            base: NumBase::Guess,
            op: UiOp::Equal,
            search_rom: false,
            results: Vec::new(),
            status: String::new(),
            editing: None,
        }
    }
}

impl MemorySearch {
    /// MemorySearch::createParams
    fn create_params(&self) -> Option<SearchParams> {
        let mut memory_flags = mf::WRITE;
        if self.search_rom {
            memory_flags |= mf::READ;
        }
        let mut p = SearchParams {
            memory_flags,
            typ: SearchType::Int,
            op: SearchOp::Equal,
            align: -1,
            width: self.width as i32,
            value_int: 0,
            value_str: Vec::new(),
        };
        match self.typ {
            ValueType::Numeric => {
                p.op = match self.op {
                    UiOp::Delta | UiOp::Unchanged => SearchOp::Delta,
                    UiOp::Greater => SearchOp::Greater,
                    UiOp::Less => SearchOp::Less,
                    UiOp::Unknown => SearchOp::Any,
                    UiOp::Increased => SearchOp::DeltaPositive,
                    UiOp::Decreased => SearchOp::DeltaNegative,
                    UiOp::Equal => SearchOp::Equal,
                };
                let needs_value = !matches!(self.op, UiOp::Unknown | UiOp::Unchanged | UiOp::Increased | UiOp::Decreased);
                let text = self.value.trim();
                if self.base == NumBase::Guess {
                    p.typ = SearchType::Guess;
                    p.value_str = if self.op == UiOp::Unchanged || (!needs_value && text.is_empty()) {
                        b"0".to_vec()
                    } else {
                        text.as_bytes().to_vec()
                    };
                    return Some(p);
                }
                let parsed = if !needs_value && text.is_empty() {
                    Some(0)
                } else if self.base == NumBase::Hex {
                    u32::from_str_radix(text.trim_start_matches("0x"), 16).ok()
                } else {
                    text.parse::<u32>().ok()
                };
                let v = parsed?;
                let fits = match p.width {
                    1 => v < 0x100,
                    2 => v < 0x10000,
                    4 => true,
                    _ => false,
                };
                if !fits {
                    return None;
                }
                p.value_int = if self.op == UiOp::Unchanged { 0 } else { v as i32 };
                Some(p)
            }
            ValueType::Text => {
                p.typ = SearchType::String;
                p.value_str = self.value.as_bytes().to_vec();
                p.width = p.value_str.len() as i32;
                (!p.value_str.is_empty()).then_some(p)
            }
        }
    }

    fn current_value(ops: &mut ConsoleSearchOps, r: &SearchResult, hex: bool) -> String {
        match r.typ {
            SearchType::Int => {
                let v = ops.raw_read(r.address, r.segment, r.width.max(1) as u32);
                if hex {
                    format!("0x{:0w$X}", v, w = (r.width.max(1) * 2) as usize)
                } else {
                    v.to_string()
                }
            }
            SearchType::String => {
                let bytes: Vec<u8> = (0..r.width.max(0) as u32)
                    .map(|i| ops.raw_read(r.address + i, r.segment, 1) as u8)
                    .collect();
                String::from_utf8_lossy(&bytes).into_owned()
            }
            SearchType::Guess => {
                let v = ops.raw_read(r.address, r.segment, r.width.max(1) as u32) as u64;
                let div = r.guess_divisor.max(1) as u64;
                let mul = r.guess_multiplier.max(1) as u64;
                if div == 1 && mul == 1 {
                    v.to_string()
                } else {
                    format!("{} ({}×{}/{})", v * div / mul, v, div, mul)
                }
            }
        }
    }

    fn write_result(c: &mut Console, r: &SearchResult, text: &str, hex: bool) -> Result<(), String> {
        let console = dbg(c);
        match r.typ {
            SearchType::String => {
                for (i, b) in text.bytes().take(r.width.max(0) as usize).enumerate() {
                    console.dbg_raw_write(r.address + i as u32, r.segment, 1, b as u32);
                }
                Ok(())
            }
            _ => {
                let t = text.trim();
                let v = if hex || t.starts_with("0x") {
                    u32::from_str_radix(t.trim_start_matches("0x"), 16)
                } else {
                    t.parse::<u32>()
                }
                .map_err(|_| "Invalid number".to_string())?;
                // Guess results: user value × multiplier ÷ divisor = memory.
                let v = if r.typ == SearchType::Guess {
                    (v as u64 * r.guess_multiplier.max(1) as u64 / r.guess_divisor.max(1) as u64) as u32
                } else {
                    v
                };
                console.dbg_raw_write(r.address, r.segment, r.width.max(1) as u32, v);
                Ok(())
            }
        }
    }
}

impl ToolWindow for MemorySearch {
    fn title(&self) -> &'static str {
        "Memory search"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        egui::Window::new(self.title())
            .open(open)
            .default_size([520.0, 480.0])
            .show(ctx, |ui| {
                let Some(s) = tc.session.as_deref_mut() else {
                    no_game(ui);
                    return;
                };
                let c = &mut s.console;
                let have_results = !self.results.is_empty();
                if !have_results && self.op.needs_results() {
                    self.op = UiOp::Equal;
                }

                ui.horizontal(|ui| {
                    ui.label("Value");
                    ui.add(egui::TextEdit::singleline(&mut self.value).desired_width(200.0));
                });
                ui.horizontal(|ui| {
                    ui.label("Type");
                    ui.radio_value(&mut self.typ, ValueType::Numeric, "Numeric");
                    ui.radio_value(&mut self.typ, ValueType::Text, "Text");
                });
                ui.add_enabled_ui(self.typ == ValueType::Numeric, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Width");
                        ui.radio_value(&mut self.width, NumWidth::Guess, "Guess");
                        ui.radio_value(&mut self.width, NumWidth::B1, "1 Byte (8-bit)");
                        ui.radio_value(&mut self.width, NumWidth::B2, "2 Bytes (16-bit)");
                        ui.radio_value(&mut self.width, NumWidth::B4, "4 Bytes (32-bit)");
                    });
                    ui.horizontal(|ui| {
                        ui.label("Number type");
                        ui.radio_value(&mut self.base, NumBase::Guess, "Guess");
                        ui.radio_value(&mut self.base, NumBase::Dec, "Decimal");
                        ui.radio_value(&mut self.base, NumBase::Hex, "Hexadecimal");
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Search type");
                        for (op, label) in UiOp::ALL {
                            ui.add_enabled_ui(!op.needs_results() || have_results, |ui| {
                                ui.radio_value(&mut self.op, op, label);
                            });
                        }
                    });
                });
                ui.checkbox(&mut self.search_rom, "Search ROM");

                ui.horizontal(|ui| {
                    if ui.button("New Search").clicked() {
                        self.results.clear();
                        self.editing = None;
                        match self.create_params() {
                            Some(p) => {
                                let mut ops = ConsoleSearchOps::new(c);
                                search(&mut ops, &p, &mut self.results, LIMIT);
                                self.status = format!("{} result(s)", self.results.len());
                            }
                            None => self.status = "Invalid search value".into(),
                        }
                    }
                    if ui.add_enabled(have_results, egui::Button::new("Search Within")).clicked() {
                        self.editing = None;
                        match self.create_params() {
                            Some(mut p) => {
                                if self.op == UiOp::Unknown {
                                    p.op = SearchOp::DeltaAny;
                                }
                                let mut ops = ConsoleSearchOps::new(c);
                                search_repeat(&mut ops, &p, &mut self.results);
                                self.status = format!("{} result(s)", self.results.len());
                            }
                            None => self.status = "Invalid search value".into(),
                        }
                    }
                    // Values are re-read every frame; Refresh just resets the
                    // edit state like the Qt button repopulating the table.
                    if ui.button("Refresh").clicked() {
                        self.editing = None;
                    }
                    ui.label(egui::RichText::new(&self.status).weak());
                });
                ui.separator();

                let hex = self.base == NumBase::Hex;
                let mut write: Option<(usize, String)> = None;
                let mut ops = ConsoleSearchOps::new(c);
                let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 4.0;
                egui::Grid::new("memsearch_head").num_columns(3).min_col_width(110.0).show(ui, |ui| {
                    ui.strong("Address");
                    ui.strong("Current Value");
                    ui.strong("Type");
                    ui.end_row();
                });
                egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, self.results.len(), |ui, range| {
                    egui::Grid::new("memsearch_rows").num_columns(3).min_col_width(110.0).show(ui, |ui| {
                        for i in range {
                            let r = self.results[i];
                            ui.monospace(format!("{:08X}", r.address));
                            match &mut self.editing {
                                Some((idx, text)) if *idx == i => {
                                    let resp = ui.add(egui::TextEdit::singleline(text).desired_width(100.0));
                                    if resp.lost_focus() {
                                        if ui.input(|inp| inp.key_pressed(egui::Key::Enter)) {
                                            write = Some((i, text.clone()));
                                        }
                                        self.editing = None;
                                    } else {
                                        resp.request_focus();
                                    }
                                }
                                _ => {
                                    let v = Self::current_value(&mut ops, &r, hex);
                                    if ui.add(egui::Label::new(egui::RichText::new(&v).monospace()).sense(egui::Sense::click()))
                                        .on_hover_text("Double-click to edit")
                                        .double_clicked()
                                    {
                                        self.editing = Some((i, v.split(' ').next().unwrap_or("").to_string()));
                                    }
                                }
                            }
                            ui.label(match r.typ {
                                SearchType::Int => format!("{}-bit", r.width * 8),
                                SearchType::String => "string".to_string(),
                                SearchType::Guess => format!("guess ({}-bit)", r.width * 8),
                            });
                            ui.end_row();
                        }
                    });
                });
                drop(ops);
                if let Some((i, text)) = write {
                    if let Some(r) = self.results.get(i).copied() {
                        match Self::write_result(c, &r, &text, hex) {
                            Ok(()) => self.status = format!("Wrote {text} to {:08X}", r.address),
                            Err(e) => self.status = e,
                        }
                    }
                }
            });
    }

    fn on_session_changed(&mut self) {
        self.results.clear();
        self.editing = None;
        self.status.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_and_refines_ewram_value() {
        let mut c = Console::Gba(rgba_gba::gba::Gba::new());
        dbg(&mut c).dbg_raw_write(0x0200_1230, -1, 4, 0x1234_ABCD);
        let p = SearchParams {
            memory_flags: mf::WRITE,
            typ: SearchType::Int,
            op: SearchOp::Equal,
            align: -1,
            width: 4,
            value_int: 0x1234_ABCDu32 as i32,
            value_str: Vec::new(),
        };
        let mut results = Vec::new();
        search(&mut ConsoleSearchOps::new(&mut c), &p, &mut results, LIMIT);
        assert!(results.iter().any(|r| r.address == 0x0200_1230), "{results:?}");

        // "Changed by value" +1 after incrementing it.
        dbg(&mut c).dbg_raw_write(0x0200_1230, -1, 4, 0x1234_ABCE);
        let p = SearchParams { op: SearchOp::Delta, value_int: 1, ..p };
        search_repeat(&mut ConsoleSearchOps::new(&mut c), &p, &mut results);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].address, 0x0200_1230);
    }
}
