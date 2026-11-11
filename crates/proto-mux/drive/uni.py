import subprocess
from harness import *
subprocess.run([BIN, "kill", "uni"], env=env(), capture_output=True); time.sleep(0.3)
LINE = "printf 'A e\\314\\201 B \\360\\237\\221\\215\\360\\237\\217\\275 C \\331\\204\\330\\247 D \\360\\237\\221\\250\\342\\200\\215\\360\\237\\221\\251\\342\\200\\215\\360\\237\\221\\247 E \\346\\227\\245 F|\\n'\r"
d = Window(["/bin/sh"], cols=60, rows=7); p = proto("uni", ["/bin/sh"], cols=60, rows=8)
d.pump(1); p.pump(1)
d.raw.clear(); p.raw.clear()
d.send(LINE, 0.8); p.send(LINE, 0.8)
open("uni-direct.bin", "wb").write(bytes(d.raw)); open("uni-proto.bin", "wb").write(bytes(p.raw))
print("direct:", repr(bytes(d.raw)[-120:]))
print("proto :", repr(bytes(p.raw)[-260:]))
d.send("exit\r", 0.3); p.send("exit\r", 0.5); d.close(); p.close()
