# References

The documents the Hardrock panel was written from, and the bench evidence behind its numbers. Copies are kept in [references/](references/), so the reasoning can be checked without hunting for the originals.

> **Copyright and credit.** The HobbyPCB manuals and schematics are the work of Jim Veatch, WA2EUJ, of HobbyPCB, and are included here with his permission, credited to him. The ATU-500 user manual and the EPP-400/500 power supply manuals are not copied here; they are cited, with links to where they are published.

## Manufacturer documents

| # | Document | File | Used for |
|---|---|---|---|
| 1 | *Hardrock-500 Operating Instructions*, Jim Veatch WA2EUJ, HobbyPCB, 1 March 2021 | [hr500-operating-instructions-2021-03-01.pdf](references/hr500-operating-instructions-2021-03-01.pdf) | The serial command set (section 6), used throughout `crates/sdroxide-hardrock/src/protocol.rs`. The status indicator levels and trip points behind the meter colours (`AmpModel::limits`): FWD yellow above 500 W, trip above 600 W, and the others. The temperature thresholds. The ATU commands passed through with `HRTM`. |
| 2 | *Hardrock-500 Assembly Instructions*, Run 1 (2020), Jim Veatch WA2EUJ, 24 February 2022 | [hr500-assembly-instructions-run1-2022-02-24.pdf](references/hr500-assembly-instructions-run1-2022-02-24.pdf) | Background on the amplifier's boards and wiring. |
| 3 | HR500 schematics: Control Rev C, Power Rev C, LPF Rev D, Rear Panel Rev C, and the PA board (AN1819 Rev B) | [control](references/hr500-control-rev-c-schematic.pdf), [power](references/hr500-power-rev-c-schematic.pdf), [LPF](references/hr500-lpf-rev-d-schematic.pdf), [rear panel](references/hr500-rear-panel-rev-c-schematic.pdf), [PA](references/an1819-rev-b-schematic.pdf) | How the amplifier switches, protects and measures, for the safety interlocks ([safety-interlocks.md](safety-interlocks.md)), and the low-pass filter in the bypass path that the ATU tests had to account for. |
| 4 | *HobbyPCB ATU-500 HF Antenna Tuning Unit User Manual* (Manuals+ printout, saved 17 October 2026) | Published by HobbyPCB ([hobbypcb.com](https://www.hobbypcb.com)) and reproduced on Manuals+ ([manuals.plus](https://manuals.plus)); not copied here | The tuner's specification: 63 L steps from 50 nH to 3.2 µH, 127 C steps from 22 pF to 2800 pF, an L network with the capacitor on either side. The 50 nH minimum is the inductance floor in [atu-values.md](atu-values.md). |
| 5 | ATU-500 schematic, sheets 1 to 3 (Rev B), from the manual above | [sheet 1, L and C](references/atu500-schematic-sheet1-LC.png), [sheet 2, relays](references/atu500-schematic-sheet2-relays.png), [sheet 3, control](references/atu500-schematic-sheet3-control.png) | Part values, cores and relay assignments for the per-relay L and C tables in [atu-values.md](atu-values.md). |
| 6 | HobbyPCB *HR500 Control Panel* V1a (`HR500_Panel_V1a.exe`, PureBasic), read by disassembly | Notes: [hr500-control-panel-v1a-notes.md](references/hr500-control-panel-v1a-notes.md). The program itself is not kept here. | The `HRTMP` tuner reply layout. The 0.05 µH and 10 pF per step. Return loss to SWR. How the stock panel colours its indicators, including the HRST status digits it reads and the two it has swapped. |
| 6a | HobbyPCB ATU-500 firmware V1.1F (`ATU500_V1_1F.hex`, PIC18F26K22), disassembled with gputils `gpdasm`, 18 Oct 2026 | Not kept here (HobbyPCB's). Findings in [atu-values.md](atu-values.md#what-the-atu-500-firmware-shows-18-oct-2026). | The tuner's status reply (`L…-C…-H…-B…-S…-F…`). The relay drive: each step bit switches one relay directly, with the inductor byte inverted. This is why the window's capacitance is the sum of the capacitor bits (changed 18 Oct 2026). |
| 6b | HobbyPCB HR500 firmware V3.4 (`HR-500_V3.4.ino.hex`, AVR), read for its strings, 18 Oct 2026 | Not kept here (HobbyPCB's). | Confirms the `HRST` layout with 8 light digits, the six tune results, the bands and the baud rates. Shows an `HRFW` (firmware version) command not in the manual. Shows the `*` commands the amp sends to the tuner. |
| 7 | Micrometals iron-powder core data, T130 size: AL of 1.5 (mix 0), 9.6 (mix 6) and 11 (mix 2) nH per turn² (Micrometals catalogue, micrometals.com) | (public; not copied) | Nominal inductance of the ATU-500 coils from turns counted on Rich's board. |
| 8 | EPP-400 and EPP-500 series power supply user's manuals | From the power supply's maker; not copied here | Supplied as background on the HR500's supply. Nothing in the code is taken from them. |

## Bench evidence

All measured by Rich (AJ3G) on his HR500 (VRF2944 output devices, ATU-500), October 2026. Full results are in [test-log.md](test-log.md).

| # | Evidence | File | Used for |
|---|---|---|---|
| 9 | Trip logs, 160 m overdrive, from `hrctl log` at 10 readings a second | [trip-160m-1.txt](references/bench/trip-160m-1.txt), [trip-160m-2.txt](references/bench/trip-160m-2.txt) | Decoding the HRST status digits (d1 = transmitting, d2 to d6 = FWD, RFL, DRV, VDD, IDD lights), and how a trip looks on the serial port. |
| 10 | AMP window tuned into two 50 Ω loads on a T: L step 3, C step 12, C IN, 1.23:1 | [amp-window-tuned-25r-t.png](references/bench/amp-window-tuned-25r-t.png) | The tune all the ATU calculations start from. |
| 11 | VNA (SV6301A), the load at the antenna jack: 30.5 − j0.88 Ω at 14.080 MHz | [vna-load-30r5.jpg](references/bench/vna-load-30r5.jpg) | The load the tuner was matching. |
| 12 | VNA at the amp's RF input, ATU in: 19.0 − j15.7 Ω | [vna-amp-input-atu-in.jpg](references/bench/vna-amp-input-atu-in.jpg) | Left out of the estimate; see [atu-values.md](atu-values.md). |
| 13 | VNA at the amp's RF input, ATU on BYP: 47.4 − j28.1 Ω | [vna-amp-input-atu-byp.jpg](references/bench/vna-amp-input-atu-byp.jpg) | Shows the bypass path transforms the load. |
| 14 | VNA at the amp's RF input, same tune, into 50 Ω: 43.6 − j20.4 Ω | [vna-amp-input-50r-load.jpg](references/bench/vna-amp-input-50r-load.jpg) | With 10 and 11, fixes the ATU's stray floor at about 50 nH and 50 pF. |

## SDRoxide

| # | Document | Used for |
|---|---|---|
| 15 | SDRoxide source and user manual, dividebysandwich/sdroxide on GitHub (imported at 75b549b) | How the panel fits in: the engine, settings, window chrome, colours and the manual's style. |
