// Recording windows — mirror mGBA Qt's VideoView.cpp ("Record A/V...") and
// GIFView.cpp ("Record GIF/WebP/APNG..."). mGBA links libavcodec directly;
// here frames are piped to the external `ffmpeg` binary instead.
//
// GIF/APNG/WebP: raw BGRA frames go straight to one ffmpeg process.
// A/V: video goes to ffmpeg as a lossless FFV1 intermediate while audio is
// copied (non-destructively) from the core's sample ring into a raw s16le
// temp file; Stop muxes both into the chosen container/codecs with a second
// ffmpeg run. Frames are captured in `on_frame`, i.e. once per emulated
// frame, only while this window is open; closing it finalizes the file.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::thread::JoinHandle;
use std::time::Instant;

use eframe::egui;

use super::{ToolCtx, ToolWindow};
use crate::emu::Session;

#[derive(Clone, Copy, PartialEq, Eq)]
enum AvPreset {
    H264Aac,
    Vp9Opus,
    Ffv1Flac,
}

impl AvPreset {
    fn label(self) -> &'static str {
        match self {
            AvPreset::H264Aac => "H.264 + AAC (.mp4)",
            AvPreset::Vp9Opus => "VP9 + Opus (.webm)",
            AvPreset::Ffv1Flac => "FFV1 + FLAC, lossless (.mkv)",
        }
    }
    fn ext(self) -> &'static str {
        match self {
            AvPreset::H264Aac => "mp4",
            AvPreset::Vp9Opus => "webm",
            AvPreset::Ffv1Flac => "mkv",
        }
    }
    fn codec_args(self) -> Vec<&'static str> {
        match self {
            AvPreset::H264Aac => vec!["-c:v", "libx264", "-preset", "medium", "-crf", "18", "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "192k"],
            AvPreset::Vp9Opus => vec!["-c:v", "libvpx-vp9", "-crf", "24", "-b:v", "0", "-pix_fmt", "yuv420p", "-c:a", "libopus", "-b:a", "128k"],
            AvPreset::Ffv1Flac => vec!["-c:v", "ffv1", "-c:a", "flac"],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AnimFormat {
    Gif,
    Apng,
    Webp,
}

impl AnimFormat {
    fn label(self) -> &'static str {
        match self {
            AnimFormat::Gif => "GIF",
            AnimFormat::Apng => "APNG",
            AnimFormat::Webp => "WebP",
        }
    }
    fn ext(self) -> &'static str {
        match self {
            AnimFormat::Gif => "gif",
            AnimFormat::Apng => "png",
            AnimFormat::Webp => "webp",
        }
    }
}

/// A running recording.
struct Recording {
    tx: Option<SyncSender<Vec<u8>>>,
    writer: Option<JoinHandle<()>>,
    child: Child,
    /// A/V only: intermediate files + final mux parameters.
    av: Option<AvTemp>,
    frames: u64,
    started: Instant,
    width: u32,
    height: u32,
    /// GIF frameskip counter.
    skip_phase: u32,
}

struct AvTemp {
    video: PathBuf,
    audio_path: PathBuf,
    audio: std::io::BufWriter<std::fs::File>,
    sample_rate: i32,
    /// Fractional samples-per-frame accumulator.
    sample_accum: f64,
    samples_per_frame: f64,
}

/// The final mux (A/V) running after Stop.
struct Finishing {
    child: Child,
    output: PathBuf,
    cleanup: Vec<PathBuf>,
}

pub struct RecordView {
    gif: bool,
    ffmpeg: Option<bool>,
    output: Option<PathBuf>,
    preset: AvPreset,
    format: AnimFormat,
    scale: u32,
    frameskip: u32,
    looping: bool,
    rec: Option<Recording>,
    finishing: Option<Finishing>,
    status: String,
}

impl RecordView {
    fn new(gif: bool) -> Self {
        RecordView {
            gif,
            ffmpeg: None,
            output: None,
            preset: AvPreset::H264Aac,
            format: AnimFormat::Gif,
            scale: if gif { 1 } else { 2 },
            frameskip: 1,
            looping: true,
            rec: None,
            finishing: None,
            status: String::new(),
        }
    }
    pub fn av() -> Self {
        Self::new(false)
    }
    pub fn gif() -> Self {
        Self::new(true)
    }

    fn ffmpeg_available(&mut self) -> bool {
        *self.ffmpeg.get_or_insert_with(|| {
            Command::new("ffmpeg")
                .arg("-version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_or(false, |s| s.success())
        })
    }

    fn ext(&self) -> &'static str {
        if self.gif { self.format.ext() } else { self.preset.ext() }
    }

    fn scale_filter(&self) -> Option<String> {
        (self.scale > 1).then(|| format!("scale=iw*{0}:ih*{0}:flags=neighbor", self.scale))
    }

    fn start(&mut self, s: &mut Session) -> Result<(), String> {
        let output = self.output.clone().ok_or("Choose an output file first")?;
        let (w, h) = s.core().base_video_size();
        let fps = s.core().frequency() as f64 / s.core().frame_cycles() as f64;
        let size = format!("{w}x{h}");
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgra", "-s", &size]);
        let mut av = None;
        if self.gif {
            let rate = fps / (self.frameskip + 1) as f64;
            cmd.args(["-r", &format!("{rate:.6}"), "-i", "-"]);
            let mut vf: Vec<String> = self.scale_filter().into_iter().collect();
            match self.format {
                AnimFormat::Gif => {
                    vf.push("split[a][b];[a]palettegen=reserve_transparent=0[p];[b][p]paletteuse=dither=none".into());
                    cmd.args(["-vf", &vf.join(",")]);
                    cmd.args(["-loop", if self.looping { "0" } else { "-1" }, "-f", "gif"]);
                }
                AnimFormat::Apng => {
                    if !vf.is_empty() {
                        cmd.args(["-vf", &vf.join(",")]);
                    }
                    cmd.args(["-plays", if self.looping { "0" } else { "1" }, "-f", "apng"]);
                }
                AnimFormat::Webp => {
                    if !vf.is_empty() {
                        cmd.args(["-vf", &vf.join(",")]);
                    }
                    cmd.args(["-c:v", "libwebp", "-lossless", "1", "-loop", if self.looping { "0" } else { "1" }, "-f", "webp"]);
                }
            }
            cmd.arg(&output);
        } else {
            let tmp = std::env::temp_dir();
            let id = std::process::id();
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis());
            let video = tmp.join(format!("rgba-rec-{id}-{stamp}.mkv"));
            let audio_path = tmp.join(format!("rgba-rec-{id}-{stamp}.s16le"));
            cmd.args(["-r", &format!("{fps:.6}"), "-i", "-", "-c:v", "ffv1"]);
            cmd.arg(&video);
            let file = std::fs::File::create(&audio_path).map_err(|e| format!("{}: {e}", audio_path.display()))?;
            let rate = s.core().audio_sample_rate();
            av = Some(AvTemp {
                video,
                audio_path,
                audio: std::io::BufWriter::new(file),
                sample_rate: rate,
                sample_accum: 0.0,
                samples_per_frame: rate as f64 * s.core().frame_cycles() as f64 / s.core().frequency() as f64,
            });
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("failed to start ffmpeg: {e}"))?;
        let mut stdin = child.stdin.take().ok_or("no ffmpeg stdin")?;
        // Writer thread so a slow encoder never stalls emulation; bounded so
        // memory can't grow without limit (~2 s of frames).
        let (tx, rx) = sync_channel::<Vec<u8>>(120);
        let writer = std::thread::spawn(move || {
            for frame in rx {
                if stdin.write_all(&frame).is_err() {
                    break;
                }
            }
        });
        self.rec = Some(Recording {
            tx: Some(tx),
            writer: Some(writer),
            child,
            av,
            frames: 0,
            started: Instant::now(),
            width: w,
            height: h,
            skip_phase: 0,
        });
        self.status = "Recording…".into();
        Ok(())
    }

    /// Stop capturing; for A/V start the final mux in the background.
    fn stop(&mut self) {
        let Some(mut rec) = self.rec.take() else { return };
        drop(rec.tx.take());
        if let Some(w) = rec.writer.take() {
            let _ = w.join();
        }
        let status = rec.child.wait();
        let err = read_stderr(&mut rec.child);
        let ok = status.map_or(false, |s| s.success());
        let output = self.output.clone().unwrap_or_default();
        match rec.av {
            None => {
                self.status = if ok {
                    format!("Saved {} ({} frames)", output.display(), rec.frames)
                } else {
                    format!("ffmpeg failed: {err}")
                };
            }
            Some(mut av) => {
                let _ = av.audio.flush();
                drop(av.audio);
                if !ok {
                    self.status = format!("ffmpeg failed: {err}");
                    let _ = std::fs::remove_file(&av.video);
                    let _ = std::fs::remove_file(&av.audio_path);
                    return;
                }
                let mut cmd = Command::new("ffmpeg");
                cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]);
                cmd.arg(&av.video);
                cmd.args(["-f", "s16le", "-ar", &av.sample_rate.to_string(), "-ac", "2", "-i"]);
                cmd.arg(&av.audio_path);
                if let Some(vf) = self.scale_filter() {
                    cmd.args(["-vf", &vf]);
                }
                cmd.args(self.preset.codec_args());
                cmd.args(["-shortest"]);
                cmd.arg(&output);
                cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
                match cmd.spawn() {
                    Ok(child) => {
                        self.status = "Encoding…".into();
                        self.finishing = Some(Finishing { child, output, cleanup: vec![av.video, av.audio_path] });
                    }
                    Err(e) => self.status = format!("failed to start ffmpeg: {e}"),
                }
            }
        }
    }

    fn poll_finishing(&mut self) {
        let Some(f) = &mut self.finishing else { return };
        match f.child.try_wait() {
            Ok(Some(st)) => {
                let err = read_stderr(&mut f.child);
                self.status = if st.success() {
                    format!("Saved {}", f.output.display())
                } else {
                    format!("ffmpeg failed: {err}")
                };
                for p in &f.cleanup {
                    let _ = std::fs::remove_file(p);
                }
                self.finishing = None;
            }
            Ok(None) => {}
            Err(e) => {
                self.status = format!("ffmpeg: {e}");
                self.finishing = None;
            }
        }
    }
}

fn read_stderr(child: &mut Child) -> String {
    use std::io::Read;
    let mut s = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut s);
    }
    s.lines().last().unwrap_or("").to_string()
}

impl Drop for RecordView {
    fn drop(&mut self) {
        self.stop();
        if let Some(mut f) = self.finishing.take() {
            let _ = f.child.wait();
            for p in &f.cleanup {
                let _ = std::fs::remove_file(p);
            }
        }
    }
}

impl ToolWindow for RecordView {
    fn title(&self) -> &'static str {
        if self.gif { "Record GIF/WebP/APNG" } else { "Record A/V" }
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        self.poll_finishing();
        let have_ffmpeg = self.ffmpeg_available();
        let recording = self.rec.is_some();
        let mut start = false;
        let mut stop = false;
        egui::Window::new(self.title()).open(open).resizable(false).show(ctx, |ui| {
            if !have_ffmpeg {
                ui.colored_label(
                    egui::Color32::from_rgb(230, 120, 80),
                    "ffmpeg was not found in PATH. Recording uses the external ffmpeg program; install it and reopen this window.",
                );
            }
            ui.add_enabled_ui(!recording && self.finishing.is_none(), |ui| {
                if self.gif {
                    ui.horizontal(|ui| {
                        ui.label("Format:");
                        for f in [AnimFormat::Gif, AnimFormat::Apng, AnimFormat::Webp] {
                            ui.radio_value(&mut self.format, f, f.label());
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Frameskip:");
                        ui.add(egui::DragValue::new(&mut self.frameskip).range(0..=9));
                        ui.small("(1 = every other frame)");
                    });
                    ui.checkbox(&mut self.looping, "Loop");
                } else {
                    egui::ComboBox::from_label("Preset").selected_text(self.preset.label()).show_ui(ui, |ui| {
                        for p in [AvPreset::H264Aac, AvPreset::Vp9Opus, AvPreset::Ffv1Flac] {
                            ui.selectable_value(&mut self.preset, p, p.label());
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.label("Scale:");
                    ui.add(egui::DragValue::new(&mut self.scale).range(1..=8).suffix("×"));
                });
                ui.horizontal(|ui| {
                    if ui.button("Select output…").clicked() {
                        let ext = self.ext();
                        let name = tc
                            .session
                            .as_ref()
                            .map(|s| s.rom_path.with_extension(ext))
                            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                            .unwrap_or_else(|| format!("recording.{ext}"));
                        let mut d = rfd::FileDialog::new().set_file_name(name).add_filter(ext, &[ext]);
                        if let Some(dir) = tc.session.as_ref().and_then(|s| s.rom_path.parent()) {
                            d = d.set_directory(dir);
                        }
                        if let Some(p) = d.save_file() {
                            self.output = Some(p);
                        }
                    }
                    if let Some(p) = &self.output {
                        ui.monospace(p.display().to_string());
                    }
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                let can_start = have_ffmpeg && !recording && self.finishing.is_none() && self.output.is_some() && tc.session.is_some();
                if ui.add_enabled(can_start, egui::Button::new("Start")).clicked() {
                    start = true;
                }
                if ui.add_enabled(recording, egui::Button::new("Stop")).clicked() {
                    stop = true;
                }
            });
            if let Some(r) = &self.rec {
                let secs = r.started.elapsed().as_secs();
                ui.label(format!("Recording: {} frames, {:02}:{:02}", r.frames, secs / 60, secs % 60));
                if !self.gif {
                    ui.small("Audio is captured from the core and muxed when you press Stop.");
                }
            }
            if self.finishing.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Encoding…");
                });
            }
            if !self.status.is_empty() {
                ui.label(&self.status);
            }
            if tc.session.is_none() {
                ui.small("Load a game to record.");
            }
            ui.small("Recording stops when this window is closed.");
        });
        if start {
            if let Some(s) = tc.session.as_deref_mut() {
                if let Err(e) = self.start(s) {
                    self.status = e;
                }
            }
        }
        if stop || (!*open && self.rec.is_some()) {
            self.stop();
        }
        if self.rec.is_some() || self.finishing.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn on_frame(&mut self, s: &mut Session) {
        let Some(rec) = &mut self.rec else { return };
        let (w, h) = s.core().base_video_size();
        if (w, h) != (rec.width, rec.height) {
            return;
        }
        // Audio first (A/V): take this frame's worth of the newest samples
        // from the core ring without consuming it (the app's audio pump
        // drains it right after on_frame).
        if let Some(av) = &mut rec.av {
            av.sample_accum += av.samples_per_frame;
            let want = av.sample_accum.floor() as usize;
            av.sample_accum -= want as f64;
            let ring = s.core().audio_buffer();
            let avail = ring.available_frames();
            let n = want.min(avail);
            let mut buf = Vec::with_capacity(want * 4);
            // Pad with silence if the core produced fewer samples than
            // expected, keeping audio aligned with video.
            for _ in n..want {
                buf.extend_from_slice(&[0, 0, 0, 0]);
            }
            for i in avail - n..avail {
                buf.extend_from_slice(&ring.peek_frame(0, i).to_le_bytes());
                buf.extend_from_slice(&ring.peek_frame(1, i).to_le_bytes());
            }
            let _ = av.audio.write_all(&buf);
        } else {
            let skip = rec.skip_phase;
            rec.skip_phase = if skip >= self.frameskip { 0 } else { skip + 1 };
            if skip != 0 {
                return;
            }
        }
        let px = &s.core().video_buffer()[..(w * h) as usize];
        let mut bytes = Vec::with_capacity(px.len() * 4);
        for p in px {
            bytes.extend_from_slice(&p.to_le_bytes());
        }
        if let Some(tx) = &rec.tx {
            if tx.send(bytes).is_err() {
                // ffmpeg exited; stop on the next UI frame.
                rec.tx = None;
            }
        }
        rec.frames += 1;
    }

    fn on_session_changed(&mut self) {
        self.stop();
    }
}
