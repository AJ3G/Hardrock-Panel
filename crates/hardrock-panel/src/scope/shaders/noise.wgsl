// Made-up band noise for where the waterfall and the 3D surface have no
// history (decision 112): a strip a recentre or a pan uncovers, rows not
// filled yet. Prepended to the waterfall, the surface and the lit surface's
// field pass.
//
// Each value is a hash of the row's number (counting every row laid down,
// so the noise moves down with the rows) and the frequency cell (Hz from
// the reference, which only changes when the history restarts, in cells a
// power of two hertz at least a pixel wide), so nothing shimmers while
// nothing moves. Its level is a Gumbel variate, the strongest of several
// bins of Rayleigh noise as each real pixel is, with its median at the
// band's quiet level and its median less its 16th percentile at the spread
// measured there (levels::noise_fill, levels::noise_sample).

fn noise_pcg(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

// The noise level, dBm, in row `row` at `f` Hz from the reference, for a
// pixel `px_hz` wide; `dbm` and `spread` the band's quiet level and spread.
// Below any floor when no level is known (`dbm` under -1e8).
fn noise_dbm(row: u32, f: f32, px_hz: f32, dbm: f32, spread: f32) -> f32 {
    if (dbm < -1.0e8) {
        return -1.0e9;
    }
    let lw = ceil(log2(max(px_hz, 0.01)));
    let cell = bitcast<u32>(i32(floor(f / exp2(lw))));
    let h = noise_pcg(row ^ noise_pcg(cell ^ noise_pcg(bitcast<u32>(i32(lw)))));
    let u = (f32(h >> 8u) + 0.5) / 16777216.0;
    let g = -log(-log(u));
    return dbm + spread * (g - 0.366513) / 0.9722;
}
