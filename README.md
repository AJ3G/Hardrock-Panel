# Hardrock Panel

A standalone control panel for the **HobbyPCB Hardrock-500** linear amplifier
and its **ATU-500** tuner, on the amplifier's USB serial port.

![The panel, transmitting into a simulated HR500](docs/images/panel.png)

It is the AMP window from [SDRoxide](https://github.com/dividebysandwich/sdroxide),
running on its own with no radio program beside it. What SDRoxide gave it
from the radio, it reads from the radio's CAT port itself. It does the following:

- Shows the amplifier's readings live: FWD, RFL, DRV, VDD, IDD, TEMP and SWR,
  in the amplifier's own colours.
- Draws two cross-needle meters, with the needles chosen by the operator.
- Works out input power, efficiency and gain while transmitting, with a short
  peak hold for FT8 and SSB.
- Has controls for PTT / OFF, antenna and band DN / UP.
- Runs the ATU-500 tuner: TUNE and ON / BYP, plus a drawing of its network with
  estimated L and C values.
- Reads an ELAD FDM-DUO over its CAT port (COM3, 38400 8N1 by default):
  its own forward and reflected power and SWR fill the amplifier's readings
  in bypass, its dial can set the amplifier's band between transmissions, and
  its menu settings are shown in a RADIO window that can pop out. SDRoxide's
  1W / 3W / 5W / MAX buttons set its menu 34, and TX TUNE keys it as its PTT
  does (keying the amplifier too) with a tone into its USB audio at the Tune
  slider's level; RIG TUNE keys its own carrier. Both run under safety
  interlocks that unkey on a trip, high SWR, overdrive or a time limit, and
  TX TUNE only with the radio's input on USB audio. Nothing else is ever sent
  to the radio but reads.
- Shows the DUO's I/Q (over its USB lead, or a demo band) in a SCOPE window:
  a GPU waterfall and 3D FFT, a band plan, markers and the panel's own
  receiver, played on any sound output, with the radio put in split so it
  transmits where you listen.
- Decodes FT8, FT4, WSPR, JT65, JT9, FST4, Q65, RTTY, PSK31, NAVTEX, Olivia,
  THOR, SSTV and WEFAX from the receiver, and HFDL, ACARS, VDL2, AIS and
  Mode S from the I/Q, in a DECODE window.
- Serves outside programs over TCI and rigctld and sends WSJT-X UDP decodes,
  all off until turned on; a program may transmit only when allowed and only
  when TUNE's checks pass.
- Refuses to switch a relay while there may be RF in the amplifier. See
  [safety interlocks](docs/safety-interlocks.md).

The amplifier is still keyed by the radio's PTT line; the panel never keys it.

> Tested with a Hardrock-500 and ATU-500 on Windows 11 only. The Hardrock-50
> can be chosen and is marked *untested*.

## Download

Windows: download `hardrock-panel-windows.zip` from Releases, unzip it, and run
`hardrock-panel.exe`. Nothing needs to be installed.

## Using it

See the [user guide](docs/user-guide.md). In short:

1. Close HobbyPCB's control panel, and any other program using the amplifier's
   port. Only one program can hold the port.
2. Start the panel. In **Settings**, tick **Enable**, choose the port (COM5,
   for example), leave the baud at 115200, and press **APPLY**.
3. Optionally, under **Radio**, tick **Enable** and choose the FDM-DUO's CAT
   port and baud (menu 70; 38400 from the factory).
4. With the radio connected the amplifier's band follows its dial between
   transmissions. Without it, switch bands with **DN** and **UP**, or let the
   radio set them over the amplifier's own CAT or ACC input.

`hardrock-panel.exe --port COM5 --radio COM3` opens straight onto the
amplifier on COM5 and the radio on COM3. `hardrock-panel.exe --demo --decode`
opens the scope on a made-up band, and DECODE, with no hardware.

## Building

You need Rust 1.85 or later.

```
cargo build --release
cargo test
```

Cross-compiling for Windows from Linux:

```
rustup target add x86_64-pc-windows-gnu    # needs mingw-w64
cargo build --release --target x86_64-pc-windows-gnu
```

### Trying it without an amplifier

On Linux or macOS, `tools/fake_hr500.py` pretends to be a Hardrock-500 with an
ATU-500 on a pseudo-terminal, and `tools/fake_duo.py` an FDM-DUO's CAT port:

```
python3 tools/fake_hr500.py            # prints the port, e.g. /dev/pts/3
python3 tools/fake_hr500.py --keyed    # transmitting at about 400 W
python3 tools/fake_hr500.py --off --drive 6   # in OFF, the radio's power through
python3 tools/fake_duo.py --keyed      # an FDM-DUO transmitting about 6 W
cargo run -p hardrock-panel -- --port /dev/pts/3 --radio /dev/pts/4
```

`cargo run -p hardrock --example hrctl -- /dev/pts/3` talks to it from the
command line.

## Layout

| Path | What it holds |
|---|---|
| `crates/hardrock` | The amplifier's serial protocol and the background link that polls it and holds the interlocks |
| `crates/rigcat` | The radio's CAT port: a `Protocol` per radio (the FDM-DUO so far), the link, menu 34, menu 32 and TUNE |
| `crates/hardrock-panel` | The window: panel, meters, settings, TUNE and its tone, the scope, the receiver, DECODE, the outside programs' `Rig`, and SDRoxide's look |
| `crates/elad-iq` | The ELAD's USB I/Q driver |
| `crates/sdr` | Spectrum, zoom FFT, the receiver, its sound output, and the pipeline |
| `crates/decoders`, `crates/datalink` | The panel's decoders: WSJT, keyboard and picture modes; HFDL, ACARS, VDL2, AIS, Mode S |
| `crates/links` | TCI, rigctld and WSJT-X UDP |
| `docs/` | User guide, [safety interlocks](docs/safety-interlocks.md), [design decisions](docs/design-decisions.md), [ATU values](docs/atu-values.md), [open items](docs/todo.md) |
| `docs/history/` | The SDRoxide build's notes, test log and bench measurements |
| `tools/` | The simulated amplifier and radio |

## Rules this code keeps

These carry over from the SDRoxide build. Each is explained in the docs.

- Every block that refuses an action for safety is marked `SAFETY INTERLOCK`
  in the code and listed in [safety-interlocks.md](docs/safety-interlocks.md).
- Raw readings are live. Only derived figures (input power, efficiency, gain)
  hold a peak.
- Yellow and red mean a warning. A neutral state is blue.
- Every scale shows its yellow and red stretches.
- Anything that can be clicked shows a pointing hand and an outline.
- Anything that departs from the original design is written up in
  [design-decisions.md](docs/design-decisions.md).

## Credits and licence

Hardrock Panel is derived from SDRoxide by dividebysandwich and its
contributors, and like SDRoxide is licensed under the
[GNU GPL, version 3 or later](LICENSE). The fonts Chakra Petch and Share Tech
Mono are under the SIL Open Font License; see `crates/hardrock-panel/assets/fonts/`.
GLOBE's map is from [Natural Earth](https://www.naturalearthdata.com) and its
relief picture from NASA's Blue Marble Next Generation, both in the public
domain; see `crates/hardrock-panel/assets/globe/`.

Hardrock-500, Hardrock-50 and ATU-500 are products of HobbyPCB. This project is
not affiliated with HobbyPCB. No HobbyPCB manuals, schematics or firmware are
included.

Maintained by AJ3G.
