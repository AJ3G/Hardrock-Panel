> Section 2.23 of the SDRoxide user manual as AJ3G's SDRoxide build has it, kept for reference. The standalone panel's own guide is [../user-guide.md](../user-guide.md).

### 2.23 Hardrock amplifier

![The AMP window](images/amp-window.png)

sdroxide can watch and control a **HobbyPCB Hardrock-500** linear amplifier
over its USB serial port. The **AMP** window shows its meters and readings,
works out input power, efficiency and gain while you transmit, and runs its
ATU-500 tuner. The amplifier is still keyed by the radio's PTT line, as before;
sdroxide never keys it.

> **Tested only with the Hardrock-500.** The Hardrock-50 can be chosen and is
> marked *untested*: it shares the command set, but its limits are scaled
> from the Hardrock-500's and its built-in tuner is shown as raw steps. Watch the amplifier's
> own display until it has been tried on a real one.
>
> The panel has been tested on Windows 11 only.

#### Setting it up

Open **⚙ SETTINGS ▸ Amplifier** ([§6](#6-settings)):

- **Enable**, then choose the **Model** and the **Port** the amplifier's USB
  lead appears as.
- **Baud** must match the amplifier's menu. HobbyPCB's own control panel needs
  115200, so an amplifier that has been used with it is already there.
- **Follow the radio's band** sends the transmit frequency to the amplifier on
  every band change, so its filters follow the radio without a CAT cable of
  their own. It is never sent during a transmission.
- **Show its power and SWR on the TX meter** fills the TX meter from the
  amplifier when the radio doesn't measure them itself, and only while the
  amplifier is in line.

Press **APPLY**. The status line reads *Connected to the Hardrock-500* when the
amplifier answers. The settings belong to the station and are kept in
`amp.json` ([§13](#13-configuration-files)), like the T/R switch: the
amplifier is on a port of the machine the radio is attached to.

Open the window with the **AMP** chip in the System module.

#### The window

**What can be clicked.** Anything in the window that does something when
clicked shows a pointing hand and a white outline when the mouse is over it.
If the outline appears, it's a control; if it doesn't, it's only a display.
PTT / OFF and the TEMP tile, which would otherwise look like a light and a
reading, also carry a small folded corner at the top right. A button greyed
out by an interlock (see below) shows no outline, because clicking it does
nothing.

**Meters.** Two cross-needle meters, each with a white outer needle and a blue
inner one. Choose what each needle shows with its **TOP** and **BOTTOM**
pickers: forward power, reflected power, drive, supply volts or drain current.
They start as FWD over DRV on the left, so the power meter shows the radio's
drive going in beside the amp's output, and IDD over RFL on the right.
Picking a reading already on the other needle swaps them. The needles are
live; they don't hold peaks. Every scale shows its yellow and red stretches,
and readings that run past the end are marked beyond the arc with their
values, so the limits are always in view.

**When a scale changes range.** The meters start on their full scales: until
power is first seen, the amplifier is taken to be in line and ready to make
full power. Only these change a scale's range, and the legend always says so:

| When | What changes | Legend | Back |
|---|---|---|---|
| The power stays at 10 W or less for 2.5 s while transmitting, in line or in bypass | FWD and DRV to 0–10 W, RFL to 0–2 W | *10 W scale* | To full scale at once when the amp is in line and the power goes over 10 W |
| In OFF (bypass), the power goes over 10 W for 2 s in all, or swings over it twice | FWD and DRV to 0–100 W, RFL to 0–20 W: a 100 W radio running through the bypassed amp | *100 W scale* | Down to the 10 W scale once the power settles at 10 W or less for 2.5 s |
| The power goes over 100 W, or over 10 W with the amp in line | FWD, DRV and RFL to their full scales | (none) | (this is the start) |
| Two needles on one meter would sit on top of each other | The inner (bottom) scale only, to ½, ⅕ or 2× its range, whichever parts them, so both can be read | *rescaled* | Once the needles are well apart on the usual scale |

In OFF (bypass) the radio's own power passes straight through the amp, so the
power counted is the higher of FWD and DRV. Most operators who run the amp in
bypass do so with a QRP radio, which the 10 W scale suits; a 100 W radio gets
the 100 W scale after its first couple of seconds. Going up to the full scales
never waits, so a needle is never held pinned at real power; coming down waits
for the power to settle.

A power range and a rescale can apply at once (*10 W scale, rescaled*).

**Running barefoot in bypass.** In OFF the DRV tile stays green whatever it
reads, tuner in or not, because the radio's power passing straight through is
not a fault. **PTT** is greyed while the drive is over the amplifier's drive
limit (8 W on the HR500): hover over it to see why, and lower the radio's
power before putting the amplifier in line.

In OFF the amplifier reports no forward or reflected power and no SWR, so
those come from the radio while it transmits:

- **SWR:** the radio's own SWR reading, where it reports one over CAT.
- **FWD:** the watts the radio measures, where it reports watts. Otherwise the
  amplifier's DRV reading, which measures the radio's power at the amplifier's
  input; in bypass that power goes straight through.
- **RFL:** the reflected watts the radio measures, where it reports them.
  Otherwise worked out from FWD and the SWR: at 3:1 a quarter of the forward
  power comes back.

The meter legend says where each came from (*from the rig*, *from DRV*, *from
the rig's SWR*), and so does hovering over the tile. The ELAD FDM-DUO reports
all three (forward and reflected power in milliwatts, and the SWR), so with a
DUO everything comes from the radio. Its readings are blank below 0.5 W, and
whenever the DUO marks them as not to be trusted, for example folding its power
back into a bad match: the legend then says *the rig flagged its reading*.
DRV is never used in their place with a DUO, because it over-reads into a
mismatch. In
PTT the amplifier's own readings are always used, here and on SDRoxide's main
TX meter.

**FDM-DUO power buttons.** With an ELAD FDM-DUO connected and its Rig
control (CAT) port set, four buttons sit beside the Drive slider in the TX
box: **1W**, **3W**, **5W** and **MAX**, captioned *ELAD FDM-DUO TX PWR*. They
appear for no other radio, not for the FDM-S2, and not for a DUO on its
receive cable alone. Each sets the DUO's own power setting, menu 34
`TX POWER`, to 1.0 W, 3.0 W, 5.0 W or MAX; the one in use is lit. Drive and
Tune then set the power within it. Your choice is saved and sent again each
time SDRoxide connects; until you pick one it is MAX.

Use **MAX** with the HR500 in line: at 5 W the DUO cannot drive the amplifier
to full output on most bands. Bring the power down with Drive instead. 1W, 3W
and 5W are for running barefoot or QRP. Each menu 34 step lets slightly more
out than it says: measured with a known-good wattmeter, 1 W gave 1.4 W, 3 W
3.8 W and 5 W about 6 W at Drive 100%.

**Drive on the FDM-DUO.** Drive sets the power through the transmit audio,
within menu 34's setting. The DUO reaches its full output (about 8 W at MAX)
with only a quarter of full audio, so SDRoxide sends at most that: at MAX,
Drive 100% is about 8 W, 50% about 4 W, and the output rises in a straight
line from about 0.5 W at the bottom. Before this, everything from about 25% up
gave the same 8 W.


**Readings.** Seven tiles: FWD, RFL, DRV, VDD, IDD, TEMP and SWR. Each takes
the amplifier's own colour for that reading: green, yellow or red. A colour
the amplifier holds after a transmission (it latches red until the next
key-up) is held here too, with one exception. Until your first transmission
after SDRoxide connects, a yellow from the amplifier shows green. The HR500 can
start up with DRV yellow, or still hold a yellow from an earlier session, with
nothing wrong now, and from remote that would look like a fault. A red (a
trip) always shows. From your first key-up the tiles follow the amplifier's
lights exactly, so its own screen and this one can differ only before then.
SWR has no colour of its own, as on the amplifier, except that it turns **red**
at or past the tune limit (Settings → General, shown beside the SWR limit).
There, a meter showing FWD or RFL measured by the radio says so across its
top in large red letters, for example *FWD UNRELIABLE AT THIS SWR*, and the
tiles say why on hover: into an open the FDM-DUO reported about three times
the power a wattmeter read, and only its SWR was right. Click
**TEMP** to switch between °C and °F; it switches the amplifier's own display
as well.

**Controls.**

- **PTT / OFF** puts the amplifier in line (green) or in bypass (blue).
- **ANT** chooses the antenna jack.
- **DN / UP** step the band.

**Performance.** Input power (volts × amps), efficiency (output over input) and
gain (output over drive, in dB), worked out only while transmitting. The
number holds its peak for a second and then falls away; the bar under it is
live, with the held peak marked on it. This suits FT8 and SSB, where the
readings move too fast to read.

**Tuner.** A drawing of the ATU-500's network: the inductor and capacitor it
has switched in, with values, and which side the capacitor is on. When the
tuner is bypassed, the drawing shows the bypass relays instead.

- **TUNE** starts a tune. It blinks blue while tuning, turns green when tuned
  and red if the tune fails, with the result and SWR on the line below.
- **ON / BYP** puts the tuner in line or bypasses it.

The tuner only reports step numbers. The values shown (for example "~0.20 µH ·
step 3") are an estimate that includes the board's own stray inductance and
capacitance, judged good to about 10%. The capacitance adds up the capacitors the
tuner's relays switch in, as its firmware drives them, so it isn't a straight
number per step. How the values were arrived at is in
`docs/hardrock/atu-values.md`.

**Pop out.** **POP OUT** moves the window into one of its own, which you can
put on another screen and resize; **DOCK** brings it back. Popped out, the
panel scales to fit the window's width and height and sits centred in it. In a
browser the window stays inside the page.

**Comes back as you left it.** When sdroxide starts, the AMP window comes back
as it was when sdroxide closed: open or closed, docked or popped out, in the
same place and at the same size, with the same meter picks.

#### What sdroxide won't do while you transmit

Switching a relay with RF on it can damage it, and on the Hardrock it makes the
amplifier flag reflected power. So while there is RF in the amplifier, sdroxide
won't switch the antenna, band or tuner:

- **ANT**, **DN**, **UP** and **ON / BYP** are greyed out, and hovering over
  them says why. Unkey first.
- The amplifier is asked whether it is transmitting before any of those is
  sent, and the command is dropped if it is, or if it doesn't answer.
- **Follow the radio's band** waits until the transmission is over.
- **PTT** can't be switched on (OFF to PTT) while the drive is over the
  amplifier's drive limit, 8 W on the HR500, as with a 100 W radio running
  barefoot through the bypassed amp. Lower the drive first.

Each of these is marked *SAFETY INTERLOCK* in the code and explained in
`docs/hardrock/safety-interlocks.md`.

#### Trips and over-temperature

When the amplifier trips, the status line at the top of the window turns red
and says *TRIPPED*, naming the reading that tripped it. Above its temperature
limit it reads *OFF LINE · over temperature*. The amplifier does the
protecting; sdroxide only reports it.

---

---

