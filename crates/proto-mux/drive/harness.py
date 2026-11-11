"""PROTOTYPE driver: runs a program in a pty read by pyte, directly or through proto-mux."""
import os, pty, re, select, struct, sys, time, fcntl, termios, signal

sys.path[:0] = [
    "/nix/store/wgp98z84kj74fpfk30snz8a5nlpx53lj-python3.14-pyte-0.8.2/lib/python3.14/site-packages",
    "/nix/store/ay094ipk7pky0yyqp4cyxmnafi54v0yc-python3.14-wcwidth-0.9.2/lib/python3.14/site-packages",
]
import pyte

BIN = os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../../target/release/proto-mux")
DIR = "/run/user/1000/pmx"


def env():
    e = {k: v for k, v in os.environ.items()
         if not k.startswith(("CLAUDE", "ZELLIJ", "TERM_PROGRAM", "GHOSTTY", "TMUX"))}
    e["TERM"] = "xterm-256color"
    e["COLORTERM"] = "truecolor"
    e["PROTO_MUX_DIR"] = DIR
    return e


class Screen(pyte.Screen):
    def report_device_status(self, *a, **k):
        # DSR 6: the cursor's place, as a terminal answers.
        if a and a[0] == 6:
            self.answer(f"\x1b[{self.cursor.y + 1};{self.cursor.x + 1}R")


class Window:
    def __init__(self, argv, cols=100, rows=30, cwd=None):
        self.cols, self.rows = cols, rows
        self.screen = Screen(cols, rows)
        self.stream = pyte.ByteStream(self.screen)
        self.raw = bytearray()
        pid, fd = pty.fork()
        if pid == 0:
            if cwd:
                os.chdir(cwd)
            os.execvpe(argv[0], argv, env())
        os.set_inheritable(fd, False)
        self.pid, self.fd = pid, fd
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, cols * 9, rows * 18))
        self.screen.answer = lambda s: os.write(fd, s.encode())
        # pyte answers DA1 itself through write_process_input.
        self.screen.write_process_input = lambda s: os.write(fd, s.encode())

    def pump(self, seconds=0.0):
        end = time.monotonic() + seconds
        while True:
            left = max(0.0, end - time.monotonic())
            r, _, _ = select.select([self.fd], [], [], left)
            if not r:
                if time.monotonic() >= end:
                    return
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                return
            if not data:
                return
            self.raw += data
            data = re.sub(rb"\x1bP.*?\x1b\\", b"", data, flags=re.S)
            self.stream.feed(data)

    def until(self, needle, timeout=10.0):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if needle in self.text():
                return True
            self.pump(0.01)
        return False

    def send(self, data, settle=0.3):
        os.write(self.fd, data if isinstance(data, bytes) else data.encode())
        self.pump(settle)

    def text(self):
        return "\n".join(line.rstrip() for line in self.screen.display)

    def resize(self, cols, rows):
        self.cols, self.rows = cols, rows
        self.screen.resize(rows, cols)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, cols * 9, rows * 18))
        os.kill(self.pid, signal.SIGWINCH)

    def close(self):
        try:
            os.kill(self.pid, signal.SIGHUP)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(self.pid, 0)
        except ChildProcessError:
            pass
        os.close(self.fd)


def proto(name, argv, **k):
    return Window([BIN, name, "--", *argv], **k)


def show(title, text):
    print(f"===== {title}")
    print(text)


def _display(self):
    import wcwidth
    out = []
    for y in range(self.lines):
        line, skip = [], False
        for x in range(self.columns):
            if skip:
                skip = False
                continue
            ch = self.buffer[y][x].data or " "
            line.append(ch)
            skip = wcwidth.wcwidth(ch[0]) == 2
        out.append("".join(line))
    return out


Screen.display = property(_display)
