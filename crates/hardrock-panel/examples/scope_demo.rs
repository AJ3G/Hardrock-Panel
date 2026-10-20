//! The panadapter scope on its own, fed by `sdr`'s synthetic demo band.
//!
//! ```text
//! cargo run --release -p hardrock-panel --example scope_demo
//! cargo run --release -p hardrock-panel --example scope_demo -- --rate 768000
//! cargo run --release -p hardrock-panel --example scope_demo -- --screenshot scope.ppm
//! ```
//!
//! `--rate` picks the I/Q rate (one of the DUO's), `--3d` starts with the 3D
//! FFT over the spectrum, `--fft N` sets the FFT size, `--height H` the window's height, `--screenshot` saves the
//! window after a few seconds as a binary PPM and exits. `--cpu` computes the
//! spectrum on the CPU instead of the GPU, `--bench` prints the repaint rate
//! and the I/Q going to the GPU every two seconds. The demo stands in
//! for the radio: a tune moves the pipeline's receiver, a recentre moves the
//! synthetic source's centre (its stations move with it, unlike a real band),
//! and the SPAN key restarts the pipeline at the new rate.

#![allow(dead_code, unused_imports)]

#[path = "../src/chrome.rs"]
mod chrome;
#[path = "../src/layout.rs"]
mod layout;
#[path = "../src/repaint.rs"]
mod repaint;
#[path = "../src/scope/mod.rs"]
mod scope;
#[path = "../src/style.rs"]
mod style;
#[path = "../src/theme.rs"]
mod theme;

use std::sync::Arc;
use std::time::Instant;

use eframe::egui;
use scope::{Scope, ScopeRig, ScopeSettings};
use sdr::{Command, Mode, Pipeline, PipelineConfig, SyntheticSource};

const CENTER_HZ: f64 = 14_200_000.0;

struct Demo {
    scope: Scope,
    pipeline: Pipeline,
    rate: f64,
    center_hz: f64,
    rx_hz: f64,
    mode: Mode,
    passband: (f32, f32),
    transmitting: bool,
    started: Instant,
    screenshot: Option<String>,
    asked: bool,
    gpu_sink: Option<Sink>,
    bench: Option<(Instant, u32)>,
}

type Sink = Arc<dyn Fn(&[f32], f64, f64) + Send + Sync>;

/// The raw I/Q onto the GPU from the pipeline's thread, as the panel's
/// receiver taps it.
fn tap(p: &Pipeline, sink: &Option<Sink>) {
    if let Some(s) = sink.clone() {
        p.send(Command::SetIqTap(Some(Box::new(move |iq, c, r| s(iq, c, r)))));
    }
}

fn start(scope: &Scope, rate: f64, center_hz: f64, rx_hz: f64, mode: Mode) -> Pipeline {
    let cfg = PipelineConfig {
        spectrum: scope.spectrum_config(0.0),
        offset_hz: rx_hz - center_hz,
        mode,
        history: 16,
        ..Default::default()
    };
    Pipeline::spawn(Box::new(SyntheticSource::demo(rate, center_hz)), cfg)
}

impl Demo {
    fn new(cc: &eframe::CreationContext<'_>, args: Args) -> Demo {
        theme::apply(&cc.egui_ctx);
        let Args { rate, screenshot, settings, bench } = args;
        let scope = Scope::new(cc.wgpu_render_state.as_ref(), settings);
        let rx_hz = CENTER_HZ + 12_500.0;
        let mode = Mode::Usb;
        let pipeline = start(&scope, rate, CENTER_HZ, rx_hz, mode);
        let gpu_sink: Option<Sink> = scope.gpu_iq_sink().map(Arc::from);
        tap(&pipeline, &gpu_sink);
        Demo {
            scope,
            pipeline,
            rate,
            center_hz: CENTER_HZ,
            rx_hz,
            mode,
            passband: mode.default_passband(),
            transmitting: false,
            started: Instant::now(),
            screenshot,
            asked: false,
            gpu_sink,
            bench: bench.then(|| (Instant::now(), 0)),
        }
    }

    fn take_screenshot(&mut self, ui: &egui::Ui) {
        let Some(path) = self.screenshot.clone() else { return };
        if !self.asked && self.started.elapsed().as_secs_f32() > 6.0 {
            self.asked = true;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let shot = ui.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let [w, h] = img.size;
            let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
            for p in &img.pixels {
                ppm.extend_from_slice(&[p.r(), p.g(), p.b()]);
            }
            match std::fs::write(&path, ppm) {
                Ok(()) => println!("saved {path} ({w}x{h})"),
                Err(e) => eprintln!("could not write {path}: {e}"),
            }
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for Demo {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let snap = self.pipeline.snapshot();
        let rig = ScopeRig {
            rx_hz: self.rx_hz,
            mode: self.mode,
            passband: self.passband,
            transmitting: self.transmitting,
        };
        let out = egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::PANEL()).inner_margin(11))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.transmitting, "Transmitting (the scope tunes nothing)");
                    ui.label(format!(
                        "RX {:.3} kHz  {:?}  centre {:.3} kHz  {}",
                        self.rx_hz / 1e3,
                        self.mode,
                        self.center_hz / 1e3,
                        snap.source
                    ));
                });
                egui::ScrollArea::vertical().show(ui, |ui| self.scope.ui(ui, &snap, rig)).inner
            })
            .inner;

        for c in out.commands {
            self.pipeline.send(c);
        }
        if let Some(m) = out.mode {
            self.mode = m;
            self.pipeline.set_mode(m);
            self.passband = m.default_passband();
        }
        if let Some((lo, hi)) = out.passband {
            self.passband = (lo, hi);
            self.pipeline.set_passband(lo, hi);
        }
        if let Some(hz) = out.recentre {
            self.center_hz = hz;
            self.pipeline.send(Command::SetCenter(hz));
        }
        if let Some(hz) = out.tune_to {
            self.rx_hz = hz;
        }
        self.pipeline.set_offset_hz(self.rx_hz - self.center_hz);
        if let Some(rate) = out.sample_rate {
            self.rate = rate as f64;
            self.pipeline = start(&self.scope, self.rate, self.center_hz, self.rx_hz, self.mode);
            tap(&self.pipeline, &self.gpu_sink);
        }
        if let Some((t, n)) = self.bench.as_mut() {
            *n += 1;
            let dt = t.elapsed().as_secs_f64();
            if dt >= 2.0 {
                println!(
                    "bench: {:.0} kS/s · {} · {:.1} repaints/s · I/Q to the GPU {:.1} MB/s",
                    self.rate / 1e3,
                    if self.scope.gpu_fft() { "FFT on GPU" } else { "FFT on CPU" },
                    *n as f64 / dt,
                    self.scope.gpu_iq_bytes_per_sec() / 1e6
                );
                *t = Instant::now();
                *n = 0;
            }
        }
        self.take_screenshot(ui);
        ui.ctx().request_repaint();
    }
}

struct Args {
    rate: f64,
    screenshot: Option<String>,
    settings: ScopeSettings,
    bench: bool,
}

fn main() -> eframe::Result {
    let mut a = Args {
        rate: 192_000.0,
        screenshot: None,
        settings: ScopeSettings::default(),
        bench: false,
    };
    let mut height = 860.0;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--rate" => a.rate = args.next().and_then(|r| r.parse().ok()).unwrap_or(a.rate),
            "--fft" => {
                let n = args.next().and_then(|r| r.parse().ok());
                a.settings.fft_size = n.unwrap_or(a.settings.fft_size);
            }
            "--3d" => a.settings.top = scope::settings::TopView::Both,
            "--height" => {
                let h = args.next().and_then(|r| r.parse().ok());
                height = h.unwrap_or(height);
            }
            "--screenshot" => a.screenshot = args.next(),
            "--cpu" => a.settings.fft_gpu = false,
            "--bench" => a.bench = true,
            _ => eprintln!("unknown argument {arg}"),
        }
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Scope demo")
            .with_inner_size([1280.0, height]),
        wgpu_options: scope::gpu::wgpu_options(),
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native("Scope demo", options, Box::new(move |cc| Ok(Box::new(Demo::new(cc, a)))))
}
