// Input helpers: gamepad button names (Settings → Controllers) and PNG
// decoding for state thumbnails.

use eframe::egui;

pub const PAD_BUTTONS: [&str; 19] = [
    "South", "East", "North", "West", "LeftTrigger", "LeftTrigger2", "RightTrigger",
    "RightTrigger2", "Select", "Start", "Mode", "LeftThumb", "RightThumb", "DPadUp",
    "DPadDown", "DPadLeft", "DPadRight", "C", "Z",
];

pub fn pad_button(name: &str) -> Option<gilrs::Button> {
    use gilrs::Button::*;
    Some(match name {
        "South" => South,
        "East" => East,
        "North" => North,
        "West" => West,
        "LeftTrigger" => LeftTrigger,
        "LeftTrigger2" => LeftTrigger2,
        "RightTrigger" => RightTrigger,
        "RightTrigger2" => RightTrigger2,
        "Select" => Select,
        "Start" => Start,
        "Mode" => Mode,
        "LeftThumb" => LeftThumb,
        "RightThumb" => RightThumb,
        "DPadUp" => DPadUp,
        "DPadDown" => DPadDown,
        "DPadLeft" => DPadLeft,
        "DPadRight" => DPadRight,
        "C" => C,
        "Z" => Z,
        _ => return None,
    })
}

/// Decode an RGB/RGBA PNG (our own screenshots/thumbnails) for display.
pub fn decode_png(data: &[u8]) -> Option<egui::ColorImage> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(data));
    dec.set_transformations(png::Transformations::EXPAND);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let px = &buf[..info.buffer_size()];
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgb => px.chunks_exact(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        png::ColorType::Rgba => px.to_vec(),
        _ => return None,
    };
    Some(egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba))
}
