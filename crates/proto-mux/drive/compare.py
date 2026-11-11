import sys
from harness import *
COLS, ROWS = 100, 30
HERE = os.path.dirname(os.path.abspath(__file__)) + "/work"

def compare(title, argv, steps, settle=2.0):
    import subprocess; subprocess.run([BIN, "kill", title], env=env(), capture_output=True); time.sleep(0.3)
    d = Window(argv, cols=COLS, rows=ROWS - 1, cwd=HERE)
    p = Window([BIN, title, "--", *argv], cols=COLS, rows=ROWS, cwd=HERE)
    d.pump(settle); p.pump(settle)
    for keys, wait in steps:
        d.send(keys, 0); p.send(keys, 0)
        d.pump(wait); p.pump(wait)
    dt = d.screen.display[:ROWS - 1]
    pt = p.screen.display[:ROWS - 1]
    diffs = [(i, a.rstrip(), b.rstrip()) for i, (a, b) in enumerate(zip(dt, pt)) if a.rstrip() != b.rstrip()]
    # attributes of the cells too
    attr = 0
    for y in range(ROWS - 1):
        for x in range(COLS):
            a, b = d.screen.buffer[y][x], p.screen.buffer[y][x]
            if (a.fg, a.bg, a.bold, a.italics, a.underscore, a.reverse) != (b.fg, b.bg, b.bold, b.italics, b.underscore, b.reverse):
                attr += 1
                if attr <= 5:
                    print(f"  attr diff {title} at {y},{x} {a.data!r}: direct {a.fg},{a.bg},{a.bold},{a.reverse} proto {b.fg},{b.bg},{b.bold},{b.reverse}")
    cur = ((d.screen.cursor.y, d.screen.cursor.x), (p.screen.cursor.y, p.screen.cursor.x))
    print(f"##### {title}: {len(diffs)} lines differ, {attr} cells differ in style, cursor direct {cur[0]} proto {cur[1]}")
    for i, a, b in diffs[:12]:
        print(f"  {i:2} direct: {a!r}\n     proto : {b!r}")
    if "-v" in sys.argv:
        show(title + " direct", d.text())
        show(title + " proto", p.text())
    print("  status:", p.screen.display[ROWS - 1].rstrip())
    return d, p

which = sys.argv[1]
if which == "fish":
    d, p = compare("fish", ["fish", "--no-config"], [
        ("echo hello; set_color red; echo red; set_color normal; printf '日本 é x\\n'\r", 1.0),
        ("ls -la\r", 1.0),
        ("echo comp", 0.3), ("\t", 0.8),
    ])
elif which == "fishcfg":
    d, p = compare("fishcfg", ["fish"], [
        ("echo hello\r", 1.0),
        ("git sta", 0.5),
        ("\t", 1.0),
    ], settle=3.0)
elif which == "nvim":
    d, p = compare("nvim", ["nvim", "--clean", "a.rs"], [
        ("", 1.0),
        (":set number cursorline\r", 0.5),
        ("jw", 0.3),
        (":vsplit\r", 0.5),
        (":help\r", 2.0),
    ])
elif which == "nvimcfg":
    d, p = compare("nvimcfg", ["nvim", "a.rs"], [
        ("jw", 1.5),
        (":vsplit\r", 0.8),
    ], settle=4.0)
for w in (d, p):
    w.send("\x1b", 0.1)
