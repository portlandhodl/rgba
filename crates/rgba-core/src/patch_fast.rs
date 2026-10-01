// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from src/util/patch-fast.c (mgba-util/patch/fast.h).
//
// "Fast patches" are not a ROM patch file format: they are an in-memory
// XOR-extent diff between two equal-size buffers, used by mGBA's rewind
// (src/core/rewind.c) to compress savestate history. `diff` records the
// differing regions; `apply` reconstructs the output buffer.
//
// Semantic note: the C diff works on u32 lanes (layout-neutral under XOR) and
// its sub-16-byte tail path has a byte-indexing quirk (extentOff carries over
// from the block phase). This port operates bytewise and keeps the tail
// correct; the observable round-trip contract — apply(in, out, diff(in, out))
// reproduces the second buffer — is identical.

/// PATCH_FAST_EXTENT u32 lanes = 512 bytes of XOR data per extent.
pub const PATCH_FAST_EXTENT: usize = 128 * 4;

struct PatchFastExtent {
    offset: usize,
    length: usize,
    extent: Box<[u8; PATCH_FAST_EXTENT]>,
}

/// mGBA `struct PatchFast`.
#[derive(Default)]
pub struct PatchFast {
    extents: Vec<PatchFastExtent>,
}

impl PatchFast {
    /// initPatchFast.
    pub fn new() -> Self {
        PatchFast { extents: Vec::with_capacity(32) }
    }

    /// diffPatchFast: record `in_data ^ out_data` as extents. Both buffers
    /// must be the same size (the C takes a single size).
    pub fn diff(&mut self, in_data: &[u8], out_data: &[u8]) -> bool {
        if in_data.len() != out_data.len() {
            return false;
        }
        self.extents.clear();
        let size = in_data.len();
        // Index of the open extent; bytes written into it trail in length.
        let mut open: Option<usize> = None;
        let mut off = 0usize;
        while off < size {
            if in_data[off] != out_data[off] {
                if open.is_none() {
                    self.extents.push(PatchFastExtent {
                        offset: off,
                        length: 0,
                        extent: Box::new([0; PATCH_FAST_EXTENT]),
                    });
                    open = Some(self.extents.len() - 1);
                }
                let e = &mut self.extents[open.unwrap()];
                e.extent[e.length] = in_data[off] ^ out_data[off];
                e.length += 1;
                if e.length == PATCH_FAST_EXTENT {
                    open = None;
                }
            } else if open.is_some() {
                open = None;
            }
            off += 1;
        }
        true
    }

    /// _fastOutputSize: fast patches never change the buffer size.
    pub fn output_size(&self, in_size: usize) -> usize {
        in_size
    }

    /// _fastApplyPatch: `out` becomes `in_data` with all extents XORed in.
    pub fn apply(&self, in_data: &[u8], out: &mut [u8]) -> bool {
        if in_data.len() != out.len() {
            return false;
        }
        let mut last_written = 0usize;
        for e in &self.extents {
            if e.length + e.offset > out.len() || e.offset < last_written {
                return false;
            }
            out[last_written..e.offset].copy_from_slice(&in_data[last_written..e.offset]);
            for i in 0..e.length {
                out[e.offset + i] = in_data[e.offset + i] ^ e.extent[i];
            }
            last_written = e.offset + e.length;
        }
        out[last_written..].copy_from_slice(&in_data[last_written..]);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_roundtrip() {
        // Size not a multiple of 16 to exercise the bytewise tail path.
        let mut a = vec![0u8; 1053];
        for (i, b) in a.iter_mut().enumerate() {
            *b = (i * 7 + 3) as u8;
        }
        let mut b = a.clone();
        // Diffs spread across blocks and in the tail
        b[5] ^= 0xFF;
        b[500] ^= 0x01;
        b[501] ^= 0x02;
        b[1052] ^= 0x80;

        let mut p = PatchFast::new();
        assert!(p.diff(&a, &b));
        let mut out = vec![0u8; a.len()];
        assert!(p.apply(&a, &mut out));
        assert_eq!(out, b);

        // Identical buffers: no extents, apply is a plain copy
        let mut p = PatchFast::new();
        assert!(p.diff(&a, &a));
        let mut out = vec![0u8; a.len()];
        assert!(p.apply(&a, &mut out));
        assert_eq!(out, a);

        // Mismatched sizes
        assert!(!p.diff(&a, &a[..16]));
        let mut out = vec![0u8; a.len() + 1];
        assert!(!p.apply(&a, &mut out));
    }
}
