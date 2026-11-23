import { lstatSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

type Status = "busy" | "idle" | "waiting";

type Context = {
  cwd: string;
  mode: string;
  sessionManager: { getSessionId(): string };
};

type Pi = {
  on(event: string, handler: (event: { reason?: string }, ctx: Context) => void): void;
};

const FOLDER = "LUNE_FOLDER";

function fileOfThisProcess(): string | undefined {
  try {
    mkdirSync(FOLDER, { mode: 0o700 });
  } catch (error) {
    if ((error as { code?: string }).code !== "EEXIST") throw error;
  }
  const found = lstatSync(FOLDER);
  const yours = found.isDirectory() && found.uid === process.getuid?.();
  if (!yours || (found.mode & 0o077) !== 0) return undefined;
  mkdirSync(join(FOLDER, "pi"), { recursive: true });
  return join(FOLDER, "pi", `${process.pid}.json`);
}

function procStart(): string | undefined {
  const stat = readFileSync("/proc/self/stat", "utf8");
  return stat.slice(stat.lastIndexOf(")") + 1).trim().split(/\s+/)[19];
}

function statusInFile(): Status | undefined {
  const file = fileOfThisProcess();
  return file ? (JSON.parse(readFileSync(file, "utf8")) as { status: Status }).status : undefined;
}

function quietly<T>(work: () => T): T | undefined {
  try {
    return work();
  } catch {
    return undefined;
  }
}

export default function (pi: Pi): void {
  const started = quietly(procStart);
  let status: Status | undefined;
  let shutDown = false;

  function say(now: Status, ctx: Context): void {
    if (now === status || shutDown) return;
    quietly(() => {
      const file = fileOfThisProcess();
      if (!file) return;
      const said = {
        sessionId: ctx.sessionManager.getSessionId(),
        cwd: ctx.cwd,
        status: now,
        statusSince: Date.now(),
        mode: ctx.mode,
        procStart: started,
      };
      writeFileSync(`${file}.new`, JSON.stringify(said));
      renameSync(`${file}.new`, file);
      status = now;
    });
  }

  pi.on("session_start", (event, ctx) => {
    status = event.reason === "reload" ? quietly(statusInFile) : undefined;
    if (status === undefined) say("idle", ctx);
  });

  pi.on("agent_start", (_event, ctx) => say("busy", ctx));
  pi.on("agent_settled", (_event, ctx) => say("idle", ctx));

  pi.on("ui_prompt_start", (_event, ctx) => {
    if (status === "busy") say("waiting", ctx);
  });
  pi.on("ui_prompt_end", (_event, ctx) => {
    if (status === "waiting") say("busy", ctx);
  });

  pi.on("session_shutdown", (event) => {
    shutDown = true;
    if (event.reason !== "quit") return;
    quietly(() => {
      const file = fileOfThisProcess();
      if (file) rmSync(file, { force: true });
    });
  });
}
