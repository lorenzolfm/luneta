import { lstatSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

type RestingReport =
  | { readonly status: "idle" }
  | { readonly status: "done" }
  | { readonly status: "error"; readonly message?: string };

type Report =
  | RestingReport
  | { readonly status: "busy" }
  | { readonly status: "waiting" };

type Phase =
  | { readonly kind: "resting"; readonly resting: RestingReport }
  | { readonly kind: "running"; readonly outcome: RestingReport };

type Context = {
  cwd: string;
  mode: string;
  sessionManager: { getSessionId(): string };
  isIdle?: () => boolean;
};

type Event = {
  reason?: string;
  aborted?: boolean;
  message?: {
    role?: string;
    stopReason?: string;
    errorMessage?: string;
  };
};

type Pi = {
  on(event: string, handler: (event: Event, ctx: Context) => void): void;
};

const FOLDER = "LUNE_FOLDER";
const MESSAGE_CHARS_MAX = 512;

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

function firstLine(text: string | undefined): string | undefined {
  if (typeof text !== "string") return undefined;
  const line = text.trim().split(/\r?\n/, 1)[0]?.trim();
  return line ? line.slice(0, MESSAGE_CHARS_MAX) : undefined;
}

function errorReport(text: string | undefined): RestingReport {
  const message = firstLine(text);
  return message ? { status: "error", message } : { status: "error" };
}

function reportInFile(): Report | undefined {
  const file = fileOfThisProcess();
  if (!file) return undefined;
  const parsed = JSON.parse(readFileSync(file, "utf8")) as {
    status?: unknown;
    statusMessage?: unknown;
  };
  switch (parsed.status) {
    case "idle":
    case "busy":
    case "done":
    case "waiting":
      return { status: parsed.status };
    case "error":
      return errorReport(typeof parsed.statusMessage === "string" ? parsed.statusMessage : undefined);
    default:
      return undefined;
  }
}

function sameReport(a: Report | undefined, b: Report): boolean {
  if (!a || a.status !== b.status) return false;
  if (a.status === "error" && b.status === "error") {
    return a.message === b.message;
  }
  return true;
}

function currentReport(phase: Phase, promptOpen: boolean): Report {
  if (promptOpen) return { status: "waiting" };
  if (phase.kind === "running") return { status: "busy" };
  return phase.resting;
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
  let phase: Phase = { kind: "resting", resting: { status: "idle" } };
  let promptOpen = false;
  let written: Report | undefined;
  let shutDown = false;

  function sync(ctx: Context): void {
    const now = currentReport(phase, promptOpen);
    if (sameReport(written, now) || shutDown) return;
    quietly(() => {
      const file = fileOfThisProcess();
      if (!file) return;
      const said: {
        sessionId: string;
        cwd: string;
        status: Report["status"];
        statusMessage?: string;
        statusSince: number;
        mode: string;
        procStart: string | undefined;
      } = {
        sessionId: ctx.sessionManager.getSessionId(),
        cwd: ctx.cwd,
        status: now.status,
        statusSince: Date.now(),
        mode: ctx.mode,
        procStart: started,
      };
      if (now.status === "error" && now.message) {
        said.statusMessage = now.message;
      }
      writeFileSync(`${file}.new`, JSON.stringify(said));
      renameSync(`${file}.new`, file);
      written = now;
    });
  }

  pi.on("session_start", (event, ctx) => {
    const existing = event.reason === "reload" ? quietly(reportInFile) : undefined;
    if (existing === undefined) {
      phase = { kind: "resting", resting: { status: "idle" } };
      promptOpen = false;
      written = undefined;
      sync(ctx);
      return;
    }
    written = existing;
    switch (existing.status) {
      case "busy":
        phase = { kind: "running", outcome: { status: "done" } };
        promptOpen = false;
        break;
      case "waiting":
        phase = ctx.isIdle?.() === false
          ? { kind: "running", outcome: { status: "done" } }
          : { kind: "resting", resting: { status: "idle" } };
        promptOpen = true;
        break;
      case "idle":
      case "done":
      case "error":
        phase = { kind: "resting", resting: existing };
        promptOpen = false;
        break;
    }
  });

  pi.on("agent_start", (_event, ctx) => {
    phase = { kind: "running", outcome: { status: "done" } };
    sync(ctx);
  });

  pi.on("message_end", (event) => {
    if (phase.kind !== "running") return;
    const message = event.message;
    if (!message || message.role !== "assistant") return;
    if (message.stopReason === "error") {
      phase = { kind: "running", outcome: errorReport(message.errorMessage) };
    } else if (message.stopReason === "aborted") {
      phase = { kind: "running", outcome: { status: "idle" } };
    } else {
      phase = { kind: "running", outcome: { status: "done" } };
    }
  });

  pi.on("agent_settled", (event, ctx) => {
    const resting: RestingReport = event.aborted
      ? { status: "idle" }
      : phase.kind === "running"
        ? phase.outcome
        : { status: "done" };
    phase = { kind: "resting", resting };
    sync(ctx);
  });

  pi.on("ui_prompt_start", (_event, ctx) => {
    promptOpen = true;
    sync(ctx);
  });

  pi.on("ui_prompt_end", (_event, ctx) => {
    promptOpen = false;
    sync(ctx);
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
