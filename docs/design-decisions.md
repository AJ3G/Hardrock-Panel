# Hardrock amplifier: design decisions

Places where the panel deliberately does something other than copy the amplifier, and why. Before changing any of these, read the reason.

Decisions 1 to 5 were made for the AMP window in SDRoxide and came over with it; where one names SDRoxide, read it as this panel unless it is marked SDRoxide only. Decisions 6 to 9 are this standalone panel's own.

## 1. A yellow the amplifier shows before your first transmission is shown green

**Decided:** AJ3G, 18 Oct 2026.

**What the panel does.** Until SDRoxide has seen a transmission on the current connection to the amplifier, any indicator the amplifier reports as yellow (FWD, RFL, DRV, VDD or IDD) is shown green. A red is always shown. From the first key-up on, the panel shows the amplifier's lights exactly as it sends them.

**What the amplifier does.** The HR500 reports its five indicator lights in its status reply (see [history/test-log.md](history/test-log.md), "Second trip log"). A light it turns yellow stays yellow until the next key-up. It can also start up with DRV lit yellow when nothing is wrong. AJ3G saw this on 18 Oct 2026, after power-cycling the amplifier while sorting out the ELAD's menu 34: the amplifier's own screen and the panel both opened with DRV yellow, and both cleared at the first key-up.

**Why the panel doesn't copy it.**

- A yellow at start-up is a quirk of the amplifier's firmware, not a warning about anything happening now. There is no RF and no drive, so there is nothing for DRV to warn about.
- The panel is used from remote, where the operator can't see the amplifier or hear its fan. A yellow light there reads as "something is wrong with the amplifier". The operator might stop operating, or start fault-finding, when nothing is wrong.
- A warning that is often false trains people to ignore the colour. Yellow and red must only appear when they mean something.

**Why it is safe.**

- Only yellows are hidden, and only before the first transmission. A yellow is a caution; it never stops the amplifier.
- A red means the amplifier tripped and went off line. That is always shown, whenever it happened, so an operator connecting after a trip still sees it.
- From the first key-up the amplifier clears its own lights, and every warning during a transmission is shown as sent and held, as before.
- The live meters and readings are not affected; only the indicator colour is.

**Where.** `crates/hardrock/src/link.rs`, `lights_seen()`. Tested by `a_yellow_held_from_before_the_link_opened_shows_green`. Described in the user manual, section 2.23, "Readings". Bench check: history/test-log.md, build dev-dabb8a6, check 7.

**When it resets.** A new connection to the amplifier starts over: after SDRoxide starts, after the amplifier's USB lead is plugged back in, or after the amplifier is powered back on and reconnects.

## 2. In OFF (bypass), FWD, RFL and SWR come from the radio (in this panel, FWD from DRV)

**Decided:** AJ3G, 18 Oct 2026.

**What the panel does.** Only while the amplifier is in OFF, and only while the radio transmits:

- SWR is the radio's own reading over CAT, where it gives one.
- FWD is the watts the radio measures, where it reports watts. Otherwise it is the amplifier's DRV reading.
- RFL is the reflected watts the radio measures, where it reports them. Otherwise it is worked out as FWD × |Γ|², where |Γ| = (SWR − 1) / (SWR + 1).

The meter legend and the tiles' hover text say where each came from. Any reading the amplifier does give in OFF is kept; only a zero is filled. In PTT nothing changes: the amplifier's own bridge is used.

**What the amplifier does.** In OFF the HR500 reports FWD 0, RFL 0 and no SWR, whatever is going through it. AJ3G checked on 18 Oct 2026: 5 W from the ELAD in OFF showed nothing, and ANT 1 at 3:1 SWR showed nothing either, so it is not a low-power cut-off. The bridge on the LPF board (Rev D, sheet 2) is in the bypass path, so this is the firmware choosing not to report.

**Why.** Running barefoot through the bypassed amplifier, with or without the tuner, is a normal way to operate, and from remote the panel is the only meter the operator has. Showing nothing reads as no output. SDRoxide already shows the radio's own power and SWR for any rig that reports them, so the panel uses the same readings.

**Why DRV when the radio gives no watts.** DRV is the amplifier measuring the radio's power at its input. In bypass that power passes straight through to the tuner, so it is the forward power, less a small loss in the relays and filter. Some radios report only a needle position on their own power meter; that is not turned into watts, since it would invent a calibration the radio never gave.

**The ELAD FDM-DUO.** It reports forward power (`FP`), reflected power (`RP`) and SWR (`WR`). ELAD's documents give `FP` and `RP` no unit, so SDRoxide's original author left them out. AJ3G measured them on 18 Oct 2026 with the DUO's TUNE carrier: into a dummy load `FP 006091;RP 000001;WR0 01.02;` at about 6 W, and into an open `FP 008291;RP 007525;WR0 33.06;`. They are milliwatts, and the rig's own SWR is exactly what the two give. Each carries a flag: a space for a valid reading, `!` for one not to trust (receive, below 0.5 W, and while TUNE ramps up and down). Flagged readings are not used. So with a DUO, FWD, RFL and SWR all come from the radio.

**SDRoxide's main TX meter.** It used to show the radio's forward power where the radio had one, and the amplifier's otherwise. With the DUO's `FP` read, that would have shown the DUO's 6 W while the amplifier was putting 500 W on the air. So while the amplifier is in PTT the main meter now shows the amplifier's power and SWR; the radio's are shown otherwise. SDRoxide's SWR protection is unchanged: it still reads the radio's SWR.

**A radio that measures its own watts never borrows DRV.** With the FDM-DUO and its Rig control port, when the DUO flags its forward reading (`!`), FWD and RFL are left empty and the legend says "the rig flagged its reading", rather than showing DRV. AJ3G keyed into an open on 18 Oct 2026: a known-good LP-100A digital vector wattmeter showed the DUO folding back to 2.94 W, while DRV read almost 10 W and the DUO's own screen 8.7 W. DRV over-reads into a mismatch, which is exactly when the DUO stops vouching for its own reading. Into a dead open no bridge here is to be trusted; the 3:1 test is the meaningful one. A radio that never reports watts still gets DRV, as above.

**Limits.** The radio's readings are measured at the radio, so they include the cable to the amplifier and the bypass path as well as the tuner and antenna.

**Where.** `crates/hardrock-panel/src/panel.rs`, `from_rig()`; the DUO's readings in `crates/sdroxide-cat/src/elad.rs`, `parse_milliwatts()`; the main TX meter in `crates/sdroxide-radio/src/engine.rs`, `amp_tx_meter()`. Tests: `in_bypass_fwd_rfl_and_swr_come_from_the_rig`, `forward_and_reflected_power_are_milliwatts`. Tested by `in_bypass_fwd_rfl_and_swr_come_from_the_rig`. User manual section 2.23, "Running barefoot in bypass".

## 3. The FDM-DUO's 1W / 3W / 5W / MAX buttons set menu 34 (SDRoxide only)

**Decided:** AJ3G, 18 Oct 2026.

**What it does.** With an ELAD FDM-DUO that can transmit and has its Rig control (CAT) port set, the TX box shows 1W, 3W, 5W and MAX beside the Drive slider, captioned "ELAD FDM-DUO TX PWR". Each sets the DUO's menu 34 `TX POWER` (`TP02`, `TP06`, `TP08`, `TP09`) at once, and saves it as `elad_tx_power` in the radio settings, so it is sent again at every connect in place of the MAX that was always sent before. Until one is picked it is MAX. Drive and Tune set the power within it, through the audio (decision 4); they never write menu 34.

**Why menu 34 and not Drive.** AJ3G wanted the buttons to set the radio's own power setting. It is a ceiling the radio holds whatever the audio does: at 5 W the DUO cannot exceed about 6 W, which matters for an amplifier with an 8 W drive limit. (A first version had the buttons set Drive instead, built at dev-482c33a and dev-da06ebf. It was replaced before testing.)

**Why MAX is the default.** With the HR500 in line the DUO needs MAX: at 5 W it cannot drive the amplifier to full output on most bands (AJ3G). The power is brought down with Drive. 1W, 3W and 5W are for running barefoot or QRP.

**What the steps give.** Measured with a known-good LP-100A digital vector wattmeter, 20 m, dummy load: 1 W gave 1.44 W on TUNE and 1.41 W on FT8; 3 W gave 3.77 W on TUNE and 3.67 W on FT8 at Drive 100%; 5 W gave 5.9 to 6.1 W; MAX about 8 W. Each step lets a little more out than it says.

**Why beside Drive, not in Settings or the AMP panel.** The FDM-DUO and the FDM-S2 share one settings page, and a power setting there would confuse S2 users (AJ3G). The AMP panel is only there with an amplifier configured, and DUO users without one need it too. Beside Drive it is where power is set anyway, and it is shown only for a DUO with a Rig control port (AJ3G: "only when ELAD TX is enabled in CAT/Rig Control"). Every other radio's TX box is unchanged.

**Where.** `crates/sdroxide-cat/src/elad.rs`, `tx_power_frame()`, `Elad::new` and `set_power_step()` (test `menu_34_is_the_operators_choice_at_connect_and_from_the_buttons`); `Command::SetRigPowerStep`; `CatConfig::elad_tx_power`; the buttons in `crates/sdroxide-ui/src/app/top_bar.rs`, `tx_duo_presets()` and `duo_presets_apply()`, and `crates/sdroxide-ui/src/app/duo_power.rs`. User manual section 2.23, "FDM-DUO power buttons".

## 4. On the FDM-DUO, Drive 100% sends a quarter of full audio (SDRoxide only)

**Decided:** AJ3G, 18 Oct 2026, from AJ3G's measurements.

**What was wrong.** AJ3G measured the DUO on 20 m with a known-good LP-100A digital vector wattmeter into a dummy load, menu 34 at MAX, using SDRoxide's TUNE: 0.94 W at Drive 1%, 2.08 W at 5%, 3.72 W at 10%, 5.39 W at 15%, 6.85 W at 20%, 7.36 W at 22%, 7.73 W at 23%, then 8.0 to 8.3 W everywhere from 24% to 100%. FT8 gave the same (5.26 W at 15%, 7.9 to 8.1 W from 25% up). So three quarters of the Drive slider did nothing, and a preset worked out on a straight line would have put 8 W into the HR500 when 5 W was asked for. The HR500's own DRV reading agreed with the LP-100A.

**What it does now.** For the DUO, Drive and Tune are scaled into the bottom 24% of the old range (`DUO_AUDIO_POWER_CEILING`, applied by `CatHandle::audio_gain_for`). Drive 100% is about 8 W and the output is a straight line from about 0.5 W at the bottom, so every point of the slider does something.

**Why voice is no different.** A voice signal's average is far below its peaks, but its peaks are as loud as the tune tone, and peaks are what the DUO's output stage and the HR500's drive limit see. Digital modes are a steady tone like TUNE, which FT8 confirmed.

**Why 24% holds at any power setting.** With menu 34 at 5 W the knee was in the same place (3.72 W at 15%, 5.9 W at 25%, 6.0 W at 75%), so it is set by the audio into the DUO, not by its power step. Drive therefore spans whatever ceiling menu 34 sets.

**Limits.** Measured on one DUO, on 20 m, at its audio input setting as it is. A DUO set up with a different audio input level would have its knee elsewhere. Not applied on a DUO connected over USB alone with no serial cable: there SDRoxide scales the tune tone itself, and that path has not been measured.

**Where.** `crates/sdroxide-cat/src/elad.rs`, `DUO_AUDIO_POWER_CEILING`; `crates/sdroxide-cat/src/lib.rs`, `audio_gain_for`; User manual section 2.23, "Drive on the FDM-DUO".

## 5. At or past the tune limit, the SWR tile is red and the radio's watts are marked unreliable

**Decided:** AJ3G, 18 Oct 2026, after checklist check 15.

**Deviation.** The HR500 has no SWR indicator, so the panel's SWR tile had no colour, to match. It now turns red when the SWR is at or past SDRoxide's tune limit (twice the SWR limit in Settings → General, 5.0:1 by default).

**Why.** Into an open circuit on 20 m, with the amp in OFF, the DUO reported 7.75 W forward and 6 to 7 W reflected, so the panel showed FWD 8 W and RFL 6–7 W "from the rig". AJ3G's known-good LP-100A digital vector wattmeter read 2.62 W forward and 2.93 W reflected: the DUO was folding back and its own watts read about three times high. The SWR it reported, 22.5:1, was the honest figure. So at a very high SWR the panel points the operator at the SWR, in red, and warns across the top of each meter showing the radio's FWD or RFL, centred in large red letters ("FWD UNRELIABLE AT THIS SWR"), with the reason on hover. AJ3G first saw it as a legend note and asked for it at the top, red and larger, because a legend is too easy to miss. Red is a warning, in line with the rule that yellow and red mean warnings only.

**Limits.** It does not stop anything; SDRoxide's own SWR guard already stops TUNE and transmit at its limits. The threshold is the tune limit so there is one figure for the operator to find in the settings.

**Where.** `crates/hardrock-panel/src/panel.rs`, `swr_past` and `readings_row`; User manual section 2.23, "Readings".

## 6. The band follows the radio only when a radio is connected

**Decided:** AJ3G, 19 Oct 2026, making the panel standalone; the radio link the same day.

**What the panel does.** With no radio connected it never sets the amplifier's band on its own: the band changes when DN or UP is pressed, or when the radio sets it over the amplifier's own CAT or ACC input, and the line along the top says so. With the FDM-DUO connected under SETTINGS, "Amplifier follows the radio's band" (on by default, as in SDRoxide) sends the radio's frequency to the amplifier when the dial moves to another band, between transmissions only (interlock #3).

**Why.** In SDRoxide the band follows the radio's frequency. Standalone, guessing with no radio would switch the low-pass filter relays for no reason; with the radio's own dial to read, following it is what SDRoxide did. It can be turned off for an amplifier that already gets its band over its own CAT or ACC cable.

**Where.** `crates/hardrock-panel/src/main.rs`, `follow_band()` and `band_of()`; the hold in `crates/hardrock/src/link.rs`. Tested by `the_band_comes_from_the_dial`.

## 7. In bypass with no radio connected, FWD shows the amplifier's DRV

**Decided:** AJ3G, 19 Oct 2026.

**What the panel does.** In OFF (bypass) the amplifier reports no FWD, RFL or SWR. With the FDM-DUO connected they come from the radio (decision 10). With no radio to ask, FWD shows the amplifier's DRV reading, marked as DRV in its legend; RFL and SWR stay empty.

**Why.** This is decision 2 for a radio that doesn't report its own watts: in bypass the radio's power goes straight through, and DRV measures it at the amplifier's input. DRV over-reads into a mismatch, so it is a guide, not a wattmeter.

**Where.** `crates/hardrock-panel/src/main.rs`, `fill_from_radio()`, passes empty radio readings to `panel::from_rig()`. Tested by `with_no_radio_bypass_fwd_comes_from_drv`.

## 8. The SWR warning is a setting, 5:1 to start

**Decided:** AJ3G, 19 Oct 2026.

**What the panel does.** Settings has "SWR warning at", 5.0:1 on a first run. At or past it the SWR tile turns red and the meter warns across its top, as decision 5 describes.

**Why.** In SDRoxide the warning is the transmitter's tune limit, which is 5:1 with its default settings. Standalone there is no transmitter setting to borrow, so it is the panel's own, starting where SDRoxide does.

**Where.** `crates/hardrock-panel/src/settings.rs`; the default in `main.rs`, `DEFAULT_SWR_WARN`. Tested by `a_first_run_warns_at_sdroxides_tune_limit`.

## 9. SDRoxide's look is carried over whole

**Decided:** AJ3G, 19 Oct 2026.

**What it is.** `theme.rs`, `chrome.rs`, `layout.rs`, `repaint.rs` and `style.rs` are SDRoxide's own, copied as they stand, with only their imports pointed here. The panel uses part of them; the rest is left in rather than trimmed, and the compiler's unused-code warnings are switched off for those files only.

**Why.** So the panel looks exactly as it does in SDRoxide, and a change made there can be copied across without unpicking a trimmed version. `panel.rs` and `settings.rs` are SDRoxide's AMP window and settings tab, changed only where they reached into the rest of SDRoxide.

## 10. The FDM-DUO's own readings come straight from its CAT port

**Decided:** AJ3G, 19 Oct 2026: "provide all data used for all calculations on FWD, PWR, DRV and SWR but … obtained directly from ELAD SDR DUO instead of SDROxide", on a configurable port, 38400 8N1 by default.

**What the panel does.** Under SETTINGS, Radio: the model (ELAD FDM-DUO), the port and the baud (the four rates of the DUO's menu 70 `CAT BAUD`, always 8N1). The link reads `FP`, `RP` and `WR` (forward and reflected power in milliwatts, and SWR, each with the radio's own trust flag) several times a second, and `FA` and `MD` once a second. In OFF (bypass) these fill FWD, RFL and SWR exactly as SDRoxide filled them (decision 2): only while the radio is transmitting, only readings it trusts, and a flagged reading is shown as flagged rather than replaced by DRV. DRV, VDD, IDD and the amplifier's own FWD, RFL and SWR in PTT are the amplifier's, as before, and input power, efficiency and gain are worked out from them unchanged.

**How "transmitting" is told.** SDRoxide knew because it keyed the radio itself. Here the radio is keyed on the radio, so the panel takes it as transmitting when the DUO trusts its forward power, or when the amplifier shows the radio's drive (0.1 W or more). The second covers the DUO below 0.5 W and in a fold-back, when it flags its reading.

**Where.** `crates/rigcat` (the link and the DUO's commands), `crates/hardrock-panel/src/main.rs`, `fill_from_radio()`. Tested by `with_the_duo_bypass_readings_are_the_radios_own`, `a_flagged_duo_reading_is_never_replaced_by_drv`, `in_receive_the_duo_fills_nothing` and `in_line_the_amplifiers_own_readings_stand`.

## 11. The radio link reads the radio's settings (amended by 15)

**Decided:** AJ3G, 19 Oct 2026 ("a way to look at read all relevant menu settings").

**What the panel does.** The RADIO window shows the radio's live readings and every setting it answers a READ for, with its menu number where it is one of the radio's menus (31 `ANTENNAS`, 32 `TX IN`, 34 `TX POWER`, and 70 `CAT BAUD` from the rate the port is open at), the meaning in words and the radio's answer as it came. Settings are read when the port opens, when READ AGAIN is pressed, and every 10 seconds between transmissions. Nothing but menu 34 is ever written to the radio (decision 15, interlock #5).

**Why.** In SDRoxide the DUO's settings were SDRoxide's to set (decision 3); standalone the radio belongs to the operator, and a panel that changed it could leave it changed. Reading is all that was asked.

**The limit.** Only the settings with a READ command in the command table SDRoxide's ELAD driver was written from (FDM-DUO manual v2.6 §6) are listed. Menus with no CAT command, such as 37 `CW IN` and 45 keyer speed, can only be read on the radio.

**Where.** `crates/rigcat/src/elad.rs`, `SETTINGS` and `describe()`; `crates/hardrock-panel/src/radio.rs`.

## 12. A port another program holds says so

**Decided:** AJ3G's bug report, 19 Oct 2026: with the HR500 on COM5 he had to unplug the lead and reinstall before the panel connected.

**What the panel does.** When a port won't open but the system still lists it, the status says it is in use by another program and names the likely ones; it reconnects by itself every two seconds once the port is free. "Not found" is kept for a port that has gone.

**Why.** The serial library reports Windows' "Access is denied", which is what another program holding the port gives, the same way as a port that isn't there, so the panel said "COM5 not found. Check the amplifier's USB lead." Unplugging the lead made the other program let go, which is why that worked. AJ3G confirmed it the same day: SDRoxide's AMP link was holding COM5.

**Where.** `open_error()` in `crates/hardrock/src/link.rs` and `crates/rigcat/src/link.rs`.

## 13. When the radio's cable holds the band, the panel says so; it cannot turn the cable off

**Asked:** AJ3G, 19 Oct 2026: the HR500 and FDM-DUO are linked by a cable (the DUO's EXT IO to the amplifier's ACC), and the amplifier follows the DUO's band whatever the panel's follow setting says. He asked for a way to turn that off from the panel.

**What can't be done.** The HR500 takes its band from that cable because its Transceiver menu is set to ELAD FDM DUO; with a radio set there it reads the band several times a second and puts back any other (operating manual, `HRBN`). The menu is stored in the amplifier and has no serial command: the V3.4 firmware's whole command table is `FA`, `IF`, `HRBN`, `HRMD`, `HRAN`, `HRTS`, `HRVT`, `HRAP`, `HRTB`, `HRTU`, `HRTR`, `HRTT`, `HRTP`, `HRPW`, `HRST`, `HRAA`, `HRFW` and `HRTM` (checked in the firmware image, 19 Oct 2026). So the panel cannot switch it.

**What the panel does.** When DN or UP is pressed with no RF present and the amplifier is back on its old band 1.5 s later, the line along the top turns yellow: "Band held by the radio's cable. To switch it here, set the HR500's Transceiver menu to None." Hovering explains why. It clears once a press holds.

**The way to have the choice.** Set the HR500's Transceiver menu to None (MENU, Transceiver, None), and connect the FDM-DUO under SETTINGS, Radio. The panel then does what the cable did, from the radio's dial, between transmissions only (interlock #3), and "Amplifier follows the radio's band" turns it on and off. DN and UP then hold whenever it is off. A switch in the cable would also do it, in hardware.

**Where.** `crates/hardrock-panel/src/main.rs`, `watch_band()`; `tools/fake_hr500.py --held` puts the band back as the cable does.

## 14. Each radio is a protocol module

**Decided:** AJ3G, 19 Oct 2026: "write the code so in the future we can add other Rigs such as Kenwood, Yaesu, Icom".

**What it is.** `crates/rigcat/src/protocol.rs` is what every radio's dialect provides: its reads for power, SWR, dial and settings, how its answers are split and understood, its power steps, its TUNE carrier and unkey, and the list of frames it may ever send (interlock #5). Frames are bytes, so Icom's binary CI-V fits as well as the `;`-terminated ASCII dialects. The link, the panel and the RADIO window work only through it. The FDM-DUO is `elad.rs`. Another radio is one more module and one more `RigModel`, and appears in SETTINGS, Radio, Model.

## 15. RIG TUNE keys the radio's own carrier at menu 34's power (amended by 17 and 18)

**Decided:** AJ3G, 19 Oct 2026: "a tune button with slider ... so I can tune and evaluate what the amp is doing", and "expose MENU 34 ... next to the slider".

**What the panel does.** With a radio connected, a strip under the top bar (and in the radio's window) has RIG TUNE, a slider for the radio's power setting (menu 34 `TX POWER` on the FDM-DUO: 0.3, 0.5, 1, 1.2, 1.5, 2, 3, 4, 5 W or MAX), and how long TUNE may run. RIG TUNE keys the radio's own TUNE carrier (`TX2;`) at that power; click again to unkey. Menu 34 is written to the radio when the slider is let go, and stays set after the panel closes, as on the radio's own menu. Interlock #6 says when it may key and when it unkeys.

**Why stepped, and the same slider.** Over CAT, menu 34 is the FDM-DUO's only power control. SDRoxide's Drive slider was smooth because SDRoxide played a tone into the DUO's USB audio, with menu 34 as the ceiling; the panel has no audio path, so the slider is menu 34. An audio drive could be added on top later (AJ3G was asked).

**Called RIG TUNE** so it isn't confused with the tuner's TUNE, which starts the ATU-500's tuning cycle.

**Not yet checked on the radio:** the power the DUO's `TX2;` carrier puts out at each menu 34 step. FWD shows what it really is, from the radio's own `FP`.

## 16. The radio's window pops out

**Decided:** AJ3G, 19 Oct 2026: "a pop out that contains a GUI for the ELAD when selected"; later radios will appear in it when chosen.

**What the panel does.** RADIO opens the radio's window over the panel; POP OUT moves it into a window of its own, and DOCK brings it back. Whether it is popped out is kept between runs. It shows the dial large, the live power and SWR, RIG TUNE and the power slider, and the settings table. Its title and contents come from the radio chosen in SETTINGS, so another radio's window takes its place when one is added.

**Where.** `crates/hardrock-panel/src/radio.rs`.

## 17. TX TUNE keys the radio as its PTT does, with the panel's tone

**Decided:** AJ3G, 19 Oct 2026: "Use the example of the SDRoxide buttons and sliders we used for power control 1, 3, 5, and Max, allow slider to control gain inside each of those menu settings. Add button for this. Also need to activate PTT, which at the moment is not working between ELAD and HR500".

**What the panel does.** TX TUNE plays a steady 1 kHz tone into the radio's USB audio and keys an ordinary transmission (`TX;`), as SDRoxide's TUNE did. On USB or LSB that is a single carrier. The Tune slider sets the tone's level, scaled into the DUO's audio ceiling (`AUDIO_CEILING`, 0.24 of full audio as a power fraction, from decision 4), so it spans the power button's setting in a straight line; the panel shows what that should come to in watts. It can be moved while keyed. The tone plays on the sound output chosen under SETTINGS, Radio, TUNE sound; by default the one whose name has FDM-DUO in it. RIG TUNE is kept beside it for the DUO's own carrier. Interlocks #6 and #7.

**Why this fixes PTT, as far as can be told here.** The HR500 keys only from its PTT input; it has no serial command to key it. With SDRoxide, which keyed the DUO with `TX;`, AJ3G saw the HR500 key ("PTT is responding correctly"). RIG TUNE keys `TX2;`, the DUO's own TUNE, and with it the HR500 did not key. So the DUO probably does not drive its PTT output for its own TUNE. This is inferred: ELAD's manual has not been checked for it. If TX TUNE doesn't key the amplifier either, the cause is the cable or the DUO's PTT output setting, not the keying command.

**Why the tone is the panel's own.** SDRoxide is not running alongside: the panel has the radio's CAT port. The tone needs only the radio's USB audio, which any program can play into.

## 18. The power buttons replace the menu 34 slider

**Decided:** AJ3G, 19 Oct 2026 (as 17).

**What the panel does.** 1W, 3W, 5W and MAX set menu 34 (`TP02`, `TP06`, `TP08`, `TP09`), captioned ELAD FDM-DUO TX PWR, as in SDRoxide (decision 3). The lit one is the radio's setting as last read. A setting the radio is on that has no button (0.3 W, for example) is named in the caption. The Tune slider sets TX TUNE's power within it. The buttons write any setting, MAX included, in line or not; interlock #6 has the change.

## 19. The panel says when the radio transmits and the amplifier doesn't key

**Decided:** AJ3G, 19 Oct 2026 (as 17).

**What the panel does.** When the radio has been transmitting for 1.5 s (its own FWD, or a TUNE the panel keyed), the amplifier is in PTT and its own TX light (from `HRST`) is off, the line along the top says, in yellow, that the radio's PTT line isn't reaching the amplifier, and after RIG TUNE suggests TX TUNE. The hover says where to look: the HR500's PTT input (the RCA jack, or pin 4 of ACC), the cable from the radio, and the radio's PTT output setting. On firmware that sends no lights it says nothing.

**Where.** `crates/hardrock-panel/src/main.rs`, `ptt_missing()`; the light in `crates/hardrock/src/link.rs` as `AmpStatus::transmitting`.

## 20. Every CAT command the DUO documents is mapped to a control, or listed as never sent

**Decided:** AJ3G, 20 Oct 2026: "map all features from the ICD into the buttons and menus I have asked for and then build".

**What the panel does.** `rigcat`'s `CONTROLS` table (in `elad.rs`) lists each settable or readable DUO command with its menu number, its group (radio face, RX, CW, TX audio, TUNE, MENU, info) and the values it takes. The radio window builds its MODE and BAND pop-outs, the RX, CW, TX audio and MEM pop-outs, and the MENU window from that table, so a new command is one row. Sets go through interlock #5 and are read back. DF, SE, UU, a BR set and IQ are never sent, and the Kenwood dummies are left out because they do nothing on the DUO.

**Where.** `crates/rigcat/src/elad.rs` (`CONTROLS`, `allowed_set`), `crates/rigcat/src/protocol.rs` (`Control`, `Kind`, `Group`), `crates/rigcat/src/link.rs` (`Msg::Set`).

## 21. Unknown encodings are shown, not set

**Decided:** by the panel's author, 20 Oct 2026, under decision 20.

**What the panel does.** ELAD's manual does not give the value format for QS, RN, SA, SI, TH, TM, TQ, TR, TV, UD, WT, or RF's FM codes. These show the radio's raw answer and cannot be changed from the panel until a bench test confirms the format. A wrong guess could leave the radio in a setting the operator didn't choose.

## 22. Wide spans work as SDR Console does: a fixed centre, the panel's own receiver, and split for transmit

**Decided:** AJ3G, 19 to 20 Oct 2026: "I want the control (what happens when you click on a signal) more like that [SDR Console]"; "I have the sound card on the computer so we are good."

**What the panel does.** The DUO's IQ window is centred on VFO A (`FA`) and stays put; clicking or dragging a signal moves the panel's own receiver inside the span, and the centre moves only when the receiver nears an edge. Above 192 kHz the DUO mutes its own audio, so the panel demodulates and plays the receiver to the sound output chosen in SETTINGS (VB-Cable can be chosen, for decoders). To transmit where the panel listens, the panel sets split (`SP1`) and `FB` to the receiver's frequency. Still to check on the bench: that the IQ window stays on VFO A while split.

**Where.** `crates/elad-iq` (USB IQ, from SDRoxide's `sdroxide-elad`), `crates/sdr` (spectrum and receiver), and the scope in `crates/hardrock-panel`.

## 23. The radio window is the radio's own face, themed per radio

**Decided:** AJ3G, 20 Oct 2026, from the ELAD look mockup: "In the future when we build out for other rigs such as Yaesu, Kenwood, Icom we will use a similar format but the rig GUI will change based on the rig chosen to match its theme loosly". Later the same day: the DUO is used with up and down converters, so the window takes a transverter offset.

**What the panel does.** The radio window is drawn as the FDM-DUO's front panel: a segment LCD whose backlight follows receive and transmit (picked under MENU, as the DUO's own LB and menu 73), with the dial as clickable seven-segment digits, annunciators, a segment S-meter and a text line; key caps for MODE, BAND, F INPUT, RX, CW, TX AUDIO, MEM and MENU; the VFO and RIT keys; the TUNE strip; and the FWD, RFL and SWR readouts. Every setting is built from the protocol's `Control` table by its kind, so another radio's window is the same layout with its own settings. Its colours, key lettering and which settings go in which pop-out are a `RigTheme` per radio. Each control shows the radio's read-back and greys while it transmits (interlock #5). Data modes set the mode, frequency and filter, and ask before setting TX IN to USB audio. BAND remembers the last frequency and mode the radio read back on each band. A=B is VFO B set to VFO A's frequency, since the DUO's `VE` format isn't known.

**Transverter.** A signed offset in Hz, 0 by default, kept by the panel and never sent to the radio. The LCD shows the radio's frequency plus the offset, with XVTR lit; typed entry takes the RF frequency (trying MHz if kHz lands outside the radio) and sends the radio the IF; the 9 kHz to 54 MHz limit applies to the IF; BAND offers 160 to 6 m with no offset, and with one the bands it brings inside the radio's range. Not yet done: the amplifier's band still follows the radio's IF (decision 13's follow), which with a converter is the wrong band; the integrator is to stop the follow while an offset is set.

**Kept between runs** in egui's memory (saved by eframe with the window's other state): band memories, backlights, the transverter offset and the CW messages as last stored from the panel (the DUO can't read them back).

**Where.** `crates/hardrock-panel/src/radio.rs` and `radio/` (`logic.rs`, `face.rs`, `controls.rs`, `rigtheme.rs`).

## 24. Beside an ELAD radio the AMP window is drawn as the DUO-ART 120's screen

**Decided:** AJ3G, 20 Oct 2026, with the ELAD look mockup (the amplifier half), following decision 23: the station's windows take the look of the radio chosen. AJ3G's condition from earlier feedback: nothing the window shows or does may be dropped, the dual meters, the tiles, PERFORMANCE and the tuner drawing included, and no interlock may change.

**What the panel does.** With the FDM-DUO chosen under SETTINGS, the AMP window is drawn as ELAD's DUO-ART 120 amplifier: the panel's TFT behind a black bezel in a black case, banded as the DUO-ART's screen is. A title bar (the amplifier, its link, its mode, the meters' power range); each meter's TOP and BOTTOM pickers; the two meters as white analog-style faces, the top reading on the outer scale with a dark needle and the bottom one on the inner scale in blue, the yellow and red stretches on the arcs, and each needle's name, scale note, source and reading under it; the seven readings, each with a live bar along its scale; PERFORMANCE (the numbers hold peaks, the bars are live, the held peak a tick); the tuner's network; a status line in blue (mode, band, antenna, where the band comes from, the tuner); a diagnostic line in green (ready or keyed, temperature, supply); and a message line, warnings on the left in yellow or red and the tuner's state on the right. PTT, ANT, DN, UP, TUNE and ATU ON/BYP are the six soft keys under the screen, each with its label above it, lit in its state's colour, and faded with the reason on hover while it can't be pressed. ELAD shows a selected setting in red; here it is blue or green, since yellow and red only warn. With no radio chosen the panel keeps its own look, unchanged.

**How.** The look is an `AmpTheme` (`panel/amptheme.rs`), picked by `amp_theme()` from the radio settings, as `RigTheme` is for the radio window; the screen is drawn in `panel/tft.rs`. What both looks show is worked out once: the readings and needles (`Shown`), the tiles (`tiles()`), the keys with whether they can be pressed and why not (`keys()`, interlocks #2 and #4), the link's state and the tuner's. Each look only draws them.

**Where.** `crates/hardrock-panel/src/panel.rs`, `panel/amptheme.rs`, `panel/tft.rs`; `main.rs` passes the radio settings to the window (`AmpMeters::set_radio`).

## 25. Outside programs connect by TCI and rigctld, and receive decodes over WSJT-X UDP

**Decided:** AJ3G, 20 Oct 2026, through the coordinator: he may use third-party decoders over TCP/IP or TCI, so the panel keeps TCI and rigctld open for them beside its own decoders.

**What the panel does.** `crates/links` has a TCI server (websocket, port 50001), a Hamlib rigctld server (port 4532) and a WSJT-X UDP sender (port 2237), all bound to 127.0.0.1 by default. Both servers are driven by one `Rig` trait the panel implements: frequency, mode, split and VFO from the panel's state, I/Q and receive audio streamed to TCI clients that ask for them, and transmit audio paced by TCI's chrono messages. Transmitting is off unless the operator allows it, and even then every request goes to the panel, which can refuse it (interlock #8). The transmit power cannot be set from outside. The panel's own WSJT decodes go out as WSJT-X Decode and WSPR Decode messages, so GridTracker, JTAlert and loggers see them as they would see WSJT-X.

**Where.** `crates/links` (`tci`, `rigctld`, `wsjtx`); the panel's `Rig` implementation is `crates/hardrock-panel/src/outside.rs` (decision 34).

## 26. WSJT slot timing comes from the system clock, the radio frequency is exact, and each decode's DT is shown

**Decided:** AJ3G, 20 Oct 2026, through the coordinator: his PC clock is disciplined by NTP with a GPS reference as fallback, and the same GPS reference clocks the DUO.

**What the panel does.** `decoders::slot` puts each block of audio where its UTC time stamp says (the system clock at the first sample, less the receive chain's known delay), so a gap leaves silence instead of shifting the slot. No time offset is learnt from the decodes, but each decode's DT is reported as measured, so a clock that drifted would show there. A slot joined more than 1 s late is not decoded. The radio's frequency is taken as exact: `FreqCal` holds a ppm correction that is 0 by default and is kept for a radio on its own crystal. All decode times are UTC from the system clock. The WSJT modes are mfsk-core's (FT8, FT4, WSPR, JT65, JT9, FST4, Q65) at the settings SDRoxide measured, at 12 kHz from the panel's 48 kHz audio.

**Where.** `crates/decoders` (`slot`, `wsjt`, `worker`, `freq`, `resample`).

## 27. Aviation and maritime data links are xng's decoders, at true RF behind the converter

**Decided:** AJ3G, 20 Oct 2026, through the coordinator: HFDL and SDRoxide's decoder set built in; and since the DUO is used with up and down converters, with a high-stability clock and GPS time, VHF ACARS, VDL Mode 2, AIS and Mode S as well, every I/Q decoder taking the converter's LO so it reports true RF.

**What the panel does.** `crates/datalink` wraps airframesio/xng's decode cores (MIT OR Apache-2.0, vendored with their licences under `vendor/xng` as their own workspace, so the panel's fmt and clippy leave upstream's code alone). One `LinkDecoder` per link splits every wanted channel out of the DUO's I/Q at once (xng's polyphase channelizer, or a shared decimation where its bins don't suit the rate) and decodes the channels in parallel on a worker thread. `Tuning` gives the I/Q centre, the converter LO and the ppm correction; channels are asked for, and reported, at true RF. HFDL takes a 24 kS/s lane or a wide span. Its ground-station list is built in from EiBi's schedule and is replaced by the network's own system table once it is heard. Mode S needs at least 2 MS/s, so it works at the DUO's 3.072 and 6.144 MHz rates and not below. Each message carries xng's full JSON model as well as a one-line summary.

**Where.** `crates/datalink`, `vendor/xng` (see its `VENDORED.md`).

## 28. What the decoders leave out for now

**Decided:** by the panel's author, 20 Oct 2026, under decisions 25 to 27.

**What the panel does.** SDRoxide's CW decoder (deepcw) is AGPL, so it is left out until AJ3G decides. JS8 and SDRoxide's other decoders are not ported. Of SDRoxide's keyboard and picture modems, RTTY, PSK31, NAVTEX, SSTV and WEFAX are wired to workers; Olivia and THOR are wired too, and Hell is ported but not yet wired to a worker. SSTV and WEFAX pictures come out as RGBA buffers, row by row and whole.

**Where.** `crates/decoders` (`modem`, `image`, `worker`).

## 29. The scope keeps levels, not colours, on the GPU

**Decided:** AJ3G, 20 Oct 2026: "use the GPU" (an RTX 5070 in a Framework 16), and "spare no expense on the scope".

**What the panel does.** The waterfall and the 3D FFT are `egui_wgpu` paint callbacks drawing into egui's own render pass. Each new spectrum row is written once into a ring texture as dBm in half floats, with a second texture holding the band that row covers. The shader turns levels into colours per pixel, takes the strongest of the bins under each pixel, and places each row by its own band. A change of floor, ceiling, palette, contrast, zoom, pan or centre is then only a uniform change, with no CPU recolouring and no re-upload; 16 384 columns at 60 rows a second is about 2 MB/s of upload. The 3D surface (SDRoxide's look: 48 rows deep, PERSP 0.85, FOG 0.55, solid or lines) is computed in the vertex shader from a 64-row texture of the same kind. eframe asks for the high-performance adapter and for textures as wide as the adapter allows, up to 16 384. The phosphor trace is the one view built on the CPU, as an egui texture of hit counts. Without a wgpu renderer, the waterfall and 3D panes show a note instead.

**Where.** `crates/hardrock-panel/src/scope/gpu.rs`, `rows.rs`, `shaders/`; `main.rs` (`wgpu_options`).

## 30. The scope shows the antenna frequency and returns the radio's

**Decided:** AJ3G, 20 Oct 2026: the DUO is used with up and down converters, and runs from a GPS-disciplined reference.

**What the panel does.** The scope has a transverter LO (signed, in Hz, 0 to start) and a ppm calibration (0 to start). Everything it shows is the radio's frequency × (1 + ppm·10⁻⁶) + LO: the axis, the markers, the readouts and the band plan lookup. Everything it returns (`tune_to`, `recentre`) is in the radio's own frequency. When either setting is not zero, the top bar shows XVTR and/or CAL in the accent colour. Marker and peak readouts go to 0.1 Hz, from a parabola fitted in dB across the peak bin and its neighbours. The waterfall's UTC ticks use the system clock as it is.

**Where.** `crates/hardrock-panel/src/scope/view.rs` (`FreqMap`), `markers.rs`, `paint.rs` (`top_bar`); `crates/sdr/src/spectrum.rs` (`interpolate_peak`).

## 31. The spectrum has its own thread, Welch overlap and a zoom FFT

**Decided:** AJ3G, 20 Oct 2026: FFTs up to at least a million points with overlap and a choice of window, and a zoom FFT that resolves the zoomed region.

**What the panel does.** In `sdr`, the spectrum runs on its own thread, fed through a bounded queue. If it falls behind, blocks are dropped and counted, and the receiver's audio is never held up. FFT sizes run from 4k to 1M (2²⁰). The windows are Hann, Blackman-Harris, rectangular and flat-top, each with its ENBW, so the RBW shown is true. Overlap can be off, 50 % or 75 %, with the segments' powers averaged (Welch). Each frame carries a noise floor (the median bin, in dBm per bin), which the scope draws as a dashed line and uses for the receiver's SNR. When the view is zoomed past what the main FFT resolves, the scope asks for a zoom FFT on the view. That zoom mixes the view's centre to 0 Hz, decimates by a power of two through half-band filters, and runs its own FFT. The scope uses the zoom's frames while they cover the view, and the main FFT's frames otherwise. The pipeline keeps 16 frames of history, since million-point frames are large.

**Where.** `crates/sdr/src/pipeline.rs`, `spectrum.rs`, `window.rs`, `zoom.rs`; `crates/hardrock-panel/src/scope/mod.rs` (`sync_pipeline`, `zoom_request`).

## 32. The scope runs the panel's own receiver from the ELAD, or from a demo band

**Decided:** by the integrator, 20 Oct 2026, under decisions 22 and 29 to 31, wiring the scope, `sdr`, `elad-iq` and the decoders into the panel.

**What the panel does.** SCOPE, on the top bar beside RADIO, opens the scope over the panel or popped out into its own window, as the radio's window does. SETTINGS, *Scope and receiver*, picks where the I/Q comes from: off (the default), the ELAD over USB, or *Demo signals*, `sdr`'s synthetic band around 14.2 MHz, so the scope and the decoders can be seen with no radio (`--demo` on the command line does the same, `--elad` opens the ELAD). One `sdr::Pipeline` runs the spectrum and the panel's receiver; the scope's SPAN key restarts it at the new I/Q rate (on the FDM-DUO that rate says how the stream is read; the radio's own rate is set on the radio). The ELAD is opened on a thread of its own, since the driver can take seconds, and its error is shown as the driver words it (the WinUSB/Zadig note on Windows) with TRY AGAIN.

**The centre.** The I/Q centre is the DUO's VFO A. The driver tunes the radio when it opens the device, so the panel waits up to 10 s for the CAT link to read VFO A and opens there; with no CAT link it opens at the centre it last used. The radio's dial is read back over USB (`follow_radio_dial`), so turning the knob moves the span, and the receiver moves with it, keeping its offset. When the scope asks for a new centre the panel sets VFO A over CAT (`FA`, interlock #5) and the span follows the read-back; with no CAT link it moves the centre over USB. Neither happens while transmitting (interlocks #9 and #10). A receiver saved outside the span (another band last time) starts on the dial rather than asking for the radio to move.

**The receiver.** It keeps its frequency, mode and passband between runs, plays on the sound output chosen under SETTINGS (the system's default to start, VB-Cable or any other by name), with a volume and a mute on the scope. The DUO plays its own audio up to 192 kHz and mutes it above (decision 22); up to 192 kHz the panel also sets the DUO's mode and receive filter (`MD`, `RF`) to the receiver's, since the DUO's audio is the one heard there. The receiver keeps running while the scope is closed, so DECODE can listen. Its I/Q and audio are copied out of the pipeline by taps (`sdr::Command::SetIqTap`, `SetAudioTap`), each block stamped in UTC from the system clock, to the decoders and to TCI.

**Transverter.** The radio window's offset and the scope's transverter LO are kept the same, whichever was changed last.

**Where.** `crates/hardrock-panel/src/rx.rs`, `scopewin.rs`, `main.rs` (`run_receiver`, `scope_window`, `sync_xvtr`); `crates/sdr/src/pipeline.rs` (the taps).

## 33. Transmit where you listen is a toggle on the scope, on by default

**Decided:** by the integrator, 20 Oct 2026, carrying out decision 22.

**What the panel does.** TX FOLLOWS RX on the scope, on to start. While the receiver is off VFO A, the panel sets `FB` to the receiver and then `SP1`; once they coincide again it sends `SP0`. Each change goes through the CAT link's `set`, which reads it back and refuses it while the radio transmits (interlock #5), and the panel doesn't ask while transmitting either. A change is sent once the receiver has stayed put for 0.3 s, so a drag doesn't send a frame a frame, and asked again only if the radio hasn't read it back after 1.5 s. Turning the toggle off takes out a split the panel made, and leaves the operator's own split alone. It works only with the ELAD's I/Q and the CAT link both connected.

**Still to check on the bench:** that the DUO's I/Q window stays on VFO A while it is in split (decision 22). If it follows VFO B instead, the scope would jump to the receiver on every move, and this has to change.

**Where.** `crates/hardrock-panel/src/rx.rs` (`SplitState`), `scopewin.rs`.

## 34. Outside programs see the panel's receiver as the radio, and key it through TUNE's checks

**Decided:** by the integrator, 20 Oct 2026, under decision 25 and interlock #8. AJ3G's condition, through the coordinator: TCI and rigctld off by default, transmitting from outside off by default, and a program's request to transmit checked as TX TUNE is.

**What the panel does.** SETTINGS, *Outside programs*: a TCI server (port 50001), a rigctld server (port 4532) and the WSJT-X UDP decodes (127.0.0.1:2237), each off until ticked, and *Allow outside programs to transmit*, off. All listen on this computer only. With the panel's receiver running, a program sees one VFO, the receiver: VFO A is the receiver's frequency, TCI's `dds` the I/Q centre, there is no split (the panel transmits where it listens, decision 33), and setting VFO A tunes the receiver, moving VFO A (the centre) only if the frequency is outside the span. Without the receiver, VFO A, B and split are the radio's own over CAT. Every change is made by the panel's loop as its own controls make it, and none while transmitting. The receiver's audio and, up to 384 kHz, its I/Q stream to TCI clients that ask.

**Transmit.** A program's request is granted only when the checks the panel worked out in its last frame pass: `tune_blocked` (interlock #6) at the radio's full power setting, since the program's audio level can bring the power that high, and nothing else keying the radio. The panel then keys through the CAT link's TUNE path: an ordinary transmission (`TX;`, after the link re-reads menu 32 and the mode, #7) for `trx` and `T 1`, the radio's carrier (`TX2;`) for `tune` and `G TUNE`. The program's audio reaches the radio as it would without the panel, on the radio's USB audio; TCI's own transmit audio stream is not taken (a TCI client must send its audio to the radio's sound card). While keyed, the panel watches it as it watches TUNE, ten times a second, and the link's own watchdog unkeys if the panel stops; a grant lasts a minute at most (TUNE's limit), which cuts a two-minute WSPR transmission short. Letting go, disconnecting, the operator's STOP, or turning the permission off all unkey.

**Where.** `crates/hardrock-panel/src/outside.rs`, `tune.rs` (`outside_ptt_blocked`), `main.rs` (`keep_tune`, `outside_req`).

## 35. DECODE runs one decoder at a time, chosen in its window

**Decided:** by the integrator, 20 Oct 2026, under decisions 26 to 28.

**What the panel does.** DECODE, on the top bar, opens a window with every mode in four rows: WSJT (FT8, FT4, WSPR, JT65, JT9, FST4-60, Q65-30A), keyboard (RTTY 45.45 Bd 170 Hz, PSK31, NAVTEX, Olivia 8/500, THOR16), pictures (SSTV, WEFAX) and from the I/Q (HFDL, ACARS, VDL2, AIS, Mode S). RUN starts the chosen one; it keeps running when the window is closed, until STOP. The audio modes take the receiver's 48 kHz audio, the data links the I/Q. The list shows each decode's UTC, SNR, DT, audio frequency and RF frequency (the receiver's dial plus the audio, corrected by the scope's ppm, plus the transverter LO), newest first, and the line above it how many blocks the decoder dropped and how long the last slot took. Keyboard modes show running text with their audio frequency to set; pictures build up as they arrive, WEFAX with its LPM and IOC and a manual start and stop. Data links take the radio window's transverter offset as their LO, so they report true RF, and say when the span doesn't hold their channels. WSJT decodes also go out as WSJT-X UDP Decode, WSPR Decode and Status messages when that is on under SETTINGS. Only one sub-mode of FST4 and Q65 is offered for now.

**Where.** `crates/hardrock-panel/src/decode.rs`.

## 36. The scope's SCF view shows cyclic features, on the GPU, from a narrow lane by default

**Decided:** AJ3G, 20 Oct 2026, asked for a real-time cyclostationary feature detector by SSCA or FAM in WGSL. The narrow-lane default and N′ = 1024 come from the measurements below: N′ = 64 could not see FT8 or CW at all.

**What the panel does.** The scope's top view has a fourth choice, *SCF*, beside Spectrum, 3D FFT and Both. It shows the spectral coherence (or the SCF itself) over frequency f and cycle frequency α, by the strip spectral correlation analyser (SSCA), as a terrain in the 3D view's perspective and palette or as a heat map, with axes in Hz and a readout of the strongest cycle frequencies (5σ over the median α row, each cell judged against its own f column), marked on the plane and labelled with a known mode whose symbol rate matches ("~FT8 rate": a rate, not an identification; nothing is reported before 8 blocks, or all of them if fewer are averaged). x·x* finds symbol and keying rates (α = k·Rs, with |α| < 3Δα left out because it is the PSD); x·x finds carriers (α = 2fc). It shows what is there and its parameters; it decodes nothing.

**Lanes.** *Narrow* (the default) down-converts 3, 6, 12 or 24 kHz around the receiver's passband on the ingestion thread (an f64 NCO and decimate-by-2 FIR stages); *Whole span* runs on the I/Q as it comes, for wideband data with kHz symbol rates. Defaults: 3 kS/s, N′ = 1024 (Δf 2.9 Hz, Kaiser β = 10 channel taper), N = 4096 (Δα 0.73 Hz, a block every 1.37 s), 16 blocks averaged, α to 50 Hz.

**Why N′ = 1024.** Δf has to be well under the signal's bandwidth. A channel wider than the signal averages S^α(f) over all of it, which leaves R^α(0), the cyclic part of |x|²: zero for constant-envelope FT8 and nearly zero for rectangular keying. Measured (`scf_sensitivity`, 9 blocks / 12 s, 5σ, SNR in 2.5 kHz): at N′ = 64 (47 Hz) FT8-like 8-GFSK and 20 WPM CW were not found even at 0 dB, and BPSK31 was found at −4 dB; at N′ = 1024 (as shipped, lavapipe):

| Signal | 12 s (9 blocks) | 60 s (44 blocks) |
|---|---|---|
| BPSK 31.25 Bd, rectangular | −12 dB | −18 dB |
| CW 20 WPM, random keying | −10 dB | −14 dB |
| FT8-like 8-GFSK, 6.25 Bd | 0 dB (4.8σ) | −2 dB |
 FT8 is only found well above WSJT-X's −21 dB decode limit, because its GFSK has weak cyclic features. The SCF is for finding and measuring what is there, not for weak-signal FT8.

**Threads and GPU.** The pipeline's I/Q tap copies each block into a bounded queue (dropping when it is full) only while the view is on screen. The ingestion thread cuts blocks of N + N′ samples. The GPU thread uploads each block with one `write_buffer` and runs six compute passes: channelizer, channel power, strip FFTs with the product, averaging, the (f, α) grid, and the column floor with the α profile. FFTs are radix-4 in workgroup memory, with a four-step FFT past 2048 points. Two blocks are in flight, and only the profile (a few KB) is read back. The UI draws the grid buffer straight from VRAM with its own vertex shader. With no compute shaders or vertex storage buffers, the view says so and does nothing. On lavapipe (CPU) a default block takes about 140 ms; checked against an f64 CPU SSCA to 3e-7 of the peak.

**Where.** `crates/hardrock-panel/src/scope/scf/` (`engine.rs` and `shaders/ssca.wgsl` the GPU SSCA, `reference.rs` the CPU one, `lane.rs` the DDC, `features.rs` the readout, `draw.rs` and `shaders/scf_view.wgsl` the drawing, `ui.rs` the view), `scope/mod.rs`, `scopewin.rs`.
