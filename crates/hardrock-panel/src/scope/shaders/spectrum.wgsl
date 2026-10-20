// The spectrum on the GPU: windowing, the FFT, |X|² to dBm, Welch
// averaging, exponential averaging, peak hold, the noise floor, the zoom
// FFT's mixer and decimator, and pooling into waterfall, 3D and trace rows.
//
// The FFT is a four-step transform: an N = N1·N2 point FFT is N2 FFTs of N1
// points down the columns (each in one workgroup's shared memory), a
// twiddle, and N1 FFTs of N2 points along the rows, so every size from 4k to
// 1M (and up to 4M) is two passes over global memory. Inside a workgroup
// each sub-FFT is a Stockham autosort transform in radix-8 stages, with a
// radix-4 or radix-2 stage to finish, so no bit reversal is needed.
//
// Every kernel takes its settings from one 256-byte slot of a uniform
// buffer (a dynamic offset), so many dispatches can be recorded in one
// frame with one write. What each field means depends on the kernel; see
// each one's comment.

struct P {
    n: u32,
    n1: u32,
    n2: u32,
    log1: u32,
    log2: u32,
    src_len: u32,
    start: u32,
    step: u32,
    segs: u32,
    flags: u32,
    count: u32,
    cols: u32,
    off_a: u32,
    off_b: u32,
    off_c: u32,
    nq: u32,
    norm: f32,
    alpha: f32,
    cal: f32,
    fall: f32,
    a: f32,
    b: f32,
    inv_count: f32,
    pitch: f32,
    q: array<vec4<u32>, 4>,
};

@group(0) @binding(0) var<uniform> p: P;
// The I/Q (or the zoom's decimated I/Q) as a ring of complex samples.
@group(0) @binding(1) var<storage, read> src: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read> win: array<f32>;
// ω_N^m = exp(-2πi·m/N) for m in 0..N, computed on the CPU in f64.
@group(0) @binding(3) var<storage, read> tw: array<vec2<f32>>;
// Between the two FFT passes: segs × N complex.
@group(0) @binding(4) var<storage, read_write> work: array<vec2<f32>>;
// Each segment's power, segs × N, in display order.
@group(0) @binding(5) var<storage, read_write> pw: array<f32>;
// The spectrum's state, N each: Welch sum, average power, held peak (dBm),
// and the frame in dBm.
@group(0) @binding(6) var<storage, read_write> st: array<f32>;
// The frame's levels in 0.125 dB steps, for the median.
@group(0) @binding(7) var<storage, read_write> hist: array<atomic<u32>>;
// Rows, the trace, and the results the CPU reads back.
@group(0) @binding(8) var<storage, read_write> outb: array<f32>;
// A row as pairs of half floats, for the waterfall's R16Float texture.
@group(0) @binding(9) var<storage, read_write> bits: array<u32>;
// The zoom's low-pass filter.
@group(0) @binding(10) var<storage, read> taps: array<f32>;
// The zoom's decimated I/Q ring, and its partial sums.
@group(0) @binding(11) var<storage, read_write> zring: array<vec2<f32>>;
@group(0) @binding(12) var<storage, read_write> part: array<vec2<f32>>;

const WG: u32 = 256u;
// The largest sub-FFT a workgroup does: 2048 complex is 16 KB, WebGPU's
// default workgroup storage.
const MAX_LOCAL: u32 = 2048u;

const F_LATEST: u32 = 1u;
const F_PRIMED: u32 = 2u;
const F_PEAK: u32 = 4u;
const F_PEAK_RESET: u32 = 8u;
const F_MAX: u32 = 16u;

// The histogram: 2048 bins of 0.125 dB from -250 dBFS.
const H0: f32 = -250.0;
const HINV: f32 = 8.0;
const HBINS: u32 = 2048u;

var<workgroup> sh: array<vec2<f32>, MAX_LOCAL>;
var<workgroup> best_v: array<f32, WG>;
var<workgroup> best_i: array<u32, WG>;

fn cmul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x);
}

// × -i
fn mul_mi(a: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.y, -a.x);
}

// exp(+2πi·c/2³²), c a phase in turns as a 32-bit fraction.
fn cis(c: u32) -> vec2<f32> {
    let a = f32(bitcast<i32>(c)) * (6.283185307179586 / 4294967296.0);
    return vec2<f32>(cos(a), sin(a));
}

// An m-point FFT (m a power of two, 2^logm, at most MAX_LOCAL) of sh[0..m],
// in place, in natural order. Stockham: stage s takes the sub-transforms of
// length `span` to length span·r. Butterfly i reads r inputs m/r apart,
// twiddles input j by ω_{span·r}^{j·k} (k = i mod span), and writes its
// outputs `span` apart from (i - k)·r + k. Each thread holds up to 8 values
// between the read and the write, which is why m is at most 8 · 256.
fn local_fft(lid: u32, m: u32, logm: u32) {
    var span = 1u;
    var done = 0u;
    loop {
        if (done >= logm) {
            break;
        }
        let rem = logm - done;
        var r = 2u;
        var lr = 1u;
        if (rem >= 3u) {
            r = 8u;
            lr = 3u;
        } else if (rem == 2u) {
            r = 4u;
            lr = 2u;
        }
        let nb = m >> lr;
        let tstep = p.n / (span * r);
        var regs: array<vec2<f32>, 8>;
        for (var t = 0u; t * WG < nb; t = t + 1u) {
            let i = lid + t * WG;
            if (i < nb) {
                let k = i & (span - 1u);
                var a: array<vec2<f32>, 8>;
                for (var j = 0u; j < r; j = j + 1u) {
                    a[j] = cmul(sh[i + j * nb], tw[j * k * tstep]);
                }
                if (r == 2u) {
                    let x0 = a[0];
                    a[0] = x0 + a[1];
                    a[1] = x0 - a[1];
                } else if (r == 4u) {
                    let t0 = a[0] + a[2];
                    let t1 = a[0] - a[2];
                    let t2 = a[1] + a[3];
                    let t3 = mul_mi(a[1] - a[3]);
                    a[0] = t0 + t2;
                    a[1] = t1 + t3;
                    a[2] = t0 - t2;
                    a[3] = t1 - t3;
                } else {
                    // Two radix-4s of the even and odd inputs, joined by ω_8.
                    let e0 = a[0] + a[4];
                    let e1 = a[0] - a[4];
                    let e2 = a[2] + a[6];
                    let e3 = mul_mi(a[2] - a[6]);
                    let ev0 = e0 + e2;
                    let ev1 = e1 + e3;
                    let ev2 = e0 - e2;
                    let ev3 = e1 - e3;
                    let o0 = a[1] + a[5];
                    let o1 = a[1] - a[5];
                    let o2 = a[3] + a[7];
                    let o3 = mul_mi(a[3] - a[7]);
                    let s = 0.7071067811865476;
                    let od0 = o0 + o2;
                    let od1 = cmul(o1 + o3, vec2<f32>(s, -s));
                    let od2 = mul_mi(o0 - o2);
                    let od3 = cmul(o1 - o3, vec2<f32>(-s, -s));
                    a[0] = ev0 + od0;
                    a[4] = ev0 - od0;
                    a[1] = ev1 + od1;
                    a[5] = ev1 - od1;
                    a[2] = ev2 + od2;
                    a[6] = ev2 - od2;
                    a[3] = ev3 + od3;
                    a[7] = ev3 - od3;
                }
                for (var j = 0u; j < r; j = j + 1u) {
                    regs[t * r + j] = a[j];
                }
            }
        }
        workgroupBarrier();
        for (var t = 0u; t * WG < nb; t = t + 1u) {
            let i = lid + t * WG;
            if (i < nb) {
                let k = i & (span - 1u);
                let base = (i - k) * r + k;
                for (var j = 0u; j < r; j = j + 1u) {
                    sh[base + j * span] = regs[t * r + j];
                }
            }
        }
        workgroupBarrier();
        span = span * r;
        done = done + lr;
    }
}

// Pass 1, one workgroup per column n2 (x) and segment (y): x[n1·N2 + n2],
// windowed, read from the ring at `start + segment·step`; its N1-point FFT,
// twiddled by ω_N^{n2·k1}, into work[k1·N2 + n2].
@compute @workgroup_size(256)
fn fft_cols(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_id) l: vec3<u32>) {
    let lid = l.x;
    let col = wid.x;
    let s = wid.y;
    let m = p.n1;
    let base = p.start + s * p.step;
    let mask = p.src_len - 1u;
    for (var i = lid; i < m; i = i + WG) {
        let idx = i * p.n2 + col;
        sh[i] = src[(base + idx) & mask] * win[idx];
    }
    workgroupBarrier();
    local_fft(lid, m, p.log1);
    for (var k1 = lid; k1 < m; k1 = k1 + WG) {
        work[s * p.n + k1 * p.n2 + col] = cmul(sh[k1], tw[col * k1]);
    }
}

// Pass 2, one workgroup per row k1 (x) and segment (y): the N2-point FFT of
// work[k1·N2 ..], which is X[k1 + N1·k2]; its power × `norm` into pw in
// display order (the negative frequencies first).
@compute @workgroup_size(256)
fn fft_rows(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_id) l: vec3<u32>) {
    let lid = l.x;
    let row = wid.x;
    let s = wid.y;
    let m = p.n2;
    for (var i = lid; i < m; i = i + WG) {
        sh[i] = work[s * p.n + row * p.n2 + i];
    }
    workgroupBarrier();
    local_fft(lid, m, p.log2);
    let half = p.n >> 1u;
    let nm = p.n - 1u;
    for (var k2 = lid; k2 < m; k2 = k2 + WG) {
        let k = row + p.n1 * k2;
        let x = sh[k2];
        pw[s * p.n + ((k + half) & nm)] = dot(x, x) * p.norm;
    }
}

// Welch: add `segs` segments' powers to the running sum.
@compute @workgroup_size(256)
fn accumulate(@builtin(global_invocation_id) g: vec3<u32>) {
    let d = g.x;
    if (d >= p.n) {
        return;
    }
    var s = 0.0;
    for (var i = 0u; i < p.segs; i = i + 1u) {
        s = s + pw[i * p.n + d];
    }
    st[d] = st[d] + s;
}

// A frame: the Welch mean (or, with F_LATEST, the one transform in pw),
// averaged into the running average (α, or taken as it is until F_PRIMED),
// in dBm (+cal), the peak hold falling `fall` dB, and the histogram.
@compute @workgroup_size(256)
fn frame(@builtin(global_invocation_id) g: vec3<u32>) {
    let d = g.x;
    if (d >= p.n) {
        return;
    }
    var pwr = 0.0;
    if ((p.flags & F_LATEST) != 0u) {
        pwr = pw[d];
    } else {
        pwr = st[d] * p.inv_count;
        st[d] = 0.0;
    }
    let ai = p.n + d;
    var avg = pwr;
    if ((p.flags & F_PRIMED) != 0u) {
        avg = st[ai] + p.alpha * (pwr - st[ai]);
    }
    st[ai] = avg;
    // 10·log10(x) = 3.0103·log2(x)
    let dbfs = 3.010299956639812 * log2(avg + 1e-20);
    let db = dbfs + p.cal;
    st[3u * p.n + d] = db;
    if ((p.flags & F_PEAK) != 0u) {
        let pi = 2u * p.n + d;
        var pk = db;
        if ((p.flags & F_PEAK_RESET) == 0u) {
            pk = max(st[pi] - p.fall, db);
        }
        st[pi] = pk;
    }
    let hb = clamp(i32(floor((dbfs - H0) * HINV)), 0, i32(HBINS) - 1);
    atomicAdd(&hist[hb], 1u);
}

// One workgroup: the median of the frame's levels from the histogram, as
// dBm per bin, into outb[off_a]; then the histogram is cleared for the
// next frame.
@compute @workgroup_size(256)
fn noise(@builtin(local_invocation_id) l: vec3<u32>) {
    let lid = l.x;
    if (lid == 0u) {
        let want = p.n / 2u;
        var c = 0u;
        var b = 0u;
        loop {
            if (b >= HBINS - 1u) {
                break;
            }
            c = c + atomicLoad(&hist[b]);
            if (c > want) {
                break;
            }
            b = b + 1u;
        }
        outb[p.off_a] = H0 + (f32(b) + 0.5) / HINV + p.cal;
    }
    storageBarrier();
    workgroupBarrier();
    for (var i = lid; i < HBINS; i = i + WG) {
        atomicStore(&hist[i], 0u);
    }
}

// Pool st[off_a ..][0..n] into `cols` columns at outb[off_b ..], each the
// strongest bin it touches: column c covers bins off_c + a + c·b to
// off_c + a + (c+1)·b (b, the bins a column, worked out on the CPU in f64,
// as sdr::pool_max_range does). With F_MAX, the strongest of that and what
// the column already holds (a slow waterfall row).
@compute @workgroup_size(64)
fn pool(@builtin(global_invocation_id) g: vec3<u32>) {
    let c = g.x;
    if (c >= p.cols) {
        return;
    }
    let n = p.n;
    let x = p.a + f32(c) * p.b;
    let lo = min(p.off_c + u32(floor(x)), n - 1u);
    let hi = max(min(p.off_c + u32(ceil(x + p.b)), n), lo + 1u);
    var m = -3.0e38;
    for (var i = lo; i < hi; i = i + 1u) {
        m = max(m, st[p.off_a + i]);
    }
    if ((p.flags & F_MAX) != 0u) {
        m = max(m, outb[p.off_b + c]);
    }
    outb[p.off_b + c] = m;
}

fn to_half(v: f32) -> f32 {
    // What the CPU's rows::to_half does: anything not a number is empty.
    if (!(v >= -250.0)) {
        return -250.0;
    }
    return min(v, 65000.0);
}

// A row at outb[off_b ..], `cols` long, as half floats in pairs.
@compute @workgroup_size(64)
fn pack(@builtin(global_invocation_id) g: vec3<u32>) {
    let t = g.x;
    if (t * 2u >= p.cols) {
        return;
    }
    let v0 = to_half(outb[p.off_b + 2u * t]);
    let v1 = to_half(outb[p.off_b + 2u * t + 1u]);
    bits[p.off_c + t] = pack2x16float(vec2<f32>(v0, v1));
}

// One workgroup per query: the strongest bin of st[off_a ..] in [lo, hi]
// (query q is q[q/2].xy or .zw), and its neighbours, into
// outb[off_b + 4q ..] as (bin, level before, level, level after).
@compute @workgroup_size(256)
fn peaks(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_id) l: vec3<u32>) {
    let lid = l.x;
    let q = wid.x;
    let pr = p.q[q / 2u];
    let odd = (q & 1u) == 1u;
    let lo = select(pr.x, pr.z, odd);
    let hi = min(select(pr.y, pr.w, odd), p.n - 1u);
    var bv = -3.0e38;
    var bi = lo;
    for (var i = lo + lid; i <= hi; i = i + WG) {
        let v = st[p.off_a + i];
        if (v >= bv) {
            bv = v;
            bi = i;
        }
    }
    best_v[lid] = bv;
    best_i[lid] = bi;
    workgroupBarrier();
    for (var s = WG / 2u; s > 0u; s = s >> 1u) {
        if (lid < s) {
            let ov = best_v[lid + s];
            let oi = best_i[lid + s];
            if (ov > best_v[lid] || (ov == best_v[lid] && oi > best_i[lid])) {
                best_v[lid] = ov;
                best_i[lid] = oi;
            }
        }
        workgroupBarrier();
    }
    if (lid == 0u) {
        let k = best_i[0];
        let km = select(k, k - 1u, k > 0u);
        let kp = select(k, k + 1u, k + 1u < p.n);
        let o = p.off_b + 4u * q;
        outb[o] = f32(k);
        outb[o + 1u] = st[p.off_a + km];
        outb[o + 2u] = st[p.off_a + k];
        outb[o + 3u] = st[p.off_a + kp];
    }
}

// The zoom's decimator, part 1. Thread (output m, chunk c) sums
// h[k]·exp(+iωk)·x[q_m - k] over its chunk of the filter (off_a taps from
// c·off_a, n taps in all), where q_m = start + m·step is output m's newest
// input in the ring. ω is q[0].x turns per sample as a 32-bit fraction. The
// rotation runs as a recurrence across the chunk, restarted exactly at each
// chunk's start. `count` outputs × `cols` chunks; the y dimension of the
// dispatch is `pitch` threads on.
@compute @workgroup_size(64)
fn decim_partial(@builtin(global_invocation_id) g: vec3<u32>) {
    let idx = g.x + g.y * u32(p.pitch);
    if (idx >= p.count * p.cols) {
        return;
    }
    let m = idx / p.cols;
    let c = idx % p.cols;
    let k0 = c * p.off_a;
    let k1 = min(k0 + p.off_a, p.n);
    let mask = p.src_len - 1u;
    let newest = p.start + m * p.step;
    let s1 = p.q[0].x;
    var rot = cis(k0 * s1);
    let dr = cis(s1);
    var acc = vec2<f32>(0.0, 0.0);
    for (var k = k0; k < k1; k = k + 1u) {
        acc = acc + taps[k] * cmul(src[(newest - k) & mask], rot);
        rot = cmul(rot, dr);
    }
    part[idx] = acc;
}

// Part 2, a thread per output: the chunks' sum, mixed down by
// exp(-2πi·(q[0].y + m·q[0].z)/2³²) (the mixer's phase at q_m), into
// zring[(off_c + m) mod src_len].
@compute @workgroup_size(64)
fn decim_sum(@builtin(global_invocation_id) g: vec3<u32>) {
    let m = g.x;
    if (m >= p.count) {
        return;
    }
    var acc = vec2<f32>(0.0, 0.0);
    for (var c = 0u; c < p.cols; c = c + 1u) {
        acc = acc + part[m * p.cols + c];
    }
    let r = cis(p.q[0].y + m * p.q[0].z);
    zring[(p.off_c + m) & (p.src_len - 1u)] = cmul(acc, vec2<f32>(r.x, -r.y));
}
