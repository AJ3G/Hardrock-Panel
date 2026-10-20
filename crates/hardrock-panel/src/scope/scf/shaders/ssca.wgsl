// The strip spectral correlation analyser (SSCA), after Brown and Loomis
// (1993) and Roberts, Brown and Loomis (1991), in six compute passes:
//
//   chan_main     windowed N'-point sliding FFTs of the block, hop L, each
//                 channel brought down to baseband: X(n, k)
//   psd_main      each channel's power, averaged over the blocks: P(k)
//   strip_main    X(n, k) times the (delayed) input, conjugated or not, and
//                 the N-point FFT along n: S(k, q). One pass when N fits in
//                 workgroup memory, else two (the four-step FFT).
//   accum_main    |S(k, q)|², averaged over the blocks
//   map_main      the strip points onto the (f, α) grid, max-pooled, in dB,
//                 as the SCF or as the spectral coherence
//   profile_main  each α row's strongest cell, its column, and the row's
//                 mean and mean square: the cyclic-domain profile
//
// Frequencies are in cycles per sample. Strip point (k, q), both signed, is
// at α = k/N' + q/N and f = (k/N' − q/N)/2.
//
// Every FFT is this file's own: a batched radix-4 (with one radix-2 stage
// when log2 of the length is odd) decimation in time in workgroup memory,
// up to 2048 points, several short FFTs to a workgroup. Twiddles come from
// a table computed in f64 on the CPU, so a 65 536-point strip is as clean
// as the table.

struct Params {
    // Strip length N, channels N', log2 N', channelizer hop L.
    n: u32,
    nc: u32,
    log_nc: u32,
    hop: u32,
    // 1: conjugate SCF (x·x, features at twice the carrier); 0: x·x*.
    conj: u32,
    // 0: SCF |S|²; 1: spectral coherence |S|² / (P(f1) P(f2)).
    measure: u32,
    grid_f: u32,
    grid_a: u32,
    // This block's weight in the running average (1 resets it).
    beta: f32,
    // The grid's α and f ranges, cycles per sample.
    a_lo: f32,
    a_hi: f32,
    f_lo: f32,
    f_hi: f32,
    // 1 / ((Σg)² (Σa)²) and 1 / (N (Σa)²): a tone of amplitude A has
    // SCF A⁴ at α = 0 and channel power A².
    norm_scf: f32,
    norm_psd: f32,
    // Twiddle table length (= N).
    ntw: u32,
};

// One dispatch's shape. FFT passes: `per_wg` FFTs of `m` points to a
// workgroup, `batches` in all; batch b is channel b / per_ch, sub-batch
// j = b % per_ch, and its element i sits at channel·ch_stride + j·in_j +
// i·in_i (out_j, out_i on the way out).
struct PassCfg {
    log_m: u32,
    m: u32,
    per_wg: u32,
    batches: u32,
    per_ch: u32,
    in_j: u32,
    in_i: u32,
    out_j: u32,
    out_i: u32,
    // Multiply output i of sub-batch j by W_N^(i·j) (four-step's middle).
    twiddle: u32,
    // Multiply each input by the delayed input sample and the strip window.
    product: u32,
    // Workgroups along x (a dispatch is folded into x·y under 65 535).
    groups_x: u32,
    ch_stride: u32,
    // Elements, for the passes that are one thread an element.
    count: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<uniform> pc: PassCfg;
// The block, interleaved I/Q exactly as the CPU had it: N + N' samples.
@group(0) @binding(2) var<storage, read> xin: array<vec2<f32>>;
@group(0) @binding(3) var<storage, read> win_a: array<f32>;
@group(0) @binding(4) var<storage, read> win_g: array<f32>;
@group(0) @binding(5) var<storage, read> tw: array<vec2<f32>>;
@group(0) @binding(6) var<storage, read> src: array<vec2<f32>>;
@group(0) @binding(7) var<storage, read_write> dst: array<vec2<f32>>;
@group(0) @binding(8) var<storage, read_write> acc: array<f32>;
@group(0) @binding(9) var<storage, read_write> psd: array<f32>;
@group(0) @binding(10) var<storage, read_write> grid: array<f32>;
@group(0) @binding(11) var<storage, read_write> prof: array<vec4<f32>>;

const WG: u32 = 256u;

var<workgroup> sh: array<vec2<f32>, 2048>;
var<workgroup> red: array<f32, 256>;

fn cmul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x);
}

fn bitrev(i: u32, log_m: u32) -> u32 {
    return reverseBits(i) >> (32u - log_m);
}

fn wg_index(wid: vec3<u32>) -> u32 {
    return wid.y * pc.groups_x + wid.x;
}

// In-place FFTs of the `g` blocks of 2^log_m points in `sh`, loaded in
// bit-reversed order. Two radix-2 stages fused into each radix-4 one: half
// the barriers and half the trips through workgroup memory.
fn fft_shared(li: u32, log_m: u32, g: u32) {
    let m = 1u << log_m;
    var s = 0u;
    if ((log_m & 1u) == 1u) {
        let pairs = (m >> 1u) * g;
        for (var t = li; t < pairs; t = t + WG) {
            let i0 = t << 1u;
            let a = sh[i0];
            let b = sh[i0 + 1u];
            sh[i0] = a + b;
            sh[i0 + 1u] = a - b;
        }
        workgroupBarrier();
        s = 1u;
    }
    loop {
        if (s >= log_m) {
            break;
        }
        let h = 1u << s;
        let quads = (m >> 2u) * g;
        let step_a = p.ntw >> (s + 1u);
        let step_b = p.ntw >> (s + 2u);
        for (var t = li; t < quads; t = t + WG) {
            let pos = t & (h - 1u);
            let i0 = ((t >> s) << (s + 2u)) + pos;
            let w_a = tw[pos * step_a];
            let w_b = tw[pos * step_b];
            let x0 = sh[i0];
            let x1 = cmul(sh[i0 + h], w_a);
            let x2 = sh[i0 + 2u * h];
            let x3 = cmul(sh[i0 + 3u * h], w_a);
            let a0 = x0 + x1;
            let a1 = x0 - x1;
            let b2 = cmul(x2 + x3, w_b);
            let b3 = cmul(x2 - x3, w_b);
            // W_4h^(pos+h) = W_4h^pos · (−j).
            let b3j = vec2<f32>(b3.y, -b3.x);
            sh[i0] = a0 + b2;
            sh[i0 + 2u * h] = a0 - b2;
            sh[i0 + h] = a1 + b3j;
            sh[i0 + 3u * h] = a1 - b3j;
        }
        workgroupBarrier();
        s = s + 2u;
    }
}

// The channelizer: batch b is the window starting at sample b·L; output
// channel k goes to dst[k·ch_stride + b], turned by e^(−j2πk·bL/N') so each
// channel is at baseband on the block's own time axis.
@compute @workgroup_size(256)
fn chan_main(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    let log_m = pc.log_m;
    let m = pc.m;
    let g = pc.per_wg;
    let total = m * g;
    let b0 = wg_index(wid) * g;
    for (var e = li; e < total; e = e + WG) {
        let sub = e >> log_m;
        let r = e & (m - 1u);
        let b = b0 + sub;
        var v = vec2<f32>(0.0);
        if (b < pc.batches) {
            v = xin[b * p.hop + r] * win_a[r];
        }
        sh[(sub << log_m) + bitrev(r, log_m)] = v;
    }
    workgroupBarrier();
    fft_shared(li, log_m, g);
    let tw_step = p.ntw >> log_m;
    for (var e = li; e < total; e = e + WG) {
        let sub = e >> log_m;
        let k = e & (m - 1u);
        let b = b0 + sub;
        if (b < pc.batches) {
            let ph = (k * ((b * p.hop) & (m - 1u))) & (m - 1u);
            dst[k * pc.ch_stride + b] = cmul(sh[e], tw[ph * tw_step]);
        }
    }
}

// Channel power: one workgroup a channel, reading the channelizer's output.
@compute @workgroup_size(256)
fn psd_main(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    let k = wg_index(wid);
    var s = 0.0;
    if (k < p.nc) {
        let base = k * p.n;
        for (var n = li; n < p.n; n = n + WG) {
            let v = src[base + n];
            s = s + dot(v, v);
        }
    }
    red[li] = s;
    workgroupBarrier();
    for (var w = WG >> 1u; w > 0u; w = w >> 1u) {
        if (li < w) {
            red[li] = red[li] + red[li + w];
        }
        workgroupBarrier();
    }
    if (li == 0u && k < p.nc) {
        psd[k] = mix(psd[k], red[0] * p.norm_psd, p.beta);
    }
}

// The strip FFTs (and, with `product`, the SSCA's multiply on the way in).
@compute @workgroup_size(256)
fn strip_main(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    let log_m = pc.log_m;
    let m = pc.m;
    let g = pc.per_wg;
    let total = m * g;
    let b0 = wg_index(wid) * g;
    let half_nc = p.nc >> 1u;
    for (var e = li; e < total; e = e + WG) {
        let sub = e >> log_m;
        let i = e & (m - 1u);
        let b = b0 + sub;
        var v = vec2<f32>(0.0);
        if (b < pc.batches) {
            let ch = b / pc.per_ch;
            let j = b % pc.per_ch;
            let n = j * pc.in_j + i * pc.in_i;
            v = src[ch * pc.ch_stride + n];
            if (pc.product == 1u) {
                // The input at the middle of channel window n.
                let x = xin[n + half_nc];
                let xs = select(vec2<f32>(x.x, -x.y), x, p.conj == 1u);
                v = cmul(v, xs) * win_g[n];
            }
        }
        sh[(sub << log_m) + bitrev(i, log_m)] = v;
    }
    workgroupBarrier();
    fft_shared(li, log_m, g);
    for (var e = li; e < total; e = e + WG) {
        let sub = e >> log_m;
        let i = e & (m - 1u);
        let b = b0 + sub;
        if (b < pc.batches) {
            let ch = b / pc.per_ch;
            let j = b % pc.per_ch;
            var v = sh[e];
            if (pc.twiddle == 1u) {
                v = cmul(v, tw[(i * j) % p.ntw]);
            }
            dst[ch * pc.ch_stride + j * pc.out_j + i * pc.out_i] = v;
        }
    }
}

// |S|², normalised, into the running average. One thread a strip point.
@compute @workgroup_size(256)
fn accum_main(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    let i = wg_index(wid) * WG + li;
    if (i < pc.count) {
        let v = src[i];
        acc[i] = mix(acc[i], dot(v, v) * p.norm_scf, p.beta);
    }
}

fn wrap(s: i32, m: u32) -> u32 {
    return u32(s + i32(m)) & (m - 1u);
}

// The averaged estimate at signed strip point (k, q), as the SCF or as the
// coherence: |S|² over the powers at f1 = k/N' and f2 (−q/N for x·x*, q/N
// for x·x), the latter interpolated between channel centres.
fn value(ks: i32, qs: i32) -> f32 {
    let k = wrap(ks, p.nc);
    let s = acc[k * p.n + wrap(qs, p.n)];
    if (p.measure == 0u) {
        return s;
    }
    var f2 = f32(qs) / f32(p.n);
    if (p.conj == 0u) {
        f2 = -f2;
    }
    let x = f2 * f32(p.nc);
    let ka = floor(x);
    let t = x - ka;
    let p2 = mix(psd[wrap(i32(ka), p.nc)], psd[wrap(i32(ka) + 1, p.nc)], t);
    return s / max(psd[k] * p2, 1.0e-30);
}

// One thread a grid cell: the strongest strip point inside it, or, when the
// cell is finer than the strips, the nearest at its α. Row a is α,
// column f; the grid is what the terrain's vertex shader reads.
@compute @workgroup_size(256)
fn map_main(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    let i = wg_index(wid) * WG + li;
    if (i >= pc.count) {
        return;
    }
    let fi = i % p.grid_f;
    let ai = i / p.grid_f;
    let nc = f32(p.nc);
    let nn = f32(p.n);
    let kmin = -i32(p.nc >> 1u);
    let kmax = i32(p.nc >> 1u) - 1;
    let qmin = -i32(p.n >> 1u);
    let qmax = i32(p.n >> 1u) - 1;
    let df = (p.f_hi - p.f_lo) / f32(p.grid_f);
    let da = (p.a_hi - p.a_lo) / f32(p.grid_a);
    let f0 = p.f_lo + f32(fi) * df;
    let f1 = f0 + df;
    let a0 = p.a_lo + f32(ai) * da;
    let a1 = a0 + da;
    // k/N' = f + α/2 over the cell.
    let k_lo = max(i32(ceil(nc * (f0 + 0.5 * a0))), kmin);
    let k_hi = min(i32(ceil(nc * (f1 + 0.5 * a1))) - 1, kmax);
    var best = 0.0;
    var found = false;
    let k_stride = max(1, (k_hi - k_lo + 16) / 16);
    for (var k = k_lo; k <= k_hi; k = k + k_stride) {
        let kf = f32(k) / nc;
        // α = kf + q/N in [a0, a1); f = (kf − q/N)/2 in [f0, f1).
        let q_lo = max(max(i32(ceil(nn * (a0 - kf))), i32(floor(nn * (kf - 2.0 * f1))) + 1), qmin);
        let q_hi = min(min(i32(ceil(nn * (a1 - kf))) - 1, i32(floor(nn * (kf - 2.0 * f0)))), qmax);
        let cnt = q_hi - q_lo + 1;
        if (cnt <= 0) {
            continue;
        }
        let q_stride = max(1, (cnt + 511) / 512);
        for (var q = q_lo; q <= q_hi; q = q + q_stride) {
            best = max(best, value(k, q));
            found = true;
        }
    }
    if (!found) {
        // Finer than the strips: on each channel line through the cell, the
        // point at the cell's α (α is what is resolved finely).
        let fc = f0 + 0.5 * df;
        let ac = a0 + 0.5 * da;
        var kl = k_lo;
        var kh = k_hi;
        if (kl > kh) {
            kl = i32(round(nc * (fc + 0.5 * ac)));
            kh = kl;
        }
        for (var k = kl; k <= kh; k = k + k_stride) {
            let q = i32(round(nn * (ac - f32(k) / nc)));
            if (k >= kmin && k <= kmax && q >= qmin && q <= qmax) {
                best = max(best, value(k, q));
            }
        }
    }
    // 10·log10(v) = 10·log10(2)·log2(v).
    grid[i] = 3.0103 * log2(max(best, 1.0e-30));
}

// The cyclic-domain profile: one thread an α row. Cells outside the SCF's
// support (below −250 dB) are left out of the mean.
@compute @workgroup_size(256)
fn profile_main(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    let a = wg_index(wid) * WG + li;
    if (a >= pc.count) {
        return;
    }
    var mx = -1.0e30;
    var arg = 0u;
    var s = 0.0;
    var s2 = 0.0;
    var used = 0.0;
    let base = a * p.grid_f;
    for (var f = 0u; f < p.grid_f; f = f + 1u) {
        let v = grid[base + f];
        if (v > mx) {
            mx = v;
            arg = f;
        }
        if (v > -250.0) {
            s = s + v;
            s2 = s2 + v * v;
            used = used + 1.0;
        }
    }
    let u = max(used, 1.0);
    prof[a] = vec4<f32>(mx, f32(arg), s / u, s2 / u);
}
