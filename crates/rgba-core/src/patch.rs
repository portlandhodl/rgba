// Copyright (c) 2013-2014 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from src/util/patch.c, src/util/patch-ips.c and src/util/patch-ups.c:
// ROM patch identification (loadPatch) and application for IPS, UPS and BPS
// patch files.
//
// The C `struct Patch` (a VFile plus outputSize/applyPatch vtable) becomes an
// enum owning the patch bytes; the dispatch order in `Patch::load` mirrors
// loadPatch: IPS first, then UPS/BPS (loadPatchUPS also accepts BPS1).

/// IPS patches can grow a ROM up to 16MiB, but not beyond (_IPSOutputSize).
pub const IPS_OUTPUT_SIZE: usize = 16 * 1024 * 1024;

/// A parsed ROM patch (mGBA `struct Patch`).
pub enum Patch {
    Ips(Vec<u8>),
    Ups(Vec<u8>),
    Bps(Vec<u8>),
}

impl Patch {
    /// loadPatch / loadPatchIPS / loadPatchUPS: identify a patch by content.
    /// Tries IPS ("PATCH"..."EOF"), then UPS ("UPS1") / BPS ("BPS1"). For
    /// UPS/BPS the patch CRC32 (last 4 bytes, over the rest of the file) is
    /// verified at load time, exactly as in the C.
    pub fn load(bytes: &[u8]) -> Option<Patch> {
        // loadPatchIPS
        if bytes.len() >= 5 && &bytes[..5] == b"PATCH" && &bytes[bytes.len() - 3..] == b"EOF" {
            return Some(Patch::Ips(bytes.to_vec()));
        }
        // loadPatchUPS
        if bytes.len() >= 16 {
            let is_ups = &bytes[..4] == b"UPS1";
            let is_bps = &bytes[..4] == b"BPS1";
            if is_ups || is_bps {
                // fileCrc32 over everything but the trailing PATCH_CHECKSUM
                let good_crc32 = u32::from_le_bytes(bytes[bytes.len() - 4..].try_into().unwrap());
                if crc32(0, &bytes[..bytes.len() - 4]) != good_crc32 {
                    return None;
                }
                return Some(if is_ups {
                    Patch::Ups(bytes.to_vec())
                } else {
                    Patch::Bps(bytes.to_vec())
                });
            }
        }
        None
    }

    /// Patch::outputSize. Returns the size of the buffer `apply` needs, or
    /// None if the patch does not apply to an input of `in_size` bytes (the C
    /// returns 0 in that case). IPS always reports 16MiB.
    pub fn output_size(&self, in_size: usize) -> Option<usize> {
        match self {
            Patch::Ips(_) => Some(IPS_OUTPUT_SIZE),
            Patch::Ups(patch) | Patch::Bps(patch) => {
                // _UPSOutputSize: the recorded input size must match.
                let mut pos = 4;
                if decode_length(patch, &mut pos) != in_size as u64 {
                    return None;
                }
                Some(decode_length(patch, &mut pos) as usize)
            }
        }
    }

    /// Patch::applyPatch: apply to `in_data`, writing into `out` (sized per
    /// `output_size`). Returns false on any failure, like the C.
    pub fn apply(&self, in_data: &[u8], out: &mut [u8]) -> bool {
        match self {
            Patch::Ips(patch) => ips_apply(patch, in_data, out),
            Patch::Ups(patch) => ups_apply(patch, in_data, out),
            Patch::Bps(patch) => bps_apply(patch, in_data, out),
        }
    }
}

/// crc32 / doCrc32 (mgba-util/crc32.c): CRC-32/ISO-HDLC, chainable
/// (doCrc32(buf) == crc32(0, buf)).
pub(crate) fn crc32(crc: u32, data: &[u8]) -> u32 {
    let mut crc = !crc;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// _decodeLength: variable-length size field. Wraps like C's size_t on
/// overlong runs. Reads directly from the slice; on EOF mid-value it returns
/// the partially decoded value (the C's unbuffered path; the buffered path's
/// "return 0" case is unreachable-ish there and nets the same failures).
fn decode_length(bytes: &[u8], pos: &mut usize) -> u64 {
    let mut shift: u64 = 1;
    let mut value: u64 = 0;
    loop {
        if *pos >= bytes.len() {
            return value;
        }
        let byte = bytes[*pos];
        *pos += 1;
        value = value.wrapping_add((byte as u64 & 0x7F).wrapping_mul(shift));
        if byte & 0x80 != 0 {
            break;
        }
        shift <<= 7;
        value = value.wrapping_add(shift);
    }
    value
}

/// _IPSApplyPatch.
fn ips_apply(patch: &[u8], in_data: &[u8], out: &mut [u8]) -> bool {
    let out_size = out.len();
    let n = in_data.len().min(out_size);
    out[..n].copy_from_slice(&in_data[..n]);

    let mut pos = 5; // seek(5, SEEK_SET)
    loop {
        if pos + 3 > patch.len() {
            return false;
        }
        if &patch[pos..pos + 3] == b"EOF" {
            return true;
        }
        let offset =
            u32::from_be_bytes([0, patch[pos], patch[pos + 1], patch[pos + 2]]) as usize;
        pos += 3;
        if pos + 2 > patch.len() {
            return false;
        }
        let size = u16::from_be_bytes([patch[pos], patch[pos + 1]]) as usize;
        pos += 2;
        if size == 0 {
            // RLE chunk
            if pos + 3 > patch.len() {
                return false;
            }
            let rle_size = u16::from_be_bytes([patch[pos], patch[pos + 1]]) as usize;
            pos += 2;
            let byte = patch[pos];
            pos += 1;
            match offset.checked_add(rle_size) {
                Some(end) if end <= out_size => out[offset..end].fill(byte),
                _ => return false,
            }
        } else {
            match offset.checked_add(size) {
                Some(end) if end <= out_size => {
                    if pos + size > patch.len() {
                        return false;
                    }
                    out[offset..end].copy_from_slice(&patch[pos..pos + size]);
                    pos += size;
                }
                _ => return false,
            }
        }
    }
}

/// Checksum offsets from the end of a UPS/BPS file (mGBA enum in patch-ups.c).
const IN_CHECKSUM: usize = 12;
const OUT_CHECKSUM: usize = 8;

/// _UPSApplyPatch. Like the C, the input checksum is not verified
/// ("TODO: Input checksum"); the output CRC32 is.
fn ups_apply(patch: &[u8], in_data: &[u8], out: &mut [u8]) -> bool {
    let out_size = out.len();
    let filesize = patch.len();
    let mut pos = 4; // seek(4, SEEK_SET)
    decode_length(patch, &mut pos); // Discard input size
    if decode_length(patch, &mut pos) != out_size as u64 {
        return false;
    }
    let n = in_data.len().min(out_size);
    out[..n].copy_from_slice(&in_data[..n]);

    let mut offset: u64 = 0;
    let end = filesize - IN_CHECKSUM;
    // C's buffered reader tracks exact consumed bytes; with a slice the
    // cursor already is the exact consumed count.
    while pos < end {
        offset = offset.wrapping_add(decode_length(patch, &mut pos));
        loop {
            if pos >= filesize {
                return false;
            }
            let byte = patch[pos];
            pos += 1;
            if byte == 0 {
                break;
            }
            if offset >= out_size as u64 {
                return false;
            }
            out[offset as usize] ^= byte;
            offset += 1;
        }
        offset += 1;
    }

    let good_crc32 =
        u32::from_le_bytes(patch[filesize - OUT_CHECKSUM..filesize - 4].try_into().unwrap());
    crc32(0, out) == good_crc32
}

/// _BPSApplyPatch.
fn bps_apply(patch: &[u8], in_data: &[u8], out: &mut [u8]) -> bool {
    let filesize = patch.len();
    let in_size = in_data.len();
    let out_size = out.len();

    let expected_in_checksum =
        u32::from_le_bytes(patch[filesize - IN_CHECKSUM..filesize - OUT_CHECKSUM].try_into().unwrap());
    let expected_out_checksum =
        u32::from_le_bytes(patch[filesize - OUT_CHECKSUM..filesize - 4].try_into().unwrap());

    if crc32(0, in_data) != expected_in_checksum {
        return false;
    }
    let mut output_checksum = 0u32;

    if in_size > i64::MAX as usize || out_size > i64::MAX as usize {
        return false;
    }
    let mut pos = 4; // seek(4, SEEK_SET)
    decode_length(patch, &mut pos); // Discard input size
    if decode_length(patch, &mut pos) != out_size as u64 {
        return false;
    }
    let metadata_length = decode_length(patch, &mut pos);
    pos = pos.saturating_add(metadata_length as usize); // Skip metadata

    let end = filesize - IN_CHECKSUM;
    let mut write_location: usize = 0;
    let mut read_source_location: i64 = 0;
    let mut read_target_location: i64 = 0;
    while pos < end {
        let command = decode_length(patch, &mut pos);
        let length64 = (command >> 2).wrapping_add(1);
        if write_location as u64 + length64 > out_size as u64 {
            return false;
        }
        let length = length64 as usize; // fits: bounded by out_size above
        match command & 0x3 {
            0x0 => {
                // SourceRead
                if write_location as u64 + length64 > in_size as u64 {
                    return false;
                }
                out[write_location..write_location + length]
                    .copy_from_slice(&in_data[write_location..write_location + length]);
                output_checksum =
                    crc32(output_checksum, &out[write_location..write_location + length]);
                write_location += length;
            }
            0x1 => {
                // TargetRead
                if pos + length > filesize {
                    return false;
                }
                out[write_location..write_location + length]
                    .copy_from_slice(&patch[pos..pos + length]);
                pos += length;
                output_checksum =
                    crc32(output_checksum, &out[write_location..write_location + length]);
                write_location += length;
            }
            0x2 => {
                // SourceCopy
                let read_offset = decode_length(patch, &mut pos);
                if read_offset > i64::MAX as u64 {
                    // Outrageously large; reject like the C.
                    return false;
                }
                if read_offset & 1 != 0 {
                    read_source_location -= (read_offset >> 1) as i64;
                } else {
                    read_source_location += (read_offset >> 1) as i64;
                }
                if read_source_location < 0 || read_source_location > in_size as i64 {
                    return false;
                }
                if read_source_location + length as i64 > in_size as i64 {
                    return false;
                }
                let rsl = read_source_location as usize;
                out[write_location..write_location + length]
                    .copy_from_slice(&in_data[rsl..rsl + length]);
                output_checksum =
                    crc32(output_checksum, &out[write_location..write_location + length]);
                write_location += length;
                read_source_location += length as i64;
            }
            _ => {
                // TargetCopy
                let read_offset = decode_length(patch, &mut pos);
                if read_offset > i64::MAX as u64 {
                    return false;
                }
                if read_offset & 1 != 0 {
                    read_target_location -= (read_offset >> 1) as i64;
                } else {
                    read_target_location += (read_offset >> 1) as i64;
                }
                if read_target_location < 0 || read_target_location > out_size as i64 {
                    return false;
                }
                if read_target_location + length as i64 > out_size as i64 {
                    return false;
                }
                let mut rtl = read_target_location as usize;
                for _ in 0..length {
                    // Bytewise as it can overlap.
                    out[write_location] = out[rtl];
                    write_location += 1;
                    rtl += 1;
                }
                output_checksum =
                    crc32(output_checksum, &out[write_location - length..write_location]);
                read_target_location = rtl as i64;
            }
        }
    }
    expected_out_checksum == output_checksum
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UPS/BPS varint encoder (inverse of decode_length).
    fn encode_length(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b | 0x80);
                break;
            }
            v -= 1;
            out.push(b);
        }
    }

    /// Build a UPS patch transforming `input` into `output` (diff -> hunks).
    fn make_ups(input: &[u8], output: &[u8]) -> Vec<u8> {
        let mut patch = b"UPS1".to_vec();
        encode_length(input.len() as u64, &mut patch);
        encode_length(output.len() as u64, &mut patch);
        // Diff only over the output's extent: when truncating, input bytes
        // past the output size are dropped (min(in,out) copy semantics).
        let n = output.len();
        let mut i = 0;
        let mut offset = 0usize;
        while i < n {
            let a = input.get(i).copied().unwrap_or(0);
            let b = output.get(i).copied().unwrap_or(0);
            if a != b {
                encode_length((i - offset) as u64, &mut patch);
                while i < n {
                    let a = input.get(i).copied().unwrap_or(0);
                    let b = output.get(i).copied().unwrap_or(0);
                    if a == b {
                        break;
                    }
                    patch.push(a ^ b);
                    i += 1;
                }
                patch.push(0);
                offset = i + 1;
            }
            i += 1;
        }
        patch.extend_from_slice(&crc32(0, input).to_le_bytes());
        patch.extend_from_slice(&crc32(0, output).to_le_bytes());
        let pcrc = crc32(0, &patch);
        patch.extend_from_slice(&pcrc.to_le_bytes());
        patch
    }

    #[test]
    fn ips_patch() {
        let rom: Vec<u8> = (0u8..16).collect();
        let mut patch = b"PATCH".to_vec();
        // Record: offset 4, 3 bytes
        patch.extend_from_slice(&[0, 0, 4, 0, 3, 0xAA, 0xBB, 0xCC]);
        // RLE: offset 10, 4 times 0x55
        patch.extend_from_slice(&[0, 0, 10, 0, 0, 0, 4, 0x55]);
        patch.extend_from_slice(b"EOF");

        let p = Patch::load(&patch).expect("IPS should load");
        let mut out = vec![0u8; p.output_size(rom.len()).unwrap()];
        assert!(p.apply(&rom, &mut out));
        let mut expect = rom.clone();
        expect[4..7].copy_from_slice(&[0xAA, 0xBB, 0xCC]);
        expect[10..14].fill(0x55);
        assert_eq!(&out[..16], &expect[..]);
    }

    #[test]
    fn ips_garbage_and_bounds() {
        // Garbage
        assert!(Patch::load(b"not a patch at all.......").is_none());
        // PATCH without EOF
        assert!(Patch::load(b"PATCHxxxxxxxx").is_none());
        // EOF without PATCH
        assert!(Patch::load(b"XXXXXaaaaEOF").is_none());

        let rom: Vec<u8> = (0u8..16).collect();
        // Record beyond the 16MiB output buffer
        let mut patch = b"PATCH".to_vec();
        patch.extend_from_slice(&[0xFF, 0xFF, 0xFE, 0, 4, 1, 2, 3, 4]);
        patch.extend_from_slice(b"EOF");
        let p = Patch::load(&patch).unwrap();
        let mut out = vec![0u8; p.output_size(rom.len()).unwrap()];
        assert!(!p.apply(&rom, &mut out));

        // Truncated record data
        let mut patch = b"PATCH".to_vec();
        patch.extend_from_slice(&[0, 0, 4, 0, 3, 0xAA]);
        patch.extend_from_slice(b"EOF");
        // Note: the "EOF" gets consumed as record data here; patch loads but
        // apply runs off the end and fails.
        let p = Patch::load(&patch).unwrap();
        let mut out = vec![0u8; p.output_size(rom.len()).unwrap()];
        assert!(!p.apply(&rom, &mut out));
    }

    #[test]
    fn ups_patch_same_size() {
        let rom: Vec<u8> = (0u8..32).collect();
        let mut target = rom.clone();
        target[3] ^= 0x55;
        target[4] ^= 0x66;
        target[20] ^= 0x77;

        let patch = make_ups(&rom, &target);
        let p = Patch::load(&patch).expect("UPS should load");
        assert_eq!(p.output_size(rom.len()), Some(32));
        assert_eq!(p.output_size(rom.len() + 1), None); // input size mismatch
        let mut out = vec![0u8; 32];
        assert!(p.apply(&rom, &mut out));
        assert_eq!(&out[..], &target[..]);
    }

    #[test]
    fn ups_patch_grow_and_truncate() {
        let rom: Vec<u8> = (0u8..16).collect();
        // Grow to 20 bytes
        let mut target = rom.clone();
        target.extend_from_slice(&[9, 9, 9, 9]);
        target[0] ^= 0x11;
        let patch = make_ups(&rom, &target);
        let p = Patch::load(&patch).expect("UPS grow should load");
        assert_eq!(p.output_size(rom.len()), Some(20));
        let mut out = vec![0u8; 20];
        assert!(p.apply(&rom, &mut out));
        assert_eq!(&out[..], &target[..]);

        // Truncate to 10 bytes (truncate behavior: output size from patch)
        let target2: Vec<u8> = rom[..10].to_vec();
        let patch2 = make_ups(&rom, &target2);
        let p2 = Patch::load(&patch2).expect("UPS truncate should load");
        assert_eq!(p2.output_size(rom.len()), Some(10));
        let mut out2 = vec![0u8; 10];
        assert!(p2.apply(&rom, &mut out2));
        assert_eq!(&out2[..], &target2[..]);
    }

    #[test]
    fn ups_crc_validation() {
        let rom: Vec<u8> = (0u8..16).collect();
        let mut target = rom.clone();
        target[7] ^= 0x99;
        let mut patch = make_ups(&rom, &target);
        // Corrupt patch body -> patch CRC32 fails at load
        let idx = patch.len() - 13;
        patch[idx] ^= 0x01;
        assert!(Patch::load(&patch).is_none());

        // Corrupt only the output CRC -> loads, but apply fails
        let mut patch = make_ups(&rom, &target);
        let n = patch.len();
        patch[n - 6] ^= 0x01;
        // fix the patch crc so it loads
        let body = patch[..n - 4].to_vec();
        let pcrc = crc32(0, &body);
        patch[n - 4..].copy_from_slice(&pcrc.to_le_bytes());
        let p = Patch::load(&patch).unwrap();
        let mut out = vec![0u8; 16];
        assert!(!p.apply(&rom, &mut out));
    }

    #[test]
    fn bps_patch() {
        // Exercises all four BPS actions:
        //   out = in[0..8] (SourceRead) ++ literal 4 bytes (TargetRead)
        //         ++ in[4..8] (SourceCopy) ++ out[8..12] (TargetCopy)
        let rom: Vec<u8> = (0u8..16).collect();
        let literals = [0xDE, 0xAD, 0xBE, 0xEF];
        let mut target = rom[..8].to_vec();
        target.extend_from_slice(&literals);
        target.extend_from_slice(&rom[4..8]);
        let copy: Vec<u8> = target[8..12].to_vec();
        target.extend_from_slice(&copy);
        assert_eq!(target.len(), 20);

        let mut patch = b"BPS1".to_vec();
        encode_length(rom.len() as u64, &mut patch);
        encode_length(target.len() as u64, &mut patch);
        encode_length(0, &mut patch); // metadata length
        // SourceRead, 8 bytes
        encode_length((8 - 1) << 2 | 0, &mut patch);
        // TargetRead, 4 bytes
        encode_length((4 - 1) << 2 | 1, &mut patch);
        patch.extend_from_slice(&literals);
        // SourceCopy, 4 bytes, readSourceLocation 0 -> +4
        encode_length((4 - 1) << 2 | 2, &mut patch);
        encode_length(4 << 1, &mut patch);
        // TargetCopy, 4 bytes, readTargetLocation 0 -> +8
        encode_length((4 - 1) << 2 | 3, &mut patch);
        encode_length(8 << 1, &mut patch);
        patch.extend_from_slice(&crc32(0, &rom).to_le_bytes());
        patch.extend_from_slice(&crc32(0, &target).to_le_bytes());
        let pcrc = crc32(0, &patch);
        patch.extend_from_slice(&pcrc.to_le_bytes());

        let p = Patch::load(&patch).expect("BPS should load");
        assert_eq!(p.output_size(rom.len()), Some(20));
        let mut out = vec![0u8; 20];
        assert!(p.apply(&rom, &mut out));
        assert_eq!(&out[..], &target[..]);

        // Wrong input -> input CRC mismatch on apply
        let mut bad_in = rom.clone();
        bad_in[0] ^= 1;
        let mut out = vec![0u8; 20];
        assert!(!p.apply(&bad_in, &mut out));
    }
}
