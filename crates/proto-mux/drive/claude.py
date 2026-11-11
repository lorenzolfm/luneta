import subprocess, sys
from harness import *
WT = "/home/lorenzo/Projects/misc/luneta/md-luneta-331-step2-handoff"
subprocess.run([BIN, "kill", "cc"], env=env(), capture_output=True); time.sleep(0.3)
p = Window([BIN, "cc", "--", "claude"], cols=110, rows=36, cwd=WT)
d = Window(["claude"], cols=110, rows=35, cwd=WT)
for w in (p, d): w.until("for shortcuts", 20) or w.pump(5)
p.pump(1); d.pump(1)
def snap(label):
    dt, pt = d.screen.display, p.screen.display[:35]
    diff = [i for i in range(35) if dt[i].rstrip() != pt[i].rstrip()]
    print(f"##### {label}: {len(diff)} lines differ; cursor direct {d.screen.cursor.y, d.screen.cursor.x} proto {p.screen.cursor.y, p.screen.cursor.x}")
    for i in diff[:8]:
        print(f"  {i:2} D {dt[i].rstrip()!r}\n     P {pt[i].rstrip()!r}")
    if "-v" in sys.argv: show("proto", p.text())
snap("start")
for keys, label, wait in [
    ("hello there", "typed", 1.0),
    ("\x1b[13;2u", "shift-enter (kitty)", 1.0),
    ("second line", "second line", 1.0),
    ("\x15\x15", "ctrl-u", 1.0),
    ("/", "slash menu", 1.5),
    ("\x1b", "esc", 1.0),
]:
    for w in (p, d): w.send(keys, 0)
    p.pump(wait); d.pump(wait)
    snap(label)
show("proto final", p.text())
for w in (p, d):
    w.send("\x03", 0.3); w.send("\x03", 1.0)
