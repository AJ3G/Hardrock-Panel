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

**What the panel does.** With no radio connected it never sets the amplifier's band on its own: the band changes when DN or UP is pressed, or when the radio sets it over the amplifier's own CAT or ACC input, and the line along the top says so. With the FDM-DUO connected under SETTINGS, Amplifier band "From this program, over the radio's CAT" (the default, as in SDRoxide) sends the radio's frequency to the amplifier when the dial moves to another band, between transmissions only (interlock #3).

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

**What the panel does.** When DN or UP is pressed with no RF present and the amplifier is back on its old band 1.5 s later, the line along the top turns yellow: "Band held by the radio's cable. To switch it here, set the HR500's Transceiver menu to None." Hovering explains why. It clears once a press holds. DISMISS beside it hides it until then (AJ3G, 25 Oct 2026), and it comes back if a later press is undone again.

**The way to have the choice.** Set the HR500's Transceiver menu to None (MENU, Transceiver, None), and connect the FDM-DUO under SETTINGS, Radio. The panel then does what the cable did, from the radio's dial, between transmissions only (interlock #3), when SETTINGS, Radio, Amplifier band is "From this program, over the radio's CAT"; "From the radio's cable to the amplifier" sends nothing, and DN and UP then hold. AJ3G (25 Oct 2026) asked for the choice to be spelled out as one, so the panel can stand in as a backup when the interface cable fails; the line under it names the Transceiver menu setting each needs. The setting is still `follow_band` in the saved file. A switch in the cable would also do it, in hardware.

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

**What the panel does.** DECODE, on the top bar, opens a window with every mode in four rows: WSJT (FT8, FT4, WSPR, JT65, JT9, FST4-60, Q65-30A), keyboard (RTTY 45.45 Bd 170 Hz, PSK31, NAVTEX, Olivia 8/500, THOR16), pictures (SSTV, WEFAX) and from the I/Q (HFDL, ACARS, VDL2, AIS, Mode S). The chosen one decodes while the window is open (decision 47; at first a RUN key started it and it ran with the window closed). The audio modes take the receiver's 48 kHz audio, the data links the I/Q. The list shows each decode's UTC, SNR, DT, audio frequency and RF frequency (the receiver's dial plus the audio, corrected by the scope's ppm, plus the transverter LO), newest first, and the line above it how many blocks the decoder dropped and how long the last slot took. Keyboard modes show running text with their audio frequency to set; pictures build up as they arrive, WEFAX with its LPM and IOC and a manual start and stop. Data links take the radio window's transverter offset as their LO, so they report true RF, and say when the span doesn't hold their channels. WSJT decodes also go out as WSJT-X UDP Decode, WSPR Decode and Status messages when that is on under SETTINGS. Only one sub-mode of FST4 and Q65 is offered for now.

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

## 37. The spectrum is computed on the GPU from the raw I/Q, and the 3D surface is lit there

**Decided:** by the integrator, 20 Oct 2026, on AJ3G's request: no CPU-to-GPU bus choking, all the raw I/Q into the GPU and the GPU draws it, wgpu, the I/Q loop and the amplifier link on threads of their own, and SDR Console's 3D fluidity in WGSL. AJ3G's GPU is an RTX 5070 (Framework 16).

**What the panel does.** SCOPE settings, *FFT on: GPU / CPU*, GPU by default where the GPU has compute shaders (any Vulkan, DX12 or Metal adapter; the GL fallback stays on the CPU). On the GPU:

* The raw interleaved f32 I/Q goes to a 64 MB ring of 2²³ complex samples in a storage buffer. The pipeline thread's I/Q tap copies each block into a lock-free CPU ring and returns; a thread of its own (`scope-gpu-iq`, raised above normal priority on Windows) writes it to the GPU in batches of up to 256 k samples with `Queue::write_buffer`, cast to bytes with no per-sample copy. `write_buffer` rather than our own mapped staging buffers: wgpu copies into staging memory it recycles itself, the copy is ordered on the queue, and nothing waits for a map or polls. Nothing on the UI or the I/Q threads waits for the GPU; if the uploader falls behind, the tap drops whole blocks and counts them.
* At 6144 kHz that is 6.144 M × 8 B = 49.2 MB/s up the bus, under 1 % of a PCIe 4.0 ×8 link. What comes back is at most 30 readbacks a second of the noise floors, the markers' peaks and the trace's columns: about 12 KB each at 1500 columns, under 0.4 MB/s.
* The spectrum is the CPU's, step for step: the same windows, ENBW and calibration, Welch segments every N/2 or N/4 summed into the frame, the frame rate limiter, exponential averaging, peak hold falling at 10 dB/s, the histogram-median noise floor, and the zoom FFT (a windowed-sinc decimator, 40 taps per output, then its own FFT). The FFT is a four-step Stockham radix-8/4/2 in WGSL with 16 KB of workgroup memory, 4 k to 1 M points, its twiddles worked out in f64. Tested against rustfft (relative error under 2·10⁻⁵ at 1 M) and against `sdr::Spectrum` (within 0.02 dB), on lavapipe in CI.
* Waterfall and 3D rows are pooled and copied into their history textures by the GPU, and the trace is drawn from the GPU's own columns, pooled to the screen's pixels every repaint, so a pan or zoom shows at once. The markers use the readback, a frame or two late, with the strongest bins found at full resolution.
* The pipeline's own spectrum thread is told to stop (`Command::SetSpectrumOn(false)`), so the I/Q is not also queued for it.

**The 3D surface (where the GPU has compute).** SDRoxide's shape, fog, palette and depth stay the default look. Added: the rows are kept as f32 with a max-mip chain across frequency, so any zoom is sampled without a carrier dropping out between columns; a compute pass works out every point's height and normal once a repaint; the front row moves smoothly from the previous spectrum to the newest between frames, and the rows glide back by the time since the last row was laid down, so the motion is at the repaint rate (60 to 144 Hz) whatever the frame or row rate; the solid surface is lit per pixel from high on the left with a little specular (*Lit*, on by default; off is SDRoxide's flat colour); the floor and ceiling ease over 0.2 s instead of jumping; and it is drawn with 4× MSAA where the GPU has it.

**Threads.** The ELAD's USB loop (`elad-iq`) and the DSP pipeline (`sdr-pipeline`) run on threads of their own and are raised to time-critical and highest priority on Windows (`SetThreadPriority`, from `windows-sys`). The amplifier link runs on its own `hardrock` thread; the UI sends it commands with `try_send` on a bounded channel and reads its status under a briefly held lock, so the UI never waits on the serial port.

**On lavapipe** (a CPU rasteriser on 4 cores) the GPU path is slower than the CPU path, since the "GPU" is the same CPU: see the report for the figures. On a real GPU the FFT and the surface are a small part of a frame.

**Where.** `crates/hardrock-panel/src/scope/gpu_iq.rs`, `gpu_spec.rs`, `gpu_fft.rs`, `gpu_run.rs`, `gpu.rs`, `shaders/`; `crates/elad-iq/src/priority.rs`; `crates/sdr/src/pipeline.rs` (`SetSpectrumOn`).

## 38. The receiver's DDC: an f64 NCO, CIC + compensating FIR by default, Kaiser passbands and a feed-forward AGC

**Decided:** AJ3G, 20 Oct 2026: "achieving absolute elite audio and decoder performance on the CPU ... a Cascaded Integrator-Comb (CIC) filter for the heavy down-sampling, followed by a compensated Polyphase FIR decimator ... >90 dB of stopband attenuation ... a feed-forward AGC ... at a fixed dBFS level". Which decimator is the default was left to measurement.

**What the panel does.**
- **NCO:** an exact 64-bit phase accumulator, mixed in f64, to bring the receiver to 0 Hz.
- **Decimation:** two kinds, selectable (`DecimatorKind`). CIC + FIR is a CIC of order 6 down to 384 kHz, a short FIR /2 that undoes the CIC's droop, then a Kaiser FIR /4 to 48 kHz. The half-band kind is a ladder of half-band /2 stages. Both are flat to 20 kHz and about 103 dB down at the 24 kHz channel edge at every DUO rate. Against signals more than 168 kHz away, CIC + FIR measured 125 dB and the half-band ladder 110 to 114 dB, so CIC + FIR is the default. It costs 0.5 to 2 ns a sample more (5 to 10 ns a sample in all at 768 kS/s and above). Order 6, because order 4 measured 86 dB and order 5 107 dB at 768 kS/s. At 192 and 384 kS/s there is nothing for a CIC to do, and both kinds are the half-band chain.
- **Passband:** three shapes, Soft (Blackman-Harris, 1 kHz skirt), Sharp (Kaiser, 95 dB 250 Hz out) and Brick wall (Kaiser, 95 dB 60 Hz out). Measured stopbands are 96.7 to 98.3 dB, with ripple under 0.0004 dB. Filters over 64 taps run by FFT overlap-save. The cost is audio delay: about 9, 24 and 64 ms.
- **AGC:** feed-forward with a lookahead delay, so a +40 dB step doesn't overshoot. Attack, decay, hang and target are adjustable. The decoders get their own feed, at −12 dBFS with a slow AGC (5 ms attack, 2 s hang, 2 s decay), whatever the listening AGC, gain or volume are set to.

**Where.** `crates/sdr/src/nco.rs`, `cic.rs`, `ddc.rs`, `decim.rs`, `design.rs`, `passband.rs`, `agc.rs`, `receiver.rs`. The table comes from `cargo test --release -p sdr -- --ignored --nocapture decimator_report`.

## 39. The ELAD look everywhere, one data-mode table, DECODE's snap, waterfall filter poles, DECODE pops out, receiver DSP settings

**Decided:** AJ3G, 20 Oct 2026: "Make them all look the same in the ELAD theme", and "make the Band Plan on the bottom match specific Modes, such as FT8 ... When you click a Decode method the ELAD should snap to the frequency of mode, and have its Filters set up for the correct mode. Filterwidth poles that are in the waterfall on the bottom should be able to be grabbed and moved ... make decode panel pop out." The rest of the shape by the integrator.

**What the panel does.**
- **One look for the station.** A new ELAD palette (`theme.rs`: the DUO's and the DUO-ART's graphite case, key-cap greys, silk-screen lettering, TFT black and the DUO-ART's status blue, the same values as `radio/rigtheme.rs` and `panel/amptheme.rs`) and a *Key caps* chrome style (`chrome.rs`: rounded keys lit from above with a near-black edge, a lit key flat in blue with a glow; windows as rounded cases). Chosen centrally, once a frame before anything is painted (`theme::set_station_look`), from the chosen radio: the FDM-DUO, enabled under SETTINGS, gives ELAD's look for every window, bar, chip, field, slider, combo, menu and tooltip, the AMP screen and the radio's face included; any other choice or none keeps SDRoxide's. Yellow and red are still only warnings (the "Opening…" status went from yellow to blue). `--theme elad` or `--theme sdroxide` forces either; `--demo` wears ELAD's unless told otherwise, since it stands in for the DUO.
- **One data-mode table** (`scope/digimodes.rs`), with its sources in its header: FT8, FT4, WSPR, JT65, JT9, FST4, FST4W, Q65, MSK144 (WSJT-X's default list, Region 2), JS8, PSK31, RTTY, SSTV, WEFAX (NOAA and DWD), NAVTEX (490, 518 and 4209.5 kHz, on SITOR-B's 1700 Hz audio centre), ACARS, VDL2, AIS, marine DSC and ADS-B. Each has its sideband (SSTV LSB below 10 MHz), its audio passband, the decoder's audio centre where it has one (RTTY 2210 Hz with mark high on USB, PSK31 1500 Hz) and its dials. The scope's DATA keys, its band-plan markers, the radio window's data keys and DECODE all read it; the two tables there were before are gone.
- **Where a mode goes.** Its dial on the band the receiver (or the radio's dial) is on; between bands, the nearest band's; if it has none there, its dial nearest, with a note ("No RTTY frequency on 160 m, so 80 m"). Only dials inside the DUO's 9 kHz to 54 MHz through the transverter offset are taken, so the airband and marine channels appear only behind a converter. (Before, a mode with no frequency on the band went to 20 m.)
- **Band-plan markers.** The strip grew to two rows: the segments on top, and below them each dial as a tick with the mode's passband shaded out from it (FT8 dial to dial + 3 kHz), named where there is room, the keyed modes first. Under the pointer a marker lights and the top bar says what a click does; a click tunes there in that mode, as a DATA key does.
- **DECODE's snap** (on by default, *Snap the radio to the mode*): picking a mode takes the receiver to its dial, recentres the I/Q there (with the ELAD and CAT, VFO A over CAT), sets the receiver's mode and passband (FT8/FT4 200–3000 Hz, WSPR 1400–1600, PSK31 1350–1650, RTTY 2000–2420, NAVTEX 1500–1900), and the decoder's audio. Up to 192 kHz the DUO's own mode and filter follow (`Rx::sync_duo_mode`, now choosing the filter as a data mode does, so WSPR and PSK31 get a narrow data filter); with no I/Q it is done over CAT alone (`logic::digi_frames_at`). Olivia, THOR and HFDL have no calling frequency, so only the passband is set. What it did is said under DECODE's mode keys and beside the scope's DATA keys. A DATA key or a marker goes through the same path, so the decoder and the radio's filter follow those too.
- **Filter poles in the waterfall.** The passband's edges run down the waterfall with handles at its top, light under the pointer and while dragged, and drag exactly as on the spectrum (`pointer::grab_at` no longer tells the two apart). A press inside the passband and a drag tunes, carrying it along (`DragKind::Centre`); a click there still tunes to the click.
- **DECODE pops out** into a window of its own (an egui viewport, as RADIO and SCOPE do) and docks back; which is kept between runs.
- **Receiver DSP** under SETTINGS → *Scope and receiver*: decimator (CIC + FIR, the default, or half-band), passband shape (Soft 9 ms, Sharp 24 ms, Brick wall 64 ms of audio delay), the listening AGC's attack, decay, hang and target, and the decoders' AGC target; taken by the running pipeline at once (decision 38's commands), used when it starts, kept.

**Safety.** Every snap, key and marker click is refused while the radio or the amplifier says it is transmitting, with a note naming interlocks #5 and #9; the scope's own refusal stays in `Drag::press` and the strip's click; the rig link refuses the frames then anyway (#5).

**Where.** `theme.rs`, `chrome.rs`, `style.rs`, `panel/amptheme.rs`, `main.rs` (`snap_to`, `--theme`), `scope/digimodes.rs`, `scope/bandplan.rs`, `scope/paint.rs`, `scope/pointer.rs`, `scope/controls.rs`, `radio/logic.rs`, `decode.rs`, `rx.rs` (`DspSettings`, `duo_filter`), `settings.rs`.

## 40. The scope holds what it showed while the radio transmits, and starts afresh after

**Decided:** AJ3G, 20 Oct 2026, from a bug report: "On transmit from ELAD FDM DUO Control, and return to RX FFT goes blank", with both TX TUNE and RIG TUNE, on the FFT on the GPU. The shape (SDR Console's) by the integrator.

**What the panel does.**
- **Held over the over.** While the radio says it is transmitting (the amplifier too, with no radio connected; decision 52), and for 0.3 s after (`TX_GUARD_SECS`, 0.5 s until decision 52), the scope takes no new frames: the trace, the waterfall and the 3D surface stay as they were, the span and centre with them (even if the dial read back during the over says otherwise), and FIT, the waterfall's auto level and the markers keep working on what is held. The top bar reads *IQ · HELD · TX* and the status line says it starts afresh after the over. Zoom, markers and the display settings still work, as before (interlock #9 still locks tuning).
- **Afresh after it.** On the GPU the frame timing is rebuilt from the newest samples (`GpuRun::resync`): nothing from before the end of the hold is transformed, and the first frame starts the averages and the held peaks. On the CPU the scope asks the pipeline to start its spectrum again (`Command::ResetSpectrum`): averages, peaks, history and anything queued from before are dropped, and the scope stays held until the reset has been done (two seconds at most).
- **Why hold rather than show.** What the DUO's I/Q does during an over is not known from here (no radio on the bench): it may stop, carry the transmitter's own signal at or near full scale, be muted to zeros, or come with a dial read-back that is VFO B (split), 0 or nonsense. Each was simulated (`sdr::TxSim`; `scope_demo --tx`). Shown, full-scale leakage dragged FIT's ceiling to full scale and the peak hold with it for many seconds after the over, zeros blacked the waterfall, and a centre of 0 moved the whole span away; samples that were not numbers blanked the spectrum for good (below). Held, every case comes back on the first frame after the guard. Watching one's own transmission on the DUO's receiver is not useful enough to keep.
- **Not numbers.** One NaN in an exponential average stays for good (`a + α(x − a)`), on the GPU and the CPU alike; a spectrum fed one was blank until the FFT size changed (GPU) or the program restarted (CPU), and the receiver's meter read NaN. Now the pipeline replaces anything that is not a finite number with zero as it reads the source (`Snapshot::nonfinite` counts them), and both averages start again from the next frame if they hold one.
- **The dial read-back.** A read-back of 0 or outside the receiver's 10 kHz to 54 MHz is ignored and the last good one kept (`sdr::plausible_dial`); before, a 0 fell back to the centre the device was opened at, which may be another band. While transmitting and for a second after, a moved dial does not move the panel's receiver (interlock #9); what it says after that is taken as a turn of the knob.
- **Watchdogs.** The GPU spectrum starts again, buffers and all, if samples keep coming but no frame has been made for a second (longer for a big FFT or a slow frame rate: `gpu_run::stalled`), and says so on the status line for a few seconds. An ELAD stream that delivers nothing for 3 s while the radio is not transmitting (counted from the end of an over), or whose thread has ended, is opened again as TRY AGAIN would, at most every 20 s, and the scope window says so (interlock #10 still holds: never while transmitting). A reopen that fails, whatever the driver's error, is tried again on the same timer until the stream is back: before, only "no ELAD device" was, and AJ3G's stream that ended at 01:34Z on 25 Oct 2026 stayed down. REOPEN on the I/Q line does the same by hand while the stream runs.
- **A new pipeline** (TRY AGAIN, SPAN, the watchdog) is now sent the scope's settings again, with the GPU's *spectrum off* among them: before, the CPU spectrum ran again unread behind the GPU's after a restart, and a CPU zoom FFT was not asked for again.

**Not confirmed without the radio.** Which of these the DUO does, and so which was AJ3G's blank. The hold covers all of them; the watchdogs cover a stream that does not come back. What the DUO's I/Q does when keyed would settle it (the ELAD trace, `elad-iq` `trace`, notes stalls and dial reads).

**Where.** `scope/mod.rs` (`TxHold`, `hold_over_tx`, `span`), `scope/gpu_run.rs` (`resync`, `stalled`, a held epoch), `scope/shaders/spectrum.wgsl` (`finite`), `scope/paint.rs` (the top bar and status line), `rx.rs` (`watch_stream`, `DIAL_SETTLE`), `sdr/src/pipeline.rs` (`sanitise`, `ResetSpectrum`), `sdr/src/spectrum.rs`, `sdr/src/source.rs` (`TxSim`, `plausible_dial`), `examples/scope_demo.rs` (`--tx`), `tools/fake_duo.py` (`--key-at`, `--unkey-at`).

**Amended 11 Nov 2026 (decision 74).** The display no longer stands still over an over. The received I/Q is still never shown while the radio transmits and for the guard after, and the restart afterwards is unchanged (the GPU resync, the CPU reset waited for, averages and peaks forgotten); but the waterfall and the trace keep moving meanwhile, showing the band at its last received level, dimmed, with our own transmitted signal drawn where it lands. The top bar reads *IQ · TX · RX DIMMED* in place of *IQ · HELD · TX*. Everything above about why the I/Q is not shown still stands.


## 41. A zoom follows the receiver only when it moves; a mode snap keeps the centre; the end of an over is seen within a round

**Decided:** AJ3G, 21 Oct 2026, from testing build 3dd6b9d (NEXT-BUILD.md items 3, 4, 6 and 10).

- **Zoom (item 3).** A zoomed view pans back to the receiver only when the receiver itself has moved since the view last followed it (tuning on the radio or the panel, a DECODE snap, a click to tune; `view::follow_rx`, `Scope::followed_rx`). A view the operator panned or zoomed (ctrl + wheel at the pointer) away from the receiver stays where it was put. Before, it jumped back on every repaint the receiver was off screen. A move during a drag is followed once the button is up.
- **DECODE snap (item 4).** A mode click moves only the receiver when the mode's dial is already inside 0.45 of the span from the centre, the same rule as the scope's own recentre (`view::keeps_centre`, decision 22's fixed centre); the centre moves only when the dial is off towards an edge or on another band. Old waterfall rows are drawn where they were taken, so moving the centre left a strip they never covered. Nothing retunes while transmitting, as before (interlocks #9 and #10).
- **After an over (item 6).** The radio link read `GI` (receive or transmit) once a second, and the settings' own read (a dozen round trips or more), deferred during the over, ran straight after it, so the panel could go on believing the radio transmitted for a second or more after it had stopped, and the scope, held over the transmission, waited that long. Now `GI` is read with every round of power readings (150 ms) while the radio says it transmits, and the settings wait until two seconds after the over (`SETTINGS_AFTER_TX`). On the pretend DUO in the tests the end of an over was seen after 0.8 s before and 0.06 s after. The scope itself takes 0.66 s from the end of an over to a fresh spectrum and waterfall row (the half-second guard of decision 40 and one FFT), measured on the CPU spectrum with a paced synthetic stream.
- **MUTE (item 10).** A MUTE chip beside the SOUND slider in place of the round checkbox, lit in the selected-chip blue while muted (a neutral state, so not yellow or red).

**Where.** `scope/view.rs` (`follow_rx`, `keeps_centre`), `scope/mod.rs` (`follow`, the timing test), `main.rs` (the DECODE snap), `scopewin.rs` (MUTE), `crates/rigcat/src/link.rs` (`run`, `SETTINGS_AFTER_TX`).

## 42. A log file beside the settings, the SCF's sizes kept valid, its engine started again once, and tuning in the ELAD log

**Decided:** AJ3G, 21 Oct 2026, from testing build 3dd6b9d (NEXT-BUILD.md, items 1, 2, 7 and 8).

**What the panel does.**
- **A log file.** Everything logged also goes to `hardrock-panel.log` in the folder eframe keeps the settings in (`eframe::storage_dir("Hardrock Panel")`: `%APPDATA%\Hardrock Panel\data` on Windows, `~/.local/share/hardrockpanel` on Linux), as well as to stderr. The Windows release build has no console, so before this a message saying "see the log" pointed at nothing. Past 2 MB the file is moved to `hardrock-panel.log.1` (replacing the one before) and a new one begun, so it never takes more than 4 MB. A panic in any thread is written to it with the thread's name and the place. No crate for it: `tracing-appender` is not to be had here, and a size-capped writer is forty lines (`logfile.rs`).
- **The SCF's N and N′.** The engine needs N ≥ 4 × N′. The N list greys out N under that as well as N too big for the GPU's buffers, each saying why on hover; choosing a larger N′ raises N to the smallest that goes with it; and a pair kept from an earlier run that the engine cannot run (N = N′ = 1024 stopped it on every start) is put right as the settings are loaded. The engine itself refuses sizes it cannot run with the reason on the view, rather than stopping on an assertion.
- **When the SCF engine stops anyway.** The message carries the panic's own text and says where the log is, with COPY LOG beside it (the last 64 KB of the log). The engine is started again once by itself, and the view says it was; a second stop is left stopped, as a fault that repeats would only fill the log.
- **Sound outputs** are read again each time one of the sound lists under SETTINGS opens, so a speaker or cable plugged in after start-up is there. REFRESH PORTS is now REFRESH DEVICES: serial ports, sound outputs and the ELAD on USB.
- **The ELAD log's tuning.** The session trace covers only the opening; on the FDM-DUO later retunes go over CAT, past the driver, so a report showed the dial it opened on as if current (7.074 MHz while the radio was on 7.0386). COPY ELAD LOG now begins with where the radio is now (the dial over CAT, the I/Q centre and the receiver) and the last ten recentres and dial moves, each with its UTC time (`elad_iq::note_dial`, `note_retune`, fed by `rx.rs`).

**Where.** `logfile.rs`, `main.rs` (`APP_NAME`, REFRESH DEVICES), `scope/scf/mod.rs` (`strip_ok`, `set_channels`, `sanitise`, the GPU thread's restart), `scope/scf/engine.rs` (`EngineCfg::problem`, `Engine::new` returns a `Result`), `scope/scf/ui.rs` (the N list, `error_row`), `settings.rs` (`reread_sounds_on_open`), `rx.rs` (`dial_line`), `elad-iq/src/trace.rs`.

## 43. MSK144 is decoded as its slot runs, each message kept once at its best

**Decided:** AJ3G, NEXT-BUILD item 15 (MSK144 for 6 m meteor scatter). The shape by the integrator.

**What the panel does.**
- **A WSJT mode.** MSK144 (`WsjtMode::Msk144`, mfsk-core 0.10's `msk144` feature) in 15 s UTC slots of 12 kHz audio, centred on WSJT-X's 1500 Hz with its ±100 Hz F Tol, the callsign table shared with FT8 so hashed calls resolve. DECODE offers it under WSJT; its snap takes the radio to 50.260 MHz USB (144.150 MHz behind a converter) with the WSJT 300–2700 Hz data filter, from the one data-mode table (decision 39). Its key's hover says a numbered CQ (CQ 265 …) asks for answers on that frequency.
- **Live pings.** A ping lasts 50 to 200 ms and comes any time, so waiting for the slot's end would show it up to 15 s late. About once a second (`LIVE_STEP`) the decoder looks at the audio no pass has seen, with a block's overlap so a ping across the join is not lost and a second before it so the noise floor, which the SNR is measured against, is known (`Msk144Live::pass`); what was heard in the window's first block is not taken from it, since the last pass measured it with the floor known. Each decode is shown at once with DT its time into the slot, as WSJT-X shows it, SNR and audio frequency; a slot is taken however late it was joined.
- **Cheap passes, one deep look.** The live passes run only the short-ping decoder (about 8 ms a pass, 1 ms a block, on the build machine). At the slot's end the whole slot is decoded again at WSJT-X's Normal depth, with the frame-averaging search too (about 0.7 s), for a signal too weak for one frame but lasting several. Deep depth everywhere cost 250 ms a pass, a quarter of a core.
- **Once each.** A station sends the same message over and over and a long ping decodes in several blocks: within a slot a message is kept once, at its best SNR with the time and frequency of that hearing. A better hearing updates its line in DECODE rather than adding one; WSJT-X UDP is sent each message once, when first heard.

**Not in mfsk-core, so not here.** Sh (short) messages and MSK40, and WSJT-X's adaptive equaliser trained on the session's decodes. A ping in a slot's first 0.6 s comes before any noise floor is known, and its SNR reads 0.

**Where.** `decoders/src/wsjt.rs` (`Msk144Live`, `keep_best`), `decoders/src/worker.rs` (`SlotMsg`, `WsjtSlot::live`), `decoders/src/slot.rs` (`SlotRecorder::partial`), `hardrock-panel/src/decode.rs` (`Kind::Msk144`, `update_row`), the workspace's mfsk-core features.

## 44. DECODE's slot timer and decode light, DIST and AZ from the operator's grid, copying, ALL.TXT, and spots to PSK Reporter and WSPRnet

**Decided:** AJ3G, 21 Oct 2026 (NEXT-BUILD items 11 to 14): a slot timer and decode light like WSJT-X's and SDRoxide's; selectable lines, COPY ALL and a running decode log like ALL.TXT; My Callsign and My Grid under SETTINGS → Station with DIST and AZ in DECODE, a call heard once with a grid keeping it, positions kept in one place for a globe; a SPOTS box with PSK Reporter and WSPRnet, off until ticked, greyed until the station is set, built as its own piece. The shape by the integrator.

**What the panel does.**
- **Slot timer** (`slotclock.rs`), for the WSJT modes: a bar across the mode's `period_s`, ticked every second (every 10 s past 30 s periods), the seconds into the slot, the UTC from the system clock (GPS-disciplined NTP at AJ3G's, so no offset is learnt), and 1st/2nd for an even/odd slot counted from 00:00 UTC. All blue; nothing here is a warning.
- **Decode light**: lit while the slot just finished is being decoded, judged in the panel (the decoder crate is untouched): the slot is being decoded if the decoder was running from within its 1 s join limit of the slot's start and has not reported it; then the light shows that slot's count. A slot the decoder never reports stays lit only until the next one ends.
- **Station** (`station.rs`, SETTINGS → Station, taken at once and kept): callsign (upper-cased, checked) and grid (4 or 6 characters, checked as typed; a bad character says so in yellow, a whole grid shows its centre's latitude and longitude). km or mi. The callsign also seeds the decoder's hash table, so hashed messages naming it resolve from the first slot.
- **Positions** in one place (`station::Heard`): each callsign heard with a grid this run, at the grid's centre, with when; read for DIST and AZ, and for PSK Reporter's locator. The sender of a message is CQ's caller or a standard message's second callsign; the grid is a final 4-character locator other than RR73. DIST is the haversine distance on a 6371 km sphere, AZ the initial great-circle bearing; tested against London–New York (5570 km, 288.3°) and the quarter and half circles.
- **Lines** are now one monospace row each, in fixed columns (UTC, SNR, DT, AF, RF, DIST, AZ, message), so they copy as they look. Click picks one, Ctrl+click toggles, Shift+click a run; COPY n and Ctrl+C copy the picked ones, COPY ALL every line, with the headings.
- **ALL.TXT** (`decodelog.rs`): every WSJT decode appended a slot at a time in WSJT-X's layout (`yyMMdd_hhmmss`, dial MHz through the transverter, `Rx`, mode, SNR, DT, AF, message) to `ALL.TXT` in the settings folder (`eframe::storage_dir("Hardrock Panel")`), a file of its own, apart from the general log. A failed write is said in yellow under the list.
- **SPOTS** (`spots/`, its own module; DECODE only hands it each slot and draws its box). Uploaders run only while their tick is set and the station is whole; clearing a tick (or changing the callsign or grid) drops the uploader and what it held. Program "Hardrock Panel" and the crate's version.
  - *PSK Reporter* (`spots/pskreporter.rs`), FT8 and FT4: IPFIX over UDP to report.pskreporter.info:4739 from one socket (one source port). Templates 0x9992 (receiver options template: receiverCallsign, receiverLocator, decodingSoftware; byte for byte PSK Reporter's published example) and 0x9993 (sender: senderCallsign, frequency in 5 bytes, sNR, mode, senderLocator, informationSource = 1, flowStartSeconds), sent in the first three datagrams and again hourly; every datagram carries the receiver record. A batch every 300 s from the uploader's start plus 0–30 s chosen per run, never clock-aligned; the strongest report of a call per band and mode in a batch, an unchanged one not again within the hour; datagrams up to 1400 bytes, more only when one is full. The IPFIX sequence number counts data records; the observation domain is random per run.
  - *WSPRnet* (`spots/wsprnet.rs`): after each WSPR slot, one form-encoded HTTP POST per spot to http://wsprnet.org/post/ (`function=wspr`, rcall, rgrid, rqrg the dial, date, time, sig, dt, drift, tqrg the signal, tcall, tgrid, dbm, version, mode=2), or one `function=wsprstat` for a quiet slot; std `TcpStream`, no new crates. A failed post is kept and tried again after a minute or with the next slot (up to 40 slots).
  - The box says what each last did and when ("PSK Reporter: 12 spots sent 01:10 UTC", "WSPRnet: 3 spots sent, 3 added …"), how many wait for PSK Reporter's next batch, or the error in yellow.
- **Tests** check the packet bytes, the WSPRnet form and reply, the grid and great-circle figures, message parsing, the slot clock and light, the ALL.TXT line, and the picking. Nothing was sent to either service from tests or the build machine.

**Not confirmed.** That WSPRnet takes a `version` as long as "Hardrock Panel 0.1.0" (the box shows how many it says it added, so a refusal would show), and that PSK Reporter shows the panel's reports (PSK Reporter has a test port, 14739, for a first try). Neither can be tried from the build machine.

**Where.** `slotclock.rs`, `station.rs`, `decodelog.rs`, `spots/mod.rs`, `spots/pskreporter.rs`, `spots/wsprnet.rs`, `decode.rs`, `settings.rs` (`settings_station`), `main.rs` (`Saved::station`).

## 45. The AMP buttons go out ahead of the next read, and show as asked until read back

**Decided:** AJ3G, 21 Oct 2026, from "AMP buttons respond slowly, slower than the SDRoxide panel" (NEXT-BUILD #5). The shape by the integrator.

**What the link does.**
- **One read at a time.** The link's loop makes one query (`Read`) and looks at the panel's channel before every one, so a click waits behind at most one read: up to the 400 ms reply timeout when the amplifier skips an answer, tens of milliseconds when it doesn't. Before, the channel was looked at once a round, and a round was the status, the temperature and four tuner reads, each up to 400 ms.
- **Spread.** The status frame stays every 250 ms; the temperature and the tuner's three queries (`HRTT`, `HRTB`, `HRTMP`, and `HRTR` after a tune) are separate reads fitted between status frames. Straight after a status frame one other due read goes first (`next_read`), so a run of unanswered status reads can't shut the others out.
- **After a click** the status is read next, to confirm it, and the tuner again after its relays settle (300 ms).
- **Interlocks #1 and #4** keep their status read just before a relay or OFF-to-PTT command, except that a frame the amplifier answered within 100 ms and showing it clear stands in for it (`FRESH`, docs/safety-interlocks.md). The band-follow read of #3 is always its own.

**What the panel shows.** A click on PTT/OFF, ANT, DN/UP or ON/BYP shows at once as asked, in the neutral blue (`AmpStatus::pending`, set by `AmpLink::action` on the panel's own thread): the PTT key reads PTT in blue until the amplifier reads PTT (then green), ANT the antenna asked for, the band the one stepped to. The first read that shows it confirms it. A refused click goes back at once; one still read in its old state after a second (`PENDING_HOLD`) goes back to what the amplifier reads. The interlocks go by the readings, never by what is pending. TUNE and the C/F tile show only what is read back.

**Measured** (`a_click_goes_out_without_waiting_for_a_round_of_reads`, a simulated HR500 answering after 25 ms, 400 ms reply timeout, 16 clicks of each at scattered moments):

| amplifier | before: mean / worst | after: mean / worst |
|---|---|---|
| answers everything, PTT/OFF | 16 / 48 ms | 6 / 25 ms |
| answers everything, ANT | 29 / 74 ms | 14 / 26 ms |
| skips `HRTP` and the tuner reads, PTT/OFF | 1191 / 1552 ms | 228 / 385 ms |
| skips `HRTP` and the tuner reads, ANT | 1184 / 1551 ms | 250 / 419 ms |

**Where.** `crates/hardrock/src/link.rs` (`run`, `Read`, `next_read`, `act`, `Clear`, `queue`), `crates/hardrock/src/types.rs` (`AmpStatus::pending`, `AmpAction::control`), `panel.rs` (`keys`, `Shown::band`), `panel/tft.rs`.

## 46. The RADIO window grows with its window; the AMP panel keeps its proportions

**Decided:** AJ3G, 21 Oct 2026: "Radio Panel and Amp Panel do not scale when window is expanded" (NEXT-BUILD #9). The shape by the integrator.

**What was wrong.** The radio window was drawn at fixed sizes: popped out, its face stopped at 760 points wide and its text never grew; docked, it was 560 wide and could not be resized. The AMP panel did scale to its window, but by whichever of width and height is tighter: in a wide window (a maximised landscape screen) the height decides, so it grew little while its boxes stretched across the whole width, which read as small text in a strip rather than as the panel grown.

**What the panel does.**
- **Radio window.** Drawn for a scale of 1 and scaled as a whole: every size it sets itself goes through `radio::zoom::z`, and the `Ui`'s style (text sizes, spacing, padding, slider and box widths) is scaled for the widgets that take their sizes from it (`zoom::scaled`). The text is laid out at the size it is shown, never a zoomed picture of it, so it stays sharp. Popped out it fits the window's width and height both (`panel::fit_scaled`, the AMP panel's fit, now shared), centred, between 0.75 and 3 times; smaller than that it scrolls. Docked, its width can be dragged and it scales with it, its height following. MENU and the confirmation questions are windows of their own and keep their size. The TUNE strip in the main window is unchanged.
- **AMP panel.** Still fits width and height both, but its boxes stretch at most 1.3 times its own proportions (`MAX_STRETCH`); wider than that it keeps that width, centred. That partly reverses AJ3G's 18 Oct choice for the SDRoxide pop-out (boxes spanning the whole window) in very wide windows only; the constant is the one place to change it.
- **Not done.** In a window much wider than it is tall the AMP panel cannot grow past what its height allows without a second, side-by-side layout (meters on the left, tuner and keys on the right). Not built.

**Screens.** `screens/scale-before-*.png` and `screens/scale-after-*.png` beside this project's notes.

**Where.** `radio/zoom.rs`, `radio.rs` (`fitted`, `radio_window`), `radio/face.rs`, `radio/controls.rs`, `tune.rs` (`tune_strip`), `panel.rs` (`fit_panel`, `fit_scaled`, `fit_scale`).

## 47. DECODE runs while its window is open (21 Oct 2026)

**Why.** With a RUN key, a stopped decoder looked like a working one: the slot bar kept ticking and the list said "Nothing decoded yet". AJ3G lost a session of FT8 to it and asked whether the key was needed.

**What the panel does.** No RUN or STOP. The decoder runs while the DECODE window is open, docked or popped out, as WSJT-X always decodes; closing the window stops it, and with it the spots it sends. The settings file's `running` is still read but is set each frame from the window.

**Also.** Each WSJT slot goes in the log at info level: the audio's RMS, how late the slot was joined, and how many it decoded in how long, so a slot with nothing decoded can be told from a slot with no audio.

## 48. RADIO reads the S-meter every round and shows the filter, the receive settings and REF (21 Oct 2026)

**Why.** AJ3G: the S-meter on RADIO was slow (it was read with the settings, every 10 s), the filter width wasn't shown on SCOPE or RADIO, and the DUO's 10 MHz reference indicator didn't come through.

**What the panel does.** `SM0;` is read with the power readings every round (150 ms) while receiving, and `RF;` with the dial every second. The LCD's first row starts with the filter width (FIL 2.40k); a second row shows AGC or MAN GAIN, NB, NR, NOTCH, SQL, the split TX frequency and REF 10M. The scope's RX label carries the passband width (BW 2.60 kHz), as does the passband itself when there is room.

**REF.** No CAT command and no documented USB field reports the reference input. SETTINGS has a "10 MHz reference connected" tick that lights REF 10M. The dial read-back's undocumented last byte read 0x30 on AJ3G's radio with a reference and 0x10 in earlier notes without; every change of it (but the dial-turning bit) now goes in the ELAD log, so a bench test with the reference pulled can show whether bit 0x20 is the reference. If it is, it replaces the tick.


## 49. No black blocks in the waterfall: unmeasured pixels at the noise floor, and a wider held band (21 Oct 2026)

**Why.** AJ3G, on the DUO at 192 kHz: "black blocks in my display when moving around in the FFT". Each waterfall row covers only the band it was taken over, and wherever an old row did not cover the frequency under a pixel (the strip a recentre uncovers, a pan past a zoomed row's margin, a zoom out) the shader painted the bottom of the palette, which is black or nearly, well under the quiet band (the floor sits 4 dB under the noise).

**What the panel does.**
- **No history is drawn as a quiet band, a shade dimmer.** A pixel with no row covering it, and the rows not filled yet, are painted in the colour of the quiet band's level (the median of the trace's columns, else the noise floor) through the same levels, contrast and brightness as a measured pixel, at 80 % of that colour (`levels::quiet_dbm`, `no_data_t`, `NO_DATA_SHADE`; the waterfall's `empty_t` and `empty_shade` uniforms). Up close it still reads as "nothing here"; it no longer reads as a fault. The palettes are unchanged. There is no CPU waterfall (without the GPU renderer the panel says it needs it), so there is nothing to match there.
- **Rows keep twice the view, held.** Zoomed, a row keeps half the view's width either side (was a quarter), slid inside the span at its edges rather than cut off there. The band is held while the view stays inside it with 5 % to spare (or the band is against the span's edge there) and is 1.15 to 3.5 views wide, as SDRoxide's `cfg_still_good`; otherwise it is laid out afresh around the view (`view::held_coverage`, held separately for the waterfall and the 3D surface in `Rows`). Pans inside it never meet an edge, and a slow row is not restarted by them. Not zoomed, rows cover the whole span as before.

**Cost.** No GPU memory: the rows are as wide (`tex_w`, up to 16 384 columns) and as many (2048) as before; the uniform block grows by 16 bytes. The price is resolution: the same columns cover 2 views rather than 1.5, so a zoomed row has three quarters of the columns per hertz it had. At 16 384 columns that is still several columns a pixel; on an adapter limited to 2048-texel textures a wide zoomed waterfall can drop just under one column a pixel. A median of the trace's columns each repaint (about a thousand floats).

**Not done.** The analysis's first fix, a second, whole-span history ring for pans and zoom-outs to fall back on (64 MB more at the widest), and its smaller recentre steps. A recentre still uncovers a strip; it is now drawn as a dim quiet band rather than black.

**Screens.** `screens/wf-fill-before.png` and `screens/wf-fill-after.png`: two seconds after a recentre back from the top edge of the span, demo source.

**Where.** `scope/view.rs` (`coverage`, `held_coverage`), `scope/rows.rs`, `scope/levels.rs`, `scope/paint.rs`, `scope/gpu.rs` (`WfParams`), `scope/shaders/waterfall.wgsl`, `scope/shaders/common.wgsl` (`palette_raw`).

## 50. Docked windows wear an edge and can be resized by their edges (21 Oct 2026)

**Why.** AJ3G: "On spectrum Display, around the border you need to add something more pronounced (like you did in AMP Panel) to determine the GUI Window edge. Also I am unable to change the size of the window when docked." A docked window is the panel's own colour on the panel, so nothing marked where it ended; SCOPE and SETTINGS could not be resized at all, and RADIO only in width.

**What the panel does.**
- **Edge.** Every window docked over the panel (SCOPE, RADIO, DECODE, SETTINGS, the radio's MENU) is framed by `chrome::window_frame`, which now has an edge as the amplifier's case has: a 1.5 pt line in the theme's window-border colour taken a quarter of the way to its text colour, and a dark shadow all round just outside it (faint on a light theme). It is one place: `chrome::window_edge`. Popped-out windows are windows of the operating system and are unchanged; the menu popups keep their own cut-corner border.
- **Resizing.** All four can be dragged by any edge or corner, and egui shows the resize cursor there (tested). egui draws its corner grip in the edge's colour, so with the edge the grip shows too. Sizes are kept from run to run in egui's memory, which eframe saves (checked: a resized SCOPE came back at its size).
  - SCOPE takes the window's width; its height is the window's, the scope and its controls scrolling within it. First opened it fills the screen as before. Smallest 560 × 320. The trace and waterfall keep their own height (HEIGHT, and the grip under the waterfall); the window does not stretch them.
  - RADIO's face fits the window's width and height both, keeping its proportions and centred (the pop-out's fit), between 0.75 and 3 times; smaller than that it scrolls.
  - DECODE already could; SETTINGS scrolls within whatever height it is given, APPLY kept below.
- **How.** egui sizes a window to what its contents measure, so a resizable window's body has to take all the room it is given (`scopewin::fill_scroll`, or `set_min_size` for RADIO); a body that shrank to its contents would put the window's edge where the contents end.

**Screens.** `screens/edges-before.png`, `screens/edges-after.png`, `screens/scope-docked-resized.png` beside this project's notes.

**Where.** `chrome.rs` (`window_frame`, `window_edge`), `scopewin.rs`, `radio.rs` (`radio_window`), `main.rs` (`settings_window`).

## 51. Slot lines on the waterfall: green at the slot's start, red where its signals end (21 Oct 2026)

**Why.** AJ3G: "Inside of Scope when Decode window start/stops. When its starts draw a Green Line across the Waterfall, and when it ends draw a RED line across Waterfall. Look at WJSTX approach, its close." WSJT-X's wide graph draws one line per T/R period at its boundary, with the UTC beside it, so a signal can be read against the slot it was sent in.

**First version, replaced.** Red first marked the end of the decoder's capture, which is the next slot's start. In FT8 and FT4 that put red and green on the same boundary; AJ3G saw a yellow-and-red pair at every boundary and asked what red meant, and there it meant nothing. Then: "Green TX Start, RED when finish, and yellow for the no information".

**What the panel does.**
- **When.** While DECODE runs a mode with slots (FT8, FT4, WSPR, JT65, JT9, FST4-60, Q65-30A, MSK144). Keyboard modes, pictures and the I/Q links have no slots and get nothing.
- **Green: the slot's start.** Slots start on whole periods from 00:00 UTC (FT8 15 s, FT4 7.5 s, WSPR 2 min from the even minute, JT65/JT9/FST4-60 1 min, Q65-30A 30 s, MSK144 15 s), as the decoder's slot recorder cuts them. The first green line is on the first slot the decoder takes: the one it joined, if it joined within `max_late_s` (1 s; MSK144 any time), or else the next (`WsjtConfig::first_slot`). The UTC is at the left, in green on a dimmed backing, where the UTC ticks are; a UTC tick that would sit on it is left out, and labels closer than 12 pt are thinned.
- **Red: where the slot's transmissions end.** The last symbol of a signal sent on time: its start (`WsjtMode::start_delay_s`, the decoder's DT = 0: 0.5 s for FT8, FT4 and Q65-30A, 1 s for the rest) plus its length, worked out from mfsk-core's frame definitions as (channel symbols + ramp symbols) × samples per symbol / 12 000 (`WsjtMode::signal_s`, `signal_end_s`). Lengths: FT8 12.64 s, FT4 5.04 s (103 symbols and 2 ramp), WSPR 110.59 s, JT65 46.83 s, JT9 48.96 s, FST4-60 51.84 s, Q65-30A 25.5 s. So red sits at FT8 13.14 s, FT4 5.54 s, WSPR 111.59 s, JT65 47.83 s, JT9 49.96 s, FST4-60 52.84 s and Q65-30A 26.0 s into the slot. mfsk-core gives JT65 and JT9 a start of 0 s and Q65-30A 1 s; the decoder's own start delays are used, as they are what its DT is measured from. Red has no label.
- **Nothing between.** The dead time from the red line to the next green was tinted, yellow at 18 % and then blue at 6 %; AJ3G had it taken out (22 Oct 2026): it hid signals and labels. The skimmer's labels and the offset bar are drawn over the lines, so the lines pass behind them as the rows scroll.
- **No end, no red.** MSK144's pings come any time in the slot, so it gets green lines only. A slot cut short by DECODE stopping before its signal end gets no red line.
- **The look.** Each line is two pixels, the waterfall's full width.
- **Placement.** By each waterfall row's own time, as the UTC ticks (which use the same code: `slotlines::age_at`, `age_span`, `row_y`). So the lines and the tint scroll with the rows at every SPEED and stand still when the waterfall is paused. Over a transmission no rows are laid down, so a line that falls in the gap sits at the gap's edge.
- **After DECODE stops or changes mode** what is already drawn stays on its rows: the scope remembers the decoder's last 32 runs, each from its first slot to when it stopped.
- **SPEED row:** "Slot lines", one tick for all three, on by default, saved with the scope's settings.

**Red and yellow.** Both are otherwise for warnings only (AJ3G's rule). The slot lines' green and red are the second named exception (the dead time was tinted yellow until 22 Oct 2026; there is no tint now), after the waterfall palettes (`scope/palette.rs`), by AJ3G's request: here they mean "transmissions start" and "transmissions end", as WSJT-X users read the slot, not a warning.

**Screens.** `screens/slot-lines.png`, demo band, DECODE on FT8, SPEED Very slow.

**Where.** `decoders/src/wsjt.rs` (`WsjtMode::signal_s`, `signal_end_s`), `decoders/src/worker.rs` (`WsjtConfig::first_slot`, `capture`), `hardrock-panel/src/decode.rs` (`Decode::slots`, `slots_of`), `scope/slotlines.rs`, `scope/paint.rs` (`paint_waterfall`), `scope/settings.rs`, `scope/controls.rs`, `main.rs`.

- **Dimmed** (22 Oct 2026). AJ3G: "Make green and Red Stop/Start Bars less bright, much dimmer." The lines are drawn at 30% (`slotlines::DIM`), the slot's UTC at 60%.

## 52. The scope comes back on the radio's word after an over, not the amplifier's (21 Oct 2026)

**Why.** AJ3G: "We need to speed up the recovery time on the scope after TX." His log of three RIG TUNEs (the "transmit:" lines from decision 48's logging) showed, after each unkey, the radio's FP and GI gone within a round, and then the HR500's DRV reading falling over most of a second: 5.9, 5.3, 2.4, 0.6 W, then nothing, 0.6 to 0.8 s after the radio. The scope's hold waited for it, then for the half-second guard: about 1.3 s after the radio was back on receive.

**What the panel does.** The display hold (decision 40) follows the radio alone when the CAT link is up (`ScopeRig::on_air`: panel TUNE, FP or GI); only with no radio connected does it fall back to the amplifier. The guard after it is 0.3 s instead of 0.5. Everything else that waits for the end of an over still waits for the amplifier too: tuning stays locked (interlock #9), settings aren't sent (#5), and the relay keys stay greyed (#2) until its drive reads nothing. So the scope moves again a little before the tuning unlocks.

**Where.** `scope/mod.rs` (`ScopeRig::on_air`, `TX_GUARD_SECS`), `main.rs` (`scope_window`).

## 53. RADIO's knobs turn, push and hold as the FDM-DUO's own, in silver (22 Oct 2026)

**Why.** AJ3G: "The large Knob should be a VFO with a Push and Hold Feature. Same with E1/E2 knobs. Turn them the same color silver as shown online. Also invoke their features. with VFO knob when hovering over it allow the vfo to be turned up or down." And: "Make them look similar in size as the elad manual." The knobs were dark and only the main knob did anything, tuning with the wheel.

**What each does.** From the DUO's manual (its knob table, receive and VFO sections). Turning is the wheel over the knob (a notch a step, a trackpad 40 points a step); the main knob also turns by dragging round it, 24 steps a turn, clockwise up. A click is the radio's short press; held past the radio's hold time it is the long press, which fires while still held as on the radio. The hold time is the radio's menu 71 (`HT`), 1 s, the manual's default, until it answers.

| Knob | Turn | Push | Hold | Over CAT |
|---|---|---|---|---|
| Main | tune the receive VFO by the step; in MEM, the next or previous channel | STEP: turning chooses the step, push again to leave | digit by digit: turning changes the lit digit, E1 and E2 move it, push or hold to leave | FA/FB, MC |
| E1 | change VOL, SQL, AGC/manual gain, NR or NB | the next of those, in the radio's order | lock (below) | VM, SQ, GC, NR+NC, NB+NK |
| E2 | change the filter, the CW pitch (CW only) or the RIT offset (RIT on only) | the next of those offered | lock (below) | RF, PI, RC+RU/RD |

- **Panel-side, with no CAT command:** the tuning step (the radio's step and QuickStep encodings aren't known, so the step is the panel's, a dial digit from 1 Hz to 1 MHz); the STEP and digit-by-digit modes; which setting E1 and E2 have selected; and the lock. The radio locks its keys and knobs when E1 and E2 are pressed together; a mouse can't press two, so holding either toggles a panel lock of the knobs and the dial. The radio's own lock has no CAT command, so it is neither set nor read.
- **Not mapped:** the AGC speed and manual gain level (GS's set encoding is unconfirmed, so it is read only here, decision 20); E2 walking the radio's settings menu (MENU is a list here); the main knob's coarse menu changes; VFO>MEM and memory delete with the knobs; the transmit-time functions (E1 mic gain or CW speed, E2 power and the TX display), since nothing is changed while the radio transmits. The FM filter has no known codes, so E2 says so in FM.
- **Safety.** While the radio transmits or isn't connected the knobs grey and take nothing (interlock #5, panel side); the link refuses the frames then anyway.

**Look.** Satin aluminium: a radial gradient from mid grey to a soft highlight at the top left, as the keys are lit, faint concentric machining rings, a knurled darker rim, and a near-black mark that turns with the knob. Under the pointer it lightens and wears a thin blue ring (neutral), with the pointing-hand cursor. The radio window keeps one look whatever the panel's look, so ELAD and SDRoxide both show silver.

**Size.** The manual's front-panel page could not be measured: the 76-page PDF in the uploads is the HR500 assembly manual, not the DUO's, and the DUO's manual and photos could not be fetched (the network refused). The sizes follow the DUO's face as published (about 178 by 70 mm) and its layout: the main knob's diameter is up to 0.58 of the LCD's height (held to what fits over E1 and E2), and E1 and E2 are 0.4 of the main knob. At scale 1 that is a 104-point main knob and 42-point E1 and E2 beside a 184-point LCD (were 92 and 40 beside about 192). The knob column is 140 points wide (was 104), so the LCD's digits are about a tenth smaller; they stay well readable. To be checked against the manual's drawing when it is to hand: `MAIN_OF_LCD` and `SMALL_OF_MAIN` in `face.rs`.

**Tested by** `radio::knobs::tests`: the wheel and drag mappings, push against hold at the hold time (and a drag cancelling both), menu 71's hold time, the step walk, E1's and E2's lists, and the frames E1 and E2 send. `tools/fake_duo.py` now answers and takes VM, SQ, GC, NR, NC, NB, NK, PI and MC, and answers HT.

**Screen.** `screens/radio-knobs.png`: the demo panel with the fake DUO, E1 turned to VOL 37, the pointer over the main knob.

**Where.** `radio/knobs.rs` (the logic), `radio/face.rs` (`knobs`, `silver_knob`, `knob_input`), `radio.rs` (`RadioUi::knobs`).

## 54. The skimmer: DECODE's stations labelled on the scope, and SKIM WIDE for the rest of the span (22 Oct 2026)

**Why.** AJ3G: "Using Decoder add a "Skimmer" Function on the decodes. If you need to know look at SDR Oxide for more information."

**What SDRoxide does.** Its skimmers (the `sdroxide-skimmer` crate) run on the wide I/Q, not on the receiver's audio: a streaming 4096-point STFT over the whole skim window (about 200 kHz) finds every CW, PSK31 and RTTY carrier, tracks each one, decodes it in place from the FFT's bins (a per-track CW decoder, or the bins as a filterbank for BPSK and FSK), pulls a callsign out of the text, and reports each track as a spot about four times a second. The panadapter draws each spot as a box beside its signal along the newest edge of the waterfall, in staggered lanes so close signals stack, fading over 5 s once the signal stops; clicking one switches to its mode, tunes onto it and opens that mode's panel. Its FT8/FT4 "station boxes" are separate: they come from the FT8 decoder's own list (the receiver's passband only), one box per callsign at the dial plus the decode's audio frequency, the newest slot solid and the slot before dimmed, gone 45 s after the last decode; a click there sets the transmit audio offset instead. It does not decode several FT8 segments at once.

**What the panel does.**
- **SKIM** (in DECODE, beside COPY ALL; on to start, saved). While a slotted mode decodes (FT8, FT4, WSPR, JT65, JT9, FST4-60, Q65-30A, MSK144), every line that names its sender becomes a label at the radio's own frequency of the signal (the dial plus the AF, or less it in LSB; the scope adds the converter and the calibration as for everything else it shows). One label per station: its newest decode. A slot's decodes are the newest through the next slot, then fade (full, 60 %, 35 %) and are gone after three slots (`scope/skim.rs`: `age_slots`, `alpha`, `labels`).
- **On the waterfall**: a box with the callsign beside the signal, a leader down from the waterfall's top to it, in up to five lanes from the top (where the newest rows are), so a box never covers another's signal or box; one that finds no lane is left out (`skim::lanes`). **On the spectrum**: the callsign turned to read upwards, standing on a tick at the signal over the frequency labels, on a dimmed backing. Neutral states are blue (AJ3G's rule): a station calling CQ is in the blue, the rest in the screen's ink; the box's edge is blue. The labels' size follows the skimmer font setting.
- **Pointing at a label** says the message, SNR, AF and dial, the frequency and the UTC. **Clicking DECODE's own label picks its line** in DECODE (alone, as a click on the line does) and scrolls it into view. A DECODE line doesn't tune anything when clicked, so neither does its label: in FT8 the station's frequency is where it transmits, not where the receiver should go.
- **SKIM WIDE** (FT8 and FT4, with SKIM; off to start). The equivalent of SDRoxide's wide skimmers for the panel's modes: the mode's other dials (from the digimodes table, decision 39) that fit in the I/Q span, nearest the centre first, up to 8, are decoded too. Each dial gets a lane (`decoders::skim::SsbLane`: its 60–2940 Hz cut from the I/Q at 3.2 kHz complex, brought to 12 kHz and moved up by 1500 Hz, so the real part is the upper sideband with nothing from below the dial folded in) and a WSJT decoder of its own; one thread does all the lanes behind a dropping queue, from the I/Q tap "skim". The dials are worked out again each frame and the skimmer started again when they change (a SPAN change, a recentre, another band). Its stations are labelled like DECODE's; a click on one tunes the receiver to that dial. They are not added to DECODE's list, to ALL.TXT, to PSK Reporter or to WSJT-X UDP: those are the receiver's decodes, at its dial. DECODE's info line says which dials are skimmed. At 192 kHz there is no other FT8 dial in reach; at 3.072 or 6.144 MHz there usually is (6.144 MHz around 16 MHz: 14.074 and 18.100).
- **Both FT8 and FT4** (22 Oct 2026). AJ3G: "When I go to FT4 all FT8 spots stop on SKIM Wide Mode… All modes covered in SKIM WIDE mode should appear." SKIM WIDE now runs a skimmer for each of FT8 and FT4 on their dials in the span, the receiver's own dial left out, so at 192 kHz on 20 m FT8 on 14.074 has FT4 on 14.080 labelled beside it and the other way round. Each label carries the mode it was heard in (`SkimSpot.wsjt`): it fades by that mode's slots, the offset bar reads its width from it, and a click on it switches DECODE to that mode as it tunes to its dial. CW, PSK31 and RTTY stay with SKIM CW/PSK/RTTY (decision 56), which works alongside.
- **Not while transmitting.** A wide label tunes nothing while the radio transmits (interlock #9, `pointer_on`); picking a DECODE line is allowed, it changes nothing on the radio.

**Why not SDRoxide's whole skimmer.** Its CW, PSK and RTTY skimmers are a few thousand lines with their own STFT tracker and, for CW, a neural decoder pool; porting them was not asked for, and the panel's decoders are the WSJT modes'. The WSJT decoders, the I/Q taps and `IqChannel` were already here, so a lane per dial was a modest step; a wide CW/PSK/RTTY skimmer is not, and is left for later.

**The demo band has FT8 now.** So the labels, DECODE and the offset bar can be tried with no radio, the demo I/Q carries seven made-up stations on 14.074 MHz (textbook calls: K1ABC, W9XYZ, WA1XYZ, G4ABC, VE3ABC, N0XYZ, JA1XYZ), four in even slots and three in odd, calling and answering, 8-FSK with eased tone steps and ramps (`decoders::demo`, added in `rx.rs`'s demo source). They appear when the span reaches 14.074 MHz, so picking FT8 in DECODE (with the snap) brings them in. One decode in about thirty through the receiver's audio was a false one in the screenshot runs (a nonsense "[FD]" message); through the skimmer's lane none were seen.

**Screens.** `screens/skimmer-offset.png`: the demo band, DECODE on FT8, the scope zoomed on 14.074, the offset bar on WA1XYZ.

**Where.** `decoders/src/skim.rs`, `decoders/src/demo.rs`, `hardrock-panel/src/scope/skim.rs`, `scope/paint.rs` (`paint_skim_spectrum`, `paint_skim_waterfall`, `pointer_on`), `scope/mod.rs` (`Scope::skim`, `ScopeOut::skim_pick`), `decode.rs` (`skim_feed`, `wide_dials`, `Decode::wide_frame`, `pick_row`, the SKIM chips), `rx.rs` (`DemoBand`), `main.rs`.

## 55. The offset bar: a measuring cursor that reads its distance from the dial (22 Oct 2026)

**Why.** AJ3G: "Also look to place a bar that can be moved through the waterfall and spectrum which shows the frequency offset from VFO frequency so I know how far up a specific signal is. As an example look at DECODE and the HZ off it displays."

**What the panel does.**
- A thin vertical bar (1 px, 1.5 while dragged) down the spectrum and the waterfall, in red: AJ3G, 22 Oct 2026, "change that to RED, and thin it up a bit that is where I will be placing my TX signal". Red is a named exception to red-for-warnings-only, as the slot lines are (decision 51); it was the neutral blue at first. On the spectrum a flag at its top reads its **offset from the dial** and its **frequency** ("+1 315 Hz · 14.075 315.2"), with a **×** that takes it away; on the waterfall the offset alone, at the foot. Off the view, the flags are pinned to that side with an arrow.
- The offset is DECODE's AF: the audio a signal under the bar makes in the receiver, `bar − dial` in USB (and CW), `dial − bar` in LSB, in whole hertz, signed (`scope/offset.rs`). The frequency is to 0.1 Hz as the markers'.
- **Placing it**: shift-click anywhere on the spectrum or the waterfall (and keep the button down to drag it there), or **OFFSET** in the Markers row, which drops it in the middle of the receiver's passband (1600 Hz up with FT8's 200–3000). Within 4 pixels of a skimmer label it goes onto the station's own frequency, so it reads exactly the AF DECODE gave. **Moving it**: drag it (the pointer shows a resize cursor on it); the receiver's handles win where they overlap, as for the markers. **Removing it**: its ×, or OFFSET again (lit while there is a bar, "OFFSET ×").
- It **retunes nothing**: no click, drag or chip of it sends anything to the receiver or the radio, so it works while transmitting as the markers do. It stays on the frequency it was put on, as a marker does, so the signal keeps its bar when the dial moves and the reading follows the dial. It is not saved between runs (nor are the markers).

**Why a bar of its own and not a third marker.** M1 and M2 read level as well as frequency, can track a peak, are placed by right-click and give M2 − M1; a marker is about the signal, and their pair is about each other. The offset bar is about the dial: its reading changes when the dial moves and not when the signal's level does, and in LSB it counts the other way. Making it a marker would have taken one of the two away from a level measurement, or put a third reading with a different meaning in the same readout. It shares their formatting (`fmt_tenth`) and their place in the Markers row.

**Where.** `hardrock-panel/src/scope/offset.rs`, `scope/paint.rs` (`paint_offset`, `pointer_on`), `scope/controls.rs` (OFFSET), `scope/mod.rs`.

## 56. SKIM CW/PSK/RTTY: the keyboard modes found and read across the I/Q (22 Oct 2026)

AJ3G: "Yes do skimmers", meaning SDRoxide's wide CW, PSK31 and RTTY skimmers.

- **Finding.** `decoders::keyskim` runs a streaming FFT over the I/Q window, up to 192 kHz (a wider I/Q is cut down to 192 kHz around its centre), in bins of about 47 Hz at 375 frames a second. Each bin's power is smoothed and set against a median noise floor per 3 kHz region; a peak that holds 4 dB over the floor for 80 ms becomes a track. The detector, the floor and the track bookkeeping follow SDRoxide's (`crates/sdroxide-skimmer/src/cw.rs` and `digi.rs`, GPL-3.0-or-later, credited in the file). The WSJT modes' sub-bands (each dial and 3 kHz above it) are never tracked: FT8 reads as PSK31 garbage.
- **Telling the modes apart.** SDRoxide gates each skimmer to its band-plan segment. This one looks at the signal's first 1.2 s instead. A second tone 170 Hz away, keyed against the first, is RTTY. A carrier that spends more than 12 % of the time in gaps of 40 ms or longer is CW. Anything else is PSK31.
- **Reading.** CW goes from its bin's magnitude through SDRoxide's timing decoder (`decoders::morse`). PSK31 and RTTY each get a lane cut from the I/Q, at 500 Hz and 2 kHz, read by the panel's own demodulators (`BpskCore`, and `RttyRx` with its AFC and unshift-on-space), the same ones DECODE uses. The callsign is the last callsign-shaped word read.
- **Cost.** One thread, at most 32 tracks (strongest first), behind a queue that drops blocks rather than make the I/Q wait; DECODE shows the tracks and the load.
- **Measured** on a synthetic band (4 CW at 16 to 30 WPM, 3 PSK31, 3 RTTY, one over each, 26 s): from +10 down to -3 dB SNR in 2.5 kHz, every station's call is read (4/4, 3/3, 3/3), with no wrong calls. At -6 dB, CW 0/4, PSK31 3/3, RTTY 1/3; at -9 dB, PSK31 3/3; nothing at -12 dB. CW's first letter is often misread at an over's start (NQ for CQ), so the callsign, not the text, is what the label shows. Not yet tried on air.
- **Labels.** They share decision 54's look and rules: callsign plus mode, blue for CQ, lanes along the waterfall's top. They fade over 5 s once the signal stops, not by slots. A click sets the receiver to CW on the carrier (at the passband's middle, 600 Hz), or USB with a PSK31 or RTTY signal at DECODE's audio frequency, and DECODE to PSK31 or RTTY. Never while transmitting (interlock #9).
- **Toggle.** SKIM CW/PSK/RTTY in DECODE, off to start, saved, in any mode, while DECODE is open. Nothing it reads goes to PSK Reporter, WSPRnet, ALL.TXT or WSJT-X UDP.
- **Demo.** The demo band has made-up CW, PSK31 and RTTY stations on 20 m (`decoders::demo::DemoKeys`).


## 57. One volume: the radio's VM and the panel's Sound follow each other (22 Oct 2026)

AJ3G found E1 on VOL did nothing he could hear, and asked for the volume controls to coordinate. The radio's own audio is live at 192 kHz SPAN; wider, the radio mutes and the panel's receiver plays. So E1, RADIO's VOLUME and the radio's own knob (CAT `VM`, 0 to 100) and SCOPE's Sound slider (0 to 1) are kept as one (`rx.rs`, `VolSync`). The radio's read-back wins when the panel connects. A change to Sound is sent to the radio as VM, at most every 150 ms while it is dragged. The old read-back doesn't pull the slider back while the new value is awaited, up to 1.5 s; after that the radio's word stands. Nothing is sent while transmitting (the link refuses it then too, #5). Tested by `the_radio_volume_and_the_panel_volume_follow_each_other`.

## 58. JTTY: WSJT-X 3.2's keyboard mode, from mfsk-core (22 Oct 2026)

AJ3G: "We need to get a JTTY decoder … I want the best JTTY decoder available", and "use those frequency assignments and bandwidths to set the scope when that mode is invoked".

- **The decoder.** mfsk-core's `jtty` (0.12 and later), by the author whose crate the panel already uses for every WSJT mode. It is a port of WSJT-X 3.2.0-rc1's `lib/jtty/`, and its author reports it agrees with WSJT-X's own decoder on every one of 360 trials, 7 to 9 times faster. The other candidates were weaker. An iPhone port (morria/jtty) has no licence. SDRoxide's PR 597 is unmerged and has no off-air decode. (Research: `notes/jtty-research.md` in the project files.)
- **Two mfsk-cores.** The rest of the WSJT modes stay on mfsk-core 0.10, whose decode API later versions replaced. JTTY comes from 0.13.1 under the name `mfsk-jtty`, GPL-3.0-or-later as 0.10 is. Moving everything to one version is a separate change, with FT8, WSPR and the rest checked against their samples.
- **Receiving.** `decoders::JttyDecoder` puts the receiver's 48 kHz audio into mfsk-core's `jtty::rx::Stream` at 12 kHz, on a thread behind the usual dropping queue. The stream times frames by its sample count, so a dropped block is filled with silence; a gap over 5 s starts it afresh. The defaults are WSJT-X's: Rx 1500 Hz ±50 (DECODE's Rx), side channels, 200 to 2800 Hz searched, subtraction and the re-sweep on. Each message is one DECODE line, rewritten as its frames come, ending "…" until its end-of-message frame. Tested on mfsk-core's own transmitter in noise (`a_jtty_over_in_the_receivers_audio_is_read_whole`). WSJT-X's sample recording couldn't be downloaded from this container; mfsk-core's own tests decode it.
- **The dials.** WSJT-X 3.2's preliminary JTTY frequencies (DXZone, Sept 2026): 1.838, 3.575, 7.090, 10.140, 14.090, 18.100, 21.090, 24.920, 28.090, 50.160 and 144.160 MHz. They are a DATA key and band-plan markers on the scope, and DECODE's snap goes to them: USB, 200 to 2800 Hz. The keyboard skimmer leaves them out, as it does the other WSJT sub-bands.
- **Not yet.** Transmit, skimmer labels for JTTY, ALL.TXT, and N1MM's MMTTY-style link.


## 59. Background GPU submits wait for the UI: windows that resize and move between screens (22 Oct 2026)

**Why.** AJ3G: windows stutter and hang when dragged between his two screens (hold #18), and the panel "has become unstable and glitching" until another window was slid away; "moving panels from screen to screen is still awful. Please commit resources to fixing this." His log had `Surface::configure: Failed to wait for GPU to come idle before reconfiguring the Surface`, then the scope drawing at 4079×3110 into a 2719×2073 target (1.5×, the other screen's scaling). wgpu reconfigures a window's surface, on every resize and on a change of screen, only with the queue idle, and gives up if another thread submits while it waits (wgpu-core 30, `device/resource.rs`). The scope's I/Q uploader (`scope-gpu-iq`) submits every few milliseconds, so a resize almost always lost that race: the surface stayed at its old size, egui drew for the new one, and every frame after failed validation and was dropped until some later resize happened to win.

**What.** `gpu_gate`: threads other than the UI's (the I/Q uploader, the SCF view's engine) submit through `gpu_gate::submit`, which goes ahead only while the UI thread is in the app's own code: opened at the top of `App::ui`, closed at its end, and shut around each `show_viewport_immediate` (RADIO, SCOPE, DECODE), since eframe paints those windows, and may reconfigure them, inside that call. `close` waits for a submit under way. With no frame for 250 ms (nothing on screen being repainted) a waiting submit goes ahead, so the stream never stalls behind a hidden window. The uploader publishes its write index only after its submit, as before, so the scope's reads still see the samples they count on. Tested by `a_background_submit_waits_for_the_ui_and_never_runs_while_it_is_shut`.

## 60. SKIM LOG: everything skimmed, every mode, in a window of its own (22 Oct 2026)

**Why.** AJ3G: "In SKIM Wide you should add another decode window that covers all modes and reports what mode each decode was collected from in addition to the data that is there. For instance I am on FT4 and I am seeing SKIMs from FT8, but only the FT4 SKIM gets put into the decode log."

**What.** A SKIM LOG chip in DECODE opens a window of its own (`skimlog`): one list, newest first, of the receiver's own WSJT and JTTY decodes (RX), SKIM WIDE's FT8 and FT4 on the other dials (WIDE), and SKIM CW/PSK/RTTY's signals that sent a callsign (SKIM), each with its UTC, mode, where it came from, true RF, SNR and message. Chips filter it by mode; COPY ALL copies what is shown. A keyboard signal is one line, rewritten as its text grows. Every line also goes to `SKIM.TXT` beside `ALL.TXT` (a keyboard one once its signal stops, with all it sent). DECODE's own list, `ALL.TXT`, PSK Reporter and WSJT-X UDP stay the receiver's decodes only, as decision 54 keeps them.

## 61. AUTO ID: what mode is the signal in the passband? (22 Oct 2026)

**Why.** AJ3G: "When I place my filter width over a signal of interest, go through all modes and attempt to identify the mode, I.e. RTTY, Packet, PSK, FT8 MSK, etc". He agreed to measuring the signal and trying the decoders, a ranked list, and a USE key.

**What.** An AUTO ID chip in DECODE (`decoders::autoid`). While it is on, the receiver's audio inside its passband (either sideband; `audio_band`) goes to an `AutoId`, which does two things at once.
- **Measures** the signal on a thread of its own, every 2 s once it has 6 s, over the last 32 s at 12 kHz. It finds the occupied width (5 to 95 % of the power above the noise), the tones and their spacing, the keying rate (the lowest strong line in the envelope's or the frequency steps' spectrum that the strongest one is a whole multiple of), and whether the envelope is constant, on-off or varying.
- **Tries the decoders** that could fit: FT8, FT4, JT65, JT9, WSPR, MSK144 and JTTY from the start, and RTTY 45/50/75 (both ways round), NAVTEX, PSK31, Olivia and THOR retuned to the measured centre.

Each of 25 signatures (CW, PSK31/63/125, RTTY, NAVTEX, Packet 300/1200, the WSJT modes, Olivia, THOR, MFSK16, Hellschreiber, SSTV, WEFAX, voice, a carrier) is scored on how well the measurement fits it. A decode is near-proof (99 %), and text is scored by how much it reads like a QSO (callsigns, the usual words). The best eight are listed with why and what was read. USE sets DECODE's mode, its audio frequency and Reverse from the guess, without snapping the radio, since the signal is where it is, and turns AUTO ID off. Moving the passband or the dial starts it again. Packet, MFSK16, Hellschreiber, PSK63/125, CW and voice are named but have no decoder in DECODE, so they have no USE. DECODE's RTTY is 45.45 baud, 170 Hz, so a 50 or 75 baud guess runs it at 45.45. Nothing it hears goes to ALL.TXT, the spots or WSJT-X UDP. Tested on generated RTTY, PSK31, FT8, CW, 1200 baud packet and noise (`decoders::autoid` tests) and by `auto_id_hears_the_passband_on_either_sideband_and_its_picks_map_to_decode`.

## 62. SKIM CW/PSK/RTTY: a CW speed range, CW labels held twice as long, the view in a wide span (22 Oct 2026)

**Why.** AJ3G: "Increase persistence on the spot by double… Just double on CW", "when Samp_Rate or SPAN increases, spots stop working", and "I think you are setting the SPEED too low… Maybe we need to add a speed adjust for a range that works."

**What.**
- **Speed.** The CW decoder searched 6 to 70 WPM, but a spot measured above 45 WPM was dropped, so fast stations never got a label (generated CW at 50, 55 and 60 WPM was read and then dropped). DECODE now has a CW WPM range beside SKIM CW/PSK/RTTY, 8 to 60 to start and saved. It bounds the speeds each track's decoder searches (`CwDecoder::set_speed_range`) and the spots kept (`KeySkimConfig::cw_wpm`). Tested by `cw_from_15_to_60_wpm_is_read_and_the_range_set_leaves_the_rest_out`.
- **Persistence.** A CW label fades over 10 s once its signal stops (`CW_FADE_S`); PSK31 and RTTY stay at 5 s. The skimmer drops a silent track after 6 s, which cut every label off then, so DECODE keeps each signal's last spot until its label has faded (`key_held`).
- **Wide spans.** Above 192 kHz the skimmer reads a 192 kHz cut of the span, meant to be around the scope's view (decision 56), but the panel never told it the view, so it was cut around the span's centre and missed everything else. DECODE now passes the scope's view, to the nearest 5 kHz, at spans over 192 kHz only. Tested by `in_a_wide_span_the_signals_in_the_scopes_view_are_read`.
- **Not changed.** At very wide spans a WSJT dial is a pixel or two wide and at most five labels stack in one place (`MAX_LANES`), so most FT8 labels are left out until the scope is zoomed in.

## 63. A WSJT slot goes on the dial it was heard on (22 Oct 2026)

**Why.** AJ3G: "When I switch bands from 40M to 20M I believe the spots from 40M carried over to 20M." They did. A slot's decodes come a second or two after it ends, and DECODE put them on the dial as it was then. So the slot heard on 40 m before a change to 20 m was labelled on 20 m, written to ALL.TXT at 20 m's dial and spotted to PSK Reporter there.

**What.** DECODE keeps where the receiver's dial has been over the last ten minutes (`DialLog`). Each slot goes on the dial it was on for the whole slot. A slot the dial moved during, half on one band and half on another, is dropped and logged rather than labelled, written or spotted anywhere. Tested by `a_slot_goes_on_the_dial_it_was_heard_on_and_one_heard_across_a_band_change_is_dropped`.

## 64. The demo band on every band, with more modes; SKIM's FT8 and FT4 in every mode (22 Oct 2026)

**Why.** AJ3G: "we should be able to see demo on all bands, unless there is a compelling reason not to"; the demo had no FT4, JT9 or JTTY; of the pulsing voice in the middle, "May want to label that as SSB FAKE"; and "Even if I click on PSK, you should still continue to demod and report other modes … skims, and tag them as you had been."

**What.**
- **Every band.** The demo's stations are written for 20 m and moved to the band nearest the centre, each mode to that band's own dial from the band plan (`demo_shift` in `rx.rs`). CW goes the same distance up from the band's foot. A mode with no dial on a band is left out there.
- **More modes.** FT4 on 14.080 (three stations), JT9 on 14.078 (a CQ in even minutes) and JTTY on 14.090 (an over every 30 s) join FT8, CW, PSK31 and RTTY. JT9 and JTTY are made by mfsk-core's own transmitters as audio and put on the band as an upper sideband (`decoders::demo::DemoAudio`). Tested decoding: `the_demo_ft4_stations_on_14_080_are_decoded`, `the_demo_jt9_station_is_decoded`, `the_demo_jtty_station_is_read`, and end to end `demo_band_on_20_m_decodes`, `demo_band_on_40_m_decodes`, `demo_band_followed_from_40_m_to_20_m_decodes` (real time, ignored by default).
- **Fakes labelled.** The five filler signals (two USB voices, one LSB, a CW beacon, a carrier) carry nothing to decode. In the demo they are labelled SSB FAKE, CW FAKE and CARRIER FAKE where they are.
- **SKIM in any mode.** With DECODE in a mode other than FT8 or FT4 (a PSK31 label clicked, say) and SKIM on, the wide skimmer decodes every FT8 and FT4 dial in the span, the receiver's included, so their labels stay. In FT8 or FT4 it is SKIM WIDE as before (decision 54). Tested by `skim_wide_takes_the_modes_other_dials_and_the_other_modes_in_the_span` and `demo_band_in_psk31_still_labels_ft8_and_ft4`.

## 65. GLOBE: the station, the stations heard, the grey line and propagation (22 Oct 2026)

**Why.** AJ3G asked for a globe like SDRoxide's pop-out, "serious" rather than SDRoxide's look, with no satellites or planets: the station, lines to the stations heard (the DECODE log), the weather, and a propagation heat map. Then: "Clock Local and GMT", countries and states drawn better, major cities with city lights, daylight and the grey line made obvious, and "a chip that makes the globe flat".

**What.**
- **A window like SCOPE and DECODE.** The GLOBE chip opens it, docked or popped out. The picture (land, day and night, the grey line, IR clouds, the aurora, PROP and city lights) is an equirectangular 2048 × 1024 image painted on a thread of its own only when one of those changes, at most every 3 s. On the globe it is wrapped on a sphere mesh; FLAT shows it as a map centred on the station's longitude. The coasts, borders, state lines, graticule, Maidenhead grid, terminator (solid at sunset, dashed at −6°), paths, stations and city names are drawn over it each frame.
- **The map** is Natural Earth's 1:50m land, country borders, states and provinces, urban areas (the lights) and populated places (the names), public domain, simplified and kept in `assets/globe/` (about half a megabyte in the program).
- **Relief** (AJ3G: "Can you show a little more relief on this rendering?" "Yes and make water blue, etc"). RELIEF, on to start, puts the Earth in natural colour under everything: NASA's Blue Marble Next Generation (public domain), its land's relief drawn out with Natural Earth's shaded relief, 2048 × 1024 as a 400 kB JPEG (`assets/globe/earth.jpg`, decoded with zune-jpeg). The night side is darkened further over it. Off, the land and sea are the flat dark blues as before.
- **Moving it** (AJ3G, after the browser preview: "Ok you are clear to build that feature in now."). SUN: LIVE, PLAY (an hour a second, round from +12 h to −12 h) and a ±12 h slider move only the Sun, so the grey line, night and city lights move while the paths, PROP, clouds, aurora and clocks stay live; the picture is painted again as fast as its thread allows while the Sun is moved, and every 3 s otherwise. Not saved: GLOBE opens LIVE. SPIN turns the globe 6°/s east (not while dragging). HOME, DX, PATH and a click in HEARD turn the view over 0.7 s instead of jumping. Pinch zooms like the wheel. None of it touches the radio. Tested by `play_runs_the_sun_an_hour_a_second_and_comes_round` and `a_turn_ends_where_it_was_sent_and_spin_waits_for_it`.
- **BAND follows the receiver** (AJ3G, 23 Oct 2026: "Globe no longer picking up decodes", then 24 Oct: "On the globe, band should track with my RX"). ONE BAND was left on 20 m while DECODE ran on another band, so the map showed none of its stations. BAND now moves when the band being listened to changes: the panel's receiver when it runs, else the connected radio's dial (as shown, with the transverter), else DECODE's latest station's band. A band picked by hand holds until that band next changes. Tested by `one_band_follows_the_receiver_but_keeps_a_hand_pick`.
- **BAND CONDITIONS and IBP BEACONS** (AJ3G, 24 Oct 2026: "analysis of what the Conditions on all bands should be present like SDROxide"). One table for every band, after SDRoxide's BANDS window: HamQSL's word for the band's group in the station's half of the day, FORECAST's word for this hour, and from the field what was heard (PATHS, the decayed receptions; REACH; BEST, the best margin over the mode's decode floor). It replaces the BANDS OPEN bars and the separate HamQSL table. IBP BEACONS (`globe/ibp.rs`, ported from SDRoxide under the GPL) gives the NCDXF schedule: eighteen beacons, 10-second slots, a 180-second cycle aligned to UTC midnight. Tested by the `globe::ibp` tests.
- **FORECAST and HAMQSL** (AJ3G, 23 Oct 2026: "Where is the Band Activity (prediction) graph." then "All data relating to actual CONDX"). FORECAST (`globe/forecast.rs`) charts each band hour by hour for 24 hours at the station from a deliberately simple model: MUF(3000) between a night value (5 + 0.035·SFI MHz) and a day value (8 + 0.14·SFI MHz) with the Sun's light on the ionosphere, LUF 1.5 MHz plus D-layer absorption with the Sun overhead, Kp over 3 lowering the MUF 4 % a step and over 4 raising the LUF; a band is open from just over the LUF to the FOT (85 % of the MUF). The nearest KC2G sounder within 3000 km scales it now, fading with a six-hour time constant. It is labelled MODEL and is no substitute for VOACAP. HAMQSL (now the CONDX column of BAND CONDITIONS) fetches hamqsl.com's solarxml.php hourly (as N0NBH asks) for its calculated day and night conditions per band group, noise and geomagnetic field. Good, Fair and Poor are shown in cyan, white and grey, not green, yellow and red. Tested by the `globe::forecast` tests and `hamqsl_gives_each_band_group_day_and_night` (a reply shaped from HamQSL's documented format; the container cannot reach hamqsl.com).
- **The Sun.** The subsolar point by the Astronomical Almanac's low-precision formula (`globe/sun.rs`), tested against the 2026 equinox and solstices and the equation of time.
- **Stations heard.** DECODE's lines, SKIM WIDE's and the keyboard skimmer's stations go to the globe (`Decode::globe_feed`) when their grid is known, from the message or from an earlier one (`Heard`). Kept for an hour, whether or not GLOBE is open. A line in HEARD or a dot on the map shows the path, distance, bearings, hops and the Sun's height at both ends; TUNE goes to that mode's dial on its band through the same snap as a DATA key (interlocks #5 and #9 unchanged); SHOW IN DECODE brings its line into view.
- **PROP.** Each path is evidence that the band was open at the middle of each ~3000 km hop, weighted by how far its report was above its mode's decode floor (FT8 −21 dB, WSPR −28 dB, …) and halving every HALF-LIFE (45 min to start). The evidence is spread over 2° cells with an 8° Gaussian. ONE BAND paints one band blue to white; ALL BANDS paints each place in the colour of its strongest band. Neither uses yellow or red, which stay the warnings'. BANDS OPEN shows each band's share of the world reached. Tested by the `globe::field` tests.
- **Fetched, only while GLOBE is open,** on one thread (`globe/fetch.rs`), identifying itself as `hardrock-panel/<version> (callsign)`: NOAA SWPC's Kp and its forecast, F10.7, X-ray and solar wind (5 minutes to hourly); SWPC's OVATION aurora (15 min); KC2G's ionosonde MUF(3000) (15 min); a NOAA nowCOAST infrared cloud mosaic (30 min); Open-Meteo's weather at the station (15 min); NWS alerts at the station (15 min, US only); HamQSL's solarxml.php (hourly); and, when PSK REPORTER is on (off to start), PSK Reporter's retrieve query for who heard your callsign in the last hour (5 min). The footer shows each source's age, or "failed" with the reason on hover. Nothing is sent anywhere but those queries. The parsers are tested with saved replies only; the tests never use the network.
- **Warnings.** Kp 4 and over is yellow, 5 and over red; an M flare yellow, an X flare red; Bz −10 nT or below yellow; an aurora chance of 25 % or more at the station yellow; a thunderstorm or an NWS alert yellow. Everything else is blue or white.
- **Not done.** The Reverse Beacon Network (a telnet feed needing a login) and a REPLAY of past hours are left out. The live formats of the fetched services could not be checked from where this was built, so a source may show "failed" until its parser is fixed.

## 66. RX slices: up to three more receivers in the span (22 Oct 2026)

**Why.** AJ3G uses SDR Console "because I can monitor multiple slices at one time": one parked on WSPR and one on FT4, each feeding a WSJT-X through a virtual audio cable, one on the speaker, and "I only transmit … from RX1 which is synched to the Transmit panel". Then: "What about RX Slices?" and "Build everything".

**What.**
- **In the pipeline.** `sdr::Command::SetSlices` runs more receivers on the same I/Q block as the main one, each a whole `Receiver` with the main one's decimator, passband shape, AGC, gain and calibration (`like_main`), its own dial, mode and passband, and its own sound feed (`SetSliceAudio`). Each one's level comes back in `Snapshot::slice_levels`. Tested by `slices_hear_their_own_dials`.
- **In the panel** (`slices.rs`). B, C and D, kept in `RxSettings::slices`, each with its dial, mode, passband, sound output, volume and mute. Only those inside the span are run (`slices::specs`); each opens its own sound output. PARK puts one on a mode's dial on the band. Drawn on the scope as a passband in its own colour (violet, green, pink: not A's blue, not the warnings' yellow or red).
- **A is the transmit slice.** Only A moves the radio, as before. SWAP A exchanges a slice with A through `Rx::tune` and `Rx::set_mode`, so it is refused while transmitting (interlock #9). DECODE decodes A, so swapping is also how a slice is decoded in the panel; SKIM WIDE already decodes every FT8 and FT4 dial in the span.
- **Not done.** A second band (the DUO's double-channel stream, needing a new driver mode), per-slice CAT for outside programs (a WSJT-X on a slice does not see its frequency over CAT), and DECODE on a slice other than A. Each slice costs about as much CPU as A.

## 67. CW in DECODE; sound outputs that come and go; PTT ON and OFF (24 Oct 2026)

AJ3G, 24 Oct 2026: "CW decodes are still an issue. I can see the CW but I get no decode", then that the Klipsch, turned on after start, wasn't recognised or played, that with another speaker picked the slider and mute should work for that speaker and only the ELAD's USB audio should go through the rig, and that the amp's PTT key should still say PTT.

- **CW in DECODE** (`decoders::modem::cw`). DECODE had no CW mode: a SKIM CW label tuned to the station and DECODE went on with its last mode. `CwRx` finds the strongest tone between 200 and 1500 Hz (a spectrum of the last second every half second, smoothed, a tone 10 dB over the median, moving to a new one 3 dB stronger or when the old one goes), mixes it to zero, low-passes it at 70 Hz into a 200 Hz keying envelope and reads it with the skimmer's own `morse::CwDecoder`, which fits the speed. A SKIM CW label and AUTO ID's CW guess now pick it. Tested by the `modem::cw` tests (22 WPM at 720 Hz at 0 dB SNR in 2.5 kHz, 12 and 35 WPM at other pitches, and noise alone) and on the demo's W2XYZ.
- **Sound outputs that come and go** (`rx.rs`, `Rx::recheck_audio`). Every two seconds an output that failed (`sdr::AudioOut::failed`, set by the stream's error callback), one that couldn't be opened, and "Default output" when Windows has a new default, are let go and opened again. A slice's output list reads the outputs again each time it opens, as the SETTINGS lists already did.
- **One volume only on the ELAD's audio.** Sound and the radio's VM follow each other (decision 57) only while the panel plays into the ELAD's USB audio (`sound_via_radio`); with another speaker, VM pulled the slider back. Tested by `only_the_elads_own_audio_ties_sound_to_the_radios_volume`.
- **PTT ON and PTT OFF.** The amp's mode key always reads PTT, its state after it, in both looks.
- **The DUO's power keys answer at once** (AJ3G, 24 Oct 2026: "the ability to go from 1, 3, 5, and max is buggy. Won't respond"). A key lit only when the radio's `TP` was read back, which waited for the settings sweep (some seventy reads, every 10 s): a click could look ignored for many seconds. The link now reads `TP` straight after setting it (`Protocol::read_power`), and the key clicked shows lit until the read-back agrees or 3 s pass; if the radio still reads another step then, a note says so and names the likely cause, another program setting its power over CAT. Tested by `a_power_step_set_is_read_back_at_once` and with `tools/fake_duo.py`.

## 68. GLOBE: who heard my WSPR, and the FT8 strobe (10 Nov 2026)

The transmit plan's items 2 and 3 (AJ3G, 10 Nov 2026).

- **WSPR HEARD ME** (`globe/wspr.rs`). A SOURCES chip, off until turned on as PSK REPORTER is. While GLOBE is open, every 5 minutes, the fetch thread asks wspr.live (the public, read-only ClickHouse mirror of WSPRnet's database) for every report with `tx_sign` = the station's call in the last hour; if that fails, WSPRnet's own spot search, as SDRoxide's "who heard us" poll asks it (ported, with its attribution). Only the callsign goes in the query, and only when it is letters, digits and `/`. Each report becomes a reverse reception, `Source::WsprHeard`, and keeps WSPR's drift, the power decoded, the distance and the site it came from for HEARD's hover and the side panel. Tested against fixed replies (the container can't reach either site).
- **Two marks.** A station AJ3G heard: a filled dot and a solid path in the decode colour (cyan), ← in HEARD. A station that heard AJ3G (PSK Reporter or WSPR): a hollow ring and a dashed path in the transmit colour (magenta, as the TX mockup), → in HEARD, its report and site named on hover. A station that is both gets both: the ring sits a size out from the dot. With ALL BANDS the paths take their band's colour, since WSPR results often mix bands; the dot and the ring still say which way. A legend under the map says which is which.
- **The FT8 strobe** (`Globe::set_qso`). The QSO's station is picked as a DECODE pick does, or, never heard, placed by its grid and turned to. A comet runs along its arc once a second (0.85 s running, then a rest so it reads as pulses): out from the station in the transmit colour while keyed (`StrobeDir::Tx`), back from the DX in green in their slot (`Rx`). Yellow and red stay for warnings. `None` stops it, and with no grid and no reception there is no strobe. It is drawn over the map, and the next frame is asked for (`repaint::animate`) only while it runs; the picture itself is still painted only when what it shows changes, at most every `PAINT_EVERY_S`, so the GPU's load is as it was.

## 69. LOG: the logbook, eQSL, LoTW and callsign lookup (10 Nov 2026)

**Why.** AJ3G's transmit plan (item 4, approved 10 Nov 2026): a LOG window on the top bar, an ADIF master file any logger can read, import from the loggers he uses, FILL MISSING from HamQTH now and QRZ.com once he subscribes, and eQSL and LoTW uploads only when he presses the button. TQSL and his LoTW certificate aren't installed yet, so the LoTW button stays greyed and says why.

**What.**
- **The master file** is `hardrock-panel-log.adi` beside the settings (`Logbook::default_path`). A QSO added is appended at once; an edit, a delete, an import, a lookup or a confirmation rewrites the file through a temporary file, and the file as it was before the first rewrite of a run is kept as `.adi.bak`. A file that can't be read is never written over (`an_unreadable_log_is_never_overwritten`). Fields the panel doesn't use are kept, so a log from another program loses nothing.
- **What the log knows** (`Logbook::worked`): a dupe (same call, band and mode), a new call, a new DXCC entity, an entity worked but not on this band, and a new grid square. Entities come from `cty.dat` (`dxcc.rs`, ported from SDRoxide with AD1C's country file), with its six WAE-only areas counted as their DXCC entity (Sicily is Italy). `cty.dat` has no ADIF DXCC numbers, so the log learns them from QSOs that carry one (imports, lookups) and fills them in.
- **Import** reads WSJT-X, Log4OM, N1MM and QRZ ADIF: the same call on the same band within 2 minutes of one already in the log (or earlier in the file) is skipped; field lengths counted in bytes or characters are both read; files in UTF-8, UTF-16, Windows-1251 or Windows-1252 are read, the code page named in the result (SDRoxide's reader). `SWL` records are left out. There is no file picker in the build: IMPORT reads the file named in the FILE box, or a file dropped on the window.
- **Export.** ADIF writes the selection, or the whole log, to the FILE box's file or to `hardrock-panel-export-<date>.adi` beside the log. FT4, FST4, FST4W and JS8 are written as ADIF's `MFSK` with their `SUBMODE`.
- **FILL MISSING** fills only empty fields: country, zones and continent from `cty.dat` at once, then name, QTH, state, county, grid, zones, DXCC and country from HamQTH (default) or QRZ.com XML (off until switched on in SETTINGS → Logbook), one call every 1.5 s (HamQTH) or 1 s (QRZ), on its own thread, with STOP. A `/` call takes only the name, since the lookup is of the home station.
- **eQSL** sends the selection, or every QSO not yet sent, through eQSL's ADIF import, with only what goes on the card (no comment). A QSO is marked ↑ only when eQSL says it was added or was already there.
- **LoTW** writes the QSOs to `hardrock-panel-lotw-upload.adi` beside the log and runs `tqsl -x -d -u -a compliant [-l location] file`; TQSL keeps the certificate and its password. QSOs are marked ↑ on TQSL's exit 0, 8 (all duplicates) or 9 (some). TQSL is looked for in SETTINGS' path, Program Files\TrustedQSL, the macOS app and PATH; without it the button is greyed with the reason.
- **QSL ✔** downloads the eQSL inbox and LoTW's confirmed-QSO report (with the LoTW website login, not TQSL) and marks matching QSOs ✔ (same call and band, the mode when both say, within a day).
- **Logins** (HamQTH, QRZ.com, eQSL, LoTW website) are in SETTINGS → Logbook and kept in the panel's settings file as plain text, as nothing else in the panel holds a secret yet. They are never logged: `Credentials`' debug print hides the password and every network error is scrubbed of it before it is shown or logged.
- **The window** groups the QSOs by UTC day, newest first, with + NEW ENTRY (the radio's frequency and mode, NOW), EDIT (or double-click), DEL (pressed twice), a WORKED BEFORE badge in the form and WB on a row worked before, FIND, and an L/E column for LoTW and eQSL (· not sent, ↑ sent, ✔ confirmed). It docks or pops out like GLOBE.
- **Tested** with fixed replies only, never the network: `logbook::tests`, `logbook::lookup::tests`, `logbook::upload::tests`, `logbook::net::tests`, `dxcc::tests`.
- **Wired by the TX build (decision 70):** logging from the TX window, the confirm card ("ask first") and the WSJT-X UDP "QSO logged" message. WORKED BEFORE badges show in the TX window's decode list; DECODE's own rows have none yet.

## 70. TX: FT8 and WSPR from the panel, the QSO sequencer and its decode list (10 Nov 2026)

**Why.** AJ3G's transmit plan (items 1, 3 and 5, approved 10 Nov 2026: "Ok execute on this build"), after he added an EFHW and can test on the air. The plan is notes/tx-plan.md in the project; the mockup, tx-mockup.png.

**What.**
- **A TX window** on the top bar between DECODE and LOG, docked or popped out. ENABLE TX is off at every start; HALT TX unkeys and turns it off; STOP QSO sends nothing more in the exchange.
- **The audio** is rendered at the sound output's own rate from mfsk-core's encoders (`tx/wave.rs`): FT8 as WSJT-X's `gen_ft8wave` (GFSK, BT 2, a raised-cosine ramp), WSPR's 162 four-tone symbols. It plays from silence up to 0.5 s (FT8) or 1 s (WSPR) into the slot (`audio::Player`), so a late start cuts the front off as WSJT-X's does.
- **Keying** is TUNE's path with `Keying::Data` (interlocks #6, #7, #8 and #12). Each mode has its own level, a share of menu 34 as the Tune slider is.
- **The sequencer** (`tx/qso.rs`) is SDRoxide's `qso.rs` ported without Fox/Hound or contest exchanges. The log point is AJ3G's: answering a CQ, their RR73, RRR or 73 after our R+report; calling CQ, our RR73 going out. Logged once; their repeats get the final message again.
- **Macros as WSJT-X:** Tx1 to Tx6 with Next (the radio button) and Now (the TxN button, or a double-click; Now while an over is going out changes the rest of that over), Generate Std Msgs, Skip Tx1, RRR, Auto Seq, Call 1st, TX even/1st, a Report box, Tx5's saved free-text lines. With Auto Seq off the picked line goes each slot.
- **The queue:** + on a decode; REPLY mid-QSO queues at the front (since decision 114 it leaves the QSO in hand instead); ALSO CALLING names can be queued; the next starts when the QSO is logged or given up, in the slot opposite where it was last heard; stale (10 min) and dupes are skipped with a note.
- **TX freq AUTO / MANUAL / HOLD** (`tx/freq.rs`): the bar on SCOPE is the TX audio while the TX window is open. AUTO keeps the over where it is while that is clear, else moves it to the middle of the widest gap of at least 60 Hz in 200 to 2 900 Hz among the signals decoded in our own period over the last two minutes, only between overs and not mid-exchange; the busy parts are shaded faintly on the scope. Dragging the bar on AUTO switches to MANUAL. The audio sent allows for VFO B in split, so the signal lands on the bar.
- **The decode list:** country, continent and heading/distance from cty.dat or the grid (dimmer when from the country's centre), log badges DXCC, BAND, GRID, NEW, DUPE (dupes fade), calling-you rows in the accent band with ▶, bold and pinned to the top of their turn, CQs open to us in green (a directed CQ only when it includes us), our own overs in the transmit colour. Sort by time, SNR, distance or country; CQ only, New only (never hiding a station calling us), CLEAR; a band change clears it.
- **GLOBE's strobe** follows the QSO under way: out while keyed, back otherwise (decision 68).
- **The log:** a QSO logged goes into LOG at once (or after LOG IT, with SETTINGS → Logbook's "Log a finished QSO without asking" off) and to WSJT-X UDP listeners as an ADIF record.

**Not done yet:** occupancy from the scope's own spectrum for AUTO (decodes only for now), a sound for "calling you", REPLY on DECODE's own rows, WSPR's own decodes in the TX list (they are on GLOBE).

## 71. TX window, round 2 (11 Nov 2026)

**Why.** What AJ3G asked for while testing the FT8 TX window on the air.

- **Our own overs aren't decoded.** The DUO's I/Q keeps flowing while it transmits, so DECODE heard AJ3G's "UN7GBX AJ3G -18" back at +25 dB in the slot he sent it, flagged NEW with a REPLY button. `main.rs` notes each frame whether the radio is keyed (`RigStatus::transmitting`, which includes the panel's TUNE path); `decode.rs` keeps those spans for 10 minutes (`KeyedLog`) and drops a WSJT slot whole, before its rows, ALL.TXT, spots, GLOBE and TX see it, when the radio was keyed for more than 1 s of it (a poll's lag or the unkey's tail can touch the next slot). SKIM WIDE's slots are dropped the same way, so a skimmed WSPR dial can't spot our own beacon. DECODE's slot line says "transmitting · decodes dropped".
- **One heading per slot** in the TX list: rows are grouped by slot, newest first, our over first in its slot, however late a slot's decodes come.
- **The DX's offset** beside their call in the QSO box ("1737 Hz", nothing more, from each decode of theirs); in WSPR the beacon's offset. `Tx::dx_mark()` gives the call, audio Hz and RF Hz for the scope's mark (wired by the scope's side).
- **Call after RR73** (`TxSettings::call_after_rr73`, on by default): in Wait CQ, the DX sending RR73, RRR, 73 or R73 to the station they are working resumes our calls from the open step (Tx1, or Tx2 with Skip Tx1) with a transcript note. While we are already calling them (Tx1/Tx2), the same sign-off no longer sends us to Wait CQ.
- **The window resizes freely**: the columns are laid out in exactly the room left (`UiBuilder::max_rect`), each with its own scroll area, and nothing sets a minimum from the available size (egui's `Resize` keeps a window at least as big as last frame's content, so content sized from the room it was given could never shrink). The window stays within the screen.
- **The transcript** is a scroll area of the whole QSO, stuck to its newest line, with a grip to drag its height (`TxSettings::transcript_h`, 16 lines to start); the room the QSO panel doesn't need goes to it.
- **REPLY, a double-click and CALL CQ turn ENABLE TX on** in FT8, as pressing it does (the watchdog starts over). WSPR still starts only from ENABLE TX. No interlock changed: ENABLE TX only lets the scheduler try, and every check runs as before.
- **The drive beside each LEVEL**: `tune::planned_watts` at that level, "— W" before the radio's power step is read.
- **WSPR's list**: `decode.rs` feeds WSPR decodes to TX as it does FT8's (`WsprHeard`); the list shows HEARD (UTC, SNR, drift, call, grid, dBm, km, heading, offset) and HEARD ME (reporter, SNR, km, MHz, site), newest 2 minute cycle first, one report per receiver per cycle with wspr.live/WSPRnet's preferred to PSK Reporter's. HEARD ME reads GLOBE's last fetch through `Globe::heard_me()`; GLOBE fetches only while it is open. CLEAR empties the list and hides earlier reports.

## 72. Amplifier silence mid-over, compound calls in FT8, and the decoder's hash table (11 Nov 2026)

**Why.** AJ3G, 11 Nov 2026: "amplifier is not connected" warnings were cutting his QSOs and CQs, which SDRoxide never did; the TX window refused `EK/RX3DPK AJ3G FM18`; and replies to compound-call stations decoded as `<...> KG7FST DM09` where SDRoxide resolved them.

**What.**
- **Interlock #6, a short silence mid-over (AJ3G approved the plan).** The HR500 often leaves its status query unanswered while keyed. The link (`hardrock::link`, `link_lost`) now calls the amplifier lost while transmitting only after 5 s with no answer, as well as four misses; at rest, four misses as before. The panel tells the link every frame whether the radio is keyed (`AmpLink::set_transmitting`); the link also counts the amplifier's own TX light (or, on old firmware, its drive) from the last answer, whichever says so first. `AmpStatus::readings_at` gives the readings' age. In the panel (`tune::AmpHold`), an over already keyed is judged by the last readings for up to 5 s from when they were taken, so a trip, SWR, over-drive or band mismatch they show still stops it; after 5 s it stops. Starting needs readings no older than 3.5 s (`tune::LIVE_MAX`), about what the link took to give up at rest before. See docs/safety-interlocks.md #6.
- **Compound calls in FT8 TX** (`tx/msg.rs`, `wsjtx_form`, `pack`): WSJT-X's forms. A grid or report to a non-standard call is a Type 1 message with that call as its 22-bit hash, written into the call field by hand as SDRoxide does (mfsk-core 0.10's `pack28` has none), hashing the whole call as WSJT-X does; RRR, RR73, 73 and a bare pair are Type 4, the non-standard call spelled out and the other hashed, in the order written (bit 70, which mfsk-core always leaves for hash-first); a grid from our own compound call goes as Type 4 without it so the far station hears our call spelled out; CQ is Type 4 (no grid, no CQ word). The sequencer's templates produce these forms, so the Tx lines and the transcript show the brackets.
- **The decoder learns every call** (`decoders::wsjt`, `learn`): standard or not, never `...`; and the panel registers its own call and the TX window's DX call before each slot (`WsjtDecoder::seed_calls`), as SDRoxide's `ApHints::calls`.

**Not done:** mfsk-core's hash table drops a `/P` or `/R` before hashing where WSJT-X hashes the whole call, so a hashed `W1AW/P` from elsewhere may still read `<...>`.

## 73. GLOBE: 3D arcs, one place per station, SHOW, the mode in use and CLEAR (11 Nov 2026)

AJ3G's round-2 GLOBE items (11 Nov 2026).

- **3D arcs.** Paths rise from 8% of the Earth's radius on a short path to 40% on the longest (`arc_lift`; it was 3% to 23%), so a path over the pole loops visibly over the cap. Each is a wide, faint glow with a thin, lighter core (`draw_arc`), its colours lightened toward white and translucent, and ends at the station's dot (the lift is 1 at both ends). What is behind the globe is no longer dropped: `Projector::arc_runs` splits an arc into front and behind pieces that join, and the behind ones are drawn at `BEHIND` (22%) of the light, so turning the globe shows the arc follow round; the part lifted past the limb stays in full. The selected path and the FT8 strobe (its arc and its pulse) are drawn the same way, and a faint ring marks a selected or QSO station that is behind the globe. The colours keep their meanings (cyan fading to slate, heard-me magenta dashed, band colours in ALL BANDS, the selected path white in a blue glow). A drag may now turn the view right over a pole (±90°, was ±85°): the projection's east and north stay a right angle there (tested). On the flat map a path cut at the map's edge is taken to the edge on both sides with a small ring at each cut, so it reads as one path. Cost: two line shapes a piece, 48 points a path, as before plus the glow.
- **One place per station.** ZS5KT showed twice, at its FT8 square's middle and at PSK Reporter's six-character locator. `History::places` gives each call the most precise grid any source gave it (six characters over four, the latest of equals), and `latest()` places every reception there, so a station heard both ways is one dot with the magenta ring round it and one label. Its paths are drawn as one: in the heard colour, with a magenta glow when it heard AJ3G too (dashed magenta when only that). The strobe takes the more precise of the QSO's grid and the heard ones.
- **SHOW: I HEARD · HEARD ME · BOTH** (`GlobeSettings::heard_view`, BOTH to start, remembered): the stations decoded, the reports of AJ3G (PSK Reporter, WSPR HEARD ME), or all. The SOURCES chips still pick within it. It and the mode below pick the stations, paths and HEARD list; PROP still takes every mode and way the SOURCES chips allow, since a path's evidence about the sky doesn't depend on which way or in which mode it was heard.
- **The mode in use.** `Globe::set_mode`, from main.rs each frame: the TX window's mode while it is open, else DECODE's running mode. GLOBE shows only that mode's stations (`field::norm_mode` compares upper case without the submode, so DECODE's "FST4-60" matches PSK Reporter's "FST4"; a report with no mode is shown); the others are hidden, not erased. **ALL MODES** shows every mode. A mode that is not a WSJT mode, or none running, shows every mode.
- **CLEAR** wipes the stations and paths (decodes and heard-me) now and rebuilds PROP. PSK Reporter and WSPR answer with the whole last hour each time, so `History` keeps the clear time and refuses a heard-me report from before it; DECODE's own lines are kept whatever their slot time, a decode landing a second after CLEAR being new.

## 74. The scope over a transmission: the band dimmed, our own signal in the TX colour (11 Nov 2026)

**Superseded by decision 83** (11 Nov 2026) for anything the panel sends: the TX monitor replaces the dimmed band and the flat marks. The dimmed hold below stays only for a keying the panel knows nothing of (an outside program).

**Why.** AJ3G, 11 Nov 2026: "When in TX do not lock scope instead playback my audio at the frequency I am on." Decision 40 froze the whole display for the over; with FT8 and WSPR going out from the panel (decision 70) that froze it for 13 s or 110 s at a time.

**What.**
- **Still not the DUO's I/Q.** While the radio says it is on the air, and for the 0.3 s guard after, nothing the receiver gives is shown (decision 40's reasons stand: silence, full-scale leakage, zeros, a moved centre). The span stays where it was.
- **The band keeps scrolling, dimmed.** The waterfall lays down a row at its own pace (its speed and the frame rate) from the last spectrum received, and every such row, and the trace, is drawn under a dark veil so it is plain nothing is being heard (`Scope::scroll_held`, `txsig::held_rows_due`; at most four rows a repaint, one after a long gap). The rows are kept by their time (`txsig::TxRow`), so the veil scrolls down with them. CPU and GPU spectra alike: the rows go up as ordinary rows with their bins, and the veil is drawn over both.
- **Our own signal, where it lands.** The panel knows what it is playing: the TX window's over (`audio::Player::recent`, the samples that have gone to the output, at their level) or TX TUNE's 1 kHz tone (`audio::Tone::recent`). Its last 8192 samples are transformed (Hann, a plain radix-2 FFT in `scope/txsig.rs`) and each audio frequency placed from the transmit dial: VFO B in split (the other VFO when receiving on B), else the receive VFO; above the dial on USB, below on LSB (`txsig::tx_dial`, `txsig::place`). The trace shows it as a line in the TX colour (GLOBE's magenta, `0xe06cf5`) with its peak a tenth below the top of the scale and labelled TX; the waterfall marks it on each keyed row where it is within 24 dB of its peak, brighter the stronger.
- **RIG TUNE** (`TX2;`) is drawn as a carrier at the transmit dial; where the DUO puts its TUNE carrier in SSB is not confirmed without the radio. **An outside program** keying the radio, or anything else the panel did not play, shows only the dimmed band, and the trace says our signal is not known here.
- **The station being worked** (`Scope::set_dx_mark`, fed by the TX window): a thin line in the trace's green at its frequency through the spectrum and the waterfall, with its call in a small flag at the top of the spectrum, below the offset bar's. It is not a control: nothing grabs or drags it.
- **After the over** the live spectrum comes back exactly as decision 40 had it, and no more dimmed rows are laid. Interlock #9 is unchanged: nothing tunes while transmitting.

**Where.** `scope/txsig.rs` (all the arithmetic, with its tests), `scope/mod.rs` (`set_tx_signal`, `scroll_held`, `TxHold::keyed`, the hold-and-resume test), `scope/paint.rs` (`paint_tx_trace`, `paint_tx_waterfall`), `audio.rs` (`Player::recent`, `Tone::recent`), `tx/mod.rs` (`Tx::sounding`), `main.rs` (`scope_window`: two lines that hand the scope what is sounding).
**Also, the same day.**
- **The TUNE strip says which path each key takes** (AJ3G, 11 Nov 2026). TX TUNE reads *TX TUNE (USB audio)*: a 1 kHz tone through the radio's USB audio at the slider's share of menu 34, the path FT8 and WSPR take. Its slider is *TX TUNE level*, its value a share of menu 34 with the ≈ watts beside it (`tune::planned_watts`). RIG TUNE, which has no slider, shows the power it keys at on its key: *RIG TUNE (carrier 8.3 W)*, menu 34's watts from the power step (`tune::carrier_label`; `tune.rs`).
- **The scope's GPU line** no longer repeats the GPU spectrum's frames a second: *FFT on the GPU* and the GPU's load (AJ3G, 11 Nov 2026).

## 75. LOG: grids for FILL MISSING and from decodes; picking the ADIF file (11 Nov 2026)

**Why.** AJ3G: "Fill Missing does not add missing grid squares … you should use online resource to fill in the data", and "If its missing and that station is heard again fill the data." With no HamQTH login FILL MISSING ran only cty.dat, which has no grids. And typing a path to import was awkward.

**What.**
- **FILL MISSING's order for a grid** (`logbook/lookup.rs`, `FillBatch`): QRZ.com XML when switched on with a login, HamQTH when it has a login (the second asked only for a grid the first didn't give), callook.info for a US call (FCC data, no login, `https://callook.info/<CALL>/json`), then with no network the same call's latest grid elsewhere in the log and the grid DECODE heard it send this run (`station::Heard`). A service with a position and no grid gives the 6-character grid it is in. Only the callsign goes to a service (with that service's own login). Each source is paced (QRZ.com 1 s, HamQTH 1.5 s, callook.info 1 s) and each call is asked once per source in a run; a source that fails is left out for the rest of the run and the others go on. Empty fields only, never `/` calls' grids.
- **The report** in LOG's status line: QSOs filled from cty.dat, grids filled by source, the calls still without one (the first eight), sources left out and why, and, with no QRZ.com or HamQTH login, a pointer to SETTINGS → Logbook.
- **Heard again:** every 15 s the log's grid-less calls are looked up in DECODE's heard table (FT8 decodes with a grid, WSPR decodes); QSOs with no grid are filled and the file is saved once for all of them, with a note such as "grid FN20 for K1ABC from a decode, 3 QSOs". Hooked in through `logbook::show`, which now takes `&decode.heard`.
- **FILE's ▾ drop-down** lists the .adi files found when it opens: WSJT-X's `wsjtx_log.adi`, the last eight imports (kept in the settings), the .adi files beside the log (the panel's exports), and those directly in Documents and Downloads, newest first. **BROWSE…** (Windows only, the `rfd` crate with no default features, on its own thread) opens the system's file dialog. Either fills the FILE box.

**Not done:** QRZ.com's free (non-XML) pages are not scraped; the form's LOOKUP still asks only the service chosen in SETTINGS.

## 76. The DUO's NB and NR switched with their level; silent settings logged (11 Nov 2026)

AJ3G found that NB and NR wouldn't switch from the panel, though the DUO's own NB button worked. The log showed every `NB1` read back as `NB0`, `NR1` as `NR0`, and no answer to a read of `NK` (the NB level), so the panel's level stood at 0.

- **On** now sets the level first (the level read, or 5 when it reads 0 or nothing), then the switch: `NK005;` `NB1;`. **Off** sends the switch and then level 0 (`NB0;` `NK000;`), which the CAT reference also calls off. This holds for RADIO's menu switches, the NB/NR annunciators and E1.
- **Silent settings:** after each full read of the radio's settings, the log has one line, `rigcat: settings read: N read; no answer to …; refused …`. It is written again only when the list changes, so COPY LOG shows which settings the DUO doesn't answer (and so can't be checked after a set).
- **Not confirmed on the radio yet:** whether the order or the level is what the DUO wanted. If `set NB1; read back NB0;` still shows, another command form is next.

## 77. WSPR stops for heat only over 155 °F (11 Nov 2026)

The WSPR temperature stop (interlock #12) used the 120 °F (48.8 °C) warning colour, so beacons stopped at 49 °C. AJ3G: "Should not trip until over 155F"; SDRoxide never stopped for heat. A beacon now isn't started, and one going out stops, only over 155 °F (68.3 °C), a step below the HR500's own 70 °C (158 °F) trip. The yellow from 120 °F stays as a warning colour only.

## 78. TUNE rides out the tuner's SWR spikes (11 Nov 2026)

**Why.** AJ3G, 11 Nov 2026: TUNE with the amplifier at PTT OFF stopped on a momentary SWR spike while the HR500's tuner was switching. The carrier dropped, the ELAD tripped, and the tuner then reported "Lo power" (it saw under 0.5 W for over a second), turning its TUNE light red. TUNE's stop took a single radio SWR reading at the warning, even with the amplifier bypassed.

**What.** SDRoxide's rule for a tune (`swr_rails` there), as time here: during TX TUNE and RIG TUNE the SWR limit is doubled (10:1 at most), nothing is judged for the first 5 s, and then the SWR must stay at or over the limit for 0.3 s before it stops the carrier. While the amplifier reports its tuner tuning, the radio's SWR doesn't stop the tune at all, and the amplifier's own SWR in line gets the doubled limit and the same grace; otherwise it stops on one reading as before. Ordinary overs are unchanged. `tune::TuneSwr`; interlock #6 amended (AJ3G to vet).

**Logged.** Every stop of an over now says why at INFO, once (`main.rs`, `stop_over`: "transmit: panel TUNE stopped after 1.3 s: The radio's SWR stayed at …"; TUNE pressed again: "transmit: TUNE stopped: pressed again"), and the link logs every unkey, with no reason as "rigcat: unkeyed as asked". The "transmit:" progress line carries the radio's SWR and "amp ATU tuning" while there is RF. AJ3G's log showed a TUNE end after 1.3 s with no reason.

## 79. The red TUNE light says why (11 Nov 2026)

AJ3G couldn't tell what the tuner's red TUNE light meant ("Lo power"). When the last tune failed, the AMP window now shows a line in plain words under the tuner drawing, whether the tuner is in line or not, and the TUNE key's hover says the same with what to do (`panel.rs`, `atu_failure`): *Lo power: the tuner saw under 0.5 W* (the HR500's `HRTR` `L`: under 0.5 W for over a second), *SWR above 6:1: too far off for the tuner*, *No match under 2:1 found*, *Power too high for the tuner to tune*. The words are the panel's own, not the manual's. TX TUNE at its first-run level (a quarter of menu 34) on the 1W key is about 0.25 W, under what the tuner needs; the drive defaults are unchanged, and the Lo power hover says to key a few watts.

## 80. BYP after a failed tune; refused relay clicks said in words (11 Nov 2026)

**Why.** AJ3G: "When the tune light goes red and I bypass the tuner, the tuner still engages." Two causes: interlock #1 refused the click while RF was on and only logged it (the key just went back), and a tune left running after a failure kept the tuner in line, since BYP sent only `HRTB0;`.

**What.**
- **The refusal is said** (`hardrock::link`, `refused_words`, `AmpStatus::notice`/`notice_at`; `panel.rs`, `notice`): a click interlock #1 or #4 refuses, or one whose status read goes unanswered, puts a line in yellow at the foot of the AMP window for 6 s, in every look: *Unkey first: the tuner can't switch to BYP with RF on.*, *Unkey first: the antenna relay can't switch with RF on.*, *Lower the drive first: …*, *The amplifier didn't answer, so … wasn't sent.* Interlock #1 itself is unchanged. A refused click never changes what the tuner shows (the drawing follows a click only once it is sent).
- **BYP cancels a running tune first** (`act`): the link asks the tuner `HRTT;` and only when it answers that it is tuning sends `HRTU;` (which cancels a tune in progress but is also the TUNE button: with no answer, or not tuning, it is not sent, so BYP never starts a tune), waits 100 ms, then `HRTB0;`. ON is unchanged.
- **Read back:** the tuner is read back 300 ms after ON or BYP, as before; if it still reads the other way, the line says *The tuner still reads in line after BYP.* (or bypassed after ON), and the key goes back to what it reads.

## 81. RADIO and DECODE windows: their size is the operator's (11 Nov 2026)

- **RADIO** (docked) wouldn't shrink after being made bigger: it claimed all its room with `set_min_size`, and its face, a little bigger than the room while its scale settled, became the size egui keeps a window at least as big as. Now the face is drawn in a child that isn't allocated in the window, clipped to the room, and only the room counts (as the TX window's columns, decision 71).
- **DECODE** (docked) slid sideways during TX: its width followed its contents. It is drawn the same way, with a minimum of 420 × 320, so it moves and resizes only when dragged.

## 82. WSPR decoded at the beacons' end (11 Nov 2026)

AJ3G: decode WSPR at the end of the transmissions, not of the slot. A WSPR transmission runs from 1 s to 111.6 s of its two minutes, so the decoder's slot recorder now finishes a WSPR slot 113 s in (`WsjtMode::decode_at_s`, `SlotRecorder::with_cut`), leaving 1.4 s for a late start, and the decode comes about 7 s sooner. The slot is still 120 s of samples, silent after the cut, and what is heard from 113 s to 120 s is not kept. DECODE's WSPR and the skimmer's both. Searching for candidates during the slot was not done.

## 83. The scope over a transmission: the TX monitor, SDRoxide's way (11 Nov 2026)

**Why.** AJ3G, 11 Nov 2026: "In Scope my TX does not look natural, the Waterfall looks awful, the Spectrum is not fluid, please look at how SDROxide handles this." Decision 74 held the last received picture dimmed and drew our signal over it from an 8192-sample snapshot taken each repaint: flat coloured bars on the waterfall, an unaveraged line that jumped, and a band that looked frozen.

**What** (after SDRoxide's `tx_analyzer`, `tx_monitor_analyzer` and `make_tx_frame` in `sdroxide-radio/src/engine.rs`).
- **Every sample.** The audio streams now have a tap (`audio.rs`, `Tap`: a lock-free ring the output callback pushes each sample into, never waiting, a second deep). `Player::recent` and `Tone::recent` give the samples sent since the last call, the over's lead-in silence included, instead of the last 8192.
- **One analyser, the scope's own settings.** `scope/txmon.rs`, `TxMonitor`: an `sdr::Spectrum` fed the audio as I/Q (Q at zero) at the output's rate, at the scope's frame rate, window and averaging, with half-overlapped segments so no sample is skipped. It is fed whether or not the radio is keyed, so it is full at key-down; its frames are thrown away until then.
- **A window for what is shown.** `monitor_fft_size`: 1024 points across the view, a power of two from 2048 to 16384 (a third of a second at 48 kHz), not the panadapter's FFT size. A change of size starts a new analyser from the last 16384 samples kept, so it is full at once.
- **Ordinary frames.** `txmon::rf_frame` lays each analyser frame across the span (16384 bins) and, zoomed in, across the view as the zoom FFT's frame (4096 bins): audio at `f` at the transmit dial plus `f` on USB, minus `f` on LSB (`txsig::tx_dial`: VFO B in split), up to 3.5 kHz; the rest of the span at the band's noise floor as last received (also the frame's NF). Full-scale audio sits 30 dB under the top of the dB scale (`TX_MON_HEADROOM_DB`; a real tone's 6 dB is put back). The frames go into the trace (drawn in the TX colour, on the CPU even with the GPU spectrum, which is not run meanwhile), the waterfall at its own row rate and colours, and the 3D surface, exactly as received frames do (`Scope::take_tx`). The RX trace is gone at key-down.
- **RIG TUNE** is fed as a full-scale carrier at the dial (`TxMonitor::feed_carrier`). **The guard after an over** keeps the monitor running on silence (`feed_silence`), so nothing stops; then receive starts afresh as before (decision 40's guard and reset unchanged). **An outside program** keying the radio: decision 74's dimmed hold, unchanged.
- **FIT** doesn't run and the waterfall's auto levels hold while the display is held, so the over never moves the scale.

**Where.** `scope/txmon.rs` (new, with tests), `scope/txsig.rs` (what is on the air: `TxSignal`, `on_air`), `scope/mod.rs` (`feed_tx_monitor`, `take_tx`, `gpu_trace`; the two over tests), `scope/paint.rs`, `audio.rs`, `main.rs` (`scope_window`: hands the scope the tap's samples and the over's name).

## 84. Our own TX colour is deep orange (11 Nov 2026)

AJ3G's rule: yellow and red for warnings only, our own TX signal orange. `txsig::TX_INK` and `globe::TX_COLOUR` (and so the TX window's TX colour, GLOBE's heard-me rings and strobe) are `#ff7a1a`, was magenta `#e06cf5`. The scope window's *Transmitting: …* line is in it too (it was the warning yellow).

## 85. The noise floor line stands out (11 Nov 2026)

The NF line is a 2-point dashed line in the strong ink (was 1 point, faint), and its reading sits at the trace's right edge on a dark box, above the line (below it when there is no room), so it is legible over the trace (`paint::nf_label_rect`).

## 86. What is on the air, by name; WSPR's labels per cycle (11 Nov 2026)

- The I/Q status line said "a TUNE from the panel is running" for every keying of the panel's. It now names it: *FT8 over* or *WSPR beacon* (the TX window's mode), *TX TUNE*, *RIG TUNE* (`txsig::on_air`, from the TUNE's keying and `Tx::over_mode`; `rigcat::RigStatus::tx_reason` names the keying too). The scope's TX caption uses the same name.
- **WSPR labels:** each new WSPR decode cycle clears the last cycle's labels from the scope (`skim::labels`: only the newest WSPR cycle in the feed is shown; a cycle with no decodes leaves the last to fade as before). FT8, FT4 and the keyboard skimmer keep the three-slot fade. Display only: DECODE's list is untouched.
## 87. TX: LEVEL changes the drive mid-over (11 Nov 2026)

The FT8 and WSPR LEVEL sliders set the over's gain once, at its start; moving one mid-over did nothing until the next over, while the interlocks already judged the drive at the slider's new place. Now the over's audio follows the slider at once, as TX TUNE's tone follows the Tune slider: `main.rs` `keep_tune()` checks the TUNE interlocks at the mode's level and, only when they pass, calls `Tx::set_level()`, which sets the player's gain to `audio::gain_for(level, ceiling)`. The player ramps to a new level over 5 ms so a move doesn't click or splatter. Interlock #6 is amended to match.

TX TUNE's path was checked end to end and is live: the Tune slider writes `saved.drive`; `keep_tune()`, while TUNE's tone is keyed, sets the tone's gain every frame to `gain_for(drive, audio_ceiling)` (amplitude the square root of the power share, within the DUO's 0.24 ceiling) and judges `tune_must_stop()` at `planned_watts(drive)`, the same value; the tone ramps over 5 ms. One difference from the TX window: TUNE sets the tone's gain before that frame's check, so a slider raised past the limit sounds for at most a frame before the unkey; `main.rs` was left as it was.

## 88. TX: WSPR's offset on the scope's bar (11 Nov 2026)

The scope's offset bar set only FT8's audio. In WSPR (with the TX window open):

- Entering WSPR the bar goes to the beacon's offset: 1500 Hz, the sub-band's centre, with ROAM on; the offset set with ROAM off.
- Dragging it sets the WSPR offset, clamped to 1400 to 1600 Hz, and turns ROAM off. A drag during a beacon applies from the next beacon; the one on the air keeps its offset (unlike FT8's bar, which can't move while transmitting, #9, nothing on the air changes).
- With ROAM on the bar moves to each beacon's chosen offset, and stays at the last one between beacons.
- The bar is an offset from the receiver's dial, the beacon's audio an offset from the transmit dial; in split the two are converted (`wspr_bar_from_audio`, `wspr_audio_from_bar`).
- Entering FT8 the bar is placed at FT8's offset, never read: before, FT8 on MANUAL took up WSPR's bar position. With the TX window closed the bar is the operator's measuring cursor and is neither read nor placed (`Env::bar_ours`).
- The AUDIO box's range is now 1400 to 1600 Hz too (was 1410 to 1590). No change was needed in `scope/`: the bar already reports drags and takes a position.

## 89. TX: the beacon's RF under its message (11 Nov 2026)

Under the WSPR message the TX window shows where the beacon lands, e.g. `TX 10.140 200 MHz (10 140.200 kHz)`: the transmit dial (VFO B in split, else the receive VFO, `scope::txsig::tx_dial`), plus the audio offset on USB (minus on LSB), plus the transverter offset set in the radio window. The offset is the beacon's on the air, else the one set, else with ROAM the last one sent; before ROAM has sent one it reads `TX ≈ … · ROAM picks each beacon's offset` from 1500 Hz.

## 90. TX: WSPR's lists as tables, sorted by their headings (11 Nov 2026)

HEARD and HEARD ME were lines of monospace text under proportional headings, so the columns drifted. Each is now a table drawn cell by cell in fixed-width columns (each sized in monospace characters for its figures or its heading with the sort arrow), headings in the same columns as the data; egui_extras isn't a dependency, so it is drawn directly. HEARD gains a MHz column, the decode's RF (dial plus its audio offset), e.g. `10.140 123`. The UTC, SNR, CALL and KM headings are clickable (accent colour, pointing hand, underlined under the pointer) and sort the table: newest, strongest, farthest first, CALL A to Z; a second click reverses it, and ▼ or ▲ shows which. Rows without a figure (no distance) go last either way. Each table's sort is saved in the settings. Sorted by UTC the rows keep their 2 minute cycle headings; sorted by anything else they are one list. A table wider than its column scrolls sideways, headings with it.

## 91. TX: ENABLE TX and WSPR's TRANSMIT OFF go together (11 Nov 2026)

In WSPR, turning ENABLE TX off (its button, HALT TX, an over stopped by an interlock, or a mode change) sets TRANSMIT to OFF, keeping the share it had (`wspr_pct_last`, saved); turning ENABLE TX on brings that share back (20 % if none was kept). Picking OFF turns ENABLE TX off. Picking a share doesn't turn ENABLE TX on: that stays the operator's own act.

## 92. TX: WSPR's lists show the band the panel is on (11 Nov 2026)

A band change cleared HEARD with the FT8 list. Now WSPR's reports are kept across band changes (up to 400), and HEARD and HEARD ME show only those on the band of the true RF the panel listens on (transverter included); the heading says the band and how many are kept from other bands. When the band isn't known both show everything. CLEAR still empties both on every band. The FT8 list is still cleared on a band change.
## 93. GLOBE: who heard AJ3G, from WSPRnet and wspr.live both, merged (11 Nov 2026)

**Why.** AJ3G saw spots of his beacon on wsprnet.org that never reached the panel: GLOBE asked wspr.live and fell back to WSPRnet only when wspr.live failed, and the mirror can lag WSPRnet or miss a spot.

**What.**
- **Both sites every poll** (`globe/fetch.rs`, `fetch_wspr`): WSPRnet's spot search (the SDRoxide-ported path) and wspr.live's ClickHouse query, each logged with its count (`globe: WSPRnet: 3 reports of AJ3G`) or its error, and each with its own footer status (WSPRNET, WSPR.LIVE). A site that fails keeps its last answer. PSK Reporter's fetch is logged with its count too.
- **One list** (`wspr::merge`): WSPRnet's reports, then wspr.live's, then PSK Reporter's WSPR receptions, kept once per receiver per 2-minute cycle per band. The first record wins (so WSPRnet's figures stand); a later site only adds its name and fills a blank (drift, power, km, SNR). The source reads `WSPRnet`, `wspr.live`, `PSK Reporter`, or `WSPRnet + live`, `WSPRnet + PSKR`, `wspr.live + PSKR`, `WSPRnet + live + PSKR`. The map's history replaces a WSPR report when another site's record of the same receiver, cycle and band arrives, so a station is one report, not two. With WSPR HEARD ME on, PSK Reporter's WSPR receptions go into this list rather than in beside it. `Globe::heard_me()` hands the TX window's HEARD ME the merged list (its own per-cycle dedupe is unchanged; tx/ is untouched).
- **Pace:** every 2 minutes (one cycle) while the panel beacons WSPR (TX window open, in WSPR, TX enabled: `Globe::set_beacon`, one line in `main.rs`), else every 5.
- **With GLOBE shut** the fetcher no longer stops: while PSK REPORTER or WSPR HEARD ME is on it goes on asking only those (the space weather, clouds, KC2G, weather and PSK BAND wait until the window opens), and the reports go into the map's history, so the TX window's HEARD ME and the globe are current. With both off a shut GLOBE fetches nothing, as before. `Globe::idle` now takes the settings and the station (`main.rs`).
- **WSPR mode turns WSPR HEARD ME on** (`Globe::set_mode` → `turn_on_wspr`): once, when the panel goes to WSPR; turned off by hand in WSPR it stays off. Its SOURCES chip is renamed **ASK WSPRNET + WSPR.LIVE**, saying what it does.
- **Tests:** canned replies of both sites through `fetch_wspr`'s injected getter (both asked; one down keeps its last answer), the merge (key, winner, site names, PSK Reporter's WSPR), the history's replacement, the merged list on the map and for TX, and WSPR mode turning it on. Nothing calls either site.

**Not done.** The TX window's empty-list text still says GLOBE fetches "while it is open" (tx/ was off limits for this change); it now fetches shut too.

## 94. GLOBE MODES ▾: the mode in use and chosen extras (11 Nov 2026)

**What.** The SHOW row's mode chips (FT8 ONLY · ALL MODES) became one **GLOBE MODES: FT8 ▾** drop-down (`modes_menu`). The mode the panel is on is shown first, ticked and locked; under it every other mode heard this hour with its station count, each to tick as an extra (saved: `GlobeSettings::extra_modes`); **ALL MODES**; **Paths for the extras too** (off to start: only the mode in use has paths, so extras don't clutter the map); **CLEAR EXTRAS**. The key names what shows: `FT8`, `FT8 + WSPR, FT4`, `FT8 + 3`, `FT8 + ALL`, or `EVERY MODE` when no WSJT mode runs.
- **Colours:** the mode in use keeps its colours (decode cyan, the TX-colour ring for heard-me, band colours under ALL BANDS); an extra mode's stations are in its mode's colour (`mode_colour`: FT8 blue, FT4 violet, WSPR pink, JT65 sea green, JT9 periwinkle, Q65 lilac, FST4 pale blue, MSK144 near-white, others silver), never yellow, red, orange or the cyan; the legend under the map adds each extra shown.
- **One dot per station** each way (`one_mark_each`): of a station heard in several modes or bands, the picked one, else the mode in use's, else the newest. The HEARD list now keeps one line per station, band, mode and source, so a mode heard later no longer hides the mode in use's line.
- Decay unchanged.

## 95. GLOBE: a pop-up for a station that heard AJ3G (11 Nov 2026)

**What.** Clicking a station on the map that heard AJ3G this hour (PSK Reporter or WSPR HEARD ME) opens a pop-up over the map's top right (`heard_me_popup`, an egui `Area`, so clicks in it don't reach the map): call, grid, country (cty.dat), distance, bearing short and long, spot UTC and age, band and mode, frequency (MHz, six places), SNR, drift, declared power, source (as in decision 93), every report of AJ3G from that station in the last hour, and AJ3G's own decodes of it this hour (DECODE, SKIM), if any. One at a time: ✕ closes it; clicking another station that heard AJ3G replaces it; clicking a station that didn't closes it (that station is picked as before); clicking the bare map or turning the globe leaves it. CLEAR closes it.

## 96. GLOBE: a solid globe (11 Nov 2026)

**What.** Nothing behind the globe is drawn any more: the faint far side of an arc (decision 73's `BEHIND`), the ring where a path ends behind the globe, and the FT8 strobe's pulse behind it are gone. An arc lifted above the surface still shows where it rises past the limb (`sphere::arc_runs` marks those pieces in front; `drawn_runs` keeps only those). The flat map is unchanged.

## 97. FT8: an over waits for the slot before to be decoded (11 Nov 2026)

AJ3G's transcript with CT7AUT showed `064230 TX CT7AUT AJ3G FM18` above `064230 RX AJ3G CT7AUT +00`: the over keyed at the slot's start, a few milliseconds before DECODE finished the slot in which CT7AUT had answered, so the grid went out a second time and the QSO lost a cycle (CT7AUT moved on). An FT8 over now picks its message only once DECODE has finished the slot before (`sched::slot_decoded`), waiting up to 1 s (`DECODE_WAIT_S`) for it, then going with what it has. Its audio still starts at 0.5 s when it can; started later, the whole message goes out a little after now (`sched::ft8_start_s`, a later DT, at most about 2.1 s) rather than losing its first symbols, which the old late start cut off.

## 98. SCOPE: a cursor where the mouse is (11 Nov 2026)

AJ3G asked for an indicator as the mouse moves through the waterfall or the spectrum. A thin line now runs through both at the pointer's frequency, with a small boxed readout beside the pointer: the frequency to the hertz (as the scope shows it, transverter and calibration included) and the strongest level within a drawn column either side, in the scale chosen (S-units, dBm or both). The box flips to the other side at the edges. No level is read while a transmission's picture is held, since those levels aren't the band's.

## 99. TX: the QSO transcript as SDRoxide shows it (11 Nov 2026)

AJ3G sent SDRoxide's transcript as the way it makes most sense: a dot and the message on each line, ours in the TX colour and theirs in green, and the contact closed by a boxed "■ QSO with KH6DC complete — logged". The panel's had a UTC and TX/RX on every line and a plain closing line. Now each line is a dot and the message (ours orange, theirs green, notes such as "CT7AUT is working KJ7NO" dimmed with a small dot), the UTC on hover, a run of the same message of ours sent again folded into one line with "×N", and the logged contact in a green box.

## 100. TX IN on USB audio when the radio connects (11 Nov 2026)

At every start AJ3G met "The radio's TX IN (menu 32) is on Auto, so TUNE would send that" and had to press USB AUDIO: the DUO keeps menu 32 across power cycles, and interlock #7 rightly refuses to key on anything but USB audio. SDRoxide asserts its TX input (`TI1;`) when its port opens. The panel now does the same, once each connection, when menu 32 reads anything else (SETTINGS, Radio, "Put TX IN (menu 32) on USB audio when connecting", on by default). Once only, so a change made on the radio afterwards stands until the next connection. Interlock #7 is unchanged: keying still needs USB audio, read again just before the key.

## 101. SCOPE: the cursor reads the audio offset (11 Nov 2026)

AJ3G wants to compare where DECODE reports a station with where it is on the scope. The mouse cursor's readout (decision 98) now also gives the audio offset from the receive dial, in whole hertz as DECODE's AF column has it and as the offset bar reads it (`OffsetBar::offset_hz`: up from the dial in USB, down in LSB): *14.097 150 MHz · 1550 Hz · −12 dB*. Outside the receiver's passband, where no audio is made, or with no dial known, the hertz are left out (`hover_af`).

## 102. SCOPE: the station being worked, in green (11 Nov 2026)

The worked-station mark was a thin line in the trace's blue with a quiet flag; AJ3G wanted it green and as pronounced as the offset bar. It is now in the slot lines' clear green (`slotlines::START_INK`, already on the scope; green is not a warning colour): a 3-point line on a dark halo through the spectrum and the waterfall; at the top of the spectrum a green-edged, green-tinted box with the call, its audio offset from the dial (as the bar writes it) and its frequency; the call boxed along the waterfall's foot. The skimmer's tick and label for that station turn green too (green wins over the bar's red there; the bar keeps its own flag naming the station). The mark is drawn before the offset bar, so a bar on the same station stays visible down the middle of the green (green = the station, red = where we transmit), and its boxes sit a row under the bar's flag on the spectrum and a row over its readout on the waterfall, so the two never overlap.

## 103. SCOPE: our TX signal in the waterfall's palette, and taller (11 Nov 2026)

The TX monitor's trace was drawn in the TX orange. AJ3G wants his own signal to look as a received one does, in the palette he picked (ICOM, ELAD, …). The monitor's rows were already ordinary waterfall rows; now its spectrum trace is coloured by level through the same palette and the same mapping as the waterfall's shader (the waterfall's levels, contrast and brightness: `TxPalette`), each segment in its level's colour, its fill the same fading to the floor. The colour is lifted to at least 0.4 of the palette so the line still shows where it lies on the floor (every palette starts at black). Orange stays only on the *TX MONITOR* caption, so being on the air stays obvious. Full-scale audio now sits 10 dB under the top of the dB scale (`TX_MON_HEADROOM_DB`, was SDRoxide's 30 dB), so the signal fills most of the trace's height (superseded by decision 113: a fixed strength, 3 dB under CEIL).

## 104. SCOPE: the worked mark only while the QSO is in progress (11 Nov 2026)

The mark stayed after a contact ended, since it followed whichever station was in the QSO box. It now shows only while the QSO is actually in progress (`Qso::in_progress`: a station in hand, the exchange going or waiting for their CQ, not yet logged). It goes once the contact is complete and logged (even while our closing 73 still goes out), when the watchdog stops it, after the last unanswered call, when Wait CQ times out, on STOP QSO, and on HALT (`Tx::halted`, cleared when a station is picked or ENABLE TX resumes the one in hand). The QSO box keeps the call and the offset it was copied at (`Tx::dx_copied`).
## 105. DECODE: country flags as SDRoxide draws them (11 Nov 2026)

AJ3G approved a country flag to the left of the signal report on each decode row, done SDRoxide's way. Ported from SDRoxide (GPL-3.0-or-later): its 265 public-domain flags from fonttools/region-flags (48×32, 64 colours, about 1 MB, `assets/flags/` with their README and provenance), compiled into the program with `include_bytes!` so nothing is fetched; its table from each DXCC entity's primary prefix to a flag code (`src/entity_flags.rs`, keyed by prefix because the country file rewords names but not prefixes; dependencies fly their administrator's flag, and the Spratlys and the SMOM fly none); its texture cache, which decodes each flag once on first use (here with the `png` crate the panel already depends on, rather than adding SDRoxide's `image` as a direct dependency for one call); and its generator, `tools/gen_flags.py`, adjusted to our paths and given `--rs-only` to rewrite the Rust list without Pillow. A WAE-only area (Sicily, Shetland) shows its DXCC entity's flag, as its name already does. The flag sits in a fixed 22-point cell between the UTC and the SNR in the DECODE window (WSJT modes) and between the ▶ and the SNR in the TX window's list, blank when there is none so the columns stay in line. Hovering it in DECODE names the country; the TX list's station card shows the flag beside the country, as SDRoxide's does. Tests check every entity in our cty.dat has a table row, every row names an entity, every code has a PNG that decodes, and unknown calls get none.
## 108. SCOPE: a noise blanker on the panel's I/Q (11 Nov 2026)

AJ3G asked to "Add NB and slide to scope". The panel's receiver gets SDRoxide's impulse blanker (`sdr::nb`, ported from `crates/sdroxide-dsp/src/nb.rs`, GPL-3.0-or-later): a sample whose magnitude reaches the threshold times a running average of ordinary samples is set to zero; the average follows only samples that were not blanked (step 5e-4 per sample), and nothing is blanked for the first 8192 samples while it settles. As in SDRoxide's engine, it runs first, on the I/Q at the radio's rate, right after the pipeline drops non-numbers and ahead of everything else: the CPU spectrum, the I/Q tap (so the GPU spectrum, the decoders and outside programs) and every receiver and slice. So the spectrum and waterfall show the I/Q blanked, as SDRoxide's do, and an impulse is caught before a decimating filter smears it. The threshold is SDRoxide's fixed 5 made a slider, 2 to 20 in steps of 0.5, labelled "lower = more" since a lower threshold blanks more. Switching it on starts a fresh blanker (a new warm-up); moving the slider keeps the learnt average. On/off and threshold are kept with the receiver's settings (`nb`, `nb_threshold`; off and 5 for an older file). The DUO's own NB (RADIO's E1, CAT `NB`/`NK`) is separate and untouched.

## 109. SCOPE: NR is a chip and a level slider (11 Nov 2026)

With NB added, NR on the same line becomes the same shape: the chip is on or off, and a slider beside it sets the level, 1 (light, SSB) to 4 (strong, CW), which is `sdr::nr`'s strength (taps, delay and step from its table of four). It was a click through off, 1, 2, 3, 4. The level is kept between runs (`nr_level`, 2 for an older file) and is what NR comes back on at; while NR is on the slider moves it live. E1's NR on the DUO still sets the panel's NR (`follow_e1`), and the slider follows it there.

## 106. TX: split moves VFO B, as WSJT-X's Rig split (11 Nov 2026)

With the radio in split and VFO B left somewhere else, AJ3G's FT8 over was refused: "The radio's transmit dial is too far from the receiver for 2526 Hz". The panel worked out the tone as the offset from VFO B and gave up when it fell outside 100 to 2950 Hz. Now it does what WSJT-X's "Rig" split does: VFO B goes to the receive dial plus the offset less 1 500 Hz, the shift rounded to 500 Hz as WSJT-X steps it (minus on LSB, where the audio rises as the signal goes down), and the tone is 1 500 Hz give or take the rest (1 250 to 1 750 Hz), well inside the radio's clean audio, while the signal lands where it did, on the bar (`tx::split_dial`, `tx::audio_plan`).

- **Only when needed.** A VFO B the tone can already be sent from (100 to 2950 Hz, the old test) is left where it is. TX follows RX (decision 22) keeps VFO B on the receiver or the bar, always inside that range, so the two never pull VFO B to and fro between overs; the move is for a VFO B the operator or an earlier session left elsewhere, or not read yet.
- **Confirmed both ways.** The move is a `FB` set read back by the link (`follow_split`, asked again after 1.5 s if the read doesn't show it, never while transmitting). The over waits for the read-back, saying "Moving VFO B to … for split" in the window, and goes later in the slot as FT8 allows or in the next. It is then keyed with `RigLink::tune_start_on`, which reads the dials again straight before keying and keys only if the transmit dial is exactly that VFO B (interlock #7, amended). The panel log says each move in words: from where, to where, and the tone it leaves.
- **Out of split nothing changes**: the tone is still the offset from VFO A, refused outside 100 to 2950 Hz. Split receiving on VFO B transmits from VFO A, which the panel doesn't move: the window says to receive on A.
- The decode list's own row shows the offset from the receive dial, as the decodes' AF does, not the tone from VFO B.
- WSPR is unchanged: its tone stays in 1 400 to 1 600 Hz from the transmit dial, which the bar already accounts for in split (decision 88).

## 107. LOG: an import fills in the QSOs it already has (11 Nov 2026)

IMPORT left a QSO the log already had (same call and band within two minutes) as it was, so a fuller copy of it, a WSJT-X log with grids, a logger's file with names or LoTW confirmations, added nothing. Now each such duplicate fills the blank fields of the QSO there, nearest in time, in the same pass, never one already set: empty text, a zero frequency, a number not given, the mode with its submode, a QSL not marked with no date, and ADIF fields kept as extras the QSO has none of (`QsoRecord::fill_blanks`). The log is saved once at the end, and only if something was added or filled. The status line says "N added, M filled in, K already in the log", K being the duplicates that had nothing to add. A duplicate later in the same file fills in what the file's first copy left blank.

## 114. TX: REPLY mid-QSO leaves the station in hand (11 Nov 2026)

AJ3G: "if I am unable to get ahold of the station or they are in a QSO, and I pick another station to reply to ... Stop QSO and load the new station and allow me to then work the other station. Right now it stays on the station until I hit STOP QSO." REPLY (or a double-click) used to queue the new station at the front while a QSO was under way (decision 70's queue). Now REPLY on another station during a QSO does STOP QSO on the one in hand (`Qso::stop`, as the button: nothing logged) and starts the new one at once, by the same rules as any REPLY: the slot opposite theirs, their SNR as our report, Wait CQ when they are working someone else, an exchange already begun with us picked up where it stands (`Tx::reply`). The station left is not queued again. An over already on the air finishes (it is not cut); the next over goes to the new station, and the old over's end is not counted as an unanswered call to the new one (`Tx::left_slot`). The scope's worked mark follows at once (decision 104's `dx_mark`). The transcript gets a plain line, "Left K1ABC for W9XYZ", and the panel log the same. REPLY on the station already in hand, or with no QSO under way, is as before; + still queues. `Qso::queue_front` is gone.

## 115. TX: AUTO remembers strong stations and hears undecoded signal (11 Nov 2026)

AJ3G approved two additions to TX freq AUTO (decision 70), which still counts only our own transmit period, as SDRoxide does:

- **Strong stations longer.** Each heard entry keeps its SNR; a decode at −5 dB or better counts as busy for 5 minutes, others for 2 minutes as before (`freq::keep_s`), timed from now, so they also age out on a quiet band.
- **Signal that nothing decoded** (`tx/occupy.rs`). The scope's spectrum is the wrong source: it is usually on the GPU and covers the zoomed view, not the receive passband. So AUTO gets its own cheap ear on the receiver's audio (a `txbusy` audio tap, installed only while the TX window is open on FT8 with AUTO and the receiver runs): 0.1 s frames, a Hann-windowed 4 800-point FFT each (10 Hz bins, about 0.1 ms per frame), the power summed per bin over each slot's message part (0.5 to 13 s). At the slot's end a bin is busy when its average is 6 dB over the slot's noise floor (the quietest quarter of the bins in 200 to 2 900 Hz); averaging over the slot is what makes it "signal for a good part of the slot" (noise and clicks don't lift it, an FT8 signal of about −10 dB SNR or better does). Runs of busy bins become spans. A slot the radio was keyed in (our own over through the I/Q, as decision 71) or heard only in part is dropped. The last four slots of each parity are kept, each for 2 minutes; those of our parity are merged into `freq::busy` with the decodes, with the same 5 Hz guard, and the shaded busy areas on the scope show the result. A band change clears them with the list.
## 112. SCOPE: made-up noise where the waterfall has no history (11 Nov 2026)

AJ3G: "the waterfall blanking is very bad so we need to fill this in with what looks like noise when it happens." When the span moves (a pan, the centre following the receiver, worst at 6144 kHz) the strip it uncovers has no history, and the rows not filled yet have none either; they were painted one flat colour (the noise floor's, a shade dimmer). Now the waterfall and the 3D surface (plain and lit) draw made-up noise there (`shaders/noise.wgsl`):

- **Stable.** Each value is a hash of the row's number (`Rows::wf_serial`, `Rows::surface_serial`: every row laid down, wrapping at 2^24 in the shader) and the frequency cell (Hz from the reference, which only changes when the history restarts anyway, in cells a power of two hertz at least a pixel wide). The noise moves down with the rows and does not shimmer while nothing moves.
- **Like the band.** The level is a Gumbel variate (the strongest of several bins of Rayleigh noise, as each real pixel is: a longer tail up than down), its median at the band's quiet level (the trace's median column) and its median less 16th percentile at the spread measured there (`levels::noise_fill`; 3 dB with too few columns, 0.5 to 10 dB). The measure is eased over half a second and held over a transmission (the TX monitor's trace has no noise to measure). Before any is known the fill is below the floor (the palette's bottom).
- **The same path as data.** The made-up level goes through the same levels, contrast, brightness and palette as a measured pixel; real rows draw over it as they arrive. The cost is one hash and two logarithms for an empty pixel only.

## 113. SCOPE: the TX monitor at a fixed strength (11 Nov 2026)

AJ3G: the TX signal on the waterfall, spectrum and 3D "do not have enough gain and do not match the SPECTRUM WATERFALL colors (ICOM)". Decision 103 drew full-scale audio 10 dB under the top of the scale, but the monitor drew the audio's real level, which with LEVEL at 50 % and a low TX AUD is well under full scale. Now each monitor frame is drawn so the over's loudest audio sits 3 dB under CEIL (`TX_MON_BELOW_CEIL_DB`, replacing `TX_MON_HEADROOM_DB`), whatever its real level, with its shape relative to that peak kept; the rest of the span stays at the band's noise floor. The reference is a peak held over the over (`TxMonitor::reference`), letting go at 6 dB/s, rather than each frame's own peak, so a pause in speech or the fade at the end of the over falls away instead of quiet audio being raised to full strength; audio under -100 dBFS is silence, drawn at the floor. The frames are CPU frames into the trace (palette-coloured, decision 103), the waterfall rows and the 3D surface, so all three show our signal at that strength in the selected palette; the GPU spectrum is not run while they are shown.

## 116. SCOPE and RADIO keep one mode, and a data over needs them to agree (11 Nov 2026)

AJ3G: "Radio Panel the mode is not tracking with Spectrum Panel and I think it caused me a problem". The panel's receiver and the DUO each had a mode of their own: a mode picked on SCOPE reached the DUO only at SPAN 192k, and a mode set on RADIO or on the radio's knob never reached SCOPE, so an FT8 over could be worked out for one sideband while the radio sent in another. Now (`rx.rs`, `ModeSync`, `Rx::sync_duo_mode`, `Rx::keep_modes`):

- **SCOPE to radio, at every SPAN.** A mode change on SCOPE (its mode keys, a DATA pick, SWAP A, an outside program) sends the DUO its mode (`MD`) whatever the I/Q rate, since the DUO transmits in its own mode. Its receive filter is still set only up to 192 kHz: it shapes only the DUO's own audio, which is muted above. A passband drag sends the filter, not the mode.
- **Radio to SCOPE.** When the mode the radio reads back changes (RADIO, or the knob), the receiver takes it (`sdr_mode`), with the running decoder's passband when the decoder is in that mode (FT8's 200–3000 Hz on USB, as a DECODE pick sets it), else the mode's default. The DUO's filter is left as the radio has it. At the start, or when the CAT link or the ELAD comes back, the radio's mode wins.
- **No ping-pong.** Whichever side changed last wins. A mode sent to the radio is remembered until the radio reads it back; meanwhile the old read-back, or the read-back of an earlier change still on its way, is not taken as a change on the radio, and a second change is weighed against the one on its way. A mode the radio never reads back within 3 s (`MODE_WAIT`) is dropped and the radio's word stands, so the two can never pull each other back and forth.
- **Not while transmitting** (interlock #9): nothing either way. A change made on SCOPE during an over goes to the radio once it is done.
- **The log** has one line per change, naming the side that made it: "mode: SCOPE changed the receiver to LSB; the radio is sent LSB" or "mode: the radio changed to USB; SCOPE's receiver follows (200 to 3000 Hz)".
- **The TX check (interlock #13).** With the panel's receiver running on the DUO's I/Q, an FT8 or WSPR over (FT4 when it comes) is refused while the receiver's mode and the radio's differ, or are not USB or LSB, and the TX window's interlock line says so in yellow ("SCOPE's receiver is in LSB and the radio in USB: set both to the same sideband…"). The sync above normally clears it within a second; the check is there for when it can't.

## 117. The DUO goes back to stand-alone 192 kHz when the panel closes (11 Nov 2026)

AJ3G: "Upon exit you must place ELAD into Stand Alone 192KHz.. If not other programs cant use it." The panel put the DUO back on its own image only when it was next opened at 192 kHz, so closing at a wide SPAN left it in remote on the wide image until a power cycle. Now `on_exit` calls `elad_iq::set_leaving()` before the receiver stops, and the stream's `Device::shutdown` then runs the same restore FDM-SW2 sends (`restore_standalone`: E3, E5 until done, F9, split off) whenever it had the DUO out of stand-alone. The restore keeps interlock #10: not while the radio transmits. A failure is logged; the next open still restores as before.
