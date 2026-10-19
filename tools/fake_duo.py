#!/usr/bin/env python3
"""A pretend ELAD FDM-DUO CAT port on a pseudo-terminal, for trying the
panel's RADIO link without a radio. Linux and macOS only.

    python3 tools/fake_duo.py              # receiving on 14.074 MHz USB
    python3 tools/fake_duo.py --keyed      # transmitting about 6 W, SWR 1.32
    python3 tools/fake_duo.py --freq 7074000
    python3 tools/fake_duo.py --mic        # menu 32 TX IN on the microphone
    python3 tools/fake_duo.py --mode 3     # in CW

It answers the READ commands the panel sends with fixed readings, takes
menu 34 (`TPnn;`) and menu 32 to USB audio (`TI1;`), keys its TUNE carrier
on `TX2;` and an ordinary transmission on `TX;`, both at menu 34's power,
unkeys on `RX;`, and answers `?;` to anything else. It prints every command
that changes something, and flags any it should never be sent.
"""

import os
import select
import sys
import tty

state = {
    "keyed": "--keyed" in sys.argv,
    "tp": 9,
    "ti": 0 if "--mic" in sys.argv else 1,
    "md": sys.argv[sys.argv.index("--mode") + 1] if "--mode" in sys.argv else "2",
}
STEPS_MW = [300, 500, 1000, 1200, 1500, 2000, 3000, 4000, 5000, 8000]
freq = int(sys.argv[sys.argv.index("--freq") + 1]) if "--freq" in sys.argv else 14_074_000

master, slave = os.openpty()
tty.setraw(slave)
print(os.ttyname(slave), flush=True)

READS = {
    "FA": lambda: f"FA{freq:011}",
    "FB": lambda: "FB00007074000",
    "FR": lambda: "FR0",
    "MD": lambda: f"MD{state['md']}",
    "RF": lambda: "RF208",
    "SP": lambda: "SP0",
    "RT": lambda: "RT0",
    "AN": lambda: "AN1",
    "TI": lambda: f"TI{state['ti']}",
    "TP": lambda: f"TP{state['tp']:02}",
    "SM0": lambda: "SM00011",
    "DT": lambda: "DT0",
    "VS": lambda: "VS01.22",
    "FP": lambda: f"FP {STEPS_MW[state['tp']]:06}" if state["keyed"] else "FP!000000",
    "RP": lambda: f"RP {STEPS_MW[state['tp']] // 50:06}" if state["keyed"] else "RP!000000",
    "WR": lambda: "WR0 01.32" if state["keyed"] else "WR0!00.00",
}

buf = ""
while True:
    select.select([master], [], [])
    buf += os.read(master, 256).decode(errors="replace")
    while ";" in buf:
        cmd, buf = buf.split(";", 1)
        cmd = cmd.strip().upper()
        reply = READS.get(cmd)
        if cmd.startswith("TP") and len(cmd) == 4:
            state["tp"] = int(cmd[2:])
            print(f"menu 34 set to {cmd[2:]}", file=sys.stderr, flush=True)
        elif cmd == "TX2":
            state["keyed"] = True
            print("TUNE keyed", file=sys.stderr, flush=True)
        elif cmd == "TX":
            state["keyed"] = True
            print(f"keyed (TX), TI{state['ti']} MD{state['md']}", file=sys.stderr, flush=True)
        elif cmd == "TI1":
            state["ti"] = 1
            print("menu 32 set to USB audio", file=sys.stderr, flush=True)
        elif cmd == "RX":
            state["keyed"] = False
            print("unkeyed", file=sys.stderr, flush=True)
        elif reply is None:
            print(f"SHOULD NEVER BE SENT: {cmd};", file=sys.stderr, flush=True)
            os.write(master, b"?;")
        else:
            os.write(master, (reply() + ";").encode())
