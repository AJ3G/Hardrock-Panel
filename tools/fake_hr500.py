#!/usr/bin/env python3
"""A pretend Hardrock-500 with an ATU-500 on a pseudo-terminal, for trying the
panel without an amplifier. Linux and macOS only.

    python3 tools/fake_hr500.py            # prints the port to choose
    python3 tools/fake_hr500.py --keyed    # transmitting at about 400 W
    python3 tools/fake_hr500.py --off --drive 6   # in OFF, 6 W passing through

It answers the commands the panel sends with fixed, plausible readings. It
does not model the amplifier: buttons change the band, mode, antenna and
tuner state it reports back, and nothing else.
"""

import os
import select
import sys
import tty

keyed = "--keyed" in sys.argv
drive = float(sys.argv[sys.argv.index("--drive") + 1]) if "--drive" in sys.argv else 0.0
state = {"band": 5, "mode": 0 if "--off" in sys.argv else 1, "ant": 1, "atu": 1}

master, slave = os.openpty()
tty.setraw(slave)
print(os.ttyname(slave), flush=True)


def status():
    if keyed and state["mode"] == 1:
        fwd, rfl, drv, swr, vdd, idd, temp, lights = 400, 10, 60, 13, 50, 180, 41, "11111101"
    else:
        fwd, rfl, drv, swr, vdd, idd, temp, lights = 0, 0, round(drive * 10), 0, 53, 0, 30, "01111101"
    return (
        f"HRST-{fwd:03}-{rfl:03}-{drv:03}-{swr:03}-{vdd:03}-{idd:03}-{temp:03}-"
        f"{state['mode']}-{state['band']:02}-{state['ant']}-{lights}-;"
    )


def answer(cmd):
    if cmd.startswith("FA") and len(cmd) == 13:
        # The radio's frequency, which sets the band. Logged so a test can
        # see when it arrived.
        print(f"band from {int(cmd[2:]) / 1e6:.3f} MHz", file=sys.stderr, flush=True)
        return None
    if cmd == "HRAA":
        return "HRAA;"
    if cmd == "HRST":
        return status()
    if cmd == "HRTP":
        return "HRTP30C;"
    if cmd == "HRVT":
        return "HRVT53V;"
    if cmd == "HRAP":
        return "HRAP1;"
    if cmd.startswith("HRTB"):
        if len(cmd) > 4:
            state["atu"] = int(cmd[4:])
        return f"HRTB{state['atu']};"
    if cmd == "HRTMP":
        return f"HRTM-L003-C012-H0-B{state['band']}-S255-F255-;"
    if cmd == "HRTT":
        return "HRTT0;"
    if cmd == "HRTR":
        return "HRTRS;"
    for key, prefix in (("band", "HRBN"), ("mode", "HRMD"), ("ant", "HRAN")):
        if cmd.startswith(prefix):
            if len(cmd) > len(prefix):
                state[key] = int(cmd[len(prefix):])
            return f"{prefix}{state[key]};"
    return None


buf = ""
while True:
    select.select([master], [], [])
    buf += os.read(master, 256).decode(errors="replace")
    while ";" in buf:
        cmd, buf = buf.split(";", 1)
        reply = answer(cmd.strip().upper())
        if reply:
            os.write(master, reply.encode())
