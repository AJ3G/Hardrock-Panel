# Hardrock amplifier panel: test log

Checks of each test build on real hardware, recorded as they're done.

Station: AJ3G. HR500 with ATU-500 on COM5 at 115200 baud, ELAD DUO, Windows. Dummy load unless noted.

## Build dev-f63ed3e (17 Oct 2026)

Changes under test: IDD read in tenths of an amp, and red/TRIPPED shown only on a real trip.

| # | Check | How | Expected | Result |
|---|---|---|---|---|
| 1 | IDD reads correctly | Key up at about 100 W; compare IDD with the amp's own screen | Matches the amp (about 7 A shows as 6.7 A, not 67 A) | **Pass** (17 Oct): panel IDD matches the amp's screen |
| 2 | No false trips | On a high band where the amp has less gain, drive between 8 and 12 W | DRV yellow, never red; no TRIPPED; AMP button yellow | pending |
| 3 | TRIPPED only on a real trip | Overdrive on 160 m until the amp trips | DRV red, TRIPPED · DRV, AMP button red | pending |
| 4 | Warnings hold | Cause a yellow warning, then unkey | Stays yellow after unkey; clears at the next key-up | pending |
| 5 | Temperature | Watch it as the amp warms | Yellow above 50 °C (122 °F), red above 65 °C (149 °F) | **Partly checked** (17 Oct): panel temperature matches the amp. The amp's own temperature turns yellow **at** 122 °F (50 °C), not above it, so the panel's "above 50" is one degree late. Panel colour not yet seen. Red not tested on purpose (amp not taken past 122 °F). Clicking the temperature toggles °C/°F: **pass**. Rich wants yellow from 120 °F; next build. |
| 6 | AMP button colour | Compare the AMP button with the panel | Takes the worst colour on the panel | pending |

## Build dev-20bb098 (17 Oct 2026)

Changes under test: the panel shows the amp's own lights (from its status digits), and temperature turns yellow from 120 °F.

**Result: all six checks passed on Rich's HR500, 17 Oct 2026.**

| # | Check | How | Expected | Result |
|---|---|---|---|---|
| 1 | Lights match the amp | Normal transmission at moderate power | Every light on the panel matches the amp's screen | **Pass** (17 Oct): all match, all green |
| 2 | Warning stays yellow | Push drive past 8 W on a high band without tripping | DRV yellow on both; no TRIPPED | **Pass** (17 Oct) on 15 m. Couldn't be produced on 10 m: the amp's gain there is too low to reach 8 W drive within limits. |
| 3 | Real trip | Overdrive on 160 m | Same lights as the amp (last time FWD red, DRV and VDD yellow); TRIPPED · FWD; AMP button red | **Pass** (17 Oct): amp and panel both red, both show the FWD trip |
| 4 | Latching | After the trip, unkey | DRV and VDD clear, FWD stays red until the next key-up, as on the amp | **Pass** (17 Oct): FWD stayed red on both until the next transmission at lower drive, then both went green together. (DRV/VDD clearing at unkey not separately reported.) |
| 5 | Temperature | Warm the amp to 120 °F | Yellow at 120 °F | **Pass** (17 Oct): panel temperature turns yellow at 120 °F |
| 6 | AMP button | Compare with the panel | Takes the worst colour shown | **Pass** (17 Oct): yellow with the temperature, matching the panel |

## Build dev-b80ac9f (17 Oct 2026)

Changes under test: the wide AMP window (two meters with TOP/BOTTOM pickers, readings as tiles, performance figures, full-width tuner drawing), OFF in yellow, resizing, and the "no inductance/capacitance needed" labels. The needles and reading tiles are raw live samples; only input power, efficiency and gain hold their peaks (1 s hold, then falling 33 % of full scale a second, Rich's figures).

| # | Check | How | Expected | Result |
|---|---|---|---|---|
| 1 | Layout | Open the AMP window, amp in standby | Two meters side by side (FWD/RFL and IDD/DRV), seven reading tiles, PTT/ANT/band row, Performance, Tuner | **Pass** (17 Oct, screenshot). FWD first showed yellow, but the amp itself was holding yellow from an earlier transmission; it cleared normally. Notes for next build: clickable items (TEMP, PTT) must look clickable; the dots before needle names render as hollow squares. |
| 2 | OFF button | Amp in standby, then PTT | OFF filled yellow; PTT filled green | **Pass** (17 Oct) |
| 3 | Needle pickers | Set the left meter TOP to DRV, BOTTOM to FWD, then BOTTOM to none | Each needle follows its picker; picking the other needle's reading swaps them; "none" removes the blue needle | **Pass** (17 Oct) |
| 4 | Colours | Look at a meter | Top needle, outer scale, its name and value white; bottom ones blue | **Pass** (17 Oct, from Rich's screenshot), apart from the name dots showing as hollow squares (fix noted) |
| 5 | Live readings | Transmit at moderate power | Needles and tiles move with every sample, no hold; tile colours match the amp's lights | **Pass** (17 Oct) |
| 6 | Performance | Transmit, then unkey | Input power, efficiency and gain appear; numbers hold the peak 1 s then fall; bars stay live; all blank in receive once the holds run out | **Pass with one bug** (17 Oct): hold and decay "almost perfect" (Rich), but gain decayed slower than efficiency and input power, because each fell a third of its own full scale a second. Fixed in bf404a8: each falls a third of its own peak a second, so all three clear together. |
| 7 | Needles apart | Put FWD on top and DRV on the bottom, transmit where both sit near the same angle | The bottom scale changes range (name says "rescaled") so the needles separate | **Pass** (17 Oct) with FWD over IDD at 100 W (FWD over DRV never overlaps on this amp: at about 20 dB gain FWD sits twice as far round). IDD went to 0 to 12.5 A; its yellow and red bands (18 and 20 A) lie beyond that range, so none showed. |
| 8 | Resize | Drag the window's right edge wider and narrower | Meters, text, buttons and tuner drawing all scale; nothing overlaps | **Pass** (17 Oct) |
| 9 | Tuner drawing | Tuner bypassed, then in line and tuned | Bypassed: relays and ATU box, "ATU BYPASSED". In line: the L network with values, or "no inductance needed" / "no capacitance needed" when 0 | Partly seen (17 Oct, screenshot, 50 Ω load): in line, "no inductance needed" and "no capacitance needed", Tuned. Still to test with a mismatched load. ON/BYP redraws correctly between the bypass and L/C drawings (17 Oct), but switching while transmitting made the amp flag RFL (relays hot-switched, a hardware limit). Fixed in 69ce78e/f0da694: ON/BYP, ANT and band are blocked while transmitting. Also from Rich: TUNE green when tuned, red when failed (69ce78e). Mismatch test (17 Oct, 20 m, amp OFF, ~5 W from the ELAD, two 50 Ω loads in parallel on a T, nominally 25 Ω): **Pass** for the drawing: Tuned 1.23:1, 0.15 µH (L step 3), 120 pF (C step 12), capacitor on the transmitter side, as theory says for a load below 50 Ω. The values do not yet settle linear versus per-relay: for a pure 25 Ω at 14.2 MHz the ideal is 0.28 µH and 224 pF, and the linear reading (0.15 µH, 120 pF) would give 1.65:1, the per-relay one (0.044 µH, 160 pF) 2.06:1, neither the 1.23:1 seen, so the load at the jack is not a pure 25 Ω (T and cable lengths). Needs the load measured at the jack. Load measured at the jack (17 Oct, VNA, 14.080 MHz): 30.5 − j0.88 Ω, so the ideal network is 0.286 µH and 181 pF. Against that load the linear reading (0.15 µH, 120 pF) gives 1.41:1 and the per-relay one (0.044 µH, 160 pF) 1.90:1; the amp saw 1.23:1. Linear needs only about 60 nH of wiring inductance and 30 pF of stray capacitance to reach 1.23:1, per-relay about 170 nH, so linear is the closer fit. Tuner input measured (17 Oct, VNA at the amp's RF input, amp OFF, ATU engaged, 14.080 MHz): 19.0 − j15.7 Ω, 2.93:1, not the 1.23:1 of the tune. With this load it fits about 278 pF across the load and no inductance (C on the antenna side), or 86 nH with 370 pF on the input side; neither is step 3 / step 12, so the tuner's state during the measurement needs confirming. Same with the ATU on BYP (14.090 MHz): 47.4 − j28.1 Ω, so the bypass path (relays, low-pass filter, wiring) transforms the load a lot. Modelled as a 53.5 Ω line of 135° (a C-L-C pi of 505 pF / 430 nH gives the same answers), step 3 / step 12 should read 48 − j20 Ω through it (linear) or 59 − j39 Ω (per-relay), not the 19 − j15.7 Ω measured. The steps that do fit are about L 9, C 25, C OUT (linear), so the tuner held a different setting during the VNA test than the panel showed at the tune, or the path model is too simple. Inconclusive. With board strays added (17 Oct, Rich: relays add a lot of C, no controlled-impedance traces): linear plus the manual's 50 nH / 22 pF minimums gives 1.27:1, close to the 1.23:1 seen; per-relay needs over 170 nH of stray inductance. Likely real network: about 200 to 230 nH and 140 pF. Working conclusion: linear steps plus a fixed stray floor. Same tune into a single 50 Ω (VNA at the amp input): 43.6 − j20.4 Ω. Fitting both tests gives a floor of about 50 nH and 50 pF, so the window now shows step 3 / step 12 as ~0.20 µH and ~170 pF. Judged within about 10% (Rich). Working in docs/hardrock/atu-values.md. |
| 10 | Temperature tile | Click the TEMP tile | Toggles °C/°F | **Pass with one bug** (17 Oct): it toggles, but after switching to °F the panel showed 71 °F creeping up to 75 °F while the amp's screen read 75 °F at once. Cause: the status frame's temperature lags the screen, and the letter was re-read only every 10 s. Fixed in e4d1bd6 (temperature from HRTP). |

## Build dev-231be5b (17 Oct 2026)

Fixes from the dev-b80ac9f checklist, plus the clickable look, the pop-out window, and tuner values that include the board strays. Amp in OFF (bypass) and low drive unless a check says otherwise.

| # | Check | How | Expected | Result |
|---|---|---|---|---|
| 1 | Needle-name dots | Open the AMP window; open both needle pickers | Round dots (white for the outer needle, blue for the inner) in the legends and pickers, no empty squares | **Pass** (18 Oct) |
| 2 | Clickable look | Hover over PTT/OFF, the buttons and the TEMP tile | Hand pointer and a white outline under the mouse; PTT/OFF and TEMP carry a folded corner; TEMP shows the unit it will switch to | **Pass** (18 Oct). Rich: document that the outline means a control; added to the user manual (2.23, "What can be clicked"). |
| 3 | Temperature after °C/°F | Click TEMP a few times | The panel matches the amp's screen within about a second each time | **Pass** (18 Oct) |
| 4 | Tuner redraw | ON/BYP on the ATU, receive only | The drawing follows at once |**Pass** (18 Oct) |
| 5 | Interlock: tuner, antenna, band | Key the radio at low drive, hover ON/BYP, ANT, DN, UP | Greyed out, with the "Not while transmitting" note; amp shows no RFL flag | **Pass** (18 Oct): interlocks work, no RFL flag. |
| 6 | TUNE colours | Run TUNE into the mismatch load | Blinks blue while tuning, green when tuned (red if it fails) | **Pass** (18 Oct): blue while tuning, green when tuned, on 20 m and 10 m. A failed tune not yet seen. |
| 7 | Pop out | Click POP OUT | The AMP window opens as its own window that can be moved to another screen and resized; the panel scales with its width | **Bug** (18 Oct): enlarging the popped-out window doesn't scale the panel up. Cause: the panel's scale follows only the width and stops at 1.8×, and the height is ignored, so a bigger window leaves empty space. Only in the popped-out window (Rich): docked, the window's height follows its width, so the panel always fills it; popped out, the operator sets both. Fix (held until this list is done): scale to fit both width and height, centred, with a higher limit. |
| 8 | Dock | Click DOCK in the popped-out window | It goes back inside the main window | **Pass** (18 Oct) |
| 9 | Remembered | Pop out, close SDRoxide, reopen it and open AMP | It opens popped out again |**Open** (18 Oct): after a restart Rich had to click AMP, and it opened docked. Checked: the pop-out choice saves and reads back (round-trip test), so whether it was popped out at close is being confirmed. Real gap found: the popped-out window doesn't remember its screen position and size (the Globe does); fix queued for the next build. |
| 10 | Close | Close the popped-out window with its own close box | The AMP window closes and the AMP button in the top bar goes unlit | **Pass** (18 Oct). |
| 11 | Tuner values | Tune into the 30.5 Ω load | Step 3 / step 12 reads ~0.20 µH and ~170 pF, with the step numbers alongside | **Pass** (18 Oct): 20 m, step 3 / step 12, C IN, 1.23:1 again (repeatable), shown as ~0.20 µH and ~170 pF. Also 10 m: step 3 / step 3, ~0.20 µH and ~80 pF, 1.13:1. |
| 12 | AMP chip on an error | Amp connected, then pull its USB lead (receive only), then plug it back in | The AMP chip next to SETTINGS turns red within a couple of seconds, hover says "not answering"; it clears once the amp answers again | **Pass with one bug** (18 Oct): the chip turned red and the window showed the error with the meters blank. Plugged back in, it took minutes to recover: the link backed off to 30 s between tries. Also the error was Windows' raw "The system cannot find the file specified". Both fixed on next-build (2 s retry, three pings, "COM5 not found. Check the amplifier's USB lead."). |

## Build dev-5dab118 (18 Oct 2026)

All 18 items are run on dev-5dab118 (Rich, 18 Oct); dev-d738cb5 was built but skipped, as 5dab118 has everything in it. Fixes from the dev-231be5b checklist: the reconnect after a USB lead is plugged back in, the pop-out window's scaling, the AMP window coming back as it was closed, the caption over EFFICIENCY, and the tuner's capacitance from the ATU-500 firmware. Then, from Rich the same evening: outer needle above inner in the legends, power ranges for FWD and RFL, DRV green in bypass, safety interlock #4, and DRV on a 0–20 W scale. Amp in OFF (bypass) and low drive unless a check says otherwise.

| # | Check | How | Expected | Result |
|---|---|---|---|---|
| 1 | Version | Settings → General | Version line ends "dev.5dab118" | **Pass** (18 Oct) |
| 2 | Pop-out fits | POP OUT, then make the window much larger, then tall and narrow, then wide and short | The panel grows to fill the window, never cut off, centred left to right when the window is wider than the panel | **Fail** (18 Oct), two bugs. (1) Enlarged, the panel shook and jittered until the window was shrunk. Cause: the fit re-measured the panel every frame and rescaled from that, and the height isn't exactly proportional to the scale, so it chased itself. Fixed: refined for 3 frames after a resize, then held. (2) Blown up (screenshot), the PERFORMANCE and TUNER captions stayed tiny and the status line under the tuner was cut off. Cause: the captions and the spacing between rows didn't scale. Fixed: both scale with the panel. |
| 3 | Caption | Look at the PERFORMANCE row | "numbers hold peaks · bars are live" sits centred above EFFICIENCY | **Pass** (18 Oct) |
| 4 | Comes back popped out | Popped out, move and size the window, change a meter pick, then close SDRoxide and start it again | AMP opens by itself, popped out, at the same place and size, with the same meter picks || **Fail** (18 Oct). On two screens it came back on the main screen, to the left, not where it was left. Size and picks not reported as wrong. Likely cause: the position was saved in egui points, and a new window starts at the main screen's display scale, so on a second monitor with a different scale the same points land elsewhere. Fixed: the position is also saved in screen pixels, and once the window exists it is moved to that pixel at its own scale. |
| 5 | Comes back docked | DOCK, drag the docked window somewhere and resize it, then close SDRoxide and start it again | AMP opens by itself, docked, at the same place and width || **Pass** (18 Oct): came back docked where it was left. |
| 6 | Comes back closed | Close the AMP window, then close SDRoxide and start it again | AMP stays closed || **Pass** (18 Oct) |
| 7 | Reconnect | Amp connected, receive only: pull the USB lead, wait 30 s, plug it back in | The chip turns red and the window says "COM5 not found. Check the amplifier's USB lead."; within about 5 s of plugging in, the readings come back || **Pass** (18 Oct) |
| 8 | Tuner capacitance | Tune into the 30.5 Ω load on 20 m | Step 12 still reads ~170 pF; step 3 (10 m) now reads ~45 pF instead of ~80 pF | |
| 9 | Legend order | Look at each meter's bottom corners | The outer needle's name and reading are on the top line, the inner one's below || **Pass** (18 Oct) |
| 10 | DRV scale | Look at the DRV scale | 0 to 20 W, labelled 4, 8, 12, 16, 20; yellow starts on 8, red on 12 || **Pass** (18 Oct) |
| 11 | Help screenshot | Open the help (F1), go to 2.23 Hardrock amplifier | The AMP window picture shows, not a placeholder || **Pass** (18 Oct) |
| 12 | Full scales at start | Start SDRoxide, look at FWD and RFL before transmitting | Full scales (FWD 0 to 625 W), no "scale" note in the legend || **Pass** (18 Oct) |
| 13 | 10 W scale | Amp in PTT, key at about 5 W for 6 s or more | After 5 s, FWD goes to 0 to 10 W and RFL to 0 to 2 W, legend "10 W scale"; drive over 10 W out: back to full scale at once | |
| 14 | QRP in bypass | Amp in OFF, key the ELAD at 5 W for 6 s or more | 10 W scale after 5 s | |
| 15 | 100 W in bypass | Amp in OFF, key at over 10 W (for example 20 W, within the ELAD's range) for 3 s | 100 W scale (FWD 0 to 100 W, RFL 0 to 20 W), legend "100 W scale". Also note: does FWD read the power in OFF, or only DRV? | |
| 16 | DRV green in bypass | Amp in OFF, key at over 8 W, ATU in, then bypassed | DRV tile and needle colour stay green both ways; AMP chip not red | |
| 17 | Interlock #4 | Amp in OFF, keyed at over 8 W: hover PTT/OFF, then try to click it | Greyed, hover says "Lower the drive first" with the drive and 8 W; nothing changes. Unkey: it works again | |
| 18 | Needle overlap still parts | FWD over IDD at about 100 W in PTT | The inner scale still rescales, legend "rescaled" | |

## Build dev-a9f702e (18 Oct 2026)

Everything in dev-5dab118, plus: the pop-out fixes from items 2 and 4 above, the tuner drawing scaling with the panel, and the ELAD menu 34 fix (492fdbc from the other session, brought into main as 7f96b1b, and 243cc7c for the USB-only path), DRV no longer opening yellow from a light held before SDRoxide connected (dabb8a6), and in OFF the FWD, RFL and SWR from the DUO (a9f702e). dev-dabb8a6 was built too and is kept, but this is the one to test. Items 5 to 18 of dev-5dab118 not yet run carry over to this build.

| # | Check | How | Expected | Result |
|---|---|---|---|---|
| 1 | Version | Settings → General | Version line ends "dev.a9f702e", and the AMP chip and AMP settings are there | |
| 2 | Menu 34 on connect | Set menu 34 to 5 W on the radio, then start SDRoxide | Menu 34 reads MAX once connected | |
| 3 | Drive slider leaves menu 34 alone | Move the Drive slider through its range, then look at menu 34 | Still MAX | |
| 4 | Drive slider sets the power | Key Tone into the dummy load at Drive 100%, 50% and 25% | About full, half and a quarter of the power | |
| 5 | Pop-out fits, no jitter | Pop out, make the window much larger, then tall and narrow, then wide and short | Fills the window, no shaking, captions and tuner drawing scale with it, nothing cut off | |
| 6 | Pop-out on the second screen | Pop out onto the second screen, close SDRoxide, start it again | It comes back on the second screen where it was left | |
| 7 | No yellow left over at start | Key the amp in PTT at over 8 W drive so DRV goes yellow, unkey, close SDRoxide and start it again | DRV opens green, though the amp's own screen still shows yellow; it follows the amp again from the next key-up | |
| 8 | DUO readings in OFF, dummy load | Amp in OFF, key the DUO at about 5 W into the dummy load | FWD about 5 W "from the rig", RFL near 0 "from the rig", SWR about 1.0:1; tile hover says "measured by the radio" | |
| 9 | DUO readings in OFF, mismatch | Amp in OFF, ANT 1 at 3:1, key briefly | SWR about 3:1, RFL about a quarter of FWD, both "from the rig" | |
| 10 | Main TX meter in PTT | Amp in PTT at low drive into the dummy load | SDRoxide's main TX meter shows the amp's output, not the DUO's few watts | |
| 11 | Main TX meter in OFF | Amp in OFF, key the DUO | The main TX meter shows the DUO's own power | |

## Build dev-482c33a (18 Oct 2026, superseded)

Built and kept, but not tested: its presets assumed about 6 W at Drive 100%, and the DUO measured below showed 5W would really give about 8 W. Do not use its presets; dev-da06ebf replaces it.

## DUO power, measured 18 Oct 2026

Measured by Rich with a known-good LP-100A digital vector wattmeter, 20 m, dummy load, amp in OFF, SDRoxide's TUNE unless it says FT8 (Drive on the old full-audio scale).

| Drive | Menu 34 MAX, TUNE | Menu 34 MAX, FT8 | Menu 34 5 W |
|---|---|---|---|
| 1% | 0.94 W | | |
| 3% | 1.31 W | | |
| 4% | 1.50 W | | |
| 5% | 2.08 W | | |
| 6% | 2.41 W | | |
| 8% | 3.07 W | | |
| 10% | 3.72 to 3.77 W | | |
| 11% | 4.04 W | | |
| 15% | 5.39 W | 5.26 W | 3.72 W (TUNE) |
| 20% | 6.85 W | | |
| 22% | 7.36 W | | |
| 23% | 7.73 W | | |
| 24% (sent as 34%) | 8.03 W | | |
| 25% | 8.34 W | 7.88 W | 5.9 W |
| 50% | 8.34 W | 8.11 W | |
| 75% | | | 5.83 to 6.03 W |
| 100% | 8.34 W | 8.11 W | 5.9 to 6.1 W |

Menu 34 at 1 W, Drive 100%: 1.44 W on TUNE, 1.41 W on FT8 (PTT).

SWR against a known 2:1: two 50 Ω dummy loads on a T, 1.88:1 on the LP-100A and on Rich's VNA. SDRoxide (the DUO's own `WR`) read just under 2:1, so the DUO's SWR agrees. Into an open no meter agreed: the LP-100A showed the DUO folding back to 2.94 W, the DUO's screen 8.7 W and SDRoxide 9.98:1, the HR500's DRV almost 10 W (fixed in 6607156: the panel no longer shows DRV as FWD when the DUO flags its reading). Menu 34 at 3 W, Drive 100%: 3.77 W on TUNE, 3.67 W on FT8.

The DUO's own F3 tune gave 8.34 W; it has its own tune power setting in the menu, so the earlier 6 W was that, not Drive 100%. The HR500's DRV agreed with the LP-100A. Design decision 4.

## Build dev-da06ebf (18 Oct 2026, superseded)

Built and kept, but not tested: its buttons set Drive, and Rich wanted them to set menu 34. dev-9ee4bd7 replaces it. Its Drive scaling is carried forward unchanged.

## Build dev-da06ebf, checks (not run)

Everything in dev-a9f702e, plus the FDM-DUO presets (design decision 3) and Drive scaled into the DUO's working range (decision 4). Checks 1 to 11 of dev-a9f702e not yet run carry over, as do items 8 and 13 to 18 of dev-5dab118. Check 4 of dev-a9f702e (Drive sets the power) is replaced by check 3 here.

| # | Check | How | Expected | Result |
|---|---|---|---|---|
| 1 | Version | Settings → General | Version line ends "dev.da06ebf" | |
| 2 | Presets only for the DUO | Look at the TX box with the DUO connected, then with any other radio or the S2 | 1W 3W 5W MAX with "ELAD FDM-DUO TX PWR" under them for the DUO only | |
| 3 | Drive spans the DUO | Menu 34 at MAX, LP-100A, FT8 or TUNE at Drive 25%, 50%, 75%, 100% | About 2.4, 4.3, 6.1 and 8 W, rising all the way | |
| 4 | A preset sets Drive | Click 3W on a band not yet tuned on | Drive goes to 33%, 3W lights, about 3 W out; hover says the band is not measured yet | |
| 5 | TUNE measures the band | Amp in OFF, Tune 50% or more, press TUNE for a few seconds | Hover on 3W now says "Measured on TUNE" with the DUO's watts at 100% | |
| 6 | Presets give their watts | After check 5, key FT8 at 1W, 3W, 5W and MAX | LP-100A about 1, 3, 5 and 8 W | |
| 7 | Hand move clears it | Click 5W, then move the Drive slider | 5W goes dark | |
| 8 | Band change | Click 3W, change band | Drive is set again for the new band | |
| 9 | Remembered | Tune on a band, close and restart SDRoxide | Hover still says the band was measured | |

## Build dev-9ee4bd7 (18 Oct 2026)

Everything in dev-a9f702e, plus Drive scaled into the DUO's working range (design decision 4) and the 1W / 3W / 5W / MAX buttons setting menu 34 (decision 3). Checks 1 to 11 of dev-a9f702e not yet run carry over, except check 4, which check 3 here replaces; so do items 8 and 13 to 18 of dev-5dab118. The meters are a known-good LP-100A digital vector wattmeter.

| # | Check | How | Expected | Result |
|---|---|---|---|---|
| 1 | Version | Settings → General | Version line ends "dev.9ee4bd7" | |
| 2 | Buttons only for a DUO with Rig control | TX box with the DUO and its serial port set; then with the port set to none; then any other radio or the S2 | 1W 3W 5W MAX with "ELAD FDM-DUO TX PWR" under them only in the first case; MAX lit | |
| 3 | Drive spans the DUO at MAX | MAX lit, FT8 or TUNE at Drive 25%, 50%, 75%, 100% | About 2.4, 4.3, 6.1 and 8 W, rising all the way | |
| 4 | Buttons set menu 34 | Click 3W, look at menu 34 on the radio; then 5W, 1W, MAX | Menu 34 follows: 3.0 W, 5.0 W, 1.0 W, MAX | |
| 5 | Drive within the setting | 3W lit, FT8 at Drive 100% and 50% | About 3.7 W and about half that | |
| 6 | Remembered | Click 5W, close and restart SDRoxide | 5W lit, menu 34 reads 5.0 W after connect | |
| 7 | Back to MAX | Click MAX, restart | MAX lit, menu 34 MAX | |
| 8 | Hover | Hover over 5W | Says the HR500 will not reach full output on most bands at 5 W | |

## Build dev-6298a06 (18 Oct 2026): the checklist

Everything still to test, in one list. It takes in dev-9ee4bd7's checks, the unrun checks of dev-a9f702e and items 8 and 13 to 18 of dev-5dab118, so those lists need not be run separately. New in this build besides dev-9ee4bd7: 6607156 (with the DUO, the AMP panel no longer shows DRV as FWD when the DUO flags its own reading) and 6298a06 (DRV on the left power meter, rescaling with FWD barefoot). dev-a47a117 was built without the DRV change and is kept.

Reference meter: a known-good LP-100A digital vector wattmeter at the DUO's output. Amp in OFF and the DUO into the dummy load unless a check says otherwise. Keep key-ups into a mismatch short.

**Start-up**

| # | Check | How | Expected | Result |
|---|---|---|---|---|
| 1 | Version | Settings → General | Version line ends "dev.6298a06" | **Pass** (18 Oct) |
| 2 | Menu 34 at connect | Set menu 34 to 5 W on the radio, then start SDRoxide | Menu 34 reads MAX once connected, MAX lit || **Pass** (18 Oct) |
| 3 | No yellow left over | Amp in PTT, drive over 8 W so DRV goes yellow, unkey, restart SDRoxide | DRV opens green though the amp's screen shows yellow; follows the amp from the next key-up || **Pass** (18 Oct) |

**ELAD power buttons**

| # | Check | How | Expected | Result |
|---|---|---|---|---|
| 4 | Only for a DUO with Rig control | TX box with the DUO and its serial port set; then the port set to none; then another radio or the S2 | 1W 3W 5W MAX and "ELAD FDM-DUO TX PWR" only in the first case | **Fail** (18 Oct): after a restart the buttons were missing until Settings was opened, because the radio settings load lazily. Fixed in the next build, which loads them at start for an ELAD DUO. Caption also changed to white, centred under the buttons (Rich's request). |
| 5 | Buttons set menu 34 | Click 3W, look at menu 34; then 5W, 1W, MAX | Menu 34 follows: 3.0 W, 5.0 W, 1.0 W, MAX | **Pass** (18 Oct) |
| 6 | Drive leaves menu 34 alone | Move Drive through its range, look at menu 34 | Unchanged | **Pass** (18 Oct) |
| 7 | Drive spans the DUO at MAX | MAX lit, FT8 at Drive 25%, 50%, 75%, 100% | About 2.4, 4.3, 6.1 and 8 W, rising all the way | **Pass** (18 Oct), LP-100A, FT8, 20 m: 2.36, 4.37, 6.15, 7.96 W |
| 8 | Drive within a setting | 3W lit, FT8 at Drive 100% and 50% | About 3.7 W, and about half that | **Pass** (18 Oct), LP-100A, FT8: 3.52 W at 100%, 1.93 W at 50% |
| 9 | Remembered | Click 5W, restart SDRoxide | 5W lit, menu 34 reads 5.0 W after connect | **Pass** (18 Oct), on dev-6298a06 with Settings opened once after restart |
| 10 | Back to MAX | Click MAX, restart | MAX lit, menu 34 MAX | **Pass** (18 Oct) |
| 11 | Hover | Hover over 5W | Says the HR500 will not reach full output on most bands at 5 W | **Pass** (18 Oct) |

**Readings with the amp in OFF**

| # | Check | How | Expected | Result |
|---|---|---|---|---|
| 12 | DRV on the power meter | Open the AMP window (meter picks never changed) | Left meter FWD over DRV, right meter IDD over RFL | **Pass** (18 Oct) |
| 13 | Dummy load | DUO at about 5 W | FWD about 5 W and RFL near 0, both "from the rig"; SWR about 1.0:1; LP-100A agrees | **Pass** (18 Oct) |
| 14 | 2:1 | The T with two dummy loads, key briefly | SWR just under 2:1, RFL about a tenth of FWD, "from the rig"; LP-100A 1.88:1 | **Pass** (18 Oct): SWR just under 2:1; FWD 5 W, the RFL needle read 0.49 W but its tile showed "1 W" (0.49 of 5 W is about 1.9:1, so the reading is right). The tiles rounded FWD and RFL to whole watts; the next build shows tenths below 10 W. DRV read 3.9 W. |
| 15 | Open | Open circuit, key briefly | FWD and RFL empty, legend "the rig flagged its reading"; no 10 W from DRV | **Pass** (18 Oct), by a different path: this time the DUO did not flag its reading, it reported FWD 8 W and RFL 7 W, SWR 23.4:1, all marked "from the rig". DRV read 11.4 W and was not borrowed. The 10 W range moved to 100 W because DRV passed 10 W. SDRoxide's tune stopped itself: "SWR 23.2:1, tune limit 5.0:1". RFL legend said "100 W scale" on its 20 W arc, fixed in dev-209ac05. Settled numbers (Rich): LP-100A 2.62 W forward, 2.93 W reflected (the DUO folds back, nearly all of it reflected); the DUO reported 7.75 W before shutting off; the panel showed FWD 8 W, RFL 6–7 W, DRV 11.4 W, SWR 22.5:1. So into an open the DUO's own watts read about three times high, while its SWR correctly reads very high. |
| 16 | 10 W scale barefoot | DUO at about 5 W for over 5 s | FWD and DRV on the 10 W scale, RFL on 2 W, legend "10 W scale" ("rescaled" too if FWD and DRV would overlap) | **Pass** (18 Oct), screenshot: MAX, FWD 8 W on the 10 W scale "from the rig", DRV 7.6 W on the 10 W range rescaled to 20 W to keep the needles apart, RFL 0 W on its 2 W arc (legend said "10 W scale", fixed in dev-209ac05), SWR 1.1:1. |
| 17 | DRV green in bypass | MAX, Drive 100% (just over 8 W), ATU in, then bypassed | DRV tile and needle stay green both ways; AMP chip not red | **Pass** (18 Oct) |
| 18 | Interlock #4 | Keyed as in 17: hover PTT/OFF, then click it | Greyed, hover says "Lower the drive first" with the drive and 8 W; nothing changes; unkey and it works | **Not testable with the DUO alone** (18 Oct): PTT stayed active, correctly. The interlock goes by the amplifier's own DRV, which read 7.6 W at MAX, Drive 100%, under its 8 W limit, while the DUO and the LP-100A read about 8 W (Rich: likely loss on the way to the HR500's bridge). Moved to the ART-120 checks as 28. |
| 19 | Main TX meter in OFF | Key the DUO | SDRoxide's main TX meter shows the DUO's own power | **Pass** (18 Oct): 6 W on 20 m and 5.7 W on 30 m, agreeing with the DUO's own display |

**Amp in PTT**

| # | Check | How | Expected | Result |
|---|---|---|---|---|
| 20 | 10 W scale in line | Amp in PTT, about 5 W out for 6 s or more | After 5 s FWD on 0 to 10 W, RFL on 0 to 2 W, "10 W scale"; over 10 W out: full scale at once | **Pass** (18 Oct) |
| 21 | Main TX meter in PTT | Amp in PTT, low drive | Main TX meter shows the amp's output, not the DUO's few watts | **Pass** (18 Oct): Rich noticed it unprompted, the main PWR reading follows the amp's output in PTT |
| 22 | Needles still part | Pick FWD over IDD, about 100 W in PTT | The inner scale rescales, legend "rescaled" | **Pass** (18 Oct): the IDD scale stretched near 200 W, where the needles began to meet; at 100 W they were still apart, so no stretch was needed (the checklist's 100 W was only a guess) |

**Window and tuner**

| # | Check | How | Expected | Result |
|---|---|---|---|---|
| 23 | Pop-out fits | Pop out, make it much larger, then tall and narrow, then wide and short | Fills the window, no shaking, captions and tuner drawing scale, nothing cut off | **Pass** (18 Oct) |
| 24 | Pop-out on the second screen | Pop out onto the second screen, restart SDRoxide | Comes back on the second screen where it was left | **Pass** (18 Oct) |
| 25 | Tuner capacitance | Tune into the 30.5 Ω load on 20 m | Step 12 about 170 pF; step 3 (10 m) about 45 pF | **Pass** (18 Oct): 20 m step 12 shows ~170 pF (screenshots). On 10 m the ATU chose C step 1 this time (step 3 on 17 Oct) and showed ~21 pF, which is the model's 11 pF bit plus the 10 pF floor, as designed. On 40 m, C step 2 showed ~34 pF (23.5 + 10), also as designed. On 80 m, L step 2 showed ~0.15 µH (2 × 0.05 + 0.05 floor) and C step 1 ~21 pF, as designed. That tune ended at 1.82:1, so on 80 m the ATU barely changed the match; the ATU's own behaviour, not the display. Retuned at MAX on 80 m: L step 3 ~0.20 µH, C step 8 ~120 pF (110 + 10), still 1.82:1; display as designed. |

**Deferred to the end (Rich): with the DUO-ART-120, 100 W through the bypassed HR500. Run only after 1 to 25, which are the HR500 and DUO alone.**

| # | Check | How | Expected | Result |
|---|---|---|---|---|
| 26 | 100 W scale in bypass | DUO into the ART-120, ART-120 into the HR500 in OFF, dummy load; over 10 W (say 30 W) for 3 s | FWD and DRV on 0 to 100 W, RFL on 0 to 20 W, legend "100 W scale" | **Not testable** (18 Oct, Rich): the FDM-DUO can't report or drive a measurable 100 W path; with the DUO-ART-120 in line the DUO reports only its own exciter, and ART support is not being added (needs its own interface). Untested until a 100 W radio that reports its own power is available. |
| 27 | Whose FWD | Same as 26: note FWD and its legend, DRV, and the LP-100A at the HR500's input | Tells us whether the DUO's FP reads the ART-120's output or only the DUO's own few watts. If only its own, FWD "from the rig" would be wrong with the ART-120 in line and needs a fix | **Answered** (18 Oct): with the DUO-ART-120 in line at 100 W (Rich: 100 W on both the DUO and the ART), the panel's FWD showed about 1 W "from the rig". The DUO's FP is its own exciter, not the ART's output, so FWD is wrong with the ART in line and needs a fix. Neither manual documents any way to read the ART's metering over the DUO's CAT (the ART manual lists only an FT-817 CAT input and its own LAN remote). |
| 28 | Interlock #4 (moved from 18) | ART-120 in line, HR500 in OFF, dummy load; drive so the HR500's DRV reads over 8 W, keep keyed; hover PTT/OFF, then click it | Greyed, hover says "Lower the drive first" with the drive and 8 W; nothing changes; unkey and it works | **Not testable** (18 Oct, Rich): the FDM-DUO can't report or drive a measurable 100 W path; with the DUO-ART-120 in line the DUO reports only its own exciter, and ART support is not being added (needs its own interface). Untested until a 100 W radio that reports its own power is available. |

## Build dev-edc650c (18 Oct 2026): the fixes from the checklist

| # | Fix | How | Expected | Result |
|---|---|---|---|---|
| F1 | DUO buttons after restart, caption | Restart SDRoxide, don't open Settings | 1W 3W 5W MAX shown; caption white, centred under them | **Pass** (18 Oct) |
| F2 | Tenths barefoot | Amp in OFF, DUO about 5 W into the dummy load for over 5 s | FWD e.g. "5.0 W", RFL e.g. "0.0 W" | **Pass** (18 Oct). Rich may want the drop to the low-power scales after 3 s instead of 5 s; to decide after these checks. |
| F3 | RFL legend | As F2 | RFL on 0 to 2 W, legend "2 W scale" | **Pass** (18 Oct), once keyed for over 5 s; at rest the meters are on the amp's full scales (FWD 625 W, RFL 60 W) by design |
| F4 | Legends clear the needles | At rest and keyed | No legend touches a needle | **Pass** (18 Oct) |
| F5 | SWR red past the tune limit | Into the open, key briefly | SWR tile red; FWD and RFL "unreliable at this SWR" | **Pass** (18 Oct). Rich wants the warning moved out of the legends to the top centre of the meter, red and larger. |

## Build dev-21e577b (18 Oct 2026)

| # | Check | How | Expected | Result |
|---|---|---|---|---|
| G1 | Installs over the old one | Run the new installer without uninstalling | Version ends dev.21e577b; one SDRoxide in Windows Apps; settings kept | **Pass** (18 Oct) |
| G2 | Temperature keeps up in TX | Transmit at high power for a minute or more, watch TEMP against the amp's screen | Within a degree or two of the amp's screen throughout | **Pass** (18 Oct) |
| G3 | SWR warning at the top | Amp in OFF, open circuit, key briefly | Large red "FWD AND RFL UNRELIABLE AT THIS SWR" centred at the top of the meter showing them; SWR tile red; legends without the note | **Pass** (18 Oct) |
| G4 | Low range after 2.5 s | Amp in OFF, DUO about 5 W into the dummy load | FWD to 0 to 10 W after about 2.5 s | **Pass** (18 Oct) |

## Findings

### DRV yellow at power-up (18 Oct)

Rich: the HR500 can boot with DRV lit yellow on its own screen with nothing wrong, a firmware quirk. The panel must show green then, because an operator watching from remote would otherwise think there is a problem. Since dev-dabb8a6, any yellow the amp shows before SDRoxide has seen a transmission on this connection is shown green; reds (trips) still show; from the first key-up the panel follows the amp's lights.

### FWD and RFL read nothing in bypass (18 Oct, open)

Rich, dev-5dab118, QRP in bypass: after the meters went to the 10 W scale, FWD and RFL showed no power. The panel draws them straight from HRST, so the amp is reporting 0 W; the 10 W scale came from DRV. The bridge (HR500 LPF Rev D, sheet 2, T303/T304) is at the RF output, after the low-pass filters that the bypass path also goes through, so it should see the power. To find out on the bench whether the firmware reports FWD only in PTT or rounds low power to 0. Rich then checked at higher reflected power: ANT 1 at 3:1 SWR in OFF also showed nothing on FWD or RFL, so the amp simply doesn't report them in bypass; not a low-power cut-off. Rich's direction: in bypass, show the power and SWR the rig reports over CAT. Done in code (see design-decisions.md, decision 2); to check in the next build after dev-dabb8a6. The DUO's own FP and RP turned out to be milliwatts (Rich's CAT test, 18 Oct: 6 W into a dummy load gave FP 006091, RP 000001; an open gave FP 008291, RP 007525, WR 33:1), so they are now read directly.

### Trip log, 160 m overdrive (before this checklist)

From hrctl's `log` command at 10 readings a second:

- Before the trip: about 305 W out on 2.4 W drive, 57 V, 8 A.
- At the trip: the amp stays in PTT. Output goes to 0 W with 5.6 W drive still present. VDD drops to 42 V, because the protection cuts the drain supply. The readings hold for about 4.5 s, then recover.
- The amp didn't answer for about 0.6 s just before the trip, so no drive reading over 12 W was seen.
- The amp sent nothing on its own at the trip.

### How the stock HobbyPCB control panel colours its indicators

From its program code: the panel never compares readings with limits. It expects a longer HRST reply than the manual shows, with 8 status digits after the antenna field. It colours FWD, RFL, VDD and IDD from those digits: 2 is yellow, 3 is red, anything else is green. Its DRV indicator is never set, so it always stays green. Its RFL red test reads the wrong variable. Whether this HR500 sends those digits is to be checked with hrctl, which now prints any fields past the antenna.

### Second trip log, 160 m: the amp sends its own indicator status

hrctl now prints the HRST fields past the antenna. This HR500 sends one more field of 8 digits, for example `HRST-…-1-+12111101`, shown here as `+d1…d8`. Selected lines:

```
  2.42  FWD   0 W  DRV 0.0 W  VDD 58 V  IDD 0.0 A   +02101101   idle, before keying
  2.55  FWD 250 W  DRV 2.0 W  VDD 57 V  IDD 7.7 A   +11111101   keyed: d1 1, everything resets
  3.03  FWD 246 W  DRV 2.0 W  VDD 57 V  IDD 7.4 A   +12111101   d2 yellow
  9.38  FWD 335 W  DRV 9.9 W  VDD 54 V  IDD 8.0 A   +13121101   drive raised: d2 red, d4 yellow
 10.17  FWD   0 W  DRV 3.5 W  VDD 42 V  IDD 0.0 A   +13122101   tripped: d5 yellow too
 11.99  FWD   0 W  DRV 0.5 W  VDD 58 V  IDD 0.0 A   +03122101   d1 0: transmission over, colours held
 12.45  FWD   0 W  DRV 0.1 W  VDD 58 V  IDD 0.0 A   +03101101   d4 and d5 back to normal; d2 stays red
```

Reading so far:
- d1 is the transmit flag: 1 while transmitting, 0 otherwise.
- d2 to d6 are indicator states: 1 green, 2 yellow, 3 red, 0 not active. d2's red stays latched after the transmission until the next key-up, as the manual describes. The idle `2` in d2 before keying is the latched yellow from an earlier transmission.
- d7 is 0 throughout. The stock panel shows it under "ATU Tuning". d8 is 1 throughout.
- Confirmed against the amp's own screen at this trip (Rich): FWD red, DRV and VDD yellow. DRV and VDD cleared on releasing PTT; FWD stayed red until the next transmission at a lower power. That fits d2 = FWD (red, latched), d4 = DRV (yellow from the 9.9 W reading on) and d5 = VDD (yellow once the supply sagged to 42 V). By elimination d3 = RFL and d6 = IDD; neither has changed yet.
- So this trip was FWD over 600 W, caused by the drive spike between readings. The stock panel has d4 and d5 the wrong way round (it calls them VDD and IDD), which is also why its DRV light never changes.

Raw logs: `trip-160m-1.txt` and `trip-160m-2.txt` in the project's hardrock-rs/logs folder.
