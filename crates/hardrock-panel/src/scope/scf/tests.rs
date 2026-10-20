//! The SSCA against theory (CPU reference) and the GPU against the CPU
//! reference (when a wgpu adapter is to be had, e.g. lavapipe).

use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

use eframe::egui_wgpu::wgpu;

use super::engine::{self, BlockParams, Engine, EngineCfg, Measure, Readable, Taper};
use super::reference::*;

/// Run a future to completion on this thread (wgpu's native futures are
/// ready at once or after a poll; no executor crate needed).
fn block_on<F: Future>(f: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(1));
    }
}

/// A device to test on, if this machine has one (lavapipe counts). Tests
/// that need it pass without it, saying so.
pub(super) fn gpu() -> Option<(wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::VULKAN | wgpu::Backends::METAL | wgpu::Backends::DX12;
    let instance = wgpu::Instance::new(desc);
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .map_err(|e| eprintln!("no adapter: {e}"))
    .ok()?;
    let limits = adapter.limits();
    let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("scf-test"),
        required_limits: wgpu::Limits {
            max_storage_buffer_binding_size: limits.max_storage_buffer_binding_size,
            max_buffer_size: limits.max_buffer_size,
            ..wgpu::Limits::default()
        },
        ..Default::default()
    }))
    .map_err(|e| eprintln!("no device: {e}"))
    .ok()?;
    engine::supported(&adapter, &device).then_some((adapter, device, queue))
}

fn parse(src: &str) -> naga::Module {
    let module =
        naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{e:?}"));
    module
}

/// The size of the struct type `name` in `module`, bytes.
fn struct_size(module: &naga::Module, name: &str) -> u32 {
    let mut layouter = naga::proc::Layouter::default();
    layouter.update(module.to_ctx()).unwrap();
    let (h, _) = module
        .types
        .iter()
        .find(|(_, t)| t.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("no struct {name}"));
    layouter[h].size
}

#[test]
fn the_compute_shader_validates_and_its_uniforms_match_the_host() {
    let m = parse(include_str!("shaders/ssca.wgsl"));
    assert_eq!(struct_size(&m, "Params") as usize, engine::PARAMS_BYTES);
    assert_eq!(struct_size(&m, "PassCfg") as usize, engine::PASS_BYTES);
    // std140-style: every uniform block a whole number of 16-byte rows.
    assert_eq!(engine::PARAMS_BYTES % 16, 0);
    assert_eq!(engine::PASS_BYTES % 16, 0);
}

#[test]
fn the_reference_fft_is_a_dft() {
    let mut rng = Rng::new(3);
    let x = noise(64, 1.0, &mut rng);
    let mut y = x.clone();
    fft(&mut y);
    for k in [0usize, 1, 17, 63] {
        let mut s = C::default();
        for (n, v) in x.iter().enumerate() {
            s = s + *v * C::cis(-std::f64::consts::TAU * (k * n) as f64 / 64.0);
        }
        assert!((s - y[k]).norm2() < 1e-18, "bin {k}");
    }
}

#[test]
fn a_tone_has_unit_coherence_at_zero_cycle_frequency() {
    let (n, nc) = (256, 16);
    let x: Vec<C> = (0..n + nc).map(|i| C::cis(std::f64::consts::TAU * 0.125 * i as f64)).collect();
    let av = averaged(&x, n, nc, Taper::Hamming, false, 1);
    // f = 0.125: channel k = 2, and q = −2·N/N' for α = 0.
    let v = av.value(2, -(n as i64) * 2 / nc as i64, Measure::Coherence);
    assert!((v - 1.0).abs() < 1e-9, "{v}");
    let s = av.value(2, -(n as i64) * 2 / nc as i64, Measure::Scf);
    assert!((s - 1.0).abs() < 1e-9, "SCF of a unit tone {s}");
}

/// The cyclic-domain profile from the strip points: the strongest value
/// with α within `tol` of each α in `alphas`.
fn profile_at(pts: &[(f64, f64, f64)], alpha: f64, tol: f64) -> f64 {
    pts.iter().filter(|p| (p.0 - alpha).abs() <= tol).map(|p| p.2).fold(0.0, f64::max)
}

/// The median over α of the profile, leaving out α near `skip`.
fn profile_median(pts: &[(f64, f64, f64)], n: usize, skip: &[f64], margin: f64) -> f64 {
    let mut best = std::collections::BTreeMap::<i64, f64>::new();
    for p in pts {
        if skip.iter().any(|s| (p.0 - s).abs() < margin) {
            continue;
        }
        let b = (p.0 * n as f64).round() as i64;
        let e = best.entry(b).or_insert(0.0);
        *e = e.max(p.2);
    }
    let mut v: Vec<f64> = best.into_values().collect();
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

#[test]
fn gaussian_noise_gives_a_flat_cycle_frequency_plane() {
    let (n, nc, blocks) = (1024usize, 32usize, 24usize);
    let mut rng = Rng::new(11);
    let x = noise(n * blocks + nc, 1.0, &mut rng);
    let av = averaged(&x, n, nc, Taper::Hamming, false, blocks as u32);
    let pts = av.points(Measure::Coherence);
    // Theory: E|ρ|² = (Σg²/(Σg)²)·((Σa)²/Σa²) for white noise, any α ≠ 0.
    let a = Taper::Hamming.coefficients(nc);
    let g = Taper::Hamming.coefficients(n);
    let sq = |w: &[f64]| w.iter().map(|v| v * v).sum::<f64>();
    let sum = |w: &[f64]| w.iter().sum::<f64>();
    let floor = sq(&g) / sum(&g).powi(2) * sum(&a).powi(2) / sq(&a);
    // Cycle frequencies alias modulo the sample rate: α = ±1 is α = 0 (the
    // PSD) again, so the plane is 0 < |α| < 1, away from both ends.
    let away = |a: f64| a.abs() > 4.0 / n as f64 && a.abs() < 1.0 - 2.0 / nc as f64;
    // That plane in 16 bands of α: each band's mean is the floor.
    let mut bands = vec![(0.0, 0usize); 16];
    for p in &pts {
        if !away(p.0) || p.1.abs() + p.0.abs() / 2.0 > 0.5 {
            continue;
        }
        let b = (((p.0 + 1.0) / 2.0) * 16.0).floor().clamp(0.0, 15.0) as usize;
        bands[b].0 += p.2;
        bands[b].1 += 1;
    }
    for (i, (s, c)) in bands.iter().enumerate() {
        if *c < 100 {
            continue;
        }

        let m = s / *c as f64;
        assert!((m / floor - 1.0).abs() < 0.05, "band {i}: mean {m:.5} vs theory {floor:.5}");
    }
    // And nothing stands out: averaged over 24 blocks, each point is the
    // floor ± floor/√24, so the largest of ~32 000 sits under 2.5× the floor.
    let mx = pts.iter().filter(|p| away(p.0)).map(|p| p.2).fold(0.0, f64::max);
    assert!(mx < 2.5 * floor, "max {mx:.4} vs floor {floor:.4}");
}

#[test]
fn bpsk_shows_its_features_at_twice_the_carrier_and_the_symbol_rate() {
    // Real BPSK: x·x* has features at α = ±2fc (and ±2fc ± k·Rs) and at
    // α = k·Rs, as theory has it for a real signal.
    let (n, nc, blocks) = (1024usize, 32usize, 8usize);
    // Rs = 3/64: no harmonic 2fc ± k·Rs lands on −2fc (or on α ≡ −2fc
    // modulo 1, where cycle frequencies alias).
    let (fc, rs) = (0.125, 3.0 / 64.0);
    let mut rng = Rng::new(5);
    let len = n * blocks + nc;
    let x = mix(&bpsk(len, 1.0, rs, fc, 1.0, true, &mut rng), &noise(len, 0.1, &mut rng));
    let av = averaged(&x, n, nc, Taper::Hamming, false, blocks as u32);
    let pts = av.points(Measure::Coherence);
    let tol = 1.5 / n as f64;
    let mut feats = vec![0.0];
    for k in -8..=8 {
        feats.push(k as f64 * rs);
        feats.push(2.0 * fc + k as f64 * rs);
        feats.push(-2.0 * fc + k as f64 * rs);
    }
    let med = profile_median(&pts, n, &feats, 4.0 / n as f64);
    let db = |v: f64| 10.0 * (v / med).log10();
    for a in [rs, -rs, 2.0 * fc, -2.0 * fc] {
        let v = db(profile_at(&pts, a, tol));
        assert!(v > 10.0, "α = {a}: {v:.1} dB over the median");
    }
    // Every multiple of 1/64 is some harmonic here; halfway between two
    // there is nothing.
    let off = db(profile_at(&pts, 10.5 / 64.0, tol));
    assert!(off < 6.0, "α = 10.5/64 has nothing: {off:.1} dB");

    // Complex BPSK on +fc: x·x (conjugate) puts it at α = 2fc only; x·x*
    // does not have it.
    let x = mix(&bpsk(len, 1.0, rs, fc, 1.0, false, &mut rng), &noise(len, 0.1, &mut rng));
    for conj in [true, false] {
        let av = averaged(&x, n, nc, Taper::Hamming, conj, blocks as u32);
        let pts = av.points(Measure::Coherence);
        let med = profile_median(&pts, n, &feats, 4.0 / n as f64);
        let at = |a: f64| 10.0 * (profile_at(&pts, a, tol) / med).log10();
        if conj {
            assert!(at(2.0 * fc) > 15.0, "conjugate α = 2fc: {:.1}", at(2.0 * fc));
            assert!(at(-2.0 * fc) < 6.0, "conjugate α = −2fc: {:.1}", at(-2.0 * fc));
        } else {
            assert!(at(2.0 * fc) < 6.0, "x·x* at 2fc: {:.1}", at(2.0 * fc));
            assert!(at(rs) > 15.0, "x·x* at Rs: {:.1}", at(rs));
        }
    }
}

fn run_gpu(
    dev: &(wgpu::Adapter, wgpu::Device, wgpu::Queue),
    cfg: EngineCfg,
    x: &[C],
    bp: BlockParams,
    avg: u32,
) -> Engine {
    let mut e = Engine::new(&dev.1, &dev.2, cfg);
    let n = cfg.n as usize;
    let mut at = 0;
    let mut b = 0;
    while at + e.block_len() <= x.len() {
        let blk = interleave(&x[at..at + e.block_len()]);
        let p = BlockParams { beta: beta(b, avg) as f32, ..bp };
        e.process(&blk, &p);
        at += n;
        b += 1;
    }
    e.flush();
    e
}

#[test]
fn the_gpu_matches_the_cpu_reference() {
    let Some(dev) = gpu() else {
        eprintln!("no wgpu adapter here: GPU comparison skipped (the shader is still validated)");
        return;
    };
    eprintln!("GPU: {:?}", dev.0.get_info().name);
    // One-pass strips (N ≤ 2048) and the four-step FFT, odd and even log2;
    // grid cells coarser than the strip points and (the last) finer.
    for (n, nc, conj, a_hi) in [
        (1024u32, 32u32, false, 0.5f32),
        (8192, 64, true, 0.5),
        (2048, 16, false, 0.5),
        // (Dyadic ranges, so f32 and f64 round the cells alike.)
        (4096, 64, false, 1.0 / 256.0),
    ] {
        let mut rng = Rng::new(n as u64);
        let blocks = 3;
        let len = n as usize * blocks + nc as usize;
        let x =
            mix(&bpsk(len, 1.0, 1.0 / 20.0, 0.1, 1.0, false, &mut rng), &noise(len, 0.5, &mut rng));
        let (ga, gf) = (64u32, nc);
        let cfg = EngineCfg { n, nc, grid_f: gf, grid_a: ga, taper: Taper::Hamming };
        let a_range = (0.0f32, a_hi);
        let f_range = (-0.5f32, 0.5f32);
        let bp = BlockParams {
            conj,
            measure: Measure::Coherence,
            beta: 1.0,
            a_range,
            f_range,
            a_mask: 0.0,
        };
        let e = run_gpu(&dev, cfg, &x, bp, 4);
        let cpu = averaged(&x, n as usize, nc as usize, Taper::Hamming, conj, 4);
        let acc = e.read_f32(Readable::Acc);
        let mx = cpu.s2.iter().copied().fold(0.0, f64::max);
        let mut worst = 0.0f64;
        for (g, c) in acc.iter().zip(&cpu.s2) {
            worst = worst.max((*g as f64 - c).abs() / mx);
        }
        assert!(worst < 2e-6, "N {n} N' {nc}: |S|² off by {worst:e} of the peak");
        let psd = e.read_f32(Readable::Psd);
        for (g, c) in psd.iter().zip(&cpu.psd) {
            assert!((*g as f64 / c - 1.0).abs() < 1e-4, "PSD {g} vs {c}");
        }
        let grid = e.read_f32(Readable::Grid);
        let cg = cpu.grid(
            Measure::Coherence,
            gf as usize,
            ga as usize,
            (a_range.0 as f64, a_range.1 as f64),
            (f_range.0 as f64, f_range.1 as f64),
        );
        let mut bad = 0;
        for (g, c) in grid.iter().zip(&cg) {
            if *c > -100.0 && (*g as f64 - c).abs() > 0.05 {
                bad += 1;
            }
        }
        assert_eq!(bad, 0, "N {n}: {bad} of {} grid cells differ", grid.len());
    }
}

#[test]
fn the_threads_find_psk31_in_the_narrow_lane() {
    use super::{LaneKind, Runtime, ScfSettings, Want, lock};
    use std::sync::atomic::Ordering;
    let Some((_, device, queue)) = gpu() else {
        eprintln!("no wgpu adapter here: the threads' test skipped");
        return;
    };
    // A 48 kS/s span; the receiver's passband centre 1.5 kHz up, and a
    // PSK31-rate BPSK 300 Hz above that.
    let fs = 48_000.0;
    let s =
        ScfSettings { lane: LaneKind::Narrow, lane_hz: 3000, avg_blocks: 8, ..Default::default() };
    let rt = Runtime::start(device, queue, Want { s, shift_hz: 1500.0, reset: 0 });
    let sh = rt.shared.clone();
    let mut rng = Rng::new(9);
    let len = (fs * 12.0) as usize;
    let x = mix(&bpsk(len, fs, 31.25, 1800.0, 0.3, false, &mut rng), &noise(len, 1.0, &mut rng));
    let iq = interleave(&x);
    for c in iq.chunks(2 * 4800) {
        sh.seen_ms.store(sh.epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
        rt.tap_tx.send((c.to_vec(), fs)).unwrap();
        // Ten times real time: the GPU thread drops blocks it has no time
        // for, as it should live, and lavapipe beside other tests is slow.
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let t = std::time::Instant::now();
    loop {
        sh.seen_ms.store(sh.epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
        let st = lock(&sh.status).clone();
        if st.blocks >= 7 && !st.features.is_empty() {
            assert_eq!(st.lane_rate, 3000.0);
            let f = &st.features[0];
            assert!((f.alpha_hz - 31.25).abs() < 0.8, "{:?}", st.features);
            assert_eq!(f.mode, Some("PSK31"));
            // At +300 Hz in the lane. The coherence spreads a strong signal's
            // features over the channels its leakage reaches, and the readout
            // judges each against its own column, so f is good to a few
            // channels, not one.
            assert!((f.f_hz - 300.0).abs() < 200.0, "{f:?}");
            break;
        }
        assert!(t.elapsed().as_secs() < 60, "no feature: {} blocks, {:?}", st.blocks, st.error);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The narrow lane's measurement set-up, as the view's defaults: 3 kS/s,
/// N' = 1024 (Kaiser, Δf 2.9 Hz), N = 4096 (Δα 0.73 Hz, blocks of 1.37 s),
/// coherence, α from 0 to 50 Hz on 256 rows. SCF_NC and SCF_N override.
struct Bench {
    /// Profile column: 0 (over the column's mean) or 3 (absolute).
    col: usize,
    fs: f64,
    cfg: EngineCfg,
    a_hi: f64,
}

impl Bench {
    fn narrow() -> Bench {
        Bench {
            col: 0,
            fs: 3000.0,
            cfg: EngineCfg { n: 4096, nc: 1024, grid_f: 256, grid_a: 256, taper: Taper::Kaiser },
            a_hi: 50.0,
        }
    }

    /// Run `blocks` blocks of `x` (linear average over all of them) and
    /// return the column-normalised profile (dB per α row).
    fn profile(&self, e: &mut Engine, x: &[C], blocks: usize) -> Vec<f32> {
        let n = self.cfg.n as usize;
        let bp = |b: u64| BlockParams {
            conj: false,
            measure: Measure::Coherence,
            beta: beta(b, blocks as u32) as f32,
            a_range: (0.0, (self.a_hi / self.fs) as f32),
            f_range: (-0.5, 0.5),
            a_mask: (3.0 / n as f64) as f32,
        };
        for b in 0..blocks {
            let blk = interleave(&x[b * n..b * n + e.block_len()]);
            e.process(&blk, &bp(b as u64));
        }
        e.flush().expect("a profile").iter().map(|r| r[self.col]).collect()
    }

    fn row_of(&self, alpha: f64) -> f64 {
        alpha / self.a_hi * self.cfg.grid_a as f64 - 0.5
    }

    /// The strongest profile value within ±0.5 Hz of `alpha`.
    fn at(&self, prof: &[f32], alpha: f64) -> f32 {
        let w = 0.5 / self.a_hi * self.cfg.grid_a as f64;
        let c = self.row_of(alpha);
        let lo = (c - w).ceil().max(0.0) as usize;
        let hi = ((c + w).floor() as usize).min(prof.len() - 1);
        prof[lo..=hi].iter().copied().fold(f32::MIN, f32::max)
    }
}

/// Mean and σ of the same statistic on noise alone: the window max at many
/// α away from 0, over several noise records.
fn noise_stats(b: &Bench, e: &mut Engine, blocks: usize, alpha: f64) -> (f64, f64) {
    let mut v = Vec::new();
    for seed in 0..3u64 {
        let mut rng = Rng::new(1000 + seed);
        let x = noise(b.cfg.n as usize * blocks + b.cfg.nc as usize, 1.0, &mut rng);
        let prof = b.profile(e, &x, blocks);
        let mut a = 3.0;
        while a < b.a_hi - 1.0 {
            if (a - alpha).abs() > 1.0 {
                v.push(b.at(&prof, a) as f64);
            }
            a += 1.1;
        }
    }
    let m = v.iter().sum::<f64>() / v.len() as f64;
    let s = (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (v.len() - 1) as f64).sqrt();
    (m, s)
}

/// The sensitivity table for the report and docs/design-decisions.md 36:
/// for FT8-like 8-GFSK, BPSK and CW in white Gaussian noise, the lowest
/// SNR (in 2.5 kHz, WSJT-X's convention) at which the feature at its
/// symbol (keying) rate stands 5σ over the noise-only statistic.
/// `cargo test -p hardrock-panel scf_sensitivity -- --ignored --nocapture`
#[test]
#[ignore]
fn scf_sensitivity() {
    let Some((ad, device, queue)) = gpu() else {
        eprintln!("needs a wgpu adapter");
        return;
    };
    eprintln!("GPU: {}", ad.get_info().name);
    let col = std::env::var("SCF_COL").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut b = Bench { col, ..Bench::narrow() };
    if let Some(nc) = std::env::var("SCF_NC").ok().and_then(|v| v.parse().ok()) {
        b.cfg.nc = nc;
        b.cfg.grid_f = nc.min(super::MAX_GRID_F);
    }
    if let Some(n) = std::env::var("SCF_N").ok().and_then(|v| v.parse().ok()) {
        b.cfg.n = n;
    }
    eprintln!(
        "N' {} (Δf {:.1} Hz), N {} (Δα {:.2} Hz)",
        b.cfg.nc,
        b.fs / b.cfg.nc as f64,
        b.cfg.n,
        b.fs / b.cfg.n as f64
    );
    eprintln!("statistic: profile column {col}");
    let mut e = Engine::new(&device, &queue, b.cfg);
    let n = b.cfg.n as usize;
    type Gen = fn(usize, f64, f64, &mut Rng) -> Vec<C>;
    let ft8: Gen = |len, fs, amp, rng| ft8_like(len, fs, 300.0, amp, rng);
    let psk: Gen = |len, fs, amp, rng| bpsk(len, fs, 31.25, 300.0, amp, false, rng);
    let cw20: Gen = |len, fs, amp, rng| cw(len, fs, 20.0, 300.0, amp, rng);
    // (name, generator, feature α, duty: mean power over amp²)
    let sigs: [(&str, Gen, f64, f64); 3] = [
        ("FT8-like 8-GFSK 6.25 Bd", ft8, 6.25, 1.0),
        ("BPSK 31.25 Bd (rect.)", psk, 31.25, 1.0),
        ("CW 20 WPM (OOK, 16.7 Bd)", cw20, 20.0 / 1.2, 0.5),
    ];
    let only: Option<usize> = std::env::var("SCF_BLOCKS").ok().and_then(|v| v.parse().ok());
    for blocks in [9usize, 44].into_iter().filter(|b| only.is_none_or(|o| o == *b)) {
        let secs = blocks as f64 * n as f64 / b.fs;
        for (name, g, alpha, duty) in sigs {
            let (m, s) = noise_stats(&b, &mut e, blocks, alpha);
            let mut found = None;
            let mut line = String::new();
            let mut snr = -36.0;
            while snr <= 0.0 {
                let mut zs = Vec::new();
                for seed in 0..2u64 {
                    let mut rng = Rng::new(77 + seed);
                    let len = n * blocks + b.cfg.nc as usize;
                    let amp = amp_for_snr(snr, 1.0, b.fs, duty);
                    let x = mix(&g(len, b.fs, amp, &mut rng), &noise(len, 1.0, &mut rng));
                    let prof = b.profile(&mut e, &x, blocks);
                    zs.push((b.at(&prof, alpha) as f64 - m) / s);
                }
                let z = zs.iter().sum::<f64>() / zs.len() as f64;
                line += &format!(" {snr:.0}:{z:.1}");
                if found.is_none() && z >= 5.0 {
                    found = Some(snr);
                }
                if z > 12.0 {
                    break;
                }
                snr += 2.0;
            }
            eprintln!(
                "{name}, {blocks} blocks ({secs:.1} s): 5σ at {} dB SNR in 2.5 kHz   [SNR:σ{line}]",
                found.map_or("> 0".to_string(), |v| format!("{v:.0}"))
            );
        }
    }
}

/// GPU time a block for a range of sizes, and the sample rate that keeps
/// up with. `cargo test -p hardrock-panel scf_throughput -- --ignored --nocapture`
#[test]
#[ignore]
fn scf_throughput() {
    let Some((ad, device, queue)) = gpu() else {
        eprintln!("needs a wgpu adapter");
        return;
    };
    eprintln!("GPU: {}", ad.get_info().name);
    for (nc, n) in [
        (64u32, 4096u32),
        (64, 16_384),
        (64, 65_536),
        (128, 4096),
        (256, 4096),
        (256, 16_384),
        (1024, 4096),
    ] {
        let cfg = EngineCfg {
            n,
            nc,
            grid_f: nc.min(super::MAX_GRID_F),
            grid_a: 256,
            taper: Taper::Kaiser,
        };
        let mut e = Engine::new(&device, &queue, cfg);
        let mut rng = Rng::new(1);
        let blk = interleave(&noise(e.block_len(), 1.0, &mut rng));
        let bp = BlockParams {
            conj: false,
            measure: Measure::Coherence,
            beta: 0.1,
            a_range: (0.0, 0.5),
            f_range: (-0.5, 0.5),
            a_mask: 0.0,
        };
        for _ in 0..3 {
            e.process(&blk, &bp);
        }
        e.flush();
        let reps = 12;
        let t = std::time::Instant::now();
        for _ in 0..reps {
            e.process(&blk, &bp);
        }
        e.flush();
        let ms = t.elapsed().as_secs_f64() * 1000.0 / reps as f64;
        eprintln!(
            "N' {nc:>3} N {n:>6}: {ms:7.2} ms a block, keeps up with {:.3} MS/s ({} points a block)",
            n as f64 / ms / 1000.0,
            n * nc
        );
    }
}
