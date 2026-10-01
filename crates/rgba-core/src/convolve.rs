// Copyright (c) 2013-2021 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/util/convolve.c and include/mgba-util/convolve.h.
//
// N-dimensional convolution kernels plus packed 1D (i32) and 2D (u8)
// convolvers. The C edge/cast quirks are preserved:
// - Convolve1DPad0PackedS32 skips `x + kx <= kx2`, so src[0] never
//   contributes to any output (matches the C's `<=`).
// - Float→int results truncate toward zero like C's implicit conversions
//   (Rust `as` adds saturation, only observable for out-of-range sums).

/// struct ConvolutionKernel
pub struct ConvolutionKernel {
    kernel: Vec<f32>,
    dims: Vec<usize>,
}

impl ConvolutionKernel {
    /// ConvolutionKernelCreate; `dims.len()` is the rank. Kernel starts zeroed
    /// (calloc in the C).
    pub fn new(dims: &[usize]) -> Self {
        let mut ksize = 1usize;
        for &d in dims {
            ksize *= d;
        }
        ConvolutionKernel { kernel: vec![0.0; ksize], dims: dims.to_vec() }
    }

    pub fn rank(&self) -> usize {
        self.dims.len()
    }

    pub fn dims(&self) -> &[usize] {
        &self.dims
    }

    /// Raw kernel storage (row-major), for filling custom kernels and tests.
    pub fn data_mut(&mut self) -> &mut [f32] {
        &mut self.kernel
    }

    pub fn data(&self) -> &[f32] {
        &self.kernel
    }

    /// ConvolutionKernelFillRadial (cone kernel; no-op unless rank == 2).
    pub fn fill_radial(&mut self, normalize: bool) {
        if self.rank() != 2 {
            return;
        }
        // C: 12.f / (M_PI * (dims[0]-1) * (dims[1]-1)) — denominator computed
        // in double precision, then truncated to float.
        let support = if normalize {
            (12.0f64 / (std::f64::consts::PI * (self.dims[0] - 1) as f64 * (self.dims[1] - 1) as f64))
                as f32
        } else {
            1.0
        };
        let wr = (self.dims[0] - 1) as f32 / 2.0;
        let hr = (self.dims[1] - 1) as f32 / 2.0;
        let mut elem = 0;
        for y in 0..self.dims[1] {
            for x in 0..self.dims[0] {
                let r = (1.0 - ((x as f32 - wr) / wr).hypot((y as f32 - hr) / hr)) * support;
                self.kernel[elem] = r.max(0.0);
                elem += 1;
            }
        }
    }

    /// ConvolutionKernelFillCircle (flat disc kernel; no-op unless rank == 2).
    pub fn fill_circle(&mut self, normalize: bool) {
        if self.rank() != 2 {
            return;
        }
        let support = if normalize {
            (4.0f64 / (std::f64::consts::PI * (self.dims[0] - 1) as f64 * (self.dims[1] - 1) as f64))
                as f32
        } else {
            1.0
        };
        let wr = (self.dims[0] - 1) as f32 / 2.0;
        let hr = (self.dims[1] - 1) as f32 / 2.0;
        let mut elem = 0;
        for y in 0..self.dims[1] {
            for x in 0..self.dims[0] {
                let r = ((x as f32 - wr) / wr).hypot((y as f32 - hr) / hr);
                self.kernel[elem] = if r <= 1.0 { support } else { 0.0 };
                elem += 1;
            }
        }
    }
}

/// Convolve1DPad0PackedS32.
pub fn convolve_1d_pad0_packed_s32(src: &[i32], dst: &mut [i32], kernel: &ConvolutionKernel) {
    if kernel.rank() != 1 {
        return;
    }
    let length = src.len().min(dst.len());
    let kx2 = kernel.dims[0] / 2;
    for x in 0..length {
        let mut sum = 0.0f32;
        for kx in 0..kernel.dims[0] {
            if x + kx <= kx2 {
                continue;
            }
            let cx = x + kx - kx2;
            if cx >= length {
                continue;
            }
            sum += src[cx] as f32 * kernel.kernel[kx];
        }
        dst[x] = sum as i32;
    }
}

/// Convolve2DClampPacked8 (edge pixels clamp to the border).
#[allow(clippy::too_many_arguments)]
pub fn convolve_2d_clamp_packed8(
    src: &[u8],
    dst: &mut [u8],
    width: usize,
    height: usize,
    stride: usize,
    kernel: &ConvolutionKernel,
) {
    if kernel.rank() != 2 {
        return;
    }
    let kx2 = kernel.dims[0] / 2;
    let ky2 = kernel.dims[1] / 2;
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0f32;
            for ky in 0..kernel.dims[1] {
                let mut cy = 0;
                if y + ky > ky2 {
                    cy = y + ky - ky2;
                }
                if cy >= height {
                    cy = height - 1;
                }
                for kx in 0..kernel.dims[0] {
                    let mut cx = 0;
                    if x + kx > kx2 {
                        cx = x + kx - kx2;
                    }
                    if cx >= width {
                        cx = width - 1;
                    }
                    sum += src[cy * stride + cx] as f32 * kernel.kernel[ky * kernel.dims[0] + kx];
                }
            }
            // C: float → uint8_t (practically: truncate via int32, then mod 256)
            dst[y * stride + x] = (sum as i32) as u8;
        }
    }
}

/// Convolve2DClampChannels8.
#[allow(clippy::too_many_arguments)]
pub fn convolve_2d_clamp_channels8(
    src: &[u8],
    dst: &mut [u8],
    width: usize,
    height: usize,
    stride: usize,
    channels: usize,
    kernel: &ConvolutionKernel,
) {
    if kernel.rank() != 2 {
        return;
    }
    let kx2 = kernel.dims[0] / 2;
    let ky2 = kernel.dims[1] / 2;
    for y in 0..height {
        for x in 0..width {
            for c in 0..channels {
                let mut sum = 0.0f32;
                for ky in 0..kernel.dims[1] {
                    let mut cy = 0;
                    if y + ky > ky2 {
                        cy = y + ky - ky2;
                    }
                    if cy >= height {
                        cy = height - 1;
                    }
                    for kx in 0..kernel.dims[0] {
                        let mut cx = 0;
                        if x + kx > kx2 {
                            cx = x + kx - kx2;
                        }
                        if cx >= width {
                            cx = width - 1;
                        }
                        cx *= channels;
                        sum += src[cy * stride + cx + c] as f32
                            * kernel.kernel[ky * kernel.dims[0] + kx];
                    }
                }
                // C: orow = &dst[y*stride], advanced once per channel per
                // pixel, i.e. dst[y*stride + x*channels + c].
                dst[y * stride + x * channels + c] = (sum as i32) as u8;
            }
        }
    }
}
