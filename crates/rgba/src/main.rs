// Copyright (c) 2013-2021 Jeffrey Pfau (mGBA), MPL-2.0.
// Minimal SDL2 frontend: window + streaming texture + queued audio + input,
// save/load state, and (GB-only for now) peripheral sync stubs.

use std::path::{Path, PathBuf};

use rgba_core::audio_resampler::{AudioResampler, InterpolatorType};
use rgba_core::core::Core;
use rgba_core::ring::RingI16;
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
    /// ROM patch file (.ips/.ups/.bps), applied before load (mGBA -p)
    #[arg(long)]
    patch: Option<PathBuf>,
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
    /// Enable rewind (hold R to rewind, one state per 30 frames kept in RAM)
    #[arg(long)]
    rewind: bool,
    /// Force Game Boy Player detection screen check (mGBA gba.forceGbp)
    #[arg(long)]
    gbp: bool,
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

/// --patch (mGBA: mCore::loadPatch → loadPatch + GBApplyPatch/GBAApplyPatch).
/// Here the patch is applied to the ROM buffer before load_rom instead of
/// swapping out the in-emulator ROM; GBApplyPatch clamps to GB_SIZE_CART_MAX,
/// GBAApplyPatch rejects anything past GBA_SIZE_ROM0.
fn patch_rom_buffer(rom: &mut Vec<u8>, patch_path: &Path, max_size: usize, clamp: bool) {
    let patch_bytes = match std::fs::read(patch_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("failed to read patch {}: {}", patch_path.display(), e);
            return;
        }
    };
    let patch = match rgba_core::patch::Patch::load(&patch_bytes) {
        Some(p) => p,
        None => {
            eprintln!("unrecognized patch format: {}", patch_path.display());
            return;
        }
    };
    let mut out_size = match patch.output_size(rom.len()) {
        Some(s) if s > 0 => s,
        _ => {
            eprintln!("patch does not apply to this ROM");
            return;
        }
    };
    if out_size > max_size {
        if clamp {
            out_size = max_size;
        } else {
            eprintln!("patched ROM too large");
            return;
        }
    }
    let mut out = vec![0u8; out_size];
    if patch.apply(rom, &mut out) {
        *rom = out;
        println!("applied patch {}", patch_path.display());
    } else {
        eprintln!("failed to apply patch {}", patch_path.display());
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let mut rom = std::fs::read(&args.rom)?;

    let mut core: Box<dyn Core> = if rgba_gba::gba::Gba::is_rom(&rom) {
        let g = Gba::new();
        let mut g = *g;
        if let Some(bios_path) = &args.bios {
            let bios = std::fs::read(bios_path)?;
            rgba_gba::bios::bios_load(&mut g, &bios);
        }
        if let Some(patch_path) = &args.patch {
            patch_rom_buffer(&mut rom, patch_path, rgba_gba::memory::GBA_SIZE_ROM0, false);
        }
        g.force_gbp = args.gbp; // mGBA config gba.forceGbp
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
        if let Some(patch_path) = &args.patch {
            patch_rom_buffer(&mut rom, patch_path, rgba_gb::memory::GB_SIZE_CART_MAX, true);
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

    // Audio queue at host rate; the core pushes interleaved stereo at its
    // native rate into a ring, and each frame we resample into the device
    // rate exactly like mGBA's SDL frontend (src/platform/sdl/sdl-audio.c
    // _mSDLAudioCallback): sinc mAudioResampler, source = core buffer
    // (consumed), destination = a `samples`-frame staging buffer drained
    // into the SDL queue. fauxClock is 1 (no fps-target override).
    let audio = sdl.audio().map_err(|e| e.to_string())?;
    let spec = sdl2::audio::AudioSpecDesired {
        freq: Some(args.rate),
        channels: Some(2),
        samples: Some(2048),
    };
    let device = audio
        .open_queue::<i16, _>(None, &spec)
        .map_err(|e| e.to_string())?;

    let src_rate = core.audio_sample_rate().max(1) as f64;
    let dst_rate = device.spec().freq.max(1) as f64; // obtainedSpec.freq
    let mut resampler = AudioResampler::new(InterpolatorType::Sinc);
    // mAudioBufferInit(&context->buffer, context->samples, 2): 2048 frames.
    let mut resampled = RingI16::new(2048 * 2);

    let mut events = sdl.event_pump().map_err(|e| e.to_string())?;

    let mut save_states: Vec<Vec<u8>> = Vec::new();
    let mut rewind_ctx = if args.rewind {
        Some(rgba_core::rewind::RewindContext::new(300))
    } else {
        None
    };
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

        if let Some(rc) = rewind_ctx.as_mut() {
            let kb2 = events.keyboard_state();
            if kb2.is_scancode_pressed(Scancode::R) {
                if !rc.restore(&mut *core, 2) {
                    // empty rewind history
                }
            } else if frame_total % 30 == 0 {
                rc.append(&mut *core);
            }
        }

        // Video
        let buf = core.video_buffer();
        let (w, h) = core.base_video_size();
        let px = unsafe {
            std::slice::from_raw_parts(buf.as_ptr() as *const u8, (w * h * 4) as usize)
        };
        texture.update(None, px, (w * 4) as usize)?;
        canvas.copy(&texture, None, None).map_err(|e| e.to_string())?;
        canvas.present();

        // Audio: resample + queue (mAudioResamplerProcess + mAudioBufferRead
        // into SDL, single-threaded so no mCoreSyncLockAudio needed).
        {
            let produced = resampler.process(
                core.audio_buffer(),
                src_rate,
                true,
                &mut resampled,
                dst_rate,
            );
            let _ = produced;
            let avail = resampled.len();
            if avail > 0 {
                let mut buf = vec![0i16; avail];
                let got = resampled.read_into(&mut buf);
                let _ = device.queue_audio(&buf[..got]);
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
