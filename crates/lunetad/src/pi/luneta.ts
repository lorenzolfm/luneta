// luneta's extension for pi: keeps a file that says what this agent is doing,
// for lunetad to list it by. lunetad keeps this file where pi finds it, over any
// other of its name, with its folder written in.
//
// One file to a process, `pi/<pid>.json` in that folder, moved into place whole
// as it changes, and gone as pi quits. lunetad tells one that a killed pi left
// behind by the process its name names.

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

/**
 * The folder lunetad reads, which it writes in here as it puts this file. This
 * process's variables would not do to find it: `nix develop` moves `TMPDIR`,
 * which names it where there is no `XDG_RUNTIME_DIR`, as on macOS.
 */
const FOLDER = "LUNETA_FOLDER";

/**
 * This process's file, in lunetad's folder, which is made for this user alone
 * where it is not there. None where that folder is another user's, or open to
 * them, or a link: what is in it says what you work on.
 */
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

/**
 * When this process started, as lunetad reads it on Linux: field 22 of its
 * `stat`, counted from the `)` that ends its name. Elsewhere this fails, and
 * lunetad goes by when the file was written.
 */
function procStart(): string | undefined {
  const stat = readFileSync("/proc/self/stat", "utf8");
  return stat.slice(stat.lastIndexOf(")") + 1).trim().split(/\s+/)[19];
}

/** What this process's file says it is doing, if there is one. */
function statusInFile(): Status | undefined {
  const file = fileOfThisProcess();
  return file ? (JSON.parse(readFileSync(file, "utf8")) as { status: Status }).status : undefined;
}

/** Does `work`, and leaves pi be if it fails: no row is all that comes of it. */
function quietly<T>(work: () => T): T | undefined {
  try {
    return work();
  } catch {
    return undefined;
  }
}

export default function (pi: Pi): void {
  const started = quietly(procStart);
  /** What this process's file says, as the last write that worked left it. */
  let status: Status | undefined;
  let shutDown = false;

  /** Says the agent came to `now`, unless that is what its file says. */
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
    // A reload is no turn: what the file says stands, and since when. One with
    // no file yet writes it, as how a pi that runs first takes this extension.
    status = event.reason === "reload" ? quietly(statusInFile) : undefined;
    if (status === undefined) say("idle", ctx);
  });

  // `agent_end` is no idle: a retry or a queued message can follow it.
  pi.on("agent_start", (_event, ctx) => say("busy", ctx));
  pi.on("agent_settled", (_event, ctx) => say("idle", ctx));

  // A prompt counts in a turn: one you opened at rest asks nothing of an agent
  // at rest, and would end in a new date for its idle.
  pi.on("ui_prompt_start", (_event, ctx) => {
    if (status === "busy") say("waiting", ctx);
  });
  pi.on("ui_prompt_end", (_event, ctx) => {
    if (status === "waiting") say("busy", ctx);
  });

  // Only a quit takes the file: /new, /resume, /fork and /reload go on in this
  // process, whose next `session_start` writes over it, after pi has loaded its
  // extensions again, which the row should not go away for.
  pi.on("session_shutdown", (event) => {
    shutDown = true;
    if (event.reason !== "quit") return;
    quietly(() => {
      const file = fileOfThisProcess();
      if (file) rmSync(file, { force: true });
    });
  });
}
