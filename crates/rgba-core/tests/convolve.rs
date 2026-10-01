// ConvolutionKernel + convolvers (mgba/src/util/convolve.c).

use rgba_core::convolve::{
    convolve_1d_pad0_packed_s32, convolve_2d_clamp_channels8, convolve_2d_clamp_packed8,
    ConvolutionKernel,
};

#[test]
fn kernel_create_zeroed() {
    let k = ConvolutionKernel::new(&[3, 3]);
    assert_eq!(k.rank(), 2);
    assert_eq!(k.dims(), &[3, 3]);
    assert_eq!(k.data().len(), 9);
    assert!(k.data().iter().all(|&v| v == 0.0));
}

#[test]
fn fill_circle_pattern() {
    // 5x5 disc: entries where (x-2)^2 + (y-2)^2 <= 4 are 1, rest 0.
    let mut k = ConvolutionKernel::new(&[5, 5]);
    k.fill_circle(false);
    let mut ones = 0;
    for y in 0..5usize {
        for x in 0..5usize {
            let inside = (x as i64 - 2).pow(2) + (y as i64 - 2).pow(2) <= 4;
            let v = k.data()[y * 5 + x];
            if inside {
                assert_eq!(v, 1.0, "({x},{y})");
                ones += 1;
            } else {
                assert_eq!(v, 0.0, "({x},{y})");
            }
        }
    }
    assert_eq!(ones, 13);
}

#[test]
fn fill_radial_values() {
    // 5x5 cone: 1 - hypot((x-2)/2, (y-2)/2), clamped at 0.
    let mut k = ConvolutionKernel::new(&[5, 5]);
    k.fill_radial(false);
    let eps = 1e-6;
    assert!((k.data()[2 * 5 + 2] - 1.0).abs() < eps); // center
    assert!((k.data()[1 * 5 + 2] - 0.5).abs() < eps); // mid-edge
    assert!((k.data()[1 * 5 + 1] - (1.0 - 0.5f32.hypot(0.5))).abs() < eps); // diagonal
    assert_eq!(k.data()[0], 0.0); // corner clamped
}

#[test]
fn convolve_1d_identity_and_src0_quirk() {
    // Delta kernel [0,1,0]: output equals input everywhere except out[0],
    // which the C's `x + kx <= kx2` skip leaves at 0 (src[0] never tapped).
    let mut k = ConvolutionKernel::new(&[3]);
    k.data_mut()[1] = 1.0;
    let src: Vec<i32> = (0..10).collect();
    let mut dst = vec![0i32; 10];
    convolve_1d_pad0_packed_s32(&src, &mut dst, &k);
    assert_eq!(dst[0], 0);
    assert_eq!(&dst[1..], &src[1..]);
}

#[test]
fn convolve_2d_delta_is_identity() {
    let mut k = ConvolutionKernel::new(&[3, 3]);
    k.data_mut()[4] = 1.0; // center
    let src: Vec<u8> = (0..25u8).map(|v| v * 3).collect();
    let mut dst = vec![0u8; 25];
    convolve_2d_clamp_packed8(&src, &mut dst, 5, 5, 5, &k);
    assert_eq!(src, dst);
}

#[test]
fn convolve_2d_circle_blur_constant_image() {
    // 3x3 disc has 5 unit taps (center + 4 edges); a constant image stays
    // constant*5 even at the borders because edges clamp.
    let mut k = ConvolutionKernel::new(&[3, 3]);
    k.fill_circle(false);
    let src = vec![10u8; 25];
    let mut dst = vec![0u8; 25];
    convolve_2d_clamp_packed8(&src, &mut dst, 5, 5, 5, &k);
    assert!(dst.iter().all(|&v| v == 50));
}

#[test]
fn convolve_2d_channels8_delta_is_identity() {
    let mut k = ConvolutionKernel::new(&[3, 3]);
    k.data_mut()[4] = 1.0;
    // 4x2 image, 3 channels, stride 4*3
    let src: Vec<u8> = (0..24u8).map(|v| v.wrapping_mul(7)).collect();
    let mut dst = vec![0u8; 24];
    convolve_2d_clamp_channels8(&src, &mut dst, 4, 2, 12, 3, &k);
    assert_eq!(src, dst);
}
