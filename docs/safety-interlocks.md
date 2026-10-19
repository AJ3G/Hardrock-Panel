# Hardrock amplifier: safety interlocks

Every place the panel refuses or delays something because doing it would be unsafe for the amplifier. #1, #2 and #4 came over unchanged from the AMP window in SDRoxide, where they were first written and bench-tested. #3 came over when the panel gained its own radio link, and #5 is new with that link. Each is marked in the code with `SAFETY INTERLOCK` and its number here. AJ3G asked (17 Oct 2026) that every such block say what it blocks and why.

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

## 5. The radio link never keys the radio or changes its settings

- **Where:** `crates/rigcat/src/elad.rs`, `is_read()`, and `crates/rigcat/src/link.rs`, `Rig::query()`.
- **Blocks:** every frame to the radio that is not one of the READ frames listed in `elad.rs` (`FP;`, `RP;`, `WR;`, `FA;`, `MD;` and the settings reads).
- **Why:** `TX;` keys the FDM-DUO with no parameter at all, so "a bare command is a read" is not safe, and `RC;` clears the RIT offset the same way. Keying the radio from the panel could put power into the amplifier when the operator did not mean to transmit, on whatever band and antenna it is on; changing a setting would leave the radio changed after the panel closes. So the link sends only frames on the list and refuses anything else, even from its own code. Tested by `nothing_that_keys_or_sets_the_radio_can_be_sent` and `a_frame_that_is_not_a_read_never_reaches_the_radio`. `tools/fake_duo.py` prints any frame that is not a read.
- **What the operator sees:** nothing; the radio is keyed and set up on the radio, as before.

## Not interlocks, but related

- TUNE is allowed while transmitting: tuning needs a carrier, and the ATU switches its own relays under its own control at tuning power.
- Yellow and red are kept for warnings only (AJ3G): states such as OFF, BYP and tuning use the neutral blue.
