#!/usr/bin/env python3
"""A pretend ELAD FDM-DUO CAT port on a pseudo-terminal, for trying the
panel's RADIO link without a radio. Linux and macOS only.

    python3 tools/fake_duo.py              # receiving on 14.074 MHz USB
    python3 tools/fake_duo.py --keyed      # transmitting about 6 W, SWR 1.32
    python3 tools/fake_duo.py --freq 7074000

It answers the READ commands the panel sends with fixed readings, `?;` to
anything else, and prints every command that is not a read, which should
never happen.
"""

import os
import select
import sys
import tty

keyed = "--keyed" in sys.argv
freq = int(sys.argv[sys.argv.index("--freq") + 1]) if "--freq" in sys.argv else 14_074_000

master, slave = os.openpty()
tty.setraw(slave)
print(os.ttyname(slave), flush=True)

READS = {
    "FA": lambda: f"FA{freq:011}",
    "FB": lambda: "FB00007074000",
    "FR": lambda: "FR0",
    "MD": lambda: "MD2",
    "RF": lambda: "RF208",
    "SP": lambda: "SP0",
    "RT": lambda: "RT0",
    "AN": lambda: "AN1",
    "TI": lambda: "TI1",
    "TP": lambda: "TP09",
    "SM0": lambda: "SM00011",
    "DT": lambda: "DT0",
    "VS": lambda: "VS01.22",
    "FP": lambda: "FP 006091" if keyed else "FP!000000",
    "RP": lambda: "RP 000120" if keyed else "RP!000000",
    "WR": lambda: "WR0 01.32" if keyed else "WR0!00.00",
}

buf = ""
while True:
    select.select([master], [], [])
    buf += os.read(master, 256).decode(errors="replace")
    while ";" in buf:
        cmd, buf = buf.split(";", 1)
        cmd = cmd.strip().upper()
        reply = READS.get(cmd)
        if reply is None:
            print(f"NOT A READ: {cmd};", file=sys.stderr, flush=True)
            os.write(master, b"?;")
        else:
            os.write(master, (reply() + ";").encode())
