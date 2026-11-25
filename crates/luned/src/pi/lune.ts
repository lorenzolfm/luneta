import { lstatSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const FOLDER = "LUNE_FOLDER";
const FILE_BYTES_MAX = 64 * 1024;
const MESSAGE_BYTES_MAX = 1024;
const MESSAGE_RAW_CODE_UNITS_MAX = MESSAGE_BYTES_MAX * 4;

function invariant(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function assertNever(value: never): never {
  throw new Error(`unexpected value: ${String(value)}`);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isControlCodePoint(codePoint: number): boolean {
  return codePoint < 0x20 || (codePoint >= 0x7f && codePoint <= 0x9f);
}

function utf8BytesOfCodePoint(codePoint: number): number {
  invariant(codePoint >= 0 && codePoint <= 0x10ffff, "code point out of range");
  if (codePoint <= 0x7f) return 1;
  if (codePoint <= 0x7ff) return 2;
  if (codePoint <= 0xffff) return 3;
  return 4;
}

class ErrorMessage {
  readonly #text: string;

  private constructor(text: string) {
    this.#text = text;
  }

  static parse(raw: unknown): ErrorMessage | undefined {
    if (typeof raw !== "string") return undefined;
    const bounded = raw.slice(0, MESSAGE_RAW_CODE_UNITS_MAX).toWellFormed();
    const trimmed = bounded.trim();
    if (trimmed.length === 0) return undefined;
    let out = "";
    let bytes = 0;
    for (const character of trimmed) {
      if (character === "\r" || character === "\n") break;
      const codePoint = character.codePointAt(0);
      invariant(codePoint !== undefined, "iterated character has a code point");
      if (isControlCodePoint(codePoint)) continue;
      const width = utf8BytesOfCodePoint(codePoint);
      if (bytes + width > MESSAGE_BYTES_MAX) break;
      out += character;
      bytes += width;
    }
    invariant(bytes <= MESSAGE_BYTES_MAX, "message exceeds byte bound");
    const cleaned = out.trim();
    if (cleaned.length === 0) return undefined;
    return new ErrorMessage(cleaned);
  }

  get text(): string {
    return this.#text;
  }
}

type RestingReport =
  | { readonly status: "stopped" }
  | { readonly status: "done" }
  | { readonly status: "error"; readonly message?: ErrorMessage };

type Report =
  | RestingReport
  | { readonly status: "busy" }
  | { readonly status: "waiting" };

type Phase =
  | { readonly kind: "resting"; readonly resting: RestingReport }
  | { readonly kind: "running"; readonly outcome: RestingReport };

type Snapshot = {
  readonly phase: Phase;
  readonly promptOpen: boolean;
};

type Written = {
  readonly snapshot: Snapshot;
  readonly since: number;
};

type ActiveState = {
  readonly kind: "active";
  phase: Phase;
  promptOpen: boolean;
  written: Written | undefined;
};

type Lifecycle = ActiveState | { readonly kind: "shutDown" };

type PersistedResting =
  | { readonly status: "stopped" }
  | { readonly status: "done" }
  | { readonly status: "error"; readonly message?: string };

type PersistedPhase =
  | { readonly kind: "resting"; readonly resting: PersistedResting }
  | { readonly kind: "running"; readonly outcome: PersistedResting };

type OutboundFile = {
  sessionId: string;
  cwd: string;
  status: Report["status"];
  statusMessage?: string;
  statusSince: number;
  mode: string;
  procStart?: string;
  phase: PersistedPhase;
  promptOpen: boolean;
};

type RestoredSnapshot = {
  readonly snapshot: Snapshot;
  readonly since: number;
  readonly exact: boolean;
};

type StopOutcome = "error" | "aborted" | "done";

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

function fileOfThisProcess(): string | undefined {
  try {
    mkdirSync(FOLDER, { mode: 0o700 });
  } catch (error) {
    if (!isRecord(error) || error["code"] !== "EEXIST") throw error;
  }
  const found = lstatSync(FOLDER);
  const yours = found.isDirectory() && found.uid === process.getuid?.();
  if (!yours || (found.mode & 0o077) !== 0) return undefined;
  mkdirSync(join(FOLDER, "pi"), { recursive: true });
  return join(FOLDER, "pi", `${process.pid}.json`);
}

function procStart(): string | undefined {
  const stat = readFileSync("/proc/self/stat", "utf8");
  const field = stat.slice(stat.lastIndexOf(")") + 1).trim().split(/\s+/)[19];
  return field !== undefined && field.length > 0 ? field : undefined;
}

function errorReport(raw: unknown): RestingReport {
  const message = ErrorMessage.parse(raw);
  if (message !== undefined) {
    return { status: "error", message };
  }
  return { status: "error" };
}

function serializeResting(resting: RestingReport): PersistedResting {
  switch (resting.status) {
    case "stopped":
      return { status: "stopped" };
    case "done":
      return { status: "done" };
    case "error":
      return resting.message !== undefined
        ? { status: "error", message: resting.message.text }
        : { status: "error" };
    default:
      return assertNever(resting);
  }
}

function serializePhase(phase: Phase): PersistedPhase {
  switch (phase.kind) {
    case "resting":
      return { kind: "resting", resting: serializeResting(phase.resting) };
    case "running":
      return { kind: "running", outcome: serializeResting(phase.outcome) };
    default:
      return assertNever(phase);
  }
}

function parseRestingStatus(status: unknown, message: unknown): RestingReport | undefined {
  switch (status) {
    case "stopped":
    case "idle":
      return { status: "stopped" };
    case "done":
      return { status: "done" };
    case "error":
      return errorReport(message);
    default:
      return undefined;
  }
}

function parseReport(status: unknown, message: unknown): Report | undefined {
  switch (status) {
    case "busy":
      return { status: "busy" };
    case "waiting":
      return { status: "waiting" };
    default:
      return parseRestingStatus(status, message);
  }
}

function parsePersistedResting(raw: unknown): RestingReport | undefined {
  if (!isRecord(raw)) return undefined;
  return parseRestingStatus(raw["status"], raw["message"]);
}

function parsePersistedPhase(raw: unknown): Phase | undefined {
  if (!isRecord(raw)) return undefined;
  switch (raw["kind"]) {
    case "resting": {
      const resting = parsePersistedResting(raw["resting"]);
      return resting !== undefined ? { kind: "resting", resting } : undefined;
    }
    case "running": {
      const outcome = parsePersistedResting(raw["outcome"]);
      return outcome !== undefined ? { kind: "running", outcome } : undefined;
    }
    default:
      return undefined;
  }
}

function parseStopReason(raw: unknown): StopOutcome | undefined {
  if (typeof raw !== "string" || raw.length === 0) return undefined;
  switch (raw) {
    case "error":
      return "error";
    case "aborted":
      return "aborted";
    default:
      return "done";
  }
}

function sameResting(a: RestingReport, b: RestingReport): boolean {
  if (a.status !== b.status) return false;
  if (a.status === "error" && b.status === "error") {
    return a.message?.text === b.message?.text;
  }
  return true;
}

function sameReport(a: Report | undefined, b: Report): boolean {
  if (a === undefined || a.status !== b.status) return false;
  if (a.status === "error" && b.status === "error") {
    return a.message?.text === b.message?.text;
  }
  return true;
}

function samePhase(a: Phase, b: Phase): boolean {
  if (a.kind !== b.kind) return false;
  if (a.kind === "resting" && b.kind === "resting") {
    return sameResting(a.resting, b.resting);
  }
  if (a.kind === "running" && b.kind === "running") {
    return sameResting(a.outcome, b.outcome);
  }
  return false;
}

function sameSnapshot(a: Snapshot, b: Snapshot): boolean {
  return a.promptOpen === b.promptOpen && samePhase(a.phase, b.phase);
}

function currentReport(phase: Phase, promptOpen: boolean): Report {
  if (promptOpen) return { status: "waiting" };
  switch (phase.kind) {
    case "running":
      return { status: "busy" };
    case "resting":
      return phase.resting;
    default:
      return assertNever(phase);
  }
}

function isCanonicalReportFields(record: Record<string, unknown>, report: Report): boolean {
  if (record["status"] !== report.status) return false;
  if (report.status === "error") {
    return record["statusMessage"] === report.message?.text;
  }
  return record["statusMessage"] === undefined;
}

function snapshotInFile(ctx: Context, started: string | undefined): RestoredSnapshot | undefined {
  const file = fileOfThisProcess();
  if (!file) return undefined;
  const stats = lstatSync(file);
  if (!stats.isFile() || stats.size > FILE_BYTES_MAX) return undefined;
  const parsed: unknown = JSON.parse(readFileSync(file, "utf8"));
  if (!isRecord(parsed)) return undefined;
  if (parsed["sessionId"] !== ctx.sessionManager.getSessionId()) return undefined;
  if (parsed["cwd"] !== ctx.cwd) return undefined;
  if (parsed["mode"] !== ctx.mode) return undefined;
  if (started !== undefined && parsed["procStart"] !== started) return undefined;
  const since = parsed["statusSince"];
  if (typeof since !== "number" || !Number.isFinite(since) || since < 0) return undefined;
  const report = parseReport(parsed["status"], parsed["statusMessage"]);
  if (report === undefined) return undefined;

  if ("phase" in parsed || "promptOpen" in parsed) {
    const phase = parsePersistedPhase(parsed["phase"]);
    const promptOpen = parsed["promptOpen"];
    if (phase === undefined || typeof promptOpen !== "boolean") return undefined;
    const projected = currentReport(phase, promptOpen);
    if (!sameReport(report, projected)) return undefined;
    const exact = isCanonicalReportFields(parsed, projected);
    return { snapshot: { phase, promptOpen }, since, exact };
  }

  switch (report.status) {
    case "busy":
      return {
        snapshot: { phase: { kind: "running", outcome: { status: "done" } }, promptOpen: false },
        since,
        exact: false,
      };
    case "waiting": {
      const running = ctx.isIdle !== undefined && !ctx.isIdle();
      const phase: Phase = running
        ? { kind: "running", outcome: { status: "done" } }
        : { kind: "resting", resting: { status: "stopped" } };
      return { snapshot: { phase, promptOpen: true }, since, exact: false };
    }
    case "stopped":
    case "done":
    case "error":
      return {
        snapshot: { phase: { kind: "resting", resting: report }, promptOpen: false },
        since,
        exact: false,
      };
    default:
      return assertNever(report);
  }
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
  let lifecycle: Lifecycle = {
    kind: "active",
    phase: { kind: "resting", resting: { status: "stopped" } },
    promptOpen: false,
    written: undefined,
  };

  function sync(state: ActiveState, ctx: Context): void {
    const next: Snapshot = { phase: state.phase, promptOpen: state.promptOpen };
    if (state.written !== undefined && sameSnapshot(state.written.snapshot, next)) return;
    const now = currentReport(next.phase, next.promptOpen);
    const previousReport =
      state.written !== undefined
        ? currentReport(state.written.snapshot.phase, state.written.snapshot.promptOpen)
        : undefined;
    const since =
      state.written !== undefined && sameReport(previousReport, now)
        ? state.written.since
        : Date.now();
    const said: OutboundFile = {
      sessionId: ctx.sessionManager.getSessionId(),
      cwd: ctx.cwd,
      status: now.status,
      statusSince: since,
      mode: ctx.mode,
      phase: serializePhase(next.phase),
      promptOpen: next.promptOpen,
    };
    if (started !== undefined) {
      said.procStart = started;
    }
    if (now.status === "error" && now.message !== undefined) {
      said.statusMessage = now.message.text;
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
      state.written = { snapshot: next, since };
    });
  }

  pi.on("session_start", (event, ctx) => {
    if (lifecycle.kind !== "active") return;
    const restored =
      event.reason === "reload" ? quietly(() => snapshotInFile(ctx, started)) : undefined;
    if (restored === undefined) {
      lifecycle.phase = { kind: "resting", resting: { status: "stopped" } };
      lifecycle.promptOpen = false;
      lifecycle.written = undefined;
      sync(lifecycle, ctx);
      return;
    }
    lifecycle.phase = restored.snapshot.phase;
    lifecycle.promptOpen = restored.snapshot.promptOpen;
    if (restored.exact) {
      lifecycle.written = { snapshot: restored.snapshot, since: restored.since };
    } else {
      lifecycle.written = undefined;
      sync(lifecycle, ctx);
    }
  });

  pi.on("agent_start", (_event, ctx) => {
    if (lifecycle.kind !== "active") return;
    lifecycle.phase = { kind: "running", outcome: { status: "done" } };
    sync(lifecycle, ctx);
  });

  pi.on("message_end", (event, ctx) => {
    if (lifecycle.kind !== "active" || lifecycle.phase.kind !== "running") return;
    const message = event.message;
    if (!message || message.role !== "assistant") return;
    const stop = parseStopReason(message.stopReason);
    if (stop === undefined) return;
    switch (stop) {
      case "error":
        lifecycle.phase = { kind: "running", outcome: errorReport(message.errorMessage) };
        break;
      case "aborted":
        lifecycle.phase = { kind: "running", outcome: { status: "stopped" } };
        break;
      case "done":
        lifecycle.phase = { kind: "running", outcome: { status: "done" } };
        break;
      default:
        assertNever(stop);
    }
    sync(lifecycle, ctx);
  });

  pi.on("agent_settled", (event, ctx) => {
    if (lifecycle.kind !== "active" || lifecycle.phase.kind !== "running") return;
    const resting: RestingReport =
      event.aborted === true ? { status: "stopped" } : lifecycle.phase.outcome;
    lifecycle.phase = { kind: "resting", resting };
    sync(lifecycle, ctx);
  });

  pi.on("ui_prompt_start", (_event, ctx) => {
    if (lifecycle.kind !== "active") return;
    lifecycle.promptOpen = true;
    sync(lifecycle, ctx);
  });

  pi.on("ui_prompt_end", (_event, ctx) => {
    if (lifecycle.kind !== "active") return;
    lifecycle.promptOpen = false;
    sync(lifecycle, ctx);
  });

  pi.on("session_shutdown", (event) => {
    if (lifecycle.kind !== "active") return;
    lifecycle = { kind: "shutDown" };
    if (event.reason !== "quit") return;
    quietly(() => {
      const file = fileOfThisProcess();
      if (file) rmSync(file, { force: true });
    });
  });
}
