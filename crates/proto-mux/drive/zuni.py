import sys; sys.argv=["x"]
exec(open("latency.py").read().split("subprocess.run([BIN")[0])
exec(open("uni.py").read().split("d = Window")[0].split("subprocess.run")[0])
LINE = open("uni.py").read().split('LINE = ')[1].split('\n')[0]
LINE = eval(LINE)
w = zellij(); w.until("$", 20); w.pump(1); w.raw.clear()
w.send(LINE, 1.0)
print("zellij:", repr(bytes(w.raw)[-300:]))
w.send("exit\r", 0.5); w.close()
subprocess.run(["zellij", "delete-session", "--force", "pmz-lat"], env={**env(), "ZELLIJ_SOCKET_DIR": "/run/user/1000/pmz"}, capture_output=True)
