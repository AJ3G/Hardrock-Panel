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
- **TX TUNE on the bench.** Check the HR500 keys with it (decision 17), and
  that the Tune slider's watts match the LP-100A at each power button, in OFF
  first, then in PTT. The panel's tone has not been measured against
  SDRoxide's.
- **RIG TUNE on the bench.** Measure what the DUO's `TX2;` carrier puts out at
  each menu 34 step, and whether the DUO keys its PTT output for it.
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
- **The scope and the receiver on the bench.** Open the ELAD's I/Q under
  WinUSB with the CAT link connected, and check that opening doesn't move the
  dial, that turning the knob moves the span, that a recentre sets VFO A and
  the span follows, and the receiver's audio on the PC and on VB-Cable. Check
  each SPAN with the same rate set on the DUO.
- **Split on the bench (decision 33).** With TX FOLLOWS RX on, check that the
  DUO's I/Q window stays on VFO A while it is in split (`SP1`), that `FB`
  follows the receiver, and that a transmission goes out on the receiver's
  frequency (into a dummy load, amplifier in OFF first).
- **Outside programs on the bench.** WSJT-X over TCI and over rigctld,
  receive first; then, with *Allow outside programs to transmit*, one FT8
  over into a dummy load in OFF, then in PTT at 5 W, and check that a trip,
  high SWR, STOP and closing WSJT-X each unkey. WSPR's two-minute
  transmissions are cut at TUNE's one-minute limit; decide whether outside
  programs need a longer one.
- **TCI transmit audio.** `trx:0,true,tci;` is refused: the panel doesn't yet
  play a TCI client's audio into the radio's USB audio.
- **The receiver's S-meter calibration.** The pipeline runs at 0 dB
  calibration, so the scope's dBm and S-units are dBFS until measured against
  a signal generator.
