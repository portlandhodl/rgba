// "Take screenshot" (mCoreTakeScreenshot, src/core/core.c): writes the
// current frame as PNG next to the ROM (or the configured directory) using
// the first free `<rom>-<n>.png` name.

use std::path::{Path, PathBuf};

/// Encode 0xFFRRGGBB pixels as an RGB PNG.
pub fn encode_png(pixels: &[u32], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(|e| e.to_string())?;
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for &p in &pixels[..(width * height) as usize] {
            rgb.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
        w.write_image_data(&rgb).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

pub fn next_screenshot_path(rom: &Path, dir: Option<&Path>) -> PathBuf {
    let stem = rom
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "screenshot".into());
    let dir = dir
        .map(Path::to_path_buf)
        .or_else(|| rom.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    (0..)
        .map(|n| dir.join(format!("{stem}-{n}.png")))
        .find(|p| !p.exists())
        .unwrap()
}

pub fn save(pixels: &[u32], width: u32, height: u32, path: &Path) -> Result<(), String> {
    let data = encode_png(pixels, width, height)?;
    std::fs::write(path, data).map_err(|e| e.to_string())
}
