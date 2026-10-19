# Hardrock amplifier: safety interlocks

Every place the panel refuses or delays something because doing it would be unsafe for the amplifier. #1, #2 and #4 came over unchanged from the AMP window in SDRoxide, where they were first written and bench-tested. #3 came over when the panel gained its own radio link, and #5 and #6 are new with that link. Each is marked in the code with `SAFETY INTERLOCK` and its number here. AJ3G asked (17 Oct 2026) that every such block say what it blocks and why.

Before removing or loosening any of these, read why it is there.

## 1. No relay switching with RF present (amplifier link)

- **Where:** `crates/hardrock/src/link.rs`, `act()`.
- **Blocks:** antenna change, band set, band up/down, and tuner in/out (ON/BYP), whoever asks: the panel's buttons or anything else that sends the command through the link.
- **When:** the amplifier says it is transmitting (status digit d1), or shows drive (0.1 W or more) or output (1 W or more), or does not answer the status query asked just before the command.
- **Why:** each of these switches a relay in the RF path. Switching one with RF on it opens or changes the load under power. On AJ3G's HR500 (17 Oct 2026) switching the tuner in or out while transmitting made the amp raise its RFL flag at once. The reflected power spike stresses the VRF2944 output devices, and arcing pits the relay contacts. The amp often skips its status reply mid-transmission, so no answer counts as "not safe": the command is dropped and a warning is logged.
- **What the operator sees:** nothing changes; unkey and click again.

## 2. Relay buttons greyed while transmitting (panel)

- **Where:** `crates/hardrock-panel/src/panel.rs`, `rf_present()`, `NOT_WHILE_TX` and the ANT, DN, UP and ON/BYP buttons.
- **Blocks:** the same switches as #1, at the button.
- **When:** the latest reading shows drive of 0.1 W or more, or output of 1 W or more.
- **Why:** as #1. The button is greyed so the operator knows why nothing would happen, and hovering explains: "Not while transmitting: switching a relay with RF on it can damage it. Unkey first." #1 stays as the last check, because a reading can be a fraction of a second old.

## 3. Band follows the radio only between transmissions

- **Where:** `crates/hardrock/src/link.rs`, `run()` (the held frequency), and `crates/hardrock-panel/src/main.rs`, `follow_band()`. In SDRoxide: `crates/sdroxide-radio/src/engine.rs`, `poll_amp()`.
- **Blocks:** sending the radio's frequency to the amplifier, which sets its band, while there is RF in the amplifier.
- **When:** the panel holds it while the radio reports transmitting (FDM-DUO `FP` with a trusted reading) or the amplifier shows drive of 0.1 W or more or output of 1 W or more. The link then holds it again until a status read just before sending shows no transmission, no drive and no output; no answer counts as not safe, as in #1. It is tried again every half second and sent once the amplifier is clear.
- **Why:** a band change switches the low-pass filter relays; under full power that arcs the contacts and can leave the output devices driving an open or wrong filter. The band is sent as soon as the transmission ends.
- **Only when:** a radio is connected under SETTINGS and "Amplifier follows the radio's band" is ticked. Without one, the band changes only with DN and UP, which #1 and #2 guard, or over the amplifier's own CAT or ACC input.

## 4. No OFF to PTT while the drive is over the amplifier's limit (panel and link)

- **Where:** `crates/hardrock-panel/src/panel.rs` (the PTT/OFF button) and `crates/hardrock/src/link.rs`, `act()`.
- **Blocks:** putting the amplifier in line (OFF to PTT).
- **When:** the drive reads over the amplifier's drive limit: its own yellow DRV level, 8 W on the HR500 (from the operating manual's indicator table; it trips at 12 W). The HR50's 6 W is scaled from the HR500's and untested. The link checks the amplifier's status just before sending; no answer refuses the command, as in #1.
- **Why:** in OFF (bypass) the radio's own power goes straight through to the tuner, so an operator can run a 100 W radio barefoot through the bypassed amp (AJ3G, 18 Oct 2026). Switching to PTT with that still applied would drive the amplifier far past its trip level and can damage the VRF2944 output devices.
- **What the operator sees:** the PTT/OFF button greys while the drive is over the limit, and hovering says "Lower the drive first", with the drive and the limit. In bypass the DRV reading and tile stay green whatever they read, ATU in or not (AJ3G), because high drive there is not a fault.

## 5. The radio link sends only reads, the power setting, the audio input, and TUNE's keying

- **Where:** `crates/rigcat/src/elad.rs`, `allowed()`, and `crates/rigcat/src/link.rs`, `Rig::write()`. Each radio's protocol has its own list (`Protocol::allowed`).
- **Blocks:** every frame to the radio except the READ frames listed in `elad.rs`, menu 34's power steps (`TP00;` to `TP09;`), menu 32 to USB audio (`TI1;`, never `TI0;` or `TI2;`), the radio's own TUNE carrier (`TX2;`), the ordinary transmission (`TX;`, never `TX0;` or `TX1;`) and unkey (`RX;`).
- **Why:** keying the radio, or changing a setting, could put power into the amplifier when the operator did not mean to transmit, or leave the radio changed after the panel closes. Commands with no parameter are not safe to assume are reads: `RC;` clears the RIT offset. So the link sends only what is on the list, and refuses anything else even from its own code. Menu 34 is written only when the operator presses a power button; menu 32 only when the operator presses USB AUDIO; the carrier only under #6; the ordinary transmission only under #6 and #7. Tested by `only_reads_menu_34_menu_32_to_usb_and_keying_can_be_sent`, `a_frame_not_on_the_list_never_reaches_the_radio` and the TUNE sequences in `link.rs`. `tools/fake_duo.py` prints anything it should never be sent.
- **Changed:** AJ3G, 19 Oct 2026. It was "reads only" until he asked for TUNE with a power slider and menu 34 beside it; `TX;` and `TI1;` were added the same day for TX TUNE, SDRoxide's way of driving the DUO (decision 17).

## 6. TX TUNE and RIG TUNE key only when the amplifier is ready, and always unkey

- **Where:** `crates/hardrock-panel/src/tune.rs`, `tune_blocked()`, `planned_watts()` and `tune_must_stop()`; `crates/hardrock-panel/src/main.rs`, `keep_tune()` and `do_tune()`; `crates/rigcat/src/link.rs`, `run()`.
- **Blocks keying when:** the radio or the amplifier isn't connected (the amplifier's state can't be checked); the amplifier has tripped; or, with the amplifier in line (PTT), the radio is not on the amplifier's band (the carrier would meet the wrong low-pass filter at full gain), the radio's frequency is in no band the amplifier covers, or the power TUNE would key at is over the amplifier's drive limit (8 W on the HR500, as #4). That power is menu 34's setting for RIG TUNE (MAX is taken as 8.3 W, what AJ3G measured as the DUO's ceiling, so RIG TUNE at MAX is refused in line), and the setting times the Tune slider for TX TUNE (MAX at 96% or less passes).
- **Unkeys when:** any of those becomes true, including the Tune slider raised past the limit while keyed; the drive goes over the limit in line; the amplifier's SWR (in line) or the radio's SWR reaches the SWR warning setting; the radio drops to receive on a high SWR; the time limit set beside the buttons runs out (1 to 60 s, 10 s to start; the link refuses more than 60 s whatever it is asked); the panel stops telling the link it still wants TUNE for more than a second (frozen or closed); TX TUNE's tone stops playing; the radio stops answering; the panel closes, or the settings are applied, which drops the link. Unkey is sent twice.
- **Why:** TUNE goes through the amplifier: in PTT the HR500 turns a few watts into hundreds. A carrier left on, or keyed into the wrong filter, a trip or a bad load, is the fastest way to damage the output devices and the relays. The checks are made by the panel before keying and ten times a second while keyed, and the link's own time limit and watchdog stand behind them in case the panel cannot.
- **What the operator sees:** the button greyed, with the reason on hover; while keyed, the seconds left; after an automatic stop, why.
- **Changed:** AJ3G, 19 Oct 2026, with the power buttons (decision 18). Before, the menu 34 slider would not write a step over the drive limit while the amplifier was in line. The buttons now write any step, MAX included, as SDRoxide's did, because MAX is the setting to use with the HR500 in line and the Tune slider brings the power down; TUNE itself is still refused over the limit. A yellow line says when menu 34 is over the limit in line, since other programs' audio can then overdrive the amplifier.

## 7. TX TUNE sends only the panel's tone

- **Where:** `crates/rigcat/src/link.rs`, `key_audio()` and `Rig::write()`; `crates/rigcat/src/elad.rs`, `audio_ready()`; `crates/hardrock-panel/src/tune.rs`, `tone_blocked()`; `crates/hardrock-panel/src/main.rs`, `do_tune()` and `keep_tune()`.
- **Blocks keying when:** the radio's TX IN (menu 32) is not on USB audio (on the microphone, or on Auto, it would send whatever the microphone picks up); the radio is not in USB or LSB (in CW, AM or FM the ordinary transmission is something other than a single carrier); there is no sound output for the tone; the tone won't start. The link reads menu 32 and the mode again straight before it sends `TX;`, and `Rig::write()` refuses `TX;` unless those reads have just passed, so no other path in the code can send it.
- **Order:** the tone starts first, then the radio is keyed; on the way out the radio is unkeyed first, then the tone stops. Keyed with no tone, the radio sends nothing.
- **Why:** an ordinary transmission sends whatever audio is on the radio's input. With the wrong input it puts the shack on the air through the amplifier; in the wrong mode it is not the carrier the operator asked for.
- **What the operator sees:** TX TUNE greyed, with the reason on hover. With menu 32 on anything but USB audio, a yellow line says so, with a USB AUDIO button that sets it.
- **Added:** AJ3G, 19 Oct 2026, with TX TUNE (decision 17). Tested by `tune_with_drive_needs_usb_audio_and_a_sideband`, `tune_with_drive_reads_the_input_and_mode_then_keys_an_ordinary_transmission`, `tune_with_drive_is_refused_while_the_radio_takes_the_microphone` and `the_tone_needs_its_sound_output_usb_audio_and_a_sideband`.

## Not interlocks, but related

- TUNE is allowed while transmitting: tuning needs a carrier, and the ATU switches its own relays under its own control at tuning power.
- Yellow and red are kept for warnings only (AJ3G): states such as OFF, BYP and tuning use the neutral blue.
