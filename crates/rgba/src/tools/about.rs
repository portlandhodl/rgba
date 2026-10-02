// "About..." (mGBA Qt AboutScreen) and "Report bug..." (mGBA Qt ReportView).
// mGBA's ReportView gathers system information and opens mGBA's issue
// tracker; rgba is a separate port, so the report is only collected here
// for the user to copy or save and send to the rgba maintainers.

use eframe::egui;

use super::{ToolCtx, ToolWindow};

#[derive(Default)]
pub struct About;

impl ToolWindow for About {
    fn title(&self) -> &'static str {
        "About rgba"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, _tc: &mut ToolCtx) {
        egui::Window::new(self.title())
            .open(open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("rgba");
                    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                    ui.add_space(4.0);
                    ui.label("A Rust port of the mGBA Game Boy Advance / Game Boy emulator.");
                });
                ui.add_space(8.0);
                ui.label(
                    "Based on mGBA by Jeffrey Pfau and contributors. rgba is a derived work \
                     of mGBA and, like mGBA, is distributed under the Mozilla Public License 2.0.",
                );
                ui.horizontal(|ui| {
                    ui.label("mGBA:");
                    ui.hyperlink("https://mgba.io");
                });
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "Game Boy and Game Boy Advance are trademarks of Nintendo Co., Ltd. \
                         rgba is not affiliated with Nintendo.",
                    )
                    .small()
                    .weak(),
                );
            });
    }
}

#[derive(Default)]
pub struct BugReport {
    report: String,
}

fn build_report(tc: &mut ToolCtx) -> String {
    use std::fmt::Write;
    let mut r = String::new();
    let _ = writeln!(r, "# rgba bug report");
    let _ = writeln!(r);
    let _ = writeln!(r, "## Build");
    let _ = writeln!(r, "rgba version: {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(r, "Target: {} / {}", std::env::consts::OS, std::env::consts::ARCH);
    if let Ok(os) = std::fs::read_to_string("/etc/os-release") {
        if let Some(name) = os.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=")) {
            let _ = writeln!(r, "OS: {}", name.trim_matches('"'));
        }
    }
    let _ = writeln!(r);
    let _ = writeln!(r, "## Game");
    match tc.session.as_deref_mut() {
        Some(s) => {
            let id = super::rom_info::game_ident(s);
            let _ = writeln!(r, "Title: {}", id.title);
            let _ = writeln!(r, "Code: {}", if id.code.is_empty() { "(none)" } else { &id.code });
            let _ = writeln!(r, "Maker: {}  Version: {}", id.maker, id.version);
            let _ = writeln!(r, "Platform: {}", id.system);
            let _ = writeln!(r, "CRC32: {:08x}", id.crc32);
            let _ = writeln!(r, "ROM size: {} bytes", id.rom_size);
            let _ = writeln!(r, "Save type: {}", id.save_type);
            let _ = writeln!(r, "Frame: {}", s.core().frame_counter());
        }
        None => {
            let _ = writeln!(r, "(no game running)");
        }
    }
    let c = &tc.config;
    let _ = writeln!(r);
    let _ = writeln!(r, "## Settings");
    let _ = writeln!(r, "GBA BIOS: {}", if c.gba_bios.is_some() { "set" } else { "none (HLE)" });
    let _ = writeln!(r, "Use BIOS: {}  Skip BIOS: {}", c.use_bios, c.skip_bios);
    let _ = writeln!(r, "GB model: {}", c.gb_model);
    let _ = writeln!(r, "Frameskip: {}  FPS target: {:.4}", c.frameskip, c.fps_target);
    let _ = writeln!(r, "Fast forward speed: {}", if c.ff_speed <= 0.0 { "unbounded".into() } else { format!("{}x", c.ff_speed) });
    let _ = writeln!(r, "Rewind: {} (capacity {}, interval {})", c.rewind_enable, c.rewind_capacity, c.rewind_interval);
    let _ = writeln!(r, "Audio: {} Hz, volume {:.2}, mute {}", c.sample_rate, c.volume, c.mute);
    let _ = writeln!(r, "Video: scale {}x, lock aspect {}, integer scaling {}, bilinear {}, interframe blending {}",
        c.frame_scale, c.lock_aspect, c.integer_scaling, c.bilinear, c.interframe_blending);
    let _ = writeln!(r, "Idle optimization: {}", c.idle_optimization);
    let _ = writeln!(r);
    let _ = writeln!(r, "## Recent log");
    let lines = tc.log.snapshot();
    for l in lines.iter().rev().take(50).collect::<Vec<_>>().into_iter().rev() {
        let _ = writeln!(r, "[{:?}] {}: {}", l.level, l.category, l.message);
    }
    let _ = writeln!(r);
    let _ = writeln!(r, "## Description");
    let _ = writeln!(r, "(Describe what happened and how to reproduce it.)");
    r
}

impl ToolWindow for BugReport {
    fn title(&self) -> &'static str {
        "Report bug"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        if self.report.is_empty() {
            self.report = build_report(tc);
        }
        egui::Window::new(self.title()).open(open).default_size([520.0, 420.0]).show(ctx, |ui| {
            ui.label(
                "rgba is a port of mGBA — please don't send rgba bugs to the mGBA tracker. \
                 Copy or save this report and send it to the rgba project maintainers.",
            );
            ui.horizontal(|ui| {
                if ui.button("Refresh").clicked() {
                    self.report = build_report(tc);
                }
                if ui.button("Copy to clipboard").clicked() {
                    ui.ctx().copy_text(self.report.clone());
                    tc.notice("Bug report copied");
                }
                if ui.button("Save report...").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .set_file_name("rgba-report.md")
                        .add_filter("Markdown", &["md", "txt"])
                        .save_file()
                    {
                        match std::fs::write(&p, &self.report) {
                            Ok(()) => tc.notice(format!("Saved {}", p.display())),
                            Err(e) => tc.notice(format!("Save failed: {e}")),
                        }
                    }
                }
            });
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.report)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY),
                );
            });
        });
        if !*open {
            self.report.clear();
        }
    }
}
