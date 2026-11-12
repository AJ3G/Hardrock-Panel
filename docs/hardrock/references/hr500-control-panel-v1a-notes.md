# What the stock HR500 Control Panel (V1a) does with the ATU

Read from `HR500_Panel_V1a.exe`, a PureBasic program uploaded by Rich, by disassembling it. Nothing here has been checked against a real amplifier yet.

## Tuner readout: `HRTMP;`

The panel sends `HRTMP;`, the tuner's `P` "report tuning parameters" message passed through with `HRTM`. It only parses replies longer than 31 characters. It reads fixed positions, 1-based, as PureBasic `Mid()` counts:

| Chars | Meaning | Use |
|---|---|---|
| 7-9 | L step (0-63) | **µH = step × 0.05** (2 decimals), so 0-3.15 µH |
| 12-14 | C step (0-127) | **pF = step × 10** (0 decimals), so 0-1270 pF |
| 17 | 1 = ATU bypassed | shows ATU_BYP image, hides L/C |
| 20 | capacitor side | 0 = CAP_IN image, 1 = CAP_OUT image |
| 23-25 | (number, unused here) | probably forward-power related |
| 28-30 | return loss in tenths of dB | used when 0 < RL < 241 |

SWR from return loss: k = 10^(RL/200), SWR = (k + 1) / (k − 1).

Images: 4 = CAP_IN.bmp, 5 = CAP_OUT.bmp, 6 = ATU_BYP.bmp (320×200, white on black).

## Other commands it sends
`HRST;` `HRAP;` `HRTU;` `HRTB0;` `HRTB1;` `HRMD0;` `HRMD1;` `HRAN1;` `HRAN2;` `HRBN…` `HRTSC;` `HRTSF;` (set temperature scale to C or F), and `HRAA;`, which is not in the 3-1-21 manual.
Settings are saved in `HR500_Panel.prefs`: COM port and window X and Y position.

## Checked on Rich's HR500 + ATU-500 (17 Oct 2026)

The real `HRTMP;` reply is `HRTM-L000-C000-H0-B1-S255-F255-` (untuned, 40 m, ANT 1). Each field is named after the tuner command behind it (manual p.25): `L`/`C` steps, `H` capacitor side, `B` the tuner's band, `S` last autotune status, `F` final return loss (255 = none). There is no bypass field: switching `HRTB0`/`HRTB1` changed neither digit, so bypass comes from `HRTB`. The stock panel's positions 17 and 20 land on the `H` and `B` digits, so SDRoxide reads the reply by letter instead. A tune into a mismatched antenna is still needed to see real L/C values and confirm `H1` means the antenna side.

Second check, 25 Ω (two 50 Ω loads on a T) on ANT 1, 40 m, about 4 W: the tuner reported L 0, C 15 (150 pF), `H0`, final return loss 14.4 dB (1.47:1). A load below 50 Ω needs the shunt capacitor on the transmitter side, so `H0` = transmitter side (CAP IN) and `H1` = antenna side (CAP OUT), as SDRoxide draws them. L stayed at 0, presumably because the T and jumpers supply the series inductance themselves.
