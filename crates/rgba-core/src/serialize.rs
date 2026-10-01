// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Rust equivalent of the serialization helpers in mgba/src/core/serialize.c
// and the LOAD_/STORE_/PUT_/GET_ macros in serialize.h. All multi-byte fields
// are little-endian, matching the C so save layouts stay comparable.

/// Writes little-endian data into a Vec, tracking whether everything fit.
pub struct Serializer<'a> {
    pub buf: &'a mut Vec<u8>,
    pub ok: bool,
}

impl<'a> Serializer<'a> {
    pub fn new(buf: &'a mut Vec<u8>) -> Self {
        Serializer { buf, ok: true }
    }

    pub fn put_u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn put_i8(&mut self, v: i8) {
        self.buf.push(v as u8);
    }
    pub fn put_bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }
    pub fn put_u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn put_i16(&mut self, v: i16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn put_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn put_i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn put_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn put_bytes(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }
    /// Pad with zeros up to absolute offset `pos` (fixed-layout structs).
    pub fn pad_to(&mut self, pos: usize) {
        while self.buf.len() < pos {
            self.buf.push(0);
        }
    }
    pub fn offset(&self) -> usize {
        self.buf.len()
    }
}

/// Reads little-endian data, allowing slop at the end like the C does with
/// fixed-size state buffers.
pub struct Deserializer<'a> {
    pub buf: &'a [u8],
    pub pos: usize,
    pub ok: bool,
}

impl<'a> Deserializer<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Deserializer {
            buf,
            pos: 0,
            ok: true,
        }
    }

    fn take(&mut self, n: usize) -> &'a [u8] {
        if self.pos + n > self.buf.len() {
            self.ok = false;
            self.pos = self.buf.len();
            return &self.buf[self.pos..self.pos];
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        s
    }

    pub fn get_u8(&mut self) -> u8 {
        self.take(1).first().copied().unwrap_or(0)
    }
    pub fn get_i8(&mut self) -> i8 {
        self.get_u8() as i8
    }
    pub fn get_bool(&mut self) -> bool {
        self.get_u8() != 0
    }
    pub fn get_u16(&mut self) -> u16 {
        let mut b = [0u8; 2];
        b.copy_from_slice(self.take(2));
        u16::from_le_bytes(b)
    }
    pub fn get_i16(&mut self) -> i16 {
        self.get_u16() as i16
    }
    pub fn get_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        b.copy_from_slice(self.take(4));
        u32::from_le_bytes(b)
    }
    pub fn get_i32(&mut self) -> i32 {
        self.get_u32() as i32
    }
    pub fn get_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        b.copy_from_slice(self.take(8));
        u64::from_le_bytes(b)
    }
    pub fn get_bytes(&mut self, out: &mut [u8]) {
        let n = out.len();
        let s = self.take(n);
        out[..s.len()].copy_from_slice(s);
    }
    pub fn skip(&mut self, n: usize) {
        self.take(n);
    }
}
