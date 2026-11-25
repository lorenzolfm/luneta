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
const FILE_BYTES_MAX = 64 * 1024;
const MESSAGE_BYTES_MAX = 1024;
const MESSAGE_RAW_CODE_UNITS_MAX = MESSAGE_BYTES_MAX * 4;

function invariant(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

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

function utf8BytesOfCodePoint(codePoint: number): number {
  invariant(codePoint >= 0 && codePoint <= 0x10ffff, "code point out of range");
  if (codePoint <= 0x7f) return 1;
  if (codePoint <= 0x7ff) return 2;
  if (codePoint <= 0xffff) return 3;
  return 4;
}

function firstLine(text: string | undefined): string | undefined {
  if (typeof text !== "string") return undefined;
  const bounded = text.slice(0, MESSAGE_RAW_CODE_UNITS_MAX).toWellFormed();
  const trimmed = bounded.trim();
  if (trimmed.length === 0) return undefined;
  let out = "";
  let bytes = 0;
  for (const character of trimmed) {
    if (character === "\r" || character === "\n") break;
    const codePoint = character.codePointAt(0);
    invariant(codePoint !== undefined, "iterated character has a code point");
    const width = utf8BytesOfCodePoint(codePoint);
    if (bytes + width > MESSAGE_BYTES_MAX) break;
    out += character;
    bytes += width;
  }
  invariant(bytes <= MESSAGE_BYTES_MAX, "message exceeds byte bound");
  const cleaned = out.trim();
  if (cleaned.length === 0) return undefined;
  invariant(!cleaned.includes("\n") && !cleaned.includes("\r"), "message spans multiple lines");
  return cleaned;
}

function errorReport(text: string | undefined): RestingReport {
  const message = firstLine(text);
  if (message !== undefined) {
    invariant(message.length > 0, "error message is empty");
    return { status: "error", message };
  }
  return { status: "error" };
}

function reportInFile(): Report | undefined {
  const file = fileOfThisProcess();
  if (!file) return undefined;
  const stats = lstatSync(file);
  if (!stats.isFile() || stats.size > FILE_BYTES_MAX) return undefined;
  const parsed: unknown = JSON.parse(readFileSync(file, "utf8"));
  if (typeof parsed !== "object" || parsed === null) return undefined;
  const record = parsed as {
    status?: unknown;
    statusMessage?: unknown;
  };
  switch (record.status) {
    case "idle":
    case "busy":
    case "done":
    case "waiting":
      return { status: record.status };
    case "error":
      return errorReport(typeof record.statusMessage === "string" ? record.statusMessage : undefined);
    default:
      return undefined;
  }
}

function sameReport(written: Report | undefined, current: Report): boolean {
  if (written === undefined) return false;
  if (written.status !== current.status) return false;
  if (written.status === "error" && current.status === "error") {
    return written.message === current.message;
  }
  return true;
}

function currentReport(phase: Phase, promptOpen: boolean): Report {
  if (promptOpen) return { status: "waiting" };
  if (phase.kind === "running") return { status: "busy" };
  invariant(phase.kind === "resting", "phase must be resting");
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
    if (shutDown) return;
    const now = currentReport(phase, promptOpen);
    if (sameReport(written, now)) return;
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
    if (now.status === "error" && now.message !== undefined) {
      invariant(now.message.length > 0, "error report holds empty message");
      said.statusMessage = now.message;
    }
    const payload = JSON.stringify(said);
    quietly(() => {
      const file = fileOfThisProcess();
      if (!file) return;
      const temporary = `${file}.new`;
      writeFileSync(temporary, payload);
      try {
        renameSync(temporary, file);
      } catch (error) {
        quietly(() => rmSync(temporary, { force: true }));
        throw error;
      }
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
      case "waiting": {
        const running = ctx.isIdle !== undefined && !ctx.isIdle();
        if (running) {
          phase = { kind: "running", outcome: { status: "done" } };
        } else {
          phase = { kind: "resting", resting: { status: "idle" } };
        }
        promptOpen = true;
        break;
      }
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
    switch (message.stopReason) {
      case "error":
        phase = { kind: "running", outcome: errorReport(message.errorMessage) };
        break;
      case "aborted":
        phase = { kind: "running", outcome: { status: "idle" } };
        break;
      default:
        phase = { kind: "running", outcome: { status: "done" } };
        break;
    }
  });

  pi.on("agent_settled", (event, ctx) => {
    if (event.aborted === true) {
      phase = { kind: "resting", resting: { status: "idle" } };
    } else if (phase.kind === "running") {
      phase = { kind: "resting", resting: phase.outcome };
    }
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
