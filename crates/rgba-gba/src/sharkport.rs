// Copyright (c) 2013-2021 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gba/sharkport.c and
// include/mgba/internal/gba/sharkport.h.
//
// SharkPort (.sps/.xps) and GameShark (.gsv) savedata containers for the
// GBA. These formats wrap a raw battery save; there is no compression and
// no emulator savestate is produced. The importer copies the payload into
// the running core's savedata (with the EEPROM 8-byte-group byte
// reversal), the exporter writes it back out.
//
// Deltas from the C (all documented, behavior preserved):
// - `struct VFile*` I/O becomes byte slices in / `Vec<u8>` out.
// - The C's host-endian LOAD_32/STORE_32 of the length/checksum words is
//   fixed to little-endian; all platforms mGBA runs on are LE, so every
//   real-world SharkPort/GSV file is LE.
// - The custom additive checksum replicates the C's shifts exactly,
//   including the signedness asymmetry: verification zero-extends the
//   0x1C ident header (uint8_t) but sign-extends the payload (int8_t*),
//   while export sign-extends both (char buffers on x86).
// - `_importSavedata`'s trailing `vf->sync` becomes setting
//   `savedata.dirty`; savedata persistence is frontend-driven in this
//   port (see the note in gba.rs).
// - The exporter's date field renders the caller-provided unix timestamp
//   in UTC; mGBA uses localtime(). The field is skipped, not parsed, on
//   import.
// - `GBASavedataGSVGetPayload` here rejects negative sizes (a C
//   crash-bait path), not just zero/oversized ones.

use crate::gba::Gba;
use crate::savedata::{
    SavedataType, GBA_SIZE_EEPROM, GBA_SIZE_EEPROM512, GBA_SIZE_FLASH1M, GBA_SIZE_FLASH512,
    GBA_SIZE_SRAM,
};

pub const SHARKPORT_HEADER: &[u8] = b"SharkPortSave";
pub const GSV_HEADER: &[u8] = b"ADVSAVEG";
pub const GSV_FOOTER: &[u8] = b"xV4\x12";
pub const GSV_IDENT_OFFSET: usize = 0xC;
pub const GSV_PAYLOAD_OFFSET: usize = 0x430;

/// Size of the cartridge-identity block inside a SharkPort payload
/// (`uint8_t header[0x1C]` in the C).
pub const SHARKPORT_IDENT_LEN: usize = 0x1C;

// ---------------------------------------------------------------------------
// Little-endian cursor over the container, standing in for VFile seeks/reads.
// Seek failures and short reads map to the C's error exits.
// ---------------------------------------------------------------------------

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, pos: 0 }
    }

    /// LOAD_32 (host-endian in the C; LE in practice).
    fn u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_le_bytes(b.try_into().unwrap()))
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let b = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(b)
    }

    /// vf->seek(vf, size, SEEK_CUR): may move backwards within the file.
    fn skip(&mut self, n: i32) -> Option<()> {
        let pos = self.pos as i64 + n as i64;
        if pos < 0 || pos > self.bytes.len() as i64 {
            return None;
        }
        self.pos = pos as usize;
        Some(())
    }
}

/// The SharkPort checksum: a running sum of each byte shifted left by the
/// running sum mod 24. `byte` is pre-extended by the caller so the C's
/// int8/uint8 asymmetries are explicit at the call sites.
fn checksum_step(checksum: u32, byte: i32) -> u32 {
    checksum.wrapping_add((byte as u32) << (checksum % 24))
}

// ---------------------------------------------------------------------------
// SharkPortSave container
// ---------------------------------------------------------------------------

/// GBASavedataSharkPortPayloadSize: walk the header and leave the cursor
/// positioned after the payload-size word. Returns the payload size
/// (including the 0x1C ident), or None on any parse failure.
fn sharkport_payload_size_inner(r: &mut Reader) -> Option<i32> {
    let size = r.u32()? as i32;
    if size != SHARKPORT_HEADER.len() as i32 {
        return None;
    }
    if r.take(size as usize)? != SHARKPORT_HEADER {
        return None;
    }
    let size = r.u32()?;
    if size != 0x000F0000 {
        // What is this value?
        return None;
    }

    // Skip first three fields (title, date, comment)
    for _ in 0..3 {
        let size = r.u32()? as i32;
        r.skip(size)?;
    }

    // Read payload size
    let size = r.u32()? as i32;
    Some(size)
}

/// GBASavedataSharkPortPayloadSize. Returns 0 on invalid containers, like
/// the C.
pub fn savedata_sharkport_payload_size(bytes: &[u8]) -> i32 {
    let mut r = Reader::new(bytes);
    sharkport_payload_size_inner(&mut r).unwrap_or(0)
}

/// GBASavedataSharkPortGetPayload. On success returns the raw payload
/// (EEPROM image still 8-byte-group byte-reversed, exactly as stored) and
/// the 0x1C cartridge-identity block. With `test_checksum`, the trailing
/// checksum is verified over the ident header (zero-extended) and payload
/// (sign-extended), mirroring the C.
pub fn savedata_sharkport_get_payload(
    bytes: &[u8],
    test_checksum: bool,
) -> Option<(Vec<u8>, [u8; SHARKPORT_IDENT_LEN])> {
    let mut r = Reader::new(bytes);
    let size = sharkport_payload_size_inner(&mut r)?;
    if size < SHARKPORT_IDENT_LEN as i32 || size > (GBA_SIZE_FLASH1M + SHARKPORT_IDENT_LEN) as i32 {
        return None;
    }
    let size = size as usize - SHARKPORT_IDENT_LEN;

    let mut header = [0u8; SHARKPORT_IDENT_LEN];
    header.copy_from_slice(r.take(SHARKPORT_IDENT_LEN)?);
    let payload = r.take(size)?.to_vec();
    let checksum = r.u32()?;

    if test_checksum {
        let mut calc = 0u32;
        for &b in header.iter() {
            // uint8_t header[] in the C: zero-extended
            calc = checksum_step(calc, b as i32);
        }
        for &b in payload.iter() {
            // int8_t* payload in the C: sign-extended
            calc = checksum_step(calc, b as i8 as i32);
        }
        if calc != checksum {
            return None;
        }
    }
    Some((payload, header))
}

// ---------------------------------------------------------------------------
// GameShark "ADVSAVEG" (.gsv) container
// ---------------------------------------------------------------------------

/// GBASavedataGSVPayloadSize. Returns 0 on invalid containers, like the C.
pub fn savedata_gsv_payload_size(bytes: &[u8]) -> i32 {
    // magic (8) + checksum (4) + header struct (0x424) = GSV_PAYLOAD_OFFSET
    let header = match bytes.get(..GSV_PAYLOAD_OFFSET) {
        Some(h) => h,
        None => return 0,
    };
    if header[..8] != *GSV_HEADER {
        return 0;
    }
    // The checksum word at 0x8 is skipped, like the C.
    if header[0x42C..0x430] != *GSV_FOOTER {
        return 0;
    }
    // struct: name[12] @0xC, padding @0x18, type @0x1C, unk[3] @0x20,
    // description[0x400] @0x2C, footer[4] @0x42C.
    let gsv_type = u32::from_le_bytes(header[0x1C..0x20].try_into().unwrap()) as i32;
    match gsv_type {
        2 => GBA_SIZE_SRAM as i32,
        3 => GBA_SIZE_EEPROM512 as i32,
        4 => GBA_SIZE_EEPROM as i32,
        5 => GBA_SIZE_FLASH512 as i32,
        6 => GBA_SIZE_FLASH1M as i32, // Unconfirmed
        _ => bytes.len() as i32 - GSV_PAYLOAD_OFFSET as i32,
    }
}

/// GBASavedataGSVGetPayload. Returns the payload and the 12-byte cartridge
/// title ident. `testChecksum` exists in the C signature but is unused
/// (the checksum format is unknown).
pub fn savedata_gsv_get_payload(bytes: &[u8]) -> Option<(Vec<u8>, [u8; 12])> {
    let size = savedata_gsv_payload_size(bytes);
    // The C only rejects 0 and > FLASH1M; we also reject the negative sizes
    // its unchecked size_t subtraction can produce on short files.
    if size <= 0 || size > GBA_SIZE_FLASH1M as i32 {
        return None;
    }
    let mut ident = [0u8; 12];
    ident.copy_from_slice(bytes.get(GSV_IDENT_OFFSET..GSV_IDENT_OFFSET + 12)?);
    let payload = bytes
        .get(GSV_PAYLOAD_OFFSET..GSV_PAYLOAD_OFFSET + size as usize)?
        .to_vec();
    Some((payload, ident))
}

// ---------------------------------------------------------------------------
// GBA-side import/export
// ---------------------------------------------------------------------------

impl Gba {
    /// The 0x1C cartridge-identity block the C builds from
    /// `(struct GBACartridge*) gba->memory.rom`: title[12]+id at 0xA0,
    /// two zeros, header checksum (0xBD), maker low byte (0xB0), 1, zeros.
    fn sharkport_ident(&self) -> Option<[u8; SHARKPORT_IDENT_LEN]> {
        let rom = &self.memory.rom;
        if rom.len() < 0xBE {
            return None;
        }
        let mut buf = [0u8; SHARKPORT_IDENT_LEN];
        buf[..0x10].copy_from_slice(&rom[0xA0..0xB0]);
        buf[0x12] = rom[0xBD];
        buf[0x13] = rom[0xB0]; // u16 maker truncated into a u8, as in the C
        buf[0x14] = 1;
        Some(buf)
    }

    /// _importSavedata. Copies `payload` into the live savedata, clamping
    /// to the configured save type (with the C's Flash512→Flash1M
    /// promotion) and applying the EEPROM 8-byte-group byte reversal.
    fn import_savedata_payload(&mut self, payload: &[u8]) -> bool {
        let mut size = payload.len();
        match self.savedata.savedata_type {
            SavedataType::Flash512 => {
                if size > GBA_SIZE_FLASH512 {
                    self.savedata_force_type(SavedataType::Flash1M);
                }
                // Fall through
                size = size.min(self.savedata.size());
            }
            SavedataType::Autodetect | SavedataType::ForceNone => return false,
            _ => size = size.min(self.savedata.size()),
        }
        if self.savedata.data.len() < size {
            // Unreachable once savedata_init_* has run; the C's fixed-size
            // allocations make this a non-issue there.
            return false;
        }

        if size == GBA_SIZE_EEPROM || size == GBA_SIZE_EEPROM512 {
            for i in (0..size).step_by(8) {
                let lo = u32::from_be_bytes(payload[i..i + 4].try_into().unwrap());
                let hi = u32::from_be_bytes(payload[i + 4..i + 8].try_into().unwrap());
                self.savedata.data[i..i + 4].copy_from_slice(&hi.to_le_bytes());
                self.savedata.data[i + 4..i + 8].copy_from_slice(&lo.to_le_bytes());
            }
        } else {
            self.savedata.data[..size].copy_from_slice(&payload[..size]);
        }
        // C: gba->memory.savedata.vf->sync(...). Persistence here is
        // frontend-driven via the dirty flag.
        self.savedata.dirty |= 1;
        true
    }

    /// GBASavedataImportSharkPort. With `test_checksum`, the container
    /// checksum is verified and the full 0x1C identity block must match
    /// the loaded ROM; otherwise only the first 0xF bytes (title + 3 id
    /// bytes) are compared, per the C's `testChecksum ? 0x1C : 0xF`.
    pub fn savedata_import_sharkport(&mut self, bytes: &[u8], test_checksum: bool) -> bool {
        let (payload, header) = match savedata_sharkport_get_payload(bytes, test_checksum) {
            Some(p) => p,
            None => return false,
        };
        let ident = match self.sharkport_ident() {
            Some(i) => i,
            None => return false,
        };
        let cmp_len = if test_checksum { SHARKPORT_IDENT_LEN } else { 0xF };
        if ident[..cmp_len] != header[..cmp_len] {
            return false;
        }

        self.import_savedata_payload(&payload)
    }

    /// GBASavedataImportGSV. The ident (12 title bytes) must match the
    /// loaded ROM's cart title.
    pub fn savedata_import_gsv(&mut self, bytes: &[u8], test_checksum: bool) -> bool {
        let _ = test_checksum; // The checksum format is currently unknown (C: UNUSED)
        let (payload, ident) = match savedata_gsv_get_payload(bytes) {
            Some(p) => p,
            None => return false,
        };
        let rom = &self.memory.rom;
        if rom.len() < 0xAC || ident[..] != rom[0xA0..0xAC] {
            return false;
        }

        self.import_savedata_payload(&payload)
    }

    /// GBASavedataExportSharkPort. Serializes the current savedata into a
    /// SharkPort container. `now_unix` fills the date field
    /// ("%m/%d/%Y %I:%M:%S %p", rendered in UTC here rather than
    /// localtime). Returns None when there is no savedata to export.
    pub fn savedata_export_sharkport(&self, now_unix: i64) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        out.extend_from_slice(&(SHARKPORT_HEADER.len() as u32).to_le_bytes());
        out.extend_from_slice(SHARKPORT_HEADER);
        out.extend_from_slice(&0x000F0000u32.to_le_bytes());

        let rom = &self.memory.rom;
        if rom.len() < 0xB0 {
            return None;
        }
        // Field 1: cart title (sizeof(cart->title) == 12)
        out.extend_from_slice(&12u32.to_le_bytes());
        out.extend_from_slice(&rom[0xA0..0xAC]);

        // Field 2: date string
        let date = format_sharkport_date(now_unix);
        out.extend_from_slice(&(date.len() as u32).to_le_bytes());
        out.extend_from_slice(date.as_bytes());

        // Last field is blank
        out.extend_from_slice(&0u32.to_le_bytes());

        // Payload
        let mut size = SHARKPORT_IDENT_LEN + self.savedata.size();
        if size == SHARKPORT_IDENT_LEN {
            return None;
        }
        out.extend_from_slice(&(size as u32).to_le_bytes());
        size -= SHARKPORT_IDENT_LEN;

        let ident = self.sharkport_ident()?;
        out.extend_from_slice(&ident);

        let data = &self.savedata.data;
        if data.len() < size {
            return None;
        }
        let mut checksum = 0u32;
        for &b in ident.iter() {
            // char buffer in the C: sign-extended
            checksum = checksum_step(checksum, b as i8 as i32);
        }
        if self.savedata.savedata_type.is_eeprom() {
            for i in 0..size {
                let byte = data[i ^ 7];
                checksum = checksum_step(checksum, byte as i8 as i32);
                out.push(byte);
            }
        } else {
            for &b in data[..size].iter() {
                checksum = checksum_step(checksum, b as i8 as i32);
            }
            out.extend_from_slice(&data[..size]);
        }

        out.extend_from_slice(&checksum.to_le_bytes());
        Some(out)
    }
}

// ---------------------------------------------------------------------------
// Date field ("%m/%d/%Y %I:%M:%S %p") from a unix timestamp, in UTC.
// civil_from_days is Howard Hinnant's algorithm (public domain).
// ---------------------------------------------------------------------------

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn format_sharkport_date(unix: i64) -> String {
    let days = unix.div_euclid(86400);
    let secs = unix.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let h = (secs / 3600) as u32;
    let min = ((secs / 60) % 60) as u32;
    let s = (secs % 60) as u32;
    let h12 = match h % 12 {
        0 => 12,
        h => h,
    };
    let ampm = if h < 12 { "AM" } else { "PM" };
    format!("{:02}/{:02}/{:04} {:02}:{:02}:{:02} {}", m, d, y, h12, min, s, ampm)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gba::Gba;

    /// Synthetic 32KiB ROM whose GBACartridge header overlay is populated
    /// (title/id/maker/checksum at 0xA0/0xAC/0xB0/0xBD).
    fn fake_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0xA0..0xAC].copy_from_slice(b"SHARKPORTTST");
        rom[0xAC..0xB0].copy_from_slice(b"TSTG");
        rom[0xB0] = 0x62;
        rom[0xB1] = 0x69;
        rom[0xBD] = 0x5A;
        rom
    }

    fn gba_with(t: SavedataType) -> Box<Gba> {
        let mut gba = Gba::new();
        gba.load_rom(fake_rom());
        gba.savedata_force_type(t);
        gba
    }

    fn pattern(size: usize) -> Vec<u8> {
        (0..size).map(|i| (i as u8).wrapping_mul(31) ^ 0xA5).collect()
    }

    fn fill(gba: &mut Gba, data: &[u8]) {
        gba.savedata.data[..data.len()].copy_from_slice(data);
        gba.savedata.dirty = 0;
    }

    fn make_gsv(title: &[u8; 12], gsv_type: u32, payload: &[u8]) -> Vec<u8> {
        let mut v = GSV_HEADER.to_vec();
        v.extend_from_slice(&[0; 4]); // checksum slot
        v.extend_from_slice(title);
        v.extend_from_slice(&[0; 4]); // padding
        v.extend_from_slice(&gsv_type.to_le_bytes());
        v.extend_from_slice(&[0; 12]); // unk
        v.extend_from_slice(&[0; 0x400]); // description
        v.extend_from_slice(GSV_FOOTER);
        v.extend_from_slice(payload);
        v
    }

    // 2025-10-01 14:00:00 UTC
    const NOW: i64 = 1_759_327_200;

    #[test]
    fn date_field_format() {
        assert_eq!(format_sharkport_date(0), "01/01/1970 12:00:00 AM");
        assert_eq!(format_sharkport_date(NOW), "10/01/2025 02:00:00 PM");
    }

    #[test]
    fn payload_size_rejects_garbage() {
        assert_eq!(savedata_sharkport_payload_size(&[]), 0);
        assert_eq!(savedata_sharkport_payload_size(b"\x0D\0\0\0SharkPortSav"), 0);
        let mut bad = Vec::new();
        bad.extend_from_slice(&12u32.to_le_bytes()); // wrong header length
        bad.extend_from_slice(SHARKPORT_HEADER);
        assert_eq!(savedata_sharkport_payload_size(&bad), 0);
        // Correct magic but truncated afterwards
        let mut short = Vec::new();
        short.extend_from_slice(&13u32.to_le_bytes());
        short.extend_from_slice(SHARKPORT_HEADER);
        assert_eq!(savedata_sharkport_payload_size(&short), 0);
    }

    #[test]
    fn sram_export_parses_and_verifies() {
        let mut gba = gba_with(SavedataType::Sram);
        let save = pattern(GBA_SIZE_SRAM);
        fill(&mut gba, &save);

        let blob = gba.savedata_export_sharkport(NOW).expect("export");
        assert_eq!(
            savedata_sharkport_payload_size(&blob),
            (SHARKPORT_IDENT_LEN + GBA_SIZE_SRAM) as i32
        );
        let (payload, header) =
            savedata_sharkport_get_payload(&blob, true).expect("payload with checksum");
        assert_eq!(payload, save);
        assert_eq!(header, gba.sharkport_ident().unwrap());
    }

    #[test]
    fn sram_import_roundtrip() {
        let mut src = gba_with(SavedataType::Sram);
        let save = pattern(GBA_SIZE_SRAM);
        fill(&mut src, &save);
        let blob = src.savedata_export_sharkport(NOW).unwrap();

        let mut dst = gba_with(SavedataType::Sram);
        assert!(dst.savedata.data.iter().all(|&b| b == 0xFF));
        assert!(dst.savedata_import_sharkport(&blob, true));
        assert_eq!(dst.savedata.data, save);
        assert_ne!(dst.savedata.dirty, 0); // frontend persistence hook
    }

    #[test]
    fn eeprom_export_reverses_and_import_restores() {
        let mut src = gba_with(SavedataType::Eeprom512);
        let save = pattern(GBA_SIZE_EEPROM512);
        fill(&mut src, &save);
        let blob = src.savedata_export_sharkport(NOW).unwrap();

        // The stored payload is each 8-byte group reversed.
        let (payload, _) = savedata_sharkport_get_payload(&blob, true).unwrap();
        let mut reversed = vec![0u8; save.len()];
        for i in 0..save.len() {
            reversed[i] = save[i ^ 7];
        }
        assert_eq!(payload, reversed);

        let mut dst = gba_with(SavedataType::Eeprom512);
        assert!(dst.savedata_import_sharkport(&blob, true));
        assert_eq!(dst.savedata.data, save);
    }

    #[test]
    fn tampered_checksum_rejected() {
        let mut src = gba_with(SavedataType::Sram);
        fill(&mut src, &pattern(GBA_SIZE_SRAM));
        let mut blob = src.savedata_export_sharkport(NOW).unwrap();
        let last_payload_byte = blob.len() - 1 - 4;
        blob[last_payload_byte] ^= 0x01;

        assert!(savedata_sharkport_get_payload(&blob, true).is_none());
        // Unchecked import still validates the identity block and applies.
        let payload_pos = last_payload_byte;
        assert!(payload_pos > 0);
        let mut dst = gba_with(SavedataType::Sram);
        assert!(dst.savedata_import_sharkport(&blob, false));
    }

    #[test]
    fn identity_block_compare_length() {
        let mut src = gba_with(SavedataType::Sram);
        fill(&mut src, &pattern(GBA_SIZE_SRAM));
        let blob = src.savedata_export_sharkport(NOW).unwrap();
        // The ident block starts right before the payload: total len minus
        // checksum (4), payload (size), ident (0x1C).
        let ident_off = blob.len() - 4 - GBA_SIZE_SRAM - SHARKPORT_IDENT_LEN;

        // Byte 0 is inside the 0xF compared prefix: rejected even unchecked.
        let mut tampered = blob.clone();
        tampered[ident_off] ^= 0xFF;
        let mut dst = gba_with(SavedataType::Sram);
        assert!(!dst.savedata_import_sharkport(&tampered, false));

        // Byte 0x12 is only compared with test_checksum: slips through unchecked.
        let mut tampered = blob.clone();
        tampered[ident_off + 0x12] ^= 0xFF;
        assert!(dst.savedata_import_sharkport(&tampered, false));
        assert!(!dst.savedata_import_sharkport(&tampered, true));
    }

    #[test]
    fn wrong_rom_title_rejected() {
        let mut src = gba_with(SavedataType::Sram);
        fill(&mut src, &pattern(GBA_SIZE_SRAM));
        let blob = src.savedata_export_sharkport(NOW).unwrap();

        let mut dst = gba_with(SavedataType::Sram);
        dst.memory.rom[0xA0] = b'X';
        assert!(!dst.savedata_import_sharkport(&blob, false));
        assert!(!dst.savedata_import_sharkport(&blob, true));
    }

    #[test]
    fn flash512_promotes_to_flash1m() {
        let mut src = gba_with(SavedataType::Flash1M);
        let save = pattern(GBA_SIZE_FLASH1M);
        fill(&mut src, &save);
        let blob = src.savedata_export_sharkport(NOW).unwrap();
        assert_eq!(
            savedata_sharkport_payload_size(&blob),
            (SHARKPORT_IDENT_LEN + GBA_SIZE_FLASH1M) as i32
        );

        let mut dst = gba_with(SavedataType::Flash512);
        assert!(dst.savedata_import_sharkport(&blob, true));
        assert_eq!(dst.savedata.savedata_type, SavedataType::Flash1M);
        assert_eq!(dst.savedata.data, save);
    }

    #[test]
    fn undersized_payload_leaves_tail() {
        let mut src = gba_with(SavedataType::Sram); // 0x8000
        let save = pattern(GBA_SIZE_SRAM);
        fill(&mut src, &save);
        let blob = src.savedata_export_sharkport(NOW).unwrap();

        let mut dst = gba_with(SavedataType::Flash512); // 0x10000
        assert!(dst.savedata_import_sharkport(&blob, true));
        assert_eq!(dst.savedata.data[..GBA_SIZE_SRAM], save[..]);
        assert!(dst.savedata.data[GBA_SIZE_SRAM..].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn autodetect_and_none_rejected() {
        let mut src = gba_with(SavedataType::Sram);
        fill(&mut src, &pattern(GBA_SIZE_SRAM));
        let blob = src.savedata_export_sharkport(NOW).unwrap();

        let mut gba = Gba::new();
        gba.load_rom(fake_rom());
        assert!(!gba.savedata_import_sharkport(&blob, false));
        gba.savedata_force_type(SavedataType::ForceNone);
        assert!(!gba.savedata_import_sharkport(&blob, false));
    }

    #[test]
    fn truncated_payload_rejected() {
        let mut src = gba_with(SavedataType::Sram);
        fill(&mut src, &pattern(GBA_SIZE_SRAM));
        let blob = src.savedata_export_sharkport(NOW).unwrap();
        let cut = &blob[..blob.len() - 0x100];
        assert!(savedata_sharkport_get_payload(cut, false).is_none());
    }

    #[test]
    fn gsv_roundtrip() {
        let payload = pattern(GBA_SIZE_FLASH512);
        let blob = make_gsv(b"SHARKPORTTST", 5, &payload);
        assert_eq!(savedata_gsv_payload_size(&blob), GBA_SIZE_FLASH512 as i32);
        let (got, ident) = savedata_gsv_get_payload(&blob).unwrap();
        assert_eq!(got, payload);
        assert_eq!(&ident, b"SHARKPORTTST");

        let mut dst = gba_with(SavedataType::Flash512);
        assert!(dst.savedata_import_gsv(&blob, false));
        assert_eq!(dst.savedata.data, payload);
    }

    #[test]
    fn gsv_eeprom_type_picks_512_bytes() {
        let payload = pattern(GBA_SIZE_EEPROM512);
        let blob = make_gsv(b"SHARKPORTTST", 3, &payload);
        assert_eq!(savedata_gsv_payload_size(&blob), GBA_SIZE_EEPROM512 as i32);

        // Type 3 stores the payload already reversed like the C export path;
        // import undoes it.
        let mut dst = gba_with(SavedataType::Eeprom512);
        assert!(dst.savedata_import_gsv(&blob, false));
        let mut unreversed = vec![0u8; payload.len()];
        for i in 0..payload.len() {
            unreversed[i] = payload[i ^ 7];
        }
        assert_eq!(dst.savedata.data, unreversed);
    }

    #[test]
    fn gsv_default_size_from_file_length() {
        let payload = pattern(0x8000);
        let blob = make_gsv(b"SHARKPORTTST", 9, &payload);
        assert_eq!(savedata_gsv_payload_size(&blob), 0x8000);
        let (got, _) = savedata_gsv_get_payload(&blob).unwrap();
        assert_eq!(got, payload);
    }

    #[test]
    fn gsv_wrong_title_rejected() {
        let blob = make_gsv(b"SHARKPORTTST", 5, &pattern(GBA_SIZE_FLASH512));
        let mut dst = gba_with(SavedataType::Flash512);
        dst.memory.rom[0xA0] = b'X';
        assert!(!dst.savedata_import_gsv(&blob, false));
    }

    #[test]
    fn gsv_bad_footer_rejected() {
        let mut blob = make_gsv(b"SHARKPORTTST", 5, &pattern(GBA_SIZE_FLASH512));
        blob[0x42C] = b'y';
        assert_eq!(savedata_gsv_payload_size(&blob), 0);
        assert!(savedata_gsv_get_payload(&blob).is_none());
    }

    #[test]
    fn imported_savedata_and_savestates() {
        // mGBA's GBASerializedState carries savedata *control state* only,
        // never the battery bytes (those persist via the frontend, which is
        // why import sets `dirty`). Assert a core with imported savedata
        // serializes into a state our deserialize accepts, and that the
        // matching-type load leaves the imported data untouched.
        let mut src = gba_with(SavedataType::Sram);
        let save = pattern(GBA_SIZE_SRAM);
        fill(&mut src, &save);
        let blob = src.savedata_export_sharkport(NOW).unwrap();

        let mut dst = gba_with(SavedataType::Sram);
        assert!(dst.savedata_import_sharkport(&blob, true));
        assert_eq!(dst.savedata.data, save);

        let state = crate::serialize::serialize(&mut dst).expect("serialize");
        assert_eq!(state.len(), crate::serialize::GBA_SAVESTATE_SIZE);
        let mut restored = gba_with(SavedataType::Sram);
        crate::serialize::deserialize(&mut restored, &state).expect("deserialize");
        assert_eq!(restored.savedata.savedata_type, SavedataType::Sram);
        // Untouched by the state load (GBASavedataDeserialize ports no data).
        assert!(restored.savedata.data.iter().all(|&b| b == 0xFF));
    }
}
