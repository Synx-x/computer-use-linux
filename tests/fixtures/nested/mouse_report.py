"""Write the mouse reports a terminal sends to argv[1], then exit.

Run inside foot. SGR mouse mode makes the terminal report each event as
ESC [ < followed by the button, column and row, separated by semicolon
characters. A press ends in M and a release ends in m. Cell coordinates
are 1-based.

Mode "press" (the default) stops at the first press or wheel event.
Mode "drag" stops at the first release and keeps every report before it.
"""

import os
import re
import sys
import termios
import tty

out = sys.argv[1]
mode = sys.argv[2] if len(sys.argv) > 2 else "press"
fd = sys.stdin.fileno()
saved = termios.tcgetattr(fd)
tty.setraw(fd)
# 1000 reports presses, 1002 adds motion while a button is held, 1006 is SGR.
sys.stdout.write("\033[?1000h\033[?1002h\033[?1006h")
sys.stdout.flush()
pattern = re.compile(rb"\033\[<(\d+;\d+;\d+)([Mm])")
buffer = b""
reports = []
try:
    done = False
    while not done:
        buffer += os.read(fd, 256)
        reports = []
        for match in pattern.finditer(buffer):
            reports.append(match.group(1).decode() + match.group(2).decode())
            if mode == "press" or match.group(2) == b"m":
                done = True
                break
finally:
    sys.stdout.write("\033[?1006l\033[?1002l\033[?1000l")
    sys.stdout.flush()
    termios.tcsetattr(fd, termios.TCSADRAIN, saved)
with open(out + ".tmp", "w") as handle:
    handle.write(" ".join(reports))
os.replace(out + ".tmp", out)
