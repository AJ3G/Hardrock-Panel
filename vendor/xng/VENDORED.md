# Vendored: airframesio/xng

- Upstream: https://github.com/airframesio/xng, commit
  `096c805278faa0a904de7d98066bf4cf395cb6c2` (20 Aug 2026), taken from
  SDRoxide's `vendor/xng` submodule.
- Licence: MIT OR Apache-2.0, at the choice of the user. Both texts are here,
  `LICENSE-MIT` and `LICENSE-APACHE`. Each mode crate's `PROVENANCE.md` says
  what its decoder was derived from.
- Test captures: `crates/xng-mode-hfdl/tests/data/hfdl_21931khz_8s.i16` is a
  slice of a sigidwiki recording under CC BY-SA (see that directory's
  README.md); `crates/xng-mode-vdl2/tests/data/vdl2_offair_6s.i16` is upstream's.

Kept: `xng-dsp`, `xng-types`, `xng-acars` (the ACARS application layer), and
the decode cores `xng-mode-hfdl`, `xng-mode-acars`, `xng-mode-vdl2`,
`xng-mode-ais` and `xng-mode-adsb`, each with its `src`, `tests` and
`PROVENANCE.md`. Left out: the rest of upstream (the `xng` binary, its SDR
backends, protobuf/gRPC output, the other modes, benchmarks and examples).

The source files are upstream's, unchanged. The workspace `Cargo.toml` here is
a trimmed copy of upstream's, listing only the kept crates and their
dependencies. It is its own workspace, excluded from the panel's, so the
panel's `cargo fmt` and clippy leave this code alone; the panel reaches it
through `crates/datalink`.
