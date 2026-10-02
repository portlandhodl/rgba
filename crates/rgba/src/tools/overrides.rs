// "Game overrides..." — mirrors mGBA Qt's OverrideView.cpp: per-game GBA
// save type / cartridge hardware / idle loop / VBA bug compat, and GB model /
// MBC / palette. mGBA stores these in overrides.ini (GBAOverrideSave /
// GBOverrideSave); rgba keeps them in `<config dir>/overrides.json`, keyed
// by game code (GBA, or GB carts with a new-style code) or ROM CRC32.
//
// The parent applies the saved entry after loading a game with
// `apply_saved_overrides` (mCoreConfig-driven GBAOverrideFind in mGBA).

use std::collections::BTreeMap;

use eframe::egui;
use serde::{Deserialize, Serialize};

use rgba_gb::gb::GbModel;
use rgba_gb::memory::MbcType;
use rgba_gb::overrides::{color_preset_list, GbOverride};
use rgba_gba::gba::{
    GBA_IDLE_LOOP_NONE, HW_GB_PLAYER_DETECTION, HW_GYRO, HW_LIGHT_SENSOR, HW_NO_OVERRIDE, HW_RTC,
    HW_RUMBLE, HW_TILT,
};
use rgba_gba::overrides::GbaOverride;
use rgba_gba::savedata::SavedataType;

use super::rom_info::game_ident;
use super::{no_game, ToolCtx, ToolWindow};
use crate::emu::{Console, Session};

const SAVE_TYPES: [(SavedataType, &str); 8] = [
    (SavedataType::Autodetect, "Autodetect"),
    (SavedataType::ForceNone, "None"),
    (SavedataType::Sram, "SRAM"),
    (SavedataType::Flash512, "Flash 512kb"),
    (SavedataType::Flash1M, "Flash 1Mb"),
    (SavedataType::Eeprom, "EEPROM 8kB"),
    (SavedataType::Eeprom512, "EEPROM 512 bytes"),
    (SavedataType::Sram512, "SRAM 64kB (bootlegs only)"),
];

const HARDWARE: [(u32, &str); 5] = [
    (HW_RTC, "Realtime clock"),
    (HW_GYRO, "Gyroscope"),
    (HW_TILT, "Tilt"),
    (HW_LIGHT_SENSOR, "Light sensor"),
    (HW_RUMBLE, "Rumble"),
];

/// GameBoy.cpp s_gbModelList / s_gbModelNames.
const GB_MODELS: [(GbModel, &str); 8] = [
    (GbModel::Autodetect, "Autodetect"),
    (GbModel::Dmg, "Game Boy (DMG)"),
    (GbModel::Mgb, "Game Boy Pocket (MGB)"),
    (GbModel::Sgb, "Super Game Boy (SGB)"),
    (GbModel::Sgb2, "Super Game Boy 2 (SGB)"),
    (GbModel::Cgb, "Game Boy Color (CGB)"),
    (GbModel::Agb, "Game Boy Advance (AGB)"),
    (GbModel::Scgb, "Super Game Boy Color (SGB + CGB)"),
];

/// GameBoy.cpp s_mbcList / s_mbcNames.
const GB_MBCS: [(MbcType, &str); 28] = [
    (MbcType::Autodetect, "Autodetect"),
    (MbcType::None, "ROM Only"),
    (MbcType::Mbc1, "MBC1"),
    (MbcType::Mbc2, "MBC2"),
    (MbcType::Mbc3, "MBC3"),
    (MbcType::Mbc3Rtc, "MBC3 + RTC"),
    (MbcType::Mbc5, "MBC5"),
    (MbcType::Mbc5Rumble, "MBC5 + Rumble"),
    (MbcType::Mbc6, "MBC6"),
    (MbcType::Mbc7, "MBC7 (Tilt)"),
    (MbcType::Mmm01, "MMM01"),
    (MbcType::PocketCam, "Pocket Cam"),
    (MbcType::Tama5, "TAMA5"),
    (MbcType::HuC1, "HuC-1"),
    (MbcType::HuC3, "HuC-3"),
    (MbcType::M161, "M161"),
    (MbcType::UnlWisdomTree, "Wisdom Tree"),
    (MbcType::UnlPkjd, "Pokémon Jade/Diamond"),
    (MbcType::UnlNtOld1, "NT (old 1)"),
    (MbcType::UnlNtOld2, "NT (old 2)"),
    (MbcType::UnlNtNew, "NT (new)"),
    (MbcType::UnlBbd, "BBD"),
    (MbcType::UnlHitek, "Hitek"),
    (MbcType::UnlGgb81, "GGB-81"),
    (MbcType::UnlLiCheng, "Li Cheng"),
    (MbcType::UnlSachenMmc1, "Sachen (MMC1)"),
    (MbcType::UnlSachenMmc2, "Sachen (MMC2)"),
    (MbcType::UnlSintax, "Sintax"),
];

/// One saved override entry (the subset of overrides.ini keys mGBA writes).
#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SavedOverride {
    // GBA
    pub savetype: Option<i32>,
    /// None = autodetect hardware.
    pub hardware: Option<u32>,
    pub idle_loop: Option<u32>,
    pub vba_bug_compat: bool,
    // GB
    pub model: Option<i32>,
    pub mbc: Option<i32>,
    /// 0xFFRRGGBB per entry, 0 = unset.
    pub colors: Vec<u32>,
}

fn store_path() -> std::path::PathBuf {
    crate::config::config_dir().join("overrides.json")
}

fn load_store() -> BTreeMap<String, SavedOverride> {
    std::fs::read_to_string(store_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_store(map: &BTreeMap<String, SavedOverride>) -> Result<(), String> {
    let p = store_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let s = serde_json::to_string_pretty(map).map_err(|e| e.to_string())?;
    std::fs::write(p, s).map_err(|e| e.to_string())
}

fn savetype_from_i32(v: i32) -> SavedataType {
    SAVE_TYPES.iter().map(|t| t.0).find(|t| *t as i32 == v).unwrap_or(SavedataType::Autodetect)
}
fn model_from_i32(v: i32) -> GbModel {
    GB_MODELS.iter().map(|t| t.0).find(|m| *m as i32 == v).unwrap_or(GbModel::Autodetect)
}
fn mbc_from_i32(v: i32) -> MbcType {
    GB_MBCS.iter().map(|t| t.0).find(|m| *m as i32 == v).unwrap_or(MbcType::Autodetect)
}

fn gba_code(s: &mut Session) -> [u8; 4] {
    let mut id = [0u8; 4];
    if let Console::Gba(g) = &s.console {
        if g.memory.rom.len() >= 0xB0 {
            id.copy_from_slice(&g.memory.rom[0xAC..0xB0]);
        }
    }
    id
}

fn gb_header_crc(s: &Session) -> u32 {
    match &s.console {
        Console::Gb(g) if g.memory.rom.len() >= 0x150 => rgba_gb::crc32(&g.memory.rom[0x100..0x150]),
        _ => 0,
    }
}

/// Apply `o` to the running game (GBAOverrideApply / GBOverrideApply).
/// A save type change keeps the bytes already in memory where they fit.
fn apply(s: &mut Session, o: &SavedOverride) {
    let id = gba_code(s);
    let crc = gb_header_crc(s);
    match &mut s.console {
        Console::Gba(g) => {
            let ov = GbaOverride {
                id,
                savetype: o.savetype.map_or(SavedataType::Autodetect, savetype_from_i32),
                hardware: o.hardware.unwrap_or(HW_NO_OVERRIDE),
                idle_loop: o.idle_loop.unwrap_or(GBA_IDLE_LOOP_NONE),
                vba_bug_compat: o.vba_bug_compat,
            };
            let old = (ov.savetype != SavedataType::Autodetect && ov.savetype != g.savedata.savedata_type)
                .then(|| g.savedata.data.clone());
            if old.is_some() {
                // savedata_force_type only allocates from a fresh state.
                g.savedata.savedata_type = SavedataType::Autodetect;
                g.savedata.data.clear();
            }
            rgba_gba::overrides::override_apply(g, &ov);
            if let Some(old) = old {
                let n = old.len().min(g.savedata.data.len());
                g.savedata.data[..n].copy_from_slice(&old[..n]);
            }
        }
        Console::Gb(g) => {
            let mut ov = GbOverride::new(crc);
            ov.model = o.model.map_or(GbModel::Autodetect, model_from_i32);
            ov.mbc = o.mbc.map_or(MbcType::Autodetect, mbc_from_i32);
            for (i, c) in o.colors.iter().take(12).enumerate() {
                ov.gb_colors[i] = *c;
            }
            rgba_gb::overrides::override_apply(g, &ov);
        }
    }
}

/// Look up the saved override for the running game and apply it. Call after
/// a game is loaded (before it starts running).
pub fn apply_saved_overrides(s: &mut Session) {
    let key = game_ident(s).key();
    if let Some(o) = load_store().get(&key) {
        apply(s, o);
    }
}

#[derive(Default)]
pub struct OverridesView {
    /// Game key the editor state belongs to.
    key: Option<String>,
    edit: SavedOverride,
    idle_text: String,
    preset: usize,
    status: String,
}

impl OverridesView {
    fn reload(&mut self, s: &mut Session) {
        let key = game_ident(s).key();
        let saved = load_store().get(&key).cloned();
        self.edit = saved.unwrap_or_else(|| {
            // Start from the built-in table entry, like OverrideView showing
            // the effective override.
            let mut o = SavedOverride::default();
            match &s.console {
                Console::Gba(_) => {
                    let mut t = GbaOverride {
                        id: gba_code(s),
                        savetype: SavedataType::Autodetect,
                        hardware: HW_NO_OVERRIDE,
                        idle_loop: GBA_IDLE_LOOP_NONE,
                        vba_bug_compat: false,
                    };
                    if rgba_gba::overrides::override_find(&mut t) {
                        o.savetype = (t.savetype != SavedataType::Autodetect).then_some(t.savetype as i32);
                        o.hardware = (t.hardware != HW_NO_OVERRIDE).then_some(t.hardware);
                        o.idle_loop = (t.idle_loop != GBA_IDLE_LOOP_NONE).then_some(t.idle_loop);
                        o.vba_bug_compat = t.vba_bug_compat;
                    }
                }
                Console::Gb(_) => {
                    let mut t = GbOverride::new(gb_header_crc(s));
                    if rgba_gb::overrides::override_find(&mut t) {
                        o.model = (t.model != GbModel::Autodetect).then_some(t.model as i32);
                        o.mbc = (t.mbc != MbcType::Autodetect).then_some(t.mbc as i32);
                        if t.gb_colors.iter().any(|c| *c != 0) {
                            o.colors = t.gb_colors.to_vec();
                        }
                    }
                }
            }
            o
        });
        self.idle_text = self.edit.idle_loop.map(|v| format!("{v:08X}")).unwrap_or_default();
        self.key = Some(key);
        self.status.clear();
    }

    fn gba_ui(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("gbaov").num_columns(2).show(ui, |ui| {
            ui.label("Save type:");
            let cur = self.edit.savetype.map_or(SavedataType::Autodetect, savetype_from_i32);
            let name = SAVE_TYPES.iter().find(|t| t.0 == cur).map_or("Autodetect", |t| t.1);
            egui::ComboBox::from_id_salt("ov_savetype").selected_text(name).show_ui(ui, |ui| {
                for (t, n) in SAVE_TYPES {
                    if ui.selectable_label(t == cur, n).clicked() {
                        self.edit.savetype = (t != SavedataType::Autodetect).then_some(t as i32);
                    }
                }
            });
            ui.end_row();

            ui.label("Idle loop:");
            let r = ui.add(egui::TextEdit::singleline(&mut self.idle_text).hint_text("autodetect").desired_width(100.0));
            if r.changed() {
                let t = self.idle_text.trim().trim_start_matches("0x");
                self.edit.idle_loop = if t.is_empty() { None } else { u32::from_str_radix(t, 16).ok() };
            }
            ui.end_row();
        });
        ui.separator();
        let mut auto = self.edit.hardware.is_none();
        if ui.checkbox(&mut auto, "Autodetect hardware").changed() {
            self.edit.hardware = if auto { None } else { Some(0) };
        }
        ui.add_enabled_ui(!auto, |ui| {
            let mut hw = self.edit.hardware.unwrap_or(0);
            for (bit, name) in HARDWARE {
                let mut on = hw & bit != 0;
                if ui.checkbox(&mut on, name).changed() {
                    hw ^= bit;
                }
            }
            let mut gbp = hw & HW_GB_PLAYER_DETECTION != 0;
            if ui.checkbox(&mut gbp, "Game Boy Player features").changed() {
                hw ^= HW_GB_PLAYER_DETECTION;
            }
            if !auto {
                self.edit.hardware = Some(hw);
            }
        });
        ui.checkbox(&mut self.edit.vba_bug_compat, "VBA bug compatibility mode");
    }

    fn gb_ui(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("gbov").num_columns(2).show(ui, |ui| {
            ui.label("Game Boy model:");
            let cur = self.edit.model.map_or(GbModel::Autodetect, model_from_i32);
            let name = GB_MODELS.iter().find(|t| t.0 == cur).map_or("Autodetect", |t| t.1);
            egui::ComboBox::from_id_salt("ov_model").selected_text(name).show_ui(ui, |ui| {
                for (m, n) in GB_MODELS.iter() {
                    if ui.selectable_label(*m == cur, *n).clicked() {
                        self.edit.model = (*m != GbModel::Autodetect).then_some(*m as i32);
                    }
                }
            });
            ui.end_row();

            ui.label("Memory bank controller:");
            let cur = self.edit.mbc.map_or(MbcType::Autodetect, mbc_from_i32);
            let name = GB_MBCS.iter().find(|t| t.0 == cur).map_or("Autodetect", |t| t.1);
            egui::ComboBox::from_id_salt("ov_mbc").selected_text(name).show_ui(ui, |ui| {
                for (m, n) in GB_MBCS.iter() {
                    if ui.selectable_label(*m == cur, *n).clicked() {
                        self.edit.mbc = (*m != MbcType::Autodetect).then_some(*m as i32);
                    }
                }
            });
            ui.end_row();

            ui.label("Palette preset:");
            let presets = color_preset_list();
            let label = if self.preset == 0 { "(custom)" } else { presets[self.preset - 1].name };
            egui::ComboBox::from_id_salt("ov_preset").selected_text(label).show_ui(ui, |ui| {
                for (i, p) in presets.iter().enumerate() {
                    if ui.selectable_label(self.preset == i + 1, p.name).clicked() {
                        self.preset = i + 1;
                        self.edit.colors = p.colors.to_vec();
                    }
                }
            });
            ui.end_row();
        });
        if self.edit.colors.len() != 12 {
            self.edit.colors.resize(12, 0);
        }
        for (row, name) in ["Background Colors", "Sprite Colors 1", "Sprite Colors 2"].iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("{name}:"));
                for i in 0..4 {
                    let c = &mut self.edit.colors[row * 4 + i];
                    let mut rgb = [(*c >> 16) as u8, (*c >> 8) as u8, *c as u8];
                    if ui.color_edit_button_srgb(&mut rgb).changed() {
                        *c = 0xFF00_0000 | (rgb[0] as u32) << 16 | (rgb[1] as u32) << 8 | rgb[2] as u32;
                        self.preset = 0;
                    }
                }
            });
        }
        if ui.small_button("Clear colors").clicked() {
            self.edit.colors.clear();
            self.preset = 0;
        }
    }
}

impl ToolWindow for OverridesView {
    fn title(&self) -> &'static str {
        "Game overrides"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        egui::Window::new(self.title()).open(open).resizable(false).show(ctx, |ui| {
            let Some(s) = tc.session.as_deref_mut() else {
                no_game(ui);
                return;
            };
            let id = game_ident(s);
            if self.key.as_deref() != Some(id.key().as_str()) {
                self.reload(s);
            }
            ui.label(format!("{} ({})", id.title, if id.code.is_empty() { format!("{:08X}", id.crc32) } else { id.code.clone() }));
            ui.separator();
            let is_gba = matches!(s.console, Console::Gba(_));
            if is_gba {
                ui.strong("Game Boy Advance");
                self.gba_ui(ui);
            } else {
                ui.strong("Game Boy");
                self.gb_ui(ui);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    apply(s, &self.edit);
                    self.status = if is_gba { "Applied".into() } else { "Applied (reset for model changes)".into() };
                }
                if ui.button("Save").clicked() {
                    let mut store = load_store();
                    store.insert(id.key(), self.edit.clone());
                    self.status = match save_store(&store) {
                        Ok(()) => "Saved; applied when this game loads".into(),
                        Err(e) => e,
                    };
                }
                if ui.button("Reset to defaults").clicked() {
                    let mut store = load_store();
                    store.remove(&id.key());
                    let _ = save_store(&store);
                    self.key = None;
                    self.status = "Saved override removed".into();
                }
            });
            if !self.status.is_empty() {
                ui.label(&self.status);
            }
        });
    }

    fn on_session_changed(&mut self) {
        self.key = None;
    }
}
