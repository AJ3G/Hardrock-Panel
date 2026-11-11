# Notice

Most of the signal processing in `sdr` is ported from **SDRoxide**
(<https://github.com/dividebysandwich/sdroxide>), by dividebysandwich,
`crates/sdroxide-dsp` and `crates/sdroxide-types`, licensed GPL-3.0-or-later —
the same licence as this workspace. Each file copied or adapted from it names
its SDRoxide source path in a header comment:

| here | SDRoxide |
| --- | --- |
| `src/nco.rs`, `src/decim.rs`, `src/fir.rs`, `src/window.rs`, `src/ddc.rs`, `src/simd.rs`, `src/agc.rs` | `crates/sdroxide-dsp/src/` (same names), copied |
| `src/demod.rs` | `crates/sdroxide-dsp/src/demod.rs`, cut down to SSB/CW, AM and NFM |
| `src/spectrum.rs` | adapted from `crates/sdroxide-dsp/src/spectrum.rs` |
| `src/nb.rs` | adapted from `crates/sdroxide-dsp/src/nb.rs`: interleaved I/Q, a settable threshold |
| `src/types.rs` | `AgcMode` and `MAX_MANUAL_GAIN_DB` from `crates/sdroxide-types` |

The receiver, the spectrum's frame limiter, Welch overlap, averaging, peak
hold, noise floor and peak interpolation, the zoom FFT (`src/zoom.rs`), the
audio output, the synthetic source and the pipeline are new for
hardrock-panel; the audio output follows the panel's own `audio.rs`.

The ELAD I/Q these consume comes through `elad-iq`, itself SDRoxide's
`sdroxide-elad`, whose vendor protocol SDRoxide took from ELAD's **gr-elad**
(<https://github.com/ELADIT/gr-elad>, GPL-3.0-or-later). See
`crates/elad-iq/NOTICE.md`.

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version.
