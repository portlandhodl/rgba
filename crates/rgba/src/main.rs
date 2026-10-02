// Copyright (c) 2013-2021 Jeffrey Pfau (mGBA), MPL-2.0.
// rgba frontend: an egui window with mGBA Qt's menu structure (Window.cpp)
// driving the GBA/GB cores, with SDL2 used for audio output.

use std::path::PathBuf;

use clap::Parser;

mod app;
mod archive;
mod audio;
mod config;
mod emu;
mod input;
mod logbuf;
mod screenshot;
mod stdio_debugger;
mod tools;

#[derive(Parser, Debug)]
#[command(name = "rgba", about = "rgba — a Rust port of mGBA (GBA/GB emulator)")]
struct Args {
    /// ROM image path (.gba/.gbc/.gb/.zip); optional — use File → Load ROM
    rom: Option<PathBuf>,
    /// GBA BIOS image (saved to settings; HLE BIOS otherwise)
    #[arg(long)]
    bios: Option<PathBuf>,
    /// Boot model for Game Boy ROMs (e.g. dmg, cgb, sgb, mgb, sgb2, scgb)
    #[arg(long)]
    gb_model: Option<String>,
    /// Cheats file (mGBA .cheats format)
    #[arg(long)]
    cheats: Option<PathBuf>,
    /// ROM patch file (.ips/.ups/.bps), applied before load (mGBA -p)
    #[arg(long)]
    patch: Option<PathBuf>,
    /// Run unthrottled (no frame pacing)
    #[arg(long)]
    no_sync: bool,
    /// Initial window scale (1-8)
    #[arg(long)]
    scale: Option<u32>,
    /// Audio sample rate
    #[arg(long)]
    rate: Option<i32>,
    /// Attach mGBA's CLI debugger on this terminal (mGBA -d)
    #[arg(long)]
    debug: bool,
    /// Enable rewind (hold ` to rewind)
    #[arg(long)]
    rewind: bool,
    /// Record an mVL video log to this file (mGBA -v/--video-log)
    #[arg(short = 'v', long)]
    video_log: Option<PathBuf>,
    /// Force Game Boy Player detection (mGBA gba.forceGbp)
    #[arg(long)]
    gbp: bool,
}

fn main() -> eframe::Result<()> {
    let args = Args::parse();
    let mut cfg = config::Config::load();
    if let Some(b) = args.bios {
        cfg.gba_bios = Some(b);
    }
    if let Some(m) = args.gb_model {
        cfg.gb_model = m;
    }
    if let Some(s) = args.scale {
        cfg.frame_scale = s.clamp(1, 8);
    }
    if let Some(r) = args.rate {
        cfg.sample_rate = r;
    }
    if args.rewind {
        cfg.rewind_enable = true;
    }
    let startup = app::Startup {
        rom: args.rom,
        patch: args.patch,
        cheats: args.cheats,
        video_log: args.video_log,
        gbp: args.gbp,
        stdio_debugger: args.debug,
        unthrottled: args.no_sync,
    };
    let scale = cfg.frame_scale as f32;
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("rgba")
            .with_inner_size([240.0 * scale, 160.0 * scale + 24.0])
            .with_min_inner_size([240.0, 184.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "rgba",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, cfg, startup)))),
    )
}
