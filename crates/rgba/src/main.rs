// Copyright (c) 2013-2021 Jeffrey Pfau (mGBA), MPL-2.0.
// Minimal SDL2 frontend: window + streaming texture + queued audio + input,
// save/load state, and (GB-only for now) peripheral sync stubs.

use std::path::PathBuf;

use rgba_core::core::Core;
use rgba_debugger::cli::{CliBackend, CliDebugger};
use rgba_gba::gba::Gba;
use rgba_gb::gb::{Gb, GbModel};

use clap::Parser;
use sdl2::event::Event;
use sdl2::keyboard::Scancode;
use sdl2::pixels::PixelFormatEnum;

#[derive(Parser, Debug)]
#[command(name = "rgba", about = "rgba — a Rust port of mGBA (GBA/GB emulator)")]
struct Args {
    /// ROM image path (.gba/.gbc/.gb)
    rom: PathBuf,
    /// GBA BIOS image (optional; HLE otherwise)
    #[arg(long)]
    bios: Option<PathBuf>,
    /// Boot model for Game Boy ROMs (e.g. dmg, cgb, sgb, mgb, sgb2, scgb)
    #[arg(long)]
    gb_model: Option<String>,
    /// Cheats file (one code per line, GameShark/GameGenie)
    #[arg(long)]
    cheats: Option<PathBuf>,
    /// Disable vsync
    #[arg(long)]
    no_sync: bool,
    /// Integer scale
    #[arg(long, default_value_t = 4)]
    scale: u32,
    /// Audio sample rate
    #[arg(long, default_value_t = 48000)]
    rate: i32,
    /// Start in the CLI debugger (mGBA -d)
    #[arg(long)]
    debug: bool,
}

const KEYMAP: [(Scancode, u32); 16] = [
    (Scancode::Z, 0),
    (Scancode::X, 1),
    (Scancode::Backspace, 2),
    (Scancode::Return, 3),
    (Scancode::Right, 4),
    (Scancode::Left, 5),
    (Scancode::Up, 6),
    (Scancode::Down, 7),
    (Scancode::A, 8), // R
    (Scancode::S, 9), // L
    // extra debug: Save states
    (Scancode::F1, 20),
    (Scancode::F2, 21),
    (Scancode::F5, 22), // home menu (debugger step)
    (Scancode::F8, 23), // frame advance
    (Scancode::F9, 24), // turbo while held
    (Scancode::F10, 25),
];

/// stdin/stdout CLI debugger backend (C: cli-el-backend with no line editing).
struct StdioBackend;

impl CliBackend for StdioBackend {
    fn printf(&mut self, text: &str) {
        print!("{}", text);
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }
    fn poll(&mut self, _timeout_ms: i32) -> i32 {
        1
    }
    fn readline(&mut self) -> Option<String> {
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => Some(line),
            Err(_) => None,
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let rom = std::fs::read(&args.rom)?;

    let mut core: Box<dyn Core> = if rgba_gba::gba::Gba::is_rom(&rom) {
        let g = Gba::new();
        let mut g = *g;
        if let Some(bios_path) = &args.bios {
            let bios = std::fs::read(bios_path)?;
            rgba_gba::bios::bios_load(&mut g, &bios);
        }
        g.load_rom(rom);
        g.arm_reset();
        if args.debug {
            g.debugger_attach();
            g.debugger_attach_module(Box::new(CliDebugger::new(StdioBackend)));
            println!("Debugger CLI active; press F3 to break in.");
        }
        Box::new(g)
    } else if Gb::is_rom(&rom) {
        let g = Gb::new();
        let mut g = *g;
        if let Some(m) = &args.gb_model {
            g.model = name_to_gb_model(m);
        }
        g.load_rom(rom);
        g.sm83_reset();
        if args.debug {
            g.debugger_attach();
            g.debugger_attach_module(Box::new(CliDebugger::new(StdioBackend)));
            println!("Debugger CLI active; press F3 to break in.");
        }
        Box::new(g)
    } else {
        return Err("unrecognized ROM (expected GBA or GB)".into());
    };

    let (w0, h0) = core.base_video_size();

    let sdl = sdl2::init().map_err(|e| e.to_string())?;
    let video = sdl.video().map_err(|e| e.to_string())?;
    let window = video
        .window("rgba", w0 * args.scale, h0 * args.scale)
        .position_centered()
        .resizable()
        .build()?;

    let mut canvas = {
        let b = window.into_canvas().software();
        b.build()?
    };
    let texture_creator = canvas.texture_creator();
    let mut texture =
        texture_creator.create_texture_streaming(PixelFormatEnum::ARGB8888, w0, h0)?;

    // Audio queue at host rate; core pushes at its native rate, we resample
    // with a nearest-neighbor upsample (good enough).
    let audio = sdl.audio().map_err(|e| e.to_string())?;
    let spec = sdl2::audio::AudioSpecDesired {
        freq: Some(args.rate),
        channels: Some(2),
        samples: Some(2048),
    };
    let device = audio
        .open_queue::<i16, _>(None, &spec)
        .map_err(|e| e.to_string())?;

    let src_rate = core.audio_sample_rate().max(1);
    let rate_ratio = args.rate as f32 / src_rate as f32;

    let mut events = sdl.event_pump().map_err(|e| e.to_string())?;

    let mut save_states: Vec<Vec<u8>> = Vec::new();
    let mut frame_total: u64 = 0;
    let start = std::time::Instant::now();

    'running: loop {
        for event in events.poll_iter() {
            if let Event::Quit { .. } = event {
                break 'running;
            }
            if let Event::KeyDown { scancode: Some(s), repeat: false, .. } = event {
                match s {
                    Scancode::Escape => break 'running,
                    Scancode::F3 => {
                        if args.debug {
                            core.debugger_break();
                        }
                    }
                    Scancode::F1 => {
                        let mut buf = Vec::new();
                        if core.save_state(&mut buf).is_ok() {
                            save_states.push(buf);
                            println!("saved state #{}", save_states.len() - 1);
                        }
                    }
                    Scancode::F2 => {
                        if let Some(state) = save_states.pop() {
                            let _ = core.load_state(&state);
                            println!("restored state #{}", save_states.len());
                        }
                    }
                    _ => {}
                }
            }
        }

        let kb = events.keyboard_state();
        let mut keys = 0u32;
        for (scan, bit) in &KEYMAP {
            if *bit < 16 && kb.is_scancode_pressed(*scan) {
                keys |= 1 << bit;
            }
        }
        core.set_keys(keys);

        if args.debug && core.debugger_attached() {
            core.debugger_run_frame();
        } else {
            core.run_frame();
        }
        frame_total += 1;

        // Video
        let buf = core.video_buffer();
        let (w, h) = core.base_video_size();
        let px = unsafe {
            std::slice::from_raw_parts(buf.as_ptr() as *const u8, (w * h * 4) as usize)
        };
        texture.update(None, px, (w * 4) as usize)?;
        canvas.copy(&texture, None, None).map_err(|e| e.to_string())?;
        canvas.present();

        // Audio: drain + naive upsample
        {
            let ring = core.audio_buffer();
            let n = ring.len();
            let mut frames: Vec<i16> = Vec::with_capacity(n * 2 + 64);
            while let Some(l) = ring.pop() {
                let _r = ring.pop().unwrap_or(0);
                frames.push(l);
                frames.push(_r);
            }
            let mut out = Vec::new();
            let stereo_frames = frames.len() / 2;
            let out_frames = (stereo_frames as f32 * rate_ratio) as usize;
            for i in 0..out_frames {
                let idx = ((i as f32) / rate_ratio) as usize * 2;
                let l = frames.get(idx).copied().unwrap_or(0);
                let r = frames.get(idx + 1).copied().unwrap_or(0);
                out.push(l);
                out.push(r);
            }
            if !out.is_empty() {
                let _ = device.queue_audio(&out);
            }
            if device.size() > 16 * 1024 {
                // overflow guard
                device.clear();
            }
        }

        // Pacing
        if !args.no_sync {
            let target = std::time::Duration::from_micros(frame_total * 16667);
            let elapsed = start.elapsed();
            if target > elapsed {
                let wait = (target - elapsed).as_micros() as f64;
                std::thread::sleep(std::time::Duration::from_micros(
                    (wait * 1.0).min(3000.0) as u64,
                ));
            }
        } else {
            std::thread::sleep(std::time::Duration::from_millis(0));
        }
    }

    Ok(())
}

fn name_to_gb_model(name: &str) -> GbModel {
    match name.to_ascii_lowercase().as_str() {
        "dmg" | "gb" => GbModel::Dmg,
        "cgb" | "gbc" => GbModel::Cgb,
        "agb" | "gba" => GbModel::Agb,
        "sgb" => GbModel::Sgb,
        "mgb" => GbModel::Mgb,
        "sgb2" => GbModel::Sgb2,
        "scgb" | "sgbc" => GbModel::Scgb,
        _ => GbModel::Autodetect,
    }
}
