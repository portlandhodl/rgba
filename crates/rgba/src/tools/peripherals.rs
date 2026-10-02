// Link-port peripherals: "Connect to Dolphin..." (mGBA Qt DolphinConnector),
// "BattleChip Gate..." (Qt BattleChipView + BattleChipModel) and
// "Game Boy Printer..." (Qt PrinterView + CoreController::attachPrinter).

use eframe::egui;

use rgba_gb::sio::{GbPrinter, SioDriver as GbSioDriver};
use rgba_gba::cart::battlechip::{BattleChipGate, BattlechipFlavor};
use rgba_gba::sio::dolphin::Dolphin;
use rgba_gba::sio::SioDriver;

use super::{no_game, upload_rgba, ToolCtx, ToolWindow};
use crate::emu::Session;

// ---------------------------------------------------------------------------
// Dolphin
// ---------------------------------------------------------------------------

pub struct DolphinConnector {
    local: bool,
    address: String,
    reset: bool,
    status: String,
}

impl Default for DolphinConnector {
    fn default() -> Self {
        DolphinConnector { local: true, address: "127.0.0.1".into(), reset: true, status: String::new() }
    }
}

fn dolphin_attached(s: &mut Session) -> bool {
    match s.console.gba() {
        Some(g) => matches!(g.sio.driver, SioDriver::Dolphin) && g.sio.dolphin.as_ref().map_or(false, |d| d.is_connected()),
        None => false,
    }
}

impl ToolWindow for DolphinConnector {
    fn title(&self) -> &'static str {
        "Connect to Dolphin"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        egui::Window::new(self.title()).open(open).resizable(false).show(ctx, |ui| {
            ui.label("Link with a GameCube game running in Dolphin (GBA link cable emulation).");
            ui.radio_value(&mut self.local, true, "Local computer");
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.local, false, "IP address:");
                ui.add_enabled(!self.local, egui::TextEdit::singleline(&mut self.address).desired_width(140.0));
            });
            ui.checkbox(&mut self.reset, "Reset on connect");
            let Some(s) = tc.session.as_deref_mut() else {
                ui.label("Start a GBA game (or File → Boot BIOS) first.");
                return;
            };
            if s.console.gba().is_none() {
                ui.label("Dolphin linking is only available for GBA games.");
                return;
            }
            let attached = dolphin_attached(s);
            ui.label(if attached { "Status: connected" } else { "Status: not connected" });
            ui.horizontal(|ui| {
                if ui.add_enabled(!attached, egui::Button::new("Connect")).clicked() {
                    let addr = if self.local { "127.0.0.1".to_string() } else { self.address.trim().to_string() };
                    let mut d = Dolphin::new();
                    match d.connect(&addr, 0, 0) {
                        Ok(()) => {
                            let g = s.console.gba().unwrap();
                            g.sio.dolphin = Some(Box::new(d));
                            g.sio_set_driver(Some(SioDriver::Dolphin));
                            if dolphin_attached(s) {
                                if self.reset {
                                    s.core().reset();
                                }
                                self.status = format!("Connected to {addr}");
                            } else {
                                self.status = "Could not connect to Dolphin.".into();
                            }
                        }
                        Err(e) => self.status = format!("Could not connect to Dolphin: {e}"),
                    }
                }
                if ui.add_enabled(attached, egui::Button::new("Disconnect")).clicked() {
                    let g = s.console.gba().unwrap();
                    g.sio_set_driver(None);
                    g.sio.dolphin = None;
                    self.status = "Disconnected".into();
                }
            });
            if !self.status.is_empty() {
                ui.label(&self.status);
            }
        });
    }
}

// ---------------------------------------------------------------------------
// BattleChip Gate
// ---------------------------------------------------------------------------

/// BattleChipModel::setFlavor: chip names per gate, one per line, ids start
/// at 1 (blank lines are unused ids). Beast Link (US) uses the Beast Link list.
fn chip_names(flavor: BattlechipFlavor) -> Vec<(u16, &'static str)> {
    let text = match flavor {
        BattlechipFlavor::BattlechipGate => include_str!("../../res/chip-names-exe4.txt"),
        BattlechipFlavor::ProgressGate => include_str!("../../res/chip-names-exe5.txt"),
        BattlechipFlavor::BeastLinkGate | BattlechipFlavor::BeastLinkGateUs => include_str!("../../res/chip-names-exe6.txt"),
    };
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| (i as u16 + 1, l.trim()))
        .collect()
}

/// BattleChipView::UNINSERTED_TIME
const UNINSERTED_TIME: i32 = 10;

pub struct BattleChipView {
    attached: bool,
    flavor: BattlechipFlavor,
    chip_id: u16,
    inserted: bool,
    filter: String,
    deck: Vec<u16>,
    selected: Option<usize>,
    /// BattleChipView::m_frameCounter / m_next (the "Insert" re-seat pulse).
    frame_counter: i32,
    next: bool,
}

impl Default for BattleChipView {
    fn default() -> Self {
        BattleChipView {
            attached: false,
            flavor: BattlechipFlavor::BattlechipGate,
            chip_id: 1,
            inserted: false,
            filter: String::new(),
            deck: Vec::new(),
            selected: None,
            frame_counter: -1,
            next: false,
        }
    }
}

fn gate(s: &mut Session) -> Option<&mut BattleChipGate> {
    match s.console.gba() {
        Some(g) => match &mut g.sio.driver {
            SioDriver::Battlechip(gate) => Some(gate),
            _ => None,
        },
        None => None,
    }
}

impl BattleChipView {
    /// BattleChipView::insertChip
    fn insert_chip(&mut self, s: &mut Session, inserted: bool) {
        self.inserted = inserted;
        if let Some(g) = gate(s) {
            if inserted {
                g.insert_chip(self.chip_id);
            } else {
                g.remove_chip();
            }
        }
    }

    fn attach(&mut self, s: &mut Session) {
        let Some(g) = s.console.gba() else { return };
        // Pick the gate from the game code like the Qt view's constructor.
        let code = if g.memory.rom.len() >= 0xB0 {
            String::from_utf8_lossy(&g.memory.rom[0xAC..0xB0]).into_owned()
        } else {
            String::new()
        };
        self.flavor = if ["B4B", "B4W", "BR4", "BZ3"].iter().any(|p| code.starts_with(p)) {
            BattlechipFlavor::BattlechipGate
        } else if code.starts_with("BRB") || code.starts_with("BRK") {
            BattlechipFlavor::ProgressGate
        } else if code.starts_with("BR5") || code.starts_with("BR6") {
            if code.ends_with('E') || code.ends_with('P') {
                BattlechipFlavor::BeastLinkGateUs
            } else {
                BattlechipFlavor::BeastLinkGate
            }
        } else {
            BattlechipFlavor::BattlechipGate
        };
        let mut gate = BattleChipGate::new();
        gate.flavor = self.flavor;
        g.attach_battlechip_gate(gate);
        self.attached = true;
        self.inserted = false;
    }

    fn detach(&mut self, s: Option<&mut Session>) {
        if self.attached {
            if let Some(g) = s.and_then(|s| s.console.gba()) {
                if matches!(g.sio.driver, SioDriver::Battlechip(_)) {
                    g.detach_battlechip_gate();
                }
            }
        }
        self.attached = false;
        self.inserted = false;
        self.frame_counter = -1;
    }
}

impl ToolWindow for BattleChipView {
    fn title(&self) -> &'static str {
        "BattleChip Gate"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        egui::Window::new(self.title()).open(open).default_width(320.0).show(ctx, |ui| {
            let Some(s) = tc.session.as_deref_mut() else {
                no_game(ui);
                return;
            };
            if s.console.gba().is_none() {
                ui.label("The BattleChip Gate is a GBA link-port accessory.");
                return;
            }
            if !self.attached || gate(s).is_none() {
                self.attach(s);
            }
            ui.horizontal(|ui| {
                ui.label("Gate:");
                let before = self.flavor;
                ui.radio_value(&mut self.flavor, BattlechipFlavor::BattlechipGate, "BattleChip Gate");
                ui.radio_value(&mut self.flavor, BattlechipFlavor::ProgressGate, "Progress Gate");
                let beast = matches!(self.flavor, BattlechipFlavor::BeastLinkGate | BattlechipFlavor::BeastLinkGateUs);
                if ui.radio(beast, "Beast Link Gate").clicked() && !beast {
                    self.flavor = BattlechipFlavor::BeastLinkGate;
                }
                if self.flavor != before {
                    let f = self.flavor;
                    if let Some(g) = gate(s) {
                        g.flavor = f;
                    }
                }
            });
            if matches!(self.flavor, BattlechipFlavor::BeastLinkGate | BattlechipFlavor::BeastLinkGateUs) {
                let mut us = self.flavor == BattlechipFlavor::BeastLinkGateUs;
                if ui.checkbox(&mut us, "US/EU gate").changed() {
                    self.flavor = if us { BattlechipFlavor::BeastLinkGateUs } else { BattlechipFlavor::BeastLinkGate };
                    let f = self.flavor;
                    if let Some(g) = gate(s) {
                        g.flavor = f;
                    }
                }
            }
            ui.separator();
            let names = chip_names(self.flavor);
            let name_of = |id: u16| names.iter().find(|(i, _)| *i == id).map_or("?", |(_, n)| n);
            ui.horizontal(|ui| {
                ui.label("Chip:");
                let before = self.chip_id;
                egui::ComboBox::from_id_salt("chipname")
                    .selected_text(name_of(self.chip_id))
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("filter"));
                        let f = self.filter.to_lowercase();
                        for (id, name) in names.iter().filter(|(_, n)| f.is_empty() || n.to_lowercase().contains(&f)) {
                            ui.selectable_value(&mut self.chip_id, *id, format!("{id:3}  {name}"));
                        }
                    });
                ui.add(egui::DragValue::new(&mut self.chip_id).range(0..=0xFFFF).prefix("ID "));
                if self.chip_id != before {
                    // Changing the chip unseats the current one (chipId → inserted).
                    self.insert_chip(s, false);
                }
            });
            ui.horizontal(|ui| {
                let mut ins = self.inserted;
                if ui.checkbox(&mut ins, "Inserted").changed() {
                    self.insert_chip(s, ins);
                }
                // BattleChipView::reinsert
                if ui.button("Insert").clicked() {
                    if self.inserted {
                        self.insert_chip(s, false);
                        self.next = true;
                        self.frame_counter = UNINSERTED_TIME;
                    } else {
                        self.insert_chip(s, true);
                    }
                }
            });
            ui.separator();
            ui.label("Deck");
            ui.horizontal(|ui| {
                if ui.button("Add").clicked() && self.chip_id >= 1 {
                    self.deck.push(self.chip_id);
                }
                if ui.add_enabled(self.selected.is_some(), egui::Button::new("Remove")).clicked() {
                    if let Some(i) = self.selected.take() {
                        if i < self.deck.len() {
                            self.deck.remove(i);
                        }
                    }
                }
            });
            egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                let mut pick = None;
                for (i, id) in self.deck.iter().enumerate() {
                    let r = ui.selectable_label(self.selected == Some(i), format!("{id:3}  {}", name_of(*id)));
                    if r.clicked() {
                        self.selected = Some(i);
                    }
                    if r.double_clicked() {
                        pick = Some(*id);
                    }
                }
                if let Some(id) = pick {
                    self.chip_id = id;
                    self.insert_chip(s, false);
                    self.insert_chip(s, true);
                }
            });
        });
        if !*open {
            self.detach(tc.session.as_deref_mut());
        }
    }

    /// BattleChipView::advanceFrameCounter (frameAvailable)
    fn on_frame(&mut self, s: &mut Session) {
        if self.frame_counter == 0 {
            let next = self.next;
            self.insert_chip(s, next);
        }
        if self.frame_counter >= 0 {
            self.frame_counter -= 1;
        }
    }

    fn on_session_changed(&mut self) {
        self.attached = false;
        self.inserted = false;
        self.frame_counter = -1;
    }
}

// ---------------------------------------------------------------------------
// Game Boy Printer
// ---------------------------------------------------------------------------

const PRINTER_WIDTH: usize = 160;

pub struct GbPrinterView {
    attached: bool,
    /// Accumulated printout (prints are appended below each other).
    image: Vec<u32>,
    /// Rows revealed so far (PrinterView's 80 ms per-line feed animation).
    shown_rows: usize,
    last_line: std::time::Instant,
    /// A print is feeding; endPrint (done_printing) fires when it finishes.
    feeding: bool,
    magnification: u32,
    texture: Option<egui::TextureHandle>,
    status: String,
}

impl Default for GbPrinterView {
    fn default() -> Self {
        GbPrinterView {
            attached: false,
            image: Vec::new(),
            shown_rows: 0,
            last_line: std::time::Instant::now(),
            feeding: false,
            magnification: 2,
            texture: None,
            status: String::new(),
        }
    }
}

fn printer(s: &mut Session) -> Option<&mut GbPrinter> {
    match s.console.gb() {
        Some(g) => match &mut g.sio.driver {
            GbSioDriver::Printer(p) => Some(p),
            _ => None,
        },
        None => None,
    }
}

impl GbPrinterView {
    fn rows(&self) -> usize {
        self.image.len() / PRINTER_WIDTH
    }

    /// PrinterView::printAll → CoreController::endPrint
    fn print_all(&mut self, s: &mut Session) {
        self.shown_rows = self.rows();
        if self.feeding {
            self.feeding = false;
            if let Some(p) = printer(s) {
                p.done_printing();
            }
        }
    }
}

impl ToolWindow for GbPrinterView {
    fn title(&self) -> &'static str {
        "Game Boy Printer"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        egui::Window::new(self.title()).open(open).default_size([360.0, 420.0]).show(ctx, |ui| {
            let Some(s) = tc.session.as_deref_mut() else {
                no_game(ui);
                return;
            };
            if s.console.gb().is_none() {
                ui.label("The Game Boy Printer is a Game Boy link-port accessory.");
                return;
            }
            if !self.attached || printer(s).is_none() {
                s.console.gb().unwrap().sio_set_driver(Some(GbSioDriver::Printer(GbPrinter::new())));
                self.attached = true;
            }
            // CoreController's print callback → PrinterView::printImage.
            if let Some(img) = printer(s).and_then(|p| p.take_image()) {
                self.image.extend_from_slice(&img);
                self.feeding = true;
                self.last_line = std::time::Instant::now();
            }
            if self.feeding {
                while self.last_line.elapsed() >= std::time::Duration::from_millis(80) && self.shown_rows < self.rows() {
                    self.shown_rows += 1;
                    self.last_line += std::time::Duration::from_millis(80);
                }
                if self.shown_rows >= self.rows() {
                    self.print_all(s);
                }
                ui.ctx().request_repaint();
            }

            ui.horizontal(|ui| {
                ui.label("Magnification:");
                ui.add(egui::DragValue::new(&mut self.magnification).range(1..=8));
                if ui.add_enabled(self.feeding, egui::Button::new("Hurry up!")).clicked() {
                    self.print_all(s);
                }
                if ui.button("Tear off").clicked() {
                    self.image.clear();
                    self.shown_rows = 0;
                    self.print_all(s);
                }
            });
            let done = !self.feeding && !self.image.is_empty();
            ui.horizontal(|ui| {
                if ui.add_enabled(done, egui::Button::new("Copy")).clicked() {
                    let rgba: Vec<u8> = self.image.iter().flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, 255]).collect();
                    ui.ctx().copy_image(egui::ColorImage::from_rgba_unmultiplied([PRINTER_WIDTH, self.rows()], &rgba));
                }
                if ui.add_enabled(done, egui::Button::new("Save Printout...")).clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("PNG", &["png"]).set_file_name("printout.png").save_file() {
                        self.status = match crate::screenshot::save(&self.image, PRINTER_WIDTH as u32, self.rows() as u32, &p) {
                            Ok(()) => format!("Saved {}", p.display()),
                            Err(e) => e,
                        };
                    }
                }
            });
            if !self.status.is_empty() {
                ui.label(&self.status);
            }
            ui.separator();
            if self.shown_rows == 0 {
                ui.label("Waiting for the game to print...");
                return;
            }
            let rows = self.shown_rows.min(self.rows());
            let tex = upload_rgba(ui.ctx(), &mut self.texture, "gbprinter", &self.image[..rows * PRINTER_WIDTH], PRINTER_WIDTH, rows);
            let mag = self.magnification as f32;
            egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                ui.image((tex.id(), egui::vec2(PRINTER_WIDTH as f32 * mag, rows as f32 * mag)));
            });
        });
        if !*open && self.attached {
            // PrinterView::~PrinterView → CoreController::detachPrinter
            if let Some(g) = tc.session.as_deref_mut().and_then(|s| s.console.gb()) {
                if matches!(g.sio.driver, GbSioDriver::Printer(_)) {
                    g.sio_set_driver(None);
                }
            }
            self.attached = false;
            self.feeding = false;
        }
    }

    fn on_session_changed(&mut self) {
        self.attached = false;
        self.feeding = false;
        self.texture = None;
    }
}
