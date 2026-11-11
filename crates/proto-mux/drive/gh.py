"""PROTOTYPE: drive proto-mux in a real Ghostty window under Hyprland."""
import json, os, subprocess, sys, time

SIG = "acc1aa0bfd4bd575dcb0f76c35ccf12b660a1ab9_1794311670_1873481171"
ENV = {**os.environ, "HYPRLAND_INSTANCE_SIGNATURE": SIG, "WAYLAND_DISPLAY": "wayland-1"}
BIN = "/home/lorenzo/Projects/misc/luneta/md-luneta-331-step2-handoff/target/release/proto-mux"
DIR = "/run/user/1000/pmg"
CLASS = "dev.luneta.probe"
SHOTS = os.path.dirname(os.path.abspath(__file__)) + "/shots"
os.makedirs(SHOTS, exist_ok=True)
os.makedirs(DIR, exist_ok=True)


def hypr(*args):
    return subprocess.run(["hyprctl", *args], env=ENV, capture_output=True, text=True).stdout


def window():
    for c in json.loads(hypr("clients", "-j")):
        if c["class"] == CLASS:
            return c
    return None


def launch(session, command=(), cwd=None):
    cmd = f"env PROTO_MUX_DIR={DIR} {BIN} {session}"
    if command:
        cmd += " -- " + " ".join(command)
    if cwd:
        cmd = f"cd {cwd} && {cmd}"
    ghostty = f"ghostty --class={CLASS} --gtk-single-instance=false --title=proto-mux-probe -e sh -c '{cmd}; sleep 30'"
    rules = '{ float = true, size = "1300 800", monitor = "HDMI-A-2", no_initial_focus = true }'
    print(hypr("dispatch", f"hl.dsp.exec_cmd([[{ghostty}]], {rules})").strip())
    for _ in range(100):
        if window():
            break
        time.sleep(0.1)
    time.sleep(1.5)
    w = window()
    print("window", w and (w["at"], w["size"], w["monitor"], w["workspace"]["name"]))


def close():
    w = window()
    if w:
        os.kill(w["pid"], 15)
    time.sleep(0.5)


def shot(name):
    w = window()
    (x, y), (wd, ht) = w["at"], w["size"]
    path = f"{SHOTS}/{name}.png"
    subprocess.run(["grim", "-g", f"{x},{y} {wd}x{ht}", path], env=ENV, check=True)
    print("shot", path)
    return path


NAMES = {" ": "space", "-": "minus", "/": "slash", ".": "period", ",": "comma", "'": "apostrophe",
         ";": "semicolon", "=": "equal", "_": ("SHIFT", "minus"), ":": ("SHIFT", "semicolon"),
         "|": ("SHIFT", "backslash"), "\"": ("SHIFT", "apostrophe"), "?": ("SHIFT", "slash"),
         "!": ("SHIFT", "1"), "*": ("SHIFT", "8"), "(": ("SHIFT", "9"), ")": ("SHIFT", "0"),
         "$": ("SHIFT", "4"), "\n": "Return"}


def key(mods, name):
    out = hypr("dispatch", f'hl.dsp.send_shortcut({{ mods = "{mods}", key = "{name}", window = "class:^({CLASS})$" }})').strip()
    if out != "ok":
        print("sendshortcut:", out)
    time.sleep(0.03)


def typed(text):
    for ch in text:
        k = NAMES.get(ch, ch)
        if isinstance(k, tuple):
            key(*k)
        elif ch.isupper():
            key("SHIFT", ch.lower())
        else:
            key("", k)


def send(session, text):
    subprocess.run([BIN, "send", session, text], env={**ENV, "PROTO_MUX_DIR": DIR}, check=True, capture_output=True)


def kill(session):
    subprocess.run([BIN, "kill", session], env={**ENV, "PROTO_MUX_DIR": DIR}, capture_output=True)


def log():
    try:
        return open(f"{DIR}/proto-mux.log").read()
    except FileNotFoundError:
        return ""
