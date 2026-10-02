// "Game Pak sensors..." — mirrors mGBA Qt's SensorView.cpp: realtime-clock
// source (system time / fixed time / offset), light sensor (Boktai solar
// sensor), tilt sensor and gyroscope, plus a rumble indicator.
//
// Tilt and gyro feed the GBA cart's `hw.rotation` (mRotationSource): the raw
// int32 values the C's readTiltX/readTiltY/readGyroZ callbacks return. The
// Qt view displays tilt as raw / 469762048 (0xE0 << 21); the sliders here use
// the same scale. The RTC override is a plain `fn() -> i64` in the cores,
// so its mode/value live in process-wide atomics.

use std::sync::atomic::{AtomicI64, AtomicU8, Ordering};

use eframe::egui;

use super::{no_game, ToolCtx, ToolWindow};
use crate::emu::{Console, LUX_LEVELS};

/// SensorView tilt display scale (0xE0 << 21).
const TILT_SCALE: f32 = 469_762_048.0;

const RTC_SYSTEM: u8 = 0;
const RTC_FIXED: u8 = 1;
const RTC_OFFSET: u8 = 2;
static RTC_MODE: AtomicU8 = AtomicU8::new(RTC_SYSTEM);
/// Fixed: unix seconds. Offset: seconds added to system time.
static RTC_VALUE: AtomicI64 = AtomicI64::new(0);

fn system_time() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The RTC source installed into the cores (mRTCGenericSource:
/// RTC_NO_OVERRIDE / RTC_FIXED / RTC_FAKE_EPOCH-like offset).
fn rtc_now() -> i64 {
    match RTC_MODE.load(Ordering::Relaxed) {
        RTC_FIXED => RTC_VALUE.load(Ordering::Relaxed),
        RTC_OFFSET => system_time() + RTC_VALUE.load(Ordering::Relaxed),
        _ => system_time(),
    }
}

pub struct SensorView {
    rtc_mode: u8,
    /// Fixed time as editable fields.
    fixed: [i64; 6],
    offset: i64,
    tilt: [f32; 2],
    gyro: f32,
    gyro_sensitivity: f32,
    follow_mouse: bool,
}

impl Default for SensorView {
    fn default() -> Self {
        let (y, mo, d, h, mi, s) = civil(system_time());
        SensorView {
            rtc_mode: RTC_SYSTEM,
            fixed: [y, mo, d, h, mi, s],
            offset: 0,
            tilt: [0.0; 2],
            gyro: 0.0,
            gyro_sensitivity: 1.0,
            follow_mouse: false,
        }
    }
}

/// Unix seconds → (year, month, day, hour, min, sec), UTC.
fn civil(t: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    // Howard Hinnant's civil_from_days
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    (y, m, d, secs / 3600, secs % 3600 / 60, secs % 60)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

impl SensorView {
    fn fixed_unix(&self) -> i64 {
        let [y, mo, d, h, mi, s] = self.fixed;
        days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + s
    }

    fn publish_rtc(&self) {
        let v = match self.rtc_mode {
            RTC_FIXED => self.fixed_unix(),
            RTC_OFFSET => self.offset,
            _ => 0,
        };
        RTC_VALUE.store(v, Ordering::Relaxed);
        RTC_MODE.store(self.rtc_mode, Ordering::Relaxed);
    }
}

impl ToolWindow for SensorView {
    fn title(&self) -> &'static str {
        "Game Pak sensors"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool, tc: &mut ToolCtx) {
        egui::Window::new(self.title()).open(open).resizable(false).show(ctx, |ui| {
            // ---- Realtime clock
            ui.strong("Realtime clock");
            let before = (self.rtc_mode, self.fixed, self.offset);
            ui.radio_value(&mut self.rtc_mode, RTC_SYSTEM, "System time");
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.rtc_mode, RTC_FIXED, "Fixed time");
                ui.add(egui::DragValue::new(&mut self.fixed[1]).range(1..=12).prefix("M "));
                ui.add(egui::DragValue::new(&mut self.fixed[2]).range(1..=31).prefix("D "));
                ui.add(egui::DragValue::new(&mut self.fixed[0]).range(1970..=2099).prefix("Y "));
                ui.add(egui::DragValue::new(&mut self.fixed[3]).range(0..=23));
                ui.label(":");
                ui.add(egui::DragValue::new(&mut self.fixed[4]).range(0..=59));
                ui.label(":");
                ui.add(egui::DragValue::new(&mut self.fixed[5]).range(0..=59));
                if ui.button("Now").clicked() {
                    let (y, mo, d, h, mi, s) = civil(system_time());
                    self.fixed = [y, mo, d, h, mi, s];
                }
            });
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.rtc_mode, RTC_OFFSET, "Offset time");
                ui.add(egui::DragValue::new(&mut self.offset).suffix(" sec"));
            });
            ui.label(egui::RichText::new("Times are UTC.").small().weak());
            if (self.rtc_mode, self.fixed, self.offset) != before {
                self.publish_rtc();
            }

            let Some(s) = tc.session.as_deref_mut() else {
                ui.separator();
                no_game(ui);
                return;
            };
            // Install our clock source (cheap; keeps it after reloads).
            match &mut s.console {
                Console::Gba(g) => g.rtc_time_fn = rtc_now,
                Console::Gb(g) => g.rtc_time_fn = rtc_now,
            }

            // ---- Light sensor
            ui.separator();
            ui.strong("Light sensor");
            if let Console::Gba(g) = &mut s.console {
                // InputController luminance value: the core reads 0xFF - value.
                let mut value = 0xFF - g.light_sensor_level as i32;
                ui.horizontal(|ui| {
                    ui.label("Brightness");
                    let r1 = ui.add(egui::Slider::new(&mut value, 0..=255));
                    if r1.changed() {
                        g.light_sensor_level = (0xFF - value.clamp(0, 255)) as u8;
                    }
                });
                let level = LUX_LEVELS.iter().rposition(|&l| value >= l as i32).map_or(0, |i| i as i32 + 1);
                ui.horizontal(|ui| {
                    ui.label(format!("Solar level {level}"));
                    if ui.small_button("−").clicked() {
                        s.set_solar_level(level - 1);
                    }
                    if ui.small_button("+").clicked() {
                        s.set_solar_level(level + 1);
                    }
                });
            } else {
                ui.label("GBA only.");
            }

            // ---- Tilt / gyro
            ui.separator();
            ui.strong("Tilt sensor");
            ui.checkbox(&mut self.follow_mouse, "Follow the mouse (position over the window)");
            if self.follow_mouse {
                if let Some(p) = ctx.input(|i| i.pointer.latest_pos()) {
                    let r = ctx.content_rect();
                    self.tilt[0] = ((p.x - r.center().x) / (r.width() / 2.0)).clamp(-1.0, 1.0);
                    self.tilt[1] = ((p.y - r.center().y) / (r.height() / 2.0)).clamp(-1.0, 1.0);
                }
            }
            ui.add(egui::Slider::new(&mut self.tilt[0], -1.0..=1.0).text("X"));
            ui.add(egui::Slider::new(&mut self.tilt[1], -1.0..=1.0).text("Y"));
            if ui.small_button("Center").clicked() {
                self.tilt = [0.0; 2];
            }
            ui.separator();
            ui.strong("Gyroscope");
            ui.add(egui::Slider::new(&mut self.gyro, -1.0..=1.0).text("Rotation"));
            ui.add(egui::Slider::new(&mut self.gyro_sensitivity, 0.0..=4.0).text("Sensitivity"));
            if ui.small_button("Stop").clicked() {
                self.gyro = 0.0;
            }
            match &mut s.console {
                Console::Gba(g) => {
                    g.hw.rotation.tilt_x = (self.tilt[0] * TILT_SCALE) as i32;
                    g.hw.rotation.tilt_y = (self.tilt[1] * TILT_SCALE) as i32;
                    let gz = (self.gyro * self.gyro_sensitivity).clamp(-1.0, 1.0) as f64 * i32::MAX as f64;
                    g.hw.rotation.gyro_z = gz as i32;
                }
                Console::Gb(_) => {
                    ui.label("GB (MBC7) tilt input is not wired up in this port.");
                }
            }

            // ---- Rumble
            ui.separator();
            let rumble = match &s.console {
                Console::Gba(g) => g.rumble_active,
                Console::Gb(g) => g.memory.rumble_state,
            };
            ui.horizontal(|ui| {
                ui.label("Rumble:");
                ui.colored_label(
                    if rumble { egui::Color32::LIGHT_GREEN } else { egui::Color32::GRAY },
                    if rumble { "● active" } else { "○ off" },
                );
            });
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        });
    }
}
