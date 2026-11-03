// luneta's extension for pi: keeps a file that says what this agent is doing,
// for lunetad to list it by. lunetad puts this file where pi finds it, over any
// other of its name, each time it starts.
//
// One file to a process, `pi/<pid>.json` in luneta's folder, moved into place
// whole as it changes, and gone as pi quits. lunetad tells one that a killed pi
// left behind by the process its name names.

import { existsSync, lstatSync, mkdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
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
 * This process's file, in luneta's folder as lunetad and the shell hook find
 * it, which is made for this user alone where it is not there. None where there
 * is no place for that folder, or where it is another user's, or open to them,
 * or a link: what is in it says what you work on.
 */
function fileOfThisProcess(): string | undefined {
  const root = process.env.XDG_RUNTIME_DIR || process.env.TMPDIR;
  if (!root) return undefined;
  const folder = join(root, "luneta");
  try {
    mkdirSync(folder, { mode: 0o700 });
  } catch (error) {
    if ((error as { code?: string }).code !== "EEXIST") throw error;
  }
  const found = lstatSync(folder);
  const yours = found.isDirectory() && found.uid === process.getuid?.();
  if (!yours || (found.mode & 0o077) !== 0) return undefined;
  mkdirSync(join(folder, "pi"), { recursive: true });
  return join(folder, "pi", `${process.pid}.json`);
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
  let status: Status | undefined;
  let shutDown = false;

  /** Says the agent came to `now`, unless that is what it is doing. */
  function say(now: Status, ctx: Context): void {
    if (now === status || shutDown) return;
    status = now;
    quietly(() => {
      const file = fileOfThisProcess();
      if (!file) return;
      const said = {
        sessionId: ctx.sessionManager.getSessionId(),
        cwd: ctx.cwd,
        status: now,
        statusSince: Date.now(),
        // `cli` is the agent a person talks to at a prompt, as Claude Code says
        // it, and the one lunetad's clients seat in a pane before any other.
        entrypoint: ctx.mode === "tui" ? "cli" : ctx.mode,
      };
      writeFileSync(`${file}.new`, JSON.stringify(said));
      renameSync(`${file}.new`, file);
    });
  }

  pi.on("session_start", (event, ctx) => {
    // A reload is no turn: what the file says stands, and since when. One with
    // no file yet writes it, as how a pi that runs first takes this extension.
    const said = quietly(() => {
      const file = fileOfThisProcess();
      return file !== undefined && existsSync(file);
    });
    if (event.reason === "reload" && said) return;
    say("idle", ctx);
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
