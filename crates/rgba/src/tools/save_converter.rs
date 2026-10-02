// "Convert save game..." — mirrors mGBA Qt's SaveConverter.cpp for GBA
// battery saves: open a raw save, a SharkPort (.sps/.xps) or a GameShark
// (.gsv) container, detect its contents, and write it back out as a raw
// .sav or a SharkPort container. (mGBA's converter also handles savestate
// extdata and GB saves; rgba savestates carry no savedata extdata, so only
// the container conversions are offered.)
//
// Writing SharkPort needs the cartridge header (title, code, checksum) the
// container embeds; that comes from the running game's ROM or from a ROM
// file the user picks.

use std::path::PathBuf;

use eframe::egui;

use rgba_gba::gba::Gba;
use rgba_gba::savedata::{
    SavedataType, GBA_SIZE_EEPROM, GBA_SIZE_EEPROM512, GBA_SIZE_FLASH1M, GBA_SIZE_FLASH512,
    GBA_SIZE_SRAM,
};
use rgba_gba::sharkport::{
    savedata_gsv_get_payload, savedata_sharkport_get_payload, GSV_HEADER, SHARKPORT_HEADER,
};

use super::{ToolCtx, ToolWindow};

#[derive(Clone, Copy, PartialEq, Eq)]
enum InFormat {
    Raw,
    SharkPort,
    Gsv,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutFormat {
    Raw,
    SharkPort,
}

struct Loaded {
    path: PathBuf,
    format: InFormat,
    /// The battery image in raw .sav layout (EEPROM groups un-reversed).
    raw: Vec<u8>,
    /// Container ident (cart title) if any.
    ident: Option<String>,
}

#[derive(Default)]
pub struct SaveConverter {
    loaded: Option<Loaded>,
    out: Option<OutFormat>,
    rom_for_header: Option<PathBuf>,
    status: String,
}

fn savetype_for_size(len: usize) -> Option<SavedataType> {
    Some(match len {
        GBA_SIZE_EEPROM512 => SavedataType::Eeprom512,
        GBA_SIZE_EEPROM => SavedataType::Eeprom,
        GBA_SIZE_SRAM => SavedataType::Sram,
        GBA_SIZE_FLASH512 => SavedataType::Flash512,
        GBA_SIZE_FLASH1M => SavedataType::Flash1M,
        _ => return None,
    })
}

fn savetype_name(len: usize) -> &'static str {
    match savetype_for_size(len) {
        Some(SavedataType::Eeprom512) => "EEPROM 512 bytes",
        Some(SavedataType::Eeprom) => "EEPROM 8 KiB",
        Some(SavedataType::Sram) => "SRAM 32 KiB",
        Some(SavedataType::Flash512) => "Flash 64 KiB",
        Some(SavedataType::Flash1M) => "Flash 128 KiB",
        _ => "unknown size",
    }
}

/// Container payloads store EEPROM in 8-byte groups reversed relative to
/// the raw save (see _importSavedata / GBASavedataExportSharkPort).
fn unreverse_eeprom(payload: &[u8]) -> Vec<u8> {
    if payload.len() == GBA_SIZE_EEPROM || payload.len() == GBA_SIZE_EEPROM512 {
        (0..payload.len()).map(|i| payload[i ^ 7]).collect()
    } else {
        payload.to_vec()
    }
}

fn ascii(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim().to_string()
}

fn load(path: PathBuf) -> Result<Loaded, String> {
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() > 4 + SHARKPORT_HEADER.len() && &bytes[4..4 + SHARKPORT_HEADER.len()] == SHARKPORT_HEADER {
        let (payload, header) = savedata_sharkport_get_payload(&bytes, false).ok_or("Corrupt SharkPort container")?;
        return Ok(Loaded {
            path,
            format: InFormat::SharkPort,
            raw: unreverse_eeprom(&payload),
            ident: Some(ascii(&header[..0x10])),
        });
    }
    if bytes.starts_with(GSV_HEADER) {
        let (payload, ident) = savedata_gsv_get_payload(&bytes).ok_or("Corrupt GameShark container")?;
        return Ok(Loaded {
            path,
            format: InFormat::Gsv,
            raw: unreverse_eeprom(&payload),
            ident: Some(ascii(&ident)),
        });
    }
    if savetype_for_size(bytes.len()).is_none() {
        return Err(format!("{} bytes is not a GBA save size", bytes.len()));
    }
    Ok(Loaded { path, format: InFormat::Raw, raw: bytes, ident: None })
}

/// Build a SharkPort container around `raw` using `rom`'s cartridge header:
/// a throwaway core with the ROM loaded and the save copied in, exported
/// with GBASavedataExportSharkPort.
fn to_sharkport(raw: &[u8], rom: Vec<u8>) -> Result<Vec<u8>, String> {
    let t = savetype_for_size(raw.len()).ok_or("unknown save size")?;
    let mut g = Gba::new();
    if !g.load_rom(rom) {
        return Err("not a GBA ROM".into());
    }
    g.savedata_force_type(t);
    let n = g.savedata.data.len().min(raw.len());
    g.savedata.data[..n].copy_from_slice(&raw[..n]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    g.savedata_export_sharkport(now).ok_or_else(|| "export failed".into())
}

impl ToolWindow for SaveConverter {
    fn title(&self) -> &'static str {
        "Convert save game"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        egui::Window::new(self.title()).open(open).resizable(false).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Input file:");
                if ui.button("Open...").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("Save games", &["sav", "sa1", "sa2", "sa3", "sa4", "sps", "xps", "gsv"])
                        .add_filter("All files", &["*"])
                        .pick_file()
                    {
                        match load(p) {
                            Ok(l) => {
                                self.out = Some(if l.format == InFormat::Raw { OutFormat::SharkPort } else { OutFormat::Raw });
                                self.loaded = Some(l);
                                self.status.clear();
                            }
                            Err(e) => {
                                self.loaded = None;
                                self.status = e;
                            }
                        }
                    }
                }
            });
            let Some(l) = &self.loaded else {
                if !self.status.is_empty() {
                    ui.colored_label(egui::Color32::LIGHT_RED, &self.status);
                }
                return;
            };
            egui::Grid::new("savein").num_columns(2).show(ui, |ui| {
                ui.label("File:");
                ui.label(l.path.display().to_string());
                ui.end_row();
                ui.label("Format:");
                ui.label(match l.format {
                    InFormat::Raw => "Raw battery save",
                    InFormat::SharkPort => "SharkPort",
                    InFormat::Gsv => "GameShark Advance (GSV)",
                });
                ui.end_row();
                ui.label("Contents:");
                ui.label(savetype_name(l.raw.len()));
                ui.end_row();
                if let Some(id) = &l.ident {
                    ui.label("Game:");
                    ui.label(id);
                    ui.end_row();
                }
            });
            ui.separator();
            let mut out = self.out.unwrap_or(OutFormat::Raw);
            ui.horizontal(|ui| {
                ui.label("Output:");
                ui.radio_value(&mut out, OutFormat::Raw, "Raw .sav");
                ui.radio_value(&mut out, OutFormat::SharkPort, "SharkPort .sps");
            });
            self.out = Some(out);

            let running_rom = tc.session.as_deref().and_then(|s| match &s.console {
                crate::emu::Console::Gba(_) => Some(s.rom_image.clone()),
                _ => None,
            });
            if out == OutFormat::SharkPort {
                ui.label("SharkPort files embed the cartridge header. Header source:");
                ui.horizontal(|ui| {
                    match (&self.rom_for_header, &running_rom) {
                        (Some(p), _) => ui.label(p.display().to_string()),
                        (None, Some(_)) => ui.label("the running game"),
                        (None, None) => ui.colored_label(egui::Color32::LIGHT_RED, "pick the game's ROM"),
                    };
                    if ui.button("Choose ROM...").clicked() {
                        if let Some(p) = rfd::FileDialog::new().add_filter("GBA ROMs", &["gba", "agb", "bin"]).pick_file() {
                            self.rom_for_header = Some(p);
                        }
                    }
                });
            }

            if ui.button("Convert and save...").clicked() {
                let stem = l.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let (ext, name) = match out {
                    OutFormat::Raw => ("sav", format!("{stem}.sav")),
                    OutFormat::SharkPort => ("sps", format!("{stem}.sps")),
                };
                let data = match out {
                    OutFormat::Raw => Ok(l.raw.clone()),
                    OutFormat::SharkPort => {
                        let rom = match &self.rom_for_header {
                            Some(p) => std::fs::read(p).map_err(|e| e.to_string()),
                            None => running_rom.clone().ok_or_else(|| "No ROM for the cartridge header".to_string()),
                        };
                        rom.and_then(|rom| to_sharkport(&l.raw, rom))
                    }
                };
                match data {
                    Ok(bytes) => {
                        let mut d = rfd::FileDialog::new().set_file_name(&name).add_filter("Save", &[ext]);
                        if let Some(dir) = l.path.parent() {
                            d = d.set_directory(dir);
                        }
                        if let Some(p) = d.save_file() {
                            self.status = match std::fs::write(&p, bytes) {
                                Ok(()) => format!("Saved {}", p.display()),
                                Err(e) => e.to_string(),
                            };
                        }
                    }
                    Err(e) => self.status = e,
                }
            }
            if !self.status.is_empty() {
                ui.label(&self.status);
            }
        });
    }
}
