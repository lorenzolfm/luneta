import statistics, subprocess, sys
from harness import *
HERE = os.path.dirname(os.path.abspath(__file__))
Z = HERE + "/z"

def zellij():
    e = env()
    e.update(ZELLIJ_SOCKET_DIR="/run/user/1000/pmz", XDG_CACHE_HOME=Z + "/cache", XDG_DATA_HOME=Z + "/data", SHELL="/bin/sh")
    pid, fd = pty.fork()
    if pid == 0:
        os.execvpe("zellij", ["zellij", "--config", Z + "/config/config.kdl", "-n", Z + "/layout.kdl", "-s", "pmz-lat"], e)
    w = Window.__new__(Window)
    w.cols, w.rows = 100, 30
    w.screen = Screen(100, 30); w.stream = pyte.ByteStream(w.screen); w.raw = bytearray()
    w.pid, w.fd = pid, fd
    os.set_inheritable(fd, False)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 900, 540))
    w.screen.answer = lambda s: os.write(fd, s.encode())
    w.screen.write_process_input = lambda s: os.write(fd, s.encode())
    return w

def echo(w, n=60):
    w.until("$", 20); w.pump(1.0)
    samples = []
    for i in range(n):
        ch = "QXZJVWYUGH"[i % 10].encode()
        t = time.monotonic()
        os.write(w.fd, ch)
        while time.monotonic() - t < 2:
            r, _, _ = select.select([w.fd], [], [], 2)
            data = os.read(w.fd, 65536)
            w.stream.feed(re.sub(rb"\x1bP.*?\x1b\\", b"", data, flags=re.S))
            if ch in data:
                break
        samples.append((time.monotonic() - t) * 1000)
        w.pump(0.03)
        if i % 20 == 19:
            os.write(w.fd, b"\x15"); w.pump(0.2)
    os.write(w.fd, b"\x15"); w.pump(0.2)
    return samples

def flood(w):
    os.write(w.fd, b"clear; seq 1 200000; echo DONE-$((40+2))\r")
    t = time.monotonic()
    w.until("DONE-42", 60)
    return (time.monotonic() - t) * 1000

def report(label, s, f):
    s = sorted(s)
    print(f"{label:8} echo median {statistics.median(s):5.2f}ms  p90 {s[int(len(s)*.9)]:5.2f}ms  max {s[-1]:6.2f}ms   seq 200000: {f:7.0f}ms")

subprocess.run([BIN, "kill", "lat"], env=env(), capture_output=True); time.sleep(0.3)
for label, make in [("direct", lambda: Window(["/bin/sh"], cols=100, rows=29)),
                    ("proto", lambda: proto("lat", ["/bin/sh"], cols=100, rows=30)),
                    ("zellij", zellij)]:
    w = make()
    s = echo(w)
    f = flood(w)
    report(label, s, f)
    os.write(w.fd, b"exit\r"); w.pump(1.0); w.close()
subprocess.run(["zellij", "delete-session", "--force", "pmz-lat"], env={**env(), "ZELLIJ_SOCKET_DIR": "/run/user/1000/pmz"}, capture_output=True)
