// "ROM info..." — mirrors mGBA Qt's ROMInfo.cpp: title, game code, maker,
// version, ROM size, CRC32 / MD5 / SHA-1, plus platform and save type.
// `game_ident` (mGBAGetGameInfo: GBAGetGameInfo / GBGetGameInfo) is shared
// with the bug report and the overrides view.

use eframe::egui;

use super::{no_game, ToolCtx, ToolWindow};
use crate::emu::{Console, Session};

/// mGameInfo + the extra fields ROMInfo shows.
#[derive(Clone, Default)]
pub struct GameIdent {
    pub system: String,
    pub title: String,
    pub code: String,
    pub maker: String,
    pub version: u8,
    pub crc32: u32,
    pub rom_size: usize,
    pub save_type: String,
}

impl GameIdent {
    /// Key used for per-game settings: the game code when the cart has one
    /// (mGBA keys GBA overrides by code), else the CRC32.
    pub fn key(&self) -> String {
        if self.code.trim().is_empty() {
            format!("crc:{:08X}", self.crc32)
        } else {
            format!("{}:{}", self.system, self.code)
        }
    }
}

fn ascii(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    bytes[..end]
        .iter()
        .map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '?' })
        .collect::<String>()
        .trim_end()
        .to_string()
}

pub fn game_ident(s: &mut Session) -> GameIdent {
    match &mut s.console {
        Console::Gba(g) => {
            let rom = &g.memory.rom;
            let mut id = GameIdent {
                system: "AGB".into(),
                crc32: g.rom_crc32,
                rom_size: g.memory.rom_size,
                save_type: format!("{:?}", g.savedata.savedata_type),
                ..Default::default()
            };
            if rom.len() >= 0xBD {
                id.title = ascii(&rom[0xA0..0xAC]);
                id.code = ascii(&rom[0xAC..0xB0]);
                id.maker = ascii(&rom[0xB0..0xB2]);
                id.version = rom[0xBC];
            } else {
                id.title = "(BIOS)".into();
            }
            id
        }
        Console::Gb(g) => {
            let rom = &g.memory.rom;
            let mut id = GameIdent {
                system: "DMG".into(),
                crc32: g.rom_crc32,
                rom_size: rom.len(),
                save_type: format!("{:?}, {} bytes SRAM", g.memory.mbc_type, g.memory.sram.len()),
                ..Default::default()
            };
            if rom.len() >= 0x150 {
                let cart = &rom[0x100..0x150];
                if cart[0x43] == 0xC0 {
                    id.system = "CGB".into();
                }
                if cart[0x4B] != 0x33 {
                    id.title = ascii(&cart[0x34..0x44]);
                    id.maker = format!("{:02X}", cart[0x4B]);
                } else {
                    id.title = ascii(&cart[0x34..0x3F]);
                    id.code = ascii(&cart[0x3F..0x43]);
                    id.maker = ascii(&cart[0x44..0x46]);
                }
                id.version = cart[0x4C];
            }
            id
        }
    }
}

#[derive(Default)]
pub struct RomInfo {
    /// (crc32 of the ROM the hashes belong to, md5 hex, sha1 hex)
    hashes: Option<(u32, String, String)>,
}

impl ToolWindow for RomInfo {
    fn title(&self) -> &'static str {
        "ROM info"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        egui::Window::new(self.title()).open(open).resizable(false).show(ctx, |ui| {
            let Some(s) = tc.session.as_deref_mut() else {
                no_game(ui);
                return;
            };
            let id = game_ident(s);
            if self.hashes.as_ref().map_or(true, |h| h.0 != id.crc32) {
                let rom: &[u8] = match &s.console {
                    Console::Gba(g) => &g.memory.rom[..g.memory.rom_size.min(g.memory.rom.len())],
                    Console::Gb(g) => &g.memory.rom,
                };
                self.hashes = Some((id.crc32, hex(&md5(rom)), hex(&sha1(rom))));
            }
            let (_, md5_hex, sha1_hex) = self.hashes.clone().unwrap_or_default();
            egui::Grid::new("rominfo").num_columns(2).striped(true).show(ui, |ui| {
                let row = |ui: &mut egui::Ui, k: &str, v: String| {
                    ui.label(k);
                    ui.add(egui::Label::new(egui::RichText::new(v).monospace()).selectable(true));
                    ui.end_row();
                };
                row(ui, "Game name:", id.title.clone());
                row(ui, "Game ID:", if id.code.is_empty() { "(unknown)".into() } else { id.code.clone() });
                row(ui, "Maker:", id.maker.clone());
                row(ui, "Version:", id.version.to_string());
                row(ui, "Platform:", id.system.clone());
                row(ui, "File size:", format!("{} bytes", id.rom_size));
                row(ui, "Save type:", id.save_type.clone());
                row(ui, "CRC32:", format!("{:08x}", id.crc32));
                row(ui, "MD5:", md5_hex);
                row(ui, "SHA-1:", sha1_hex);
                row(ui, "File:", s.rom_path.display().to_string());
            });
        });
    }

    fn on_session_changed(&mut self) {
        self.hashes = None;
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// MD5 (RFC 1321).
pub fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32).collect();
    let (mut a0, mut b0, mut c0, mut d0) = (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_le_bytes());
    for chunk in msg.chunks_exact(64) {
        let m: Vec<u32> = chunk.chunks_exact(4).map(|w| u32::from_le_bytes(w.try_into().unwrap())).collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    for (i, v) in [a0, b0, c0, d0].iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}

/// SHA-1 (FIPS 180-1).
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i / 20 {
                0 => ((b & c) | (!b & d), 0x5A827999),
                1 => (b ^ c ^ d, 0x6ED9EBA1),
                2 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6u32),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, v) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(v);
        }
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn digests() {
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
    }
}
