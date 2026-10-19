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

## 11. The radio link only reads

**Decided:** AJ3G, 19 Oct 2026 ("a way to look at read all relevant menu settings").

**What the panel does.** The RADIO window shows the radio's live readings and every setting it answers a READ for, with its menu number where it is one of the radio's menus (31 `ANTENNAS`, 32 `TX IN`, 34 `TX POWER`, and 70 `CAT BAUD` from the rate the port is open at), the meaning in words and the radio's answer as it came. Settings are read when the port opens, when READ AGAIN is pressed, and every 10 seconds between transmissions. Nothing is ever written to the radio (interlock #5).

**Why.** In SDRoxide the DUO's settings were SDRoxide's to set (decision 3); standalone the radio belongs to the operator, and a panel that changed it could leave it changed. Reading is all that was asked.

**The limit.** Only the settings with a READ command in the command table SDRoxide's ELAD driver was written from (FDM-DUO manual v2.6 §6) are listed. Menus with no CAT command, such as 37 `CW IN` and 45 keyer speed, can only be read on the radio.

**Where.** `crates/rigcat/src/elad.rs`, `SETTINGS` and `describe()`; `crates/hardrock-panel/src/radio.rs`.

## 12. A port another program holds says so

**Decided:** AJ3G's bug report, 19 Oct 2026: with the HR500 on COM5 he had to unplug the lead and reinstall before the panel connected.

**What the panel does.** When a port won't open but the system still lists it, the status says it is in use by another program and names the likely ones; it reconnects by itself every two seconds once the port is free. "Not found" is kept for a port that has gone.

**Why.** The serial library reports Windows' "Access is denied", which is what another program holding the port gives, the same way as a port that isn't there, so the panel said "COM5 not found. Check the amplifier's USB lead." Unplugging the lead made the other program let go, which is why that worked. AJ3G confirmed it the same day: SDRoxide's AMP link was holding COM5.

**Where.** `open_error()` in `crates/hardrock/src/link.rs` and `crates/rigcat/src/link.rs`.
