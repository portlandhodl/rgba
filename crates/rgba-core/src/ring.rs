// Copyright (c) 2013-2014 Jeffrey Pfau (mGBA), MPL-2.0.
// Rust equivalent of mgba's mCircleBuffer / mRingFIFO (src/util/ring-fifo.c,
// circle-buffer.c), specialized to interleaved i16 stereo audio samples.

/// A single-producer/single-consumer-ish ring of i16. The core writes
/// interleaved stereo samples; the frontend drains. Not synchronized — the
/// frontend wraps it in a Mutex when sharing across threads.
pub struct RingI16 {
    data: Vec<i16>,
    read: usize,
    write: usize,
    size: usize,
}

impl RingI16 {
    pub fn new(capacity: usize) -> Self {
        RingI16 {
            data: vec![0; capacity.max(2)],
            read: 0,
            write: 0,
            size: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.data.len()
    }

    pub fn resize(&mut self, capacity: usize) {
        self.data.clear();
        self.data.resize(capacity.max(2), 0);
        self.read = 0;
        self.write = 0;
        self.size = 0;
    }

    pub fn clear(&mut self) {
        self.read = 0;
        self.write = 0;
        self.size = 0;
    }

    pub fn len(&self) -> usize {
        self.size
    }

    pub fn is_full(&self) -> bool {
        self.size == self.data.len()
    }

    /// Write one interleaved stereo frame (left, right). Drops when full,
    /// matching mGBA's WriteTruncate behavior on a full audio buffer.
    pub fn write_stereo(&mut self, left: i16, right: i16) {
        if self.size + 2 > self.data.len() {
            return;
        }
        let cap = self.data.len();
        self.data[self.write] = left;
        self.write = (self.write + 1) % cap;
        self.data[self.write] = right;
        self.write = (self.write + 1) % cap;
        self.size += 2;
    }

    pub fn push(&mut self, v: i16) {
        if self.size == self.data.len() {
            return;
        }
        let cap = self.data.len();
        self.data[self.write] = v;
        self.write = (self.write + 1) % cap;
        self.size += 1;
    }

    pub fn pop(&mut self) -> Option<i16> {
        if self.size == 0 {
            return None;
        }
        let cap = self.data.len();
        let v = self.data[self.read];
        self.read = (self.read + 1) % cap;
        self.size -= 1;
        Some(v)
    }

    /// Drain up to `out.len()` samples.
    pub fn read_into(&mut self, out: &mut [i16]) -> usize {
        let n = out.len().min(self.size);
        for slot in out.iter_mut().take(n) {
            *slot = self.pop().unwrap();
        }
        n
    }
}
