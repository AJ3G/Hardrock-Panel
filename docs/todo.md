# To do

Open items for the standalone panel. Items marked *from SDRoxide* were open
when the panel was part of SDRoxide and still apply.

- **An installer.** Releases are a zipped `.exe` for now. An `.msi` with a
  Start-menu shortcut would suit most users better.
- **The FDM-DUO on the bench.** The radio link is tested against
  `tools/fake_duo.py` only. Check FWD, RFL and SWR in bypass against the
  LP-100A, the band following the dial, and what `DT` and `VS` answer.
- **More DUO settings.** Only the reads in the command table SDRoxide used are
  listed. With the full FDM-DUO manual, add any other READ it has.
- **Other radios.** `crates/rigcat` has one model; each new one (Kenwood,
  Yaesu, Icom) is a module implementing `Protocol`, with every frame it may
  send on its own allow-list (interlock #5), and appears in the pop-out.
- **RIG TUNE on the bench.** Measure what the DUO's `TX2;` carrier puts out at
  each menu 34 step, against the LP-100A, in OFF first, then in PTT.
- **Audio drive (optional).** A smooth Drive slider, as SDRoxide had, by
  playing a tone into the DUO's USB audio with menu 34 as the ceiling.
- **A fuller simulated amplifier.** `tools/fake_hr500.py` answers with fixed
  readings, idle or keyed. Add a trip and a tune, so every part of the panel
  can be shown without an amplifier.
- **Firmware version** (*from SDRoxide*). Show it in Settings via `HRFW`,
  found in the V3.4 firmware but not in the manual. Log its reply on the bench
  first.
- **Drive limit, 8 W or 12 W** (*from SDRoxide*). Interlock #4 uses the
  HR500's yellow DRV level, 8 W; it trips at 12 W. Not yet testable with a
  radio that tops out at 8 W.
- **The ATU's S and F fields** (*from SDRoxide*). Still unidentified in the
  `HRTMP` reply; if one is forward power or SWR it could fill RFL and SWR in
  bypass.
- **Other models** (*from SDRoxide*). The HR50 is selectable and untested;
  the HR100 needs its own limits. Each needs its manual and a bench test.
- **Linux and macOS.** It builds for both; it has only been tried on Windows
  11.
