// The running game: console core + the per-game frontend state mGBA keeps in
// CoreController / mCoreThread (save file, state slots and their undo
// buffers, rewind history, peripheral settings).

use std::path::{Path, PathBuf};

use rgba_core::core::{Core, Platform};
use rgba_core::rewind::RewindContext;
use rgba_gb::gb::{Gb, GbModel};
use rgba_gba::gba::Gba;
use rgba_gba::savedata::SavedataType;

use crate::config::Config;

pub enum Console {
    Gba(Box<Gba>),
    Gb(Box<Gb>),
}

impl Console {
    pub fn core(&mut self) -> &mut dyn Core {
        match self {
            Console::Gba(g) => g.as_mut(),
            Console::Gb(g) => g.as_mut(),
        }
    }
    pub fn core_ref(&self) -> &dyn Core {
        match self {
            Console::Gba(g) => g.as_ref(),
            Console::Gb(g) => g.as_ref(),
        }
    }
    pub fn platform(&self) -> Platform {
        self.core_ref().platform()
    }
    pub fn gba(&mut self) -> Option<&mut Gba> {
        match self {
            Console::Gba(g) => Some(g),
            _ => None,
        }
    }
    pub fn gb(&mut self) -> Option<&mut Gb> {
        match self {
            Console::Gb(g) => Some(g),
            _ => None,
        }
    }
}

pub fn gb_model_from_name(name: &str) -> GbModel {
    match name.to_ascii_lowercase().as_str() {
        "dmg" | "gb" => GbModel::Dmg,
        "cgb" | "gbc" => GbModel::Cgb,
        "agb" | "gba" => GbModel::Agb,
        "sgb" => GbModel::Sgb,
        "mgb" => GbModel::Mgb,
        "sgb2" => GbModel::Sgb2,
        "scgb" | "sgbc" => GbModel::Scgb,
        _ => GbModel::Autodetect,
    }
}

/// Video layers as named in mGBA's core.c `_GBAVideoLayers` / `_GBVideoLayers`.
pub const GBA_LAYERS: [&str; 8] = [
    "Background 0", "Background 1", "Background 2", "Background 3",
    "Objects", "Window 0", "Window 1", "Object Window",
];
pub const GB_LAYERS: [&str; 3] = ["Background", "Window", "Objects"];
/// Audio channels as named in `_GBAAudioChannels` / `_GBAudioChannels`.
pub const GBA_CHANNELS: [&str; 6] = [
    "PSG Channel 1", "PSG Channel 2", "PSG Channel 3", "PSG Channel 4",
    "FIFO Channel A", "FIFO Channel B",
];
pub const GB_CHANNELS: [&str; 4] = ["Channel 1", "Channel 2", "Channel 3", "Channel 4"];

/// GBA_LUX_LEVELS, as used by the Qt InputController's solar sensor.
pub const LUX_LEVELS: [u8; 10] = [5, 11, 18, 27, 42, 62, 84, 109, 139, 183];

/// Apply an IPS/UPS/BPS patch to a ROM image (mCore::loadPatch). GB clamps
/// to the cart maximum; GBA rejects patches that grow past ROM0.
pub fn apply_patch(rom: &mut Vec<u8>, patch: &[u8], gb: bool) -> Result<(), String> {
    let patch = rgba_core::patch::Patch::load(patch).ok_or("unrecognized patch format")?;
    let max = if gb {
        rgba_gb::memory::GB_SIZE_CART_MAX
    } else {
        rgba_gba::memory::GBA_SIZE_ROM0
    };
    let mut size = patch
        .output_size(rom.len())
        .filter(|&s| s > 0)
        .ok_or("patch does not apply to this ROM")?;
    if size > max {
        if gb {
            size = max;
        } else {
            return Err("patched ROM too large".into());
        }
    }
    let mut out = vec![0u8; size];
    if !patch.apply(rom, &mut out) {
        return Err("patch failed to apply".into());
    }
    *rom = out;
    Ok(())
}

pub struct LoadOptions {
    pub patch: Option<PathBuf>,
    /// Override the save path ("Load alternate save game").
    pub save_path: Option<PathBuf>,
    /// Don't write the save back ("Load temporary save game").
    pub temporary_save: bool,
    pub force_gbp: bool,
}

impl Default for LoadOptions {
    fn default() -> Self {
        LoadOptions { patch: None, save_path: None, temporary_save: false, force_gbp: false }
    }
}

pub struct Session {
    pub console: Console,
    pub rom_path: PathBuf,
    /// The ROM image as loaded (after patching), for reset-with-new-BIOS.
    pub rom_image: Vec<u8>,
    pub save_path: PathBuf,
    pub temporary_save: bool,
    /// Last state slot touched by quick save / load ("Load/Save recent").
    pub last_slot: u32,
    /// Undo buffers (CoreController::loadBackupState / saveBackupState).
    pub backup_load: Option<Vec<u8>>,
    pub backup_save: Option<(PathBuf, Option<Vec<u8>>)>,
    pub rewind: Option<RewindContext>,
    rewind_tick: u32,
    pub solar_level: i32,
    pub video_log: bool,
}

impl Session {
    pub fn open(path: &Path, cfg: &Config, opts: &LoadOptions) -> Result<Session, String> {
        let mut rom = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let is_gba = Gba::is_rom(&rom);
        let is_gb = !is_gba && Gb::is_rom(&rom);
        if !is_gba && !is_gb {
            return Err(format!("{}: not a GBA or GB ROM", path.display()));
        }
        if let Some(p) = &opts.patch {
            let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
            apply_patch(&mut rom, &bytes, is_gb)?;
        }
        let console = if is_gba {
            let mut g = Gba::new();
            if cfg.use_bios {
                if let Some(bios) = cfg.gba_bios.as_ref().and_then(|p| std::fs::read(p).ok()) {
                    rgba_gba::bios::bios_load(&mut g, &bios);
                }
            }
            g.force_gbp = opts.force_gbp;
            if !g.load_rom(rom.clone()) {
                return Err("GBA core rejected the ROM".into());
            }
            g.arm_reset();
            if cfg.skip_bios && g.has_bios {
                g.skip_bios();
            }
            Console::Gba(g)
        } else {
            let mut g = Gb::new();
            g.model = gb_model_from_name(&cfg.gb_model);
            if !g.load_rom(rom.clone()) {
                return Err("GB core rejected the ROM".into());
            }
            if cfg.use_bios && !cfg.skip_bios {
                let cgb = rom.get(0x143).map_or(false, |&f| f & 0x80 != 0);
                let bios_path = if cgb { cfg.gbc_bios.as_ref() } else { cfg.gb_bios.as_ref() };
                if let Some(bios) = bios_path.and_then(|p| std::fs::read(p).ok()) {
                    g.memory.bios = Some(bios);
                }
            }
            g.sm83_reset();
            Console::Gb(g)
        };

        let save_path = opts.save_path.clone().unwrap_or_else(|| save_path_for(path, cfg.save_player));
        let mut s = Session {
            console,
            rom_path: path.to_path_buf(),
            rom_image: rom,
            save_path,
            temporary_save: opts.temporary_save,
            last_slot: 1,
            backup_load: None,
            backup_save: None,
            rewind: None,
            rewind_tick: 0,
            solar_level: 0,
            video_log: false,
        };
        s.set_rewind(cfg.rewind_enable, cfg.rewind_capacity);
        s.set_frameskip(cfg.frameskip);
        if let Ok(data) = std::fs::read(&s.save_path) {
            s.load_savedata(&data);
        }
        Ok(s)
    }

    /// "Boot BIOS": run the GBA BIOS with no cartridge inserted.
    pub fn boot_bios(bios_path: &Path, cfg: &Config) -> Result<Session, String> {
        let bios = std::fs::read(bios_path).map_err(|e| format!("{}: {e}", bios_path.display()))?;
        let mut g = Gba::new();
        rgba_gba::bios::bios_load(&mut g, &bios);
        if !g.has_bios {
            return Err("Not a valid GBA BIOS".into());
        }
        g.arm_reset();
        let mut s = Session {
            console: Console::Gba(g),
            rom_path: bios_path.to_path_buf(),
            rom_image: Vec::new(),
            save_path: PathBuf::new(),
            temporary_save: true,
            last_slot: 1,
            backup_load: None,
            backup_save: None,
            rewind: None,
            rewind_tick: 0,
            solar_level: 0,
            video_log: false,
        };
        s.set_rewind(cfg.rewind_enable, cfg.rewind_capacity);
        Ok(s)
    }

    /// "Replace ROM...": swap the cartridge image without resetting
    /// (Window::replaceROM → mCore::loadROM on the running core).
    pub fn replace_rom(&mut self, path: &Path) -> Result<(), String> {
        let rom = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let ok = match &mut self.console {
            Console::Gba(g) => Gba::is_rom(&rom) && g.load_rom(rom.clone()),
            Console::Gb(g) => Gb::is_rom(&rom) && g.load_rom(rom.clone()),
        };
        if !ok {
            return Err("ROM is not for the running console".into());
        }
        self.rom_image = rom;
        Ok(())
    }

    /// "Yank game pak" (GBAYankROM / GBYankROM).
    pub fn yank(&mut self) {
        match &mut self.console {
            Console::Gba(g) => g.yank_rom(),
            Console::Gb(g) => g.yank_rom(),
        }
    }

    pub fn title(&self) -> String {
        self.rom_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn core(&mut self) -> &mut dyn Core {
        self.console.core()
    }

    // ---- Battery saves ----------------------------------------------------

    /// Copy a .sav image into the core (GBASavedataLoad / GBLoadSave).
    pub fn load_savedata(&mut self, data: &[u8]) {
        match &mut self.console {
            Console::Gba(g) => {
                if g.savedata.data.is_empty() {
                    // Autodetect carts: pick the chip from the file size,
                    // the same sizes mGBA's savedata loader accepts.
                    let t = match data.len() {
                        512 => Some(SavedataType::Eeprom512),
                        8192 => Some(SavedataType::Eeprom),
                        32768 => Some(SavedataType::Sram),
                        65536 => Some(SavedataType::Flash512),
                        131072 => Some(SavedataType::Flash1M),
                        _ => None,
                    };
                    if let Some(t) = t {
                        g.savedata_force_type(t);
                    }
                }
                let n = g.savedata.data.len().min(data.len());
                g.savedata.data[..n].copy_from_slice(&data[..n]);
                g.savedata.dirty = 0;
            }
            Console::Gb(g) => {
                let n = g.memory.sram.len().min(data.len());
                g.memory.sram[..n].copy_from_slice(&data[..n]);
                if let Some(len) = rgba_gb::mbc::rtc::rtc_suffix_len(g) {
                    if data.len() >= n + len {
                        rgba_gb::mbc::rtc::rtc_load_bytes(g, &data[n..n + len]);
                    }
                }
                g.sram_dirty = 0;
            }
        }
    }

    pub fn savedata_bytes(&mut self) -> Option<Vec<u8>> {
        match &mut self.console {
            Console::Gba(g) => (!g.savedata.data.is_empty()).then(|| g.savedata.data.clone()),
            Console::Gb(g) => {
                let mut out = g.memory.sram.clone();
                if out.is_empty() {
                    return None;
                }
                if let Some(rtc) = rgba_gb::mbc::rtc::rtc_save_bytes(g) {
                    out.extend_from_slice(&rtc);
                }
                Some(out)
            }
        }
    }

    fn savedata_dirty(&self) -> bool {
        match &self.console {
            Console::Gba(g) => g.savedata.dirty != 0,
            Console::Gb(g) => g.sram_dirty != 0,
        }
    }

    /// Write the battery save to disk if the game changed it.
    pub fn flush_savedata(&mut self, force: bool) {
        if !(force || self.savedata_dirty()) {
            return;
        }
        if let Console::Gb(g) = &mut self.console {
            g.synchronize_savedata();
        }
        if let Console::Gba(g) = &mut self.console {
            g.savedata.dirty = 0;
        }
        if self.temporary_save {
            return;
        }
        if let Some(bytes) = self.savedata_bytes() {
            if let Err(e) = std::fs::write(&self.save_path, bytes) {
                eprintln!("failed to write {}: {e}", self.save_path.display());
            }
        }
    }

    // ---- Save states --------------------------------------------------------

    pub fn state_path(&self, slot: u32) -> PathBuf {
        self.rom_path.with_extension(format!("ss{slot}"))
    }

    pub fn save_state_bytes(&mut self) -> Result<Vec<u8>, String> {
        let mut buf = Vec::new();
        self.core().save_state(&mut buf).map_err(str::to_string)?;
        Ok(buf)
    }

    pub fn load_state_bytes(&mut self, state: &[u8]) -> Result<(), String> {
        let backup = self.save_state_bytes().ok();
        self.core().load_state(state).map_err(str::to_string)?;
        self.backup_load = backup;
        Ok(())
    }

    pub fn save_state_file(&mut self, path: &Path) -> Result<(), String> {
        let state = self.save_state_bytes()?;
        self.backup_save = Some((path.to_path_buf(), std::fs::read(path).ok()));
        std::fs::write(path, state).map_err(|e| e.to_string())
    }

    pub fn load_state_file(&mut self, path: &Path) -> Result<(), String> {
        let state = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.load_state_bytes(&state)
    }

    pub fn save_slot(&mut self, slot: u32) -> Result<(), String> {
        self.last_slot = slot;
        let path = self.state_path(slot);
        self.save_state_file(&path)
    }

    pub fn load_slot(&mut self, slot: u32) -> Result<(), String> {
        self.last_slot = slot;
        let path = self.state_path(slot);
        self.load_state_file(&path)
    }

    pub fn undo_load(&mut self) -> Result<(), String> {
        let state = self.backup_load.take().ok_or("nothing to undo")?;
        self.core().load_state(&state).map_err(str::to_string)
    }

    pub fn undo_save(&mut self) -> Result<(), String> {
        let (path, old) = self.backup_save.take().ok_or("nothing to undo")?;
        match old {
            Some(bytes) => std::fs::write(&path, bytes).map_err(|e| e.to_string()),
            None => std::fs::remove_file(&path).map_err(|e| e.to_string()),
        }
    }

    // ---- Rewind -------------------------------------------------------------

    pub fn set_rewind(&mut self, enable: bool, capacity: usize) {
        self.rewind = enable.then(|| RewindContext::new(capacity.max(1)));
    }

    /// Called once per emulated frame (mCoreRewindAppend on the interval).
    pub fn rewind_tick(&mut self, interval: u32) {
        if self.rewind.is_none() {
            return;
        }
        self.rewind_tick += 1;
        if self.rewind_tick >= interval.max(1) {
            self.rewind_tick = 0;
            let Session { rewind, console, .. } = self;
            rewind.as_mut().unwrap().append(console.core());
        }
    }

    pub fn rewind_step(&mut self, frames: usize) -> bool {
        let Session { rewind, console, .. } = self;
        match rewind.as_mut() {
            Some(r) => r.restore(console.core(), frames),
            None => false,
        }
    }

    // ---- Misc controls ------------------------------------------------------

    pub fn set_frameskip(&mut self, n: u32) {
        match &mut self.console {
            Console::Gba(g) => g.video.frameskip = n as i32,
            Console::Gb(g) => g.video.frameskip = n as i32,
        }
    }

    pub fn layer_names(&self) -> &'static [&'static str] {
        match &self.console {
            Console::Gba(_) => &GBA_LAYERS,
            Console::Gb(_) => &GB_LAYERS,
        }
    }

    pub fn channel_names(&self) -> &'static [&'static str] {
        match &self.console {
            Console::Gba(_) => &GBA_CHANNELS,
            Console::Gb(_) => &GB_CHANNELS,
        }
    }

    /// Mutable "layer enabled" flags: `true` = visible.
    pub fn layer_enabled(&mut self, i: usize) -> Option<bool> {
        Some(match &mut self.console {
            Console::Gba(g) => {
                let sw = &g.video.sw;
                !match i {
                    0..=3 => sw.disable_bg[i],
                    4 => sw.disable_obj,
                    5 | 6 => sw.disable_win[i - 5],
                    7 => sw.disable_objwin,
                    _ => return None,
                }
            }
            Console::Gb(g) => {
                let v = &g.video;
                !match i {
                    0 => v.disable_bg,
                    1 => v.disable_win,
                    2 => v.disable_obj,
                    _ => return None,
                }
            }
        })
    }

    pub fn set_layer_enabled(&mut self, i: usize, on: bool) {
        match &mut self.console {
            Console::Gba(g) => {
                let sw = &mut g.video.sw;
                match i {
                    0..=3 => sw.disable_bg[i] = !on,
                    4 => sw.disable_obj = !on,
                    5 | 6 => sw.disable_win[i - 5] = !on,
                    7 => sw.disable_objwin = !on,
                    _ => {}
                }
                // Force a full redraw so the change shows on a paused frame.
                sw.scanline_dirty = [0xFFFF_FFFF; 5];
            }
            Console::Gb(g) => match i {
                0 => g.video.disable_bg = !on,
                1 => g.video.disable_win = !on,
                2 => g.video.disable_obj = !on,
                _ => {}
            },
        }
    }

    pub fn channel_enabled(&mut self, i: usize) -> bool {
        match &mut self.console {
            Console::Gba(g) => match i {
                0..=3 => !g.audio.psg.force_disable_ch[i],
                4 => !g.audio.force_disable_ch_a,
                _ => !g.audio.force_disable_ch_b,
            },
            Console::Gb(g) => !g.audio.force_disable_ch[i.min(3)],
        }
    }

    pub fn set_channel_enabled(&mut self, i: usize, on: bool) {
        match &mut self.console {
            Console::Gba(g) => match i {
                0..=3 => g.audio.psg.force_disable_ch[i] = !on,
                4 => g.audio.force_disable_ch_a = !on,
                _ => g.audio.force_disable_ch_b = !on,
            },
            Console::Gb(g) => g.audio.force_disable_ch[i.min(3)] = !on,
        }
    }

    /// Solar sensor level 0..=10 (InputController luminance level): the GPIO
    /// light sensor reads 0xFF - lux, so level 0 (dark) is 0xFF.
    pub fn set_solar_level(&mut self, level: i32) {
        self.solar_level = level.clamp(0, 10);
        if let Console::Gba(g) = &mut self.console {
            g.light_sensor_level = if self.solar_level == 0 {
                0xFF
            } else {
                0xFF - LUX_LEVELS[self.solar_level as usize - 1]
            };
        }
    }
}

/// `<rom>.sav`, or `<rom>.saN` for "Use player N save game" with N > 1
/// (mCoreAutoloadSave's savePlayerId handling).
pub fn save_path_for(rom: &Path, player: u32) -> PathBuf {
    if player > 1 {
        rom.with_extension(format!("sa{player}"))
    } else {
        rom.with_extension("sav")
    }
}
