import { createHash } from "node:crypto";
import {
  appendFileSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";

const FOLDER = "LUNE_FOLDER";
const FILE_BYTES_MAX = 64 * 1024;
const MESSAGE_BYTES_MAX = 1024;
const MESSAGE_RAW_CODE_UNITS_MAX = MESSAGE_BYTES_MAX * 4;
const OSC7577_TOKEN_VAR = "OSC7577_TOKEN";
const OSC7577_PAYLOAD_BYTES_MAX = 2048;
const OSC7577_TOKEN_LEN_MIN = 22;
const OSC7577_TOKEN_LEN_MAX = 64;

type ResumeFlagSpec = {
  readonly action: "keep" | "drop";
  readonly values: "none" | "one";
};

const PI_RESUME_FLAGS: Readonly<Record<string, ResumeFlagSpec>> = {
  "--no-extensions": { action: "keep", values: "none" },
  "--no-prompt-templates": { action: "keep", values: "none" },
  "--no-skills": { action: "keep", values: "none" },
  "--no-themes": { action: "keep", values: "none" },
  "--no-tools": { action: "keep", values: "none" },
  "--offline": { action: "keep", values: "none" },
  "--verbose": { action: "keep", values: "none" },
  "-e": { action: "keep", values: "one" },
  "--api-key": { action: "keep", values: "one" },
  "--append-system-prompt": { action: "keep", values: "one" },
  "--extension": { action: "keep", values: "one" },
  "--model": { action: "keep", values: "one" },
  "--models": { action: "keep", values: "one" },
  "--prompt-template": { action: "keep", values: "one" },
  "--provider": { action: "keep", values: "one" },
  "--session-dir": { action: "keep", values: "one" },
  "--skill": { action: "keep", values: "one" },
  "--system-prompt": { action: "keep", values: "one" },
  "--theme": { action: "keep", values: "one" },
  "--thinking": { action: "keep", values: "one" },
  "--tools": { action: "keep", values: "one" },
  "-c": { action: "drop", values: "none" },
  "--continue": { action: "drop", values: "none" },
  "-r": { action: "drop", values: "none" },
  "--resume": { action: "drop", values: "none" },
  "--fork": { action: "drop", values: "one" },
  "--session": { action: "drop", values: "one" },
};

class OscToken {
  readonly #value: string;

  private constructor(value: string) {
    this.#value = value;
  }

  static parse(raw: unknown): OscToken | undefined {
    if (typeof raw !== "string") return undefined;
    if (raw.length < OSC7577_TOKEN_LEN_MIN || raw.length > OSC7577_TOKEN_LEN_MAX) return undefined;
    for (let i = 0; i < raw.length; i += 1) {
      const code = raw.charCodeAt(i);
      const ok =
        (code >= 0x41 && code <= 0x5a) ||
        (code >= 0x61 && code <= 0x7a) ||
        (code >= 0x30 && code <= 0x39) ||
        code === 0x2d ||
        code === 0x5f;
      if (!ok) return undefined;
    }
    return new OscToken(raw);
  }

  get value(): string {
    return this.#value;
  }
}

const OSC7577_GLOBAL_KEY = Symbol.for("lune.osc7577.token");

function captureOscToken(): OscToken | undefined {
  const store = globalThis as Record<symbol, unknown>;
  const raw = process.env[OSC7577_TOKEN_VAR];
  delete process.env[OSC7577_TOKEN_VAR];
  if (typeof raw === "string") {
    const parsed = OscToken.parse(raw);
    if (parsed !== undefined) {
      store[OSC7577_GLOBAL_KEY] = parsed.value;
      return parsed;
    }
    delete store[OSC7577_GLOBAL_KEY];
    return undefined;
  }
  return OscToken.parse(store[OSC7577_GLOBAL_KEY]);
}

const OSC7577_TOKEN = captureOscToken();

function hasControlChar(text: string): boolean {
  for (const ch of text) {
    const codePoint = ch.codePointAt(0);
    if (codePoint === undefined) return true;
    if (codePoint < 0x20 || (codePoint >= 0x7f && codePoint <= 0x9f)) return true;
  }
  return false;
}

function parsePiLaunchFlags(args: readonly string[]): readonly string[] | undefined {
  const kept: string[] = [];
  let i = 0;
  while (i < args.length) {
    const arg = args[i];
    if (arg === undefined || arg === "--") break;
    if (!arg.startsWith("-") || arg === "-") {
      i += 1;
      continue;
    }
    let flagName = arg;
    let inlineVal: string | undefined;
    if (arg.startsWith("--")) {
      const eq = arg.indexOf("=");
      if (eq !== -1) {
        flagName = arg.slice(0, eq);
        inlineVal = arg.slice(eq + 1);
      }
    }
    const spec = PI_RESUME_FLAGS[flagName];
    if (spec === undefined) return undefined;
    const values: string[] = [];
    let next = i + 1;
    switch (spec.values) {
      case "none":
        if (inlineVal !== undefined) return undefined;
        break;
      case "one":
        if (inlineVal !== undefined) {
          values.push(inlineVal);
        } else {
          const val = args[i + 1];
          if (val === undefined) return undefined;
          values.push(val);
          next = i + 2;
        }
        break;
      default:
        return assertNever(spec.values);
    }
    if (spec.action === "keep") {
      const single = values.length === 1 ? values[0] : undefined;
      if (flagName.startsWith("--") && single !== undefined && single.startsWith("-")) {
        kept.push(`${flagName}=${single}`);
      } else {
        kept.push(flagName, ...values);
      }
    }
    i = next;
  }
  return kept;
}

function writeOsc7577(token: OscToken, bodyWithoutMac: string): boolean {
  const mac = createHash("sha256").update(`${bodyWithoutMac}:${token.value}`).digest("hex");
  invariant(mac.length === 64, "sha256 hex digest must be 64 bytes");
  const payload = `${bodyWithoutMac}:mac=${mac}`;
  if (Buffer.byteLength(payload, "utf8") > OSC7577_PAYLOAD_BYTES_MAX) return false;
  const sequence = `\x1b]7577;${payload}\x1b\\`;
  const wrote = quietly(() => {
    appendFileSync("/dev/tty", sequence);
    return true;
  });
  return wrote === true;
}

function buildOsc7577Body(
  identity: SessionIdentity,
  keptFlags: readonly string[],
): string | undefined {
  const argv = ["pi", "--session", identity.sessionId, ...keptFlags];
  invariant(argv.length >= 3, "pi resume argv holds program and session");
  for (const word of argv) {
    if (hasControlChar(word)) return undefined;
  }
  if (!identity.cwd.startsWith("/") || hasControlChar(identity.cwd)) return undefined;
  const encodedArgv = argv.map((word) => Buffer.from(word, "utf8").toString("base64")).join(",");
  const encodedCwd = Buffer.from(identity.cwd, "utf8").toString("base64");
  const time = Math.floor(Date.now() / 1000);
  return `pid=${process.pid}:time=${time}:argv=${encodedArgv}:cwd=${encodedCwd}`;
}

function clearOsc7577(token: OscToken): boolean {
  const time = Math.floor(Date.now() / 1000);
  return writeOsc7577(token, `pid=${process.pid}:time=${time}:clear=1`);
}

function invariant(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function assertNever(value: never): never {
  throw new Error(`unexpected value: ${String(value)}`);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isInvisibleFormatting(codePoint: number): boolean {
  return (
    codePoint < 0x20 ||
    (codePoint >= 0x7f && codePoint <= 0x9f) ||
    codePoint === 0x00ad ||
    (codePoint >= 0x0600 && codePoint <= 0x0605) ||
    codePoint === 0x061c ||
    codePoint === 0x06dd ||
    codePoint === 0x070f ||
    (codePoint >= 0x0890 && codePoint <= 0x0891) ||
    codePoint === 0x08e2 ||
    codePoint === 0x180e ||
    (codePoint >= 0x200b && codePoint <= 0x200f) ||
    (codePoint >= 0x202a && codePoint <= 0x202e) ||
    (codePoint >= 0x2060 && codePoint <= 0x2064) ||
    (codePoint >= 0x2066 && codePoint <= 0x206f) ||
    codePoint === 0xfeff ||
    (codePoint >= 0xfff9 && codePoint <= 0xfffb) ||
    codePoint === 0x110bd ||
    codePoint === 0x110cd ||
    (codePoint >= 0x13430 && codePoint <= 0x1343f) ||
    (codePoint >= 0x1bca0 && codePoint <= 0x1bca3) ||
    (codePoint >= 0x1d173 && codePoint <= 0x1d17a) ||
    codePoint === 0xe0001 ||
    (codePoint >= 0xe0020 && codePoint <= 0xe007f)
  );
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
    let out = "";
    let bytes = 0;
    for (const character of bounded) {
      if (character === "\r" || character === "\n") {
        if (out.length > 0) break;
        continue;
      }
      const codePoint = character.codePointAt(0);
      invariant(codePoint !== undefined, "iterated character has a code point");
      if (isInvisibleFormatting(codePoint)) continue;
      if (out.length === 0 && character.trim().length === 0) continue;
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

type SessionIdentity = {
  readonly sessionId: string;
  readonly cwd: string;
  readonly mode?: string;
};

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

type TelemetryContext = {
  readonly usedPercent?: number;
  readonly windowTokens?: number;
  readonly usedTokens?: number;
};

type TelemetrySnapshot = {
  readonly model?: string;
  readonly effort?: string;
  readonly context?: TelemetryContext;
  readonly costUsd?: number;
};

type Snapshot = {
  readonly phase: Phase;
  readonly promptOpen: boolean;
  readonly telemetry?: TelemetrySnapshot;
};

type DiskState =
  | { readonly kind: "unwritten" }
  | { readonly kind: "needsRewrite"; readonly report: Report; readonly since: number }
  | {
      readonly kind: "synced";
      readonly identity: SessionIdentity;
      readonly snapshot: Snapshot;
      readonly since: number;
    };

type ActiveState = {
  readonly kind: "active";
  phase: Phase;
  promptOpen: boolean;
  disk: DiskState;
};

type Lifecycle = ActiveState | { readonly kind: "shutDown" };

type PersistedResting =
  | { readonly status: "stopped" }
  | { readonly status: "done" }
  | { readonly status: "error"; readonly message?: string };

type PersistedPhase =
  | { readonly kind: "resting"; readonly resting: PersistedResting }
  | { readonly kind: "running"; readonly outcome: PersistedResting };

type OutboundReport =
  | { readonly status: "stopped" }
  | { readonly status: "done" }
  | { readonly status: "busy" }
  | { readonly status: "waiting" }
  | { readonly status: "error"; readonly statusMessage?: string };

type OutboundFile = OutboundReport & {
  readonly sessionId: string;
  readonly cwd: string;
  readonly statusSince: number;
  readonly mode?: string;
  readonly procStart?: string;
  readonly phase: PersistedPhase;
  readonly promptOpen: boolean;
  readonly telemetry?: TelemetrySnapshot;
};

type RestoredSnapshot =
  | {
      readonly kind: "exact";
      readonly identity: SessionIdentity;
      readonly snapshot: Snapshot;
      readonly since: number;
    }
  | {
      readonly kind: "needsRewrite";
      readonly identity: SessionIdentity;
      readonly snapshot: Snapshot;
      readonly since: number;
    };

type StopOutcome = "error" | "aborted" | "done";

type Context = {
  cwd: string;
  mode?: string;
  model?: {
    readonly id?: string;
    readonly name?: string;
    readonly provider?: string;
    readonly contextWindow?: number;
  };
  sessionManager: {
    getSessionId(): string;
    getEntries?: () => readonly unknown[];
  };
  getContextUsage?: () =>
    | {
        readonly tokens: number | null;
        readonly contextWindow: number;
        readonly percent: number | null;
      }
    | undefined;
  isIdle?: () => boolean;
};

type PiEvents = {
  session_start: { readonly reason?: string };
  agent_start: Record<string, never>;
  message_end: {
    readonly message?: {
      readonly role?: string;
      readonly stopReason?: string;
      readonly errorMessage?: string;
      readonly usage?: {
        readonly cost?: {
          readonly total?: number;
        };
      };
    };
  };
  turn_end: Record<string, never>;
  model_select: Record<string, never>;
  thinking_level_select: Record<string, never>;
  session_compact: Record<string, never>;
  agent_settled: { readonly aborted?: boolean };
  ui_prompt_start: Record<string, never>;
  ui_prompt_end: Record<string, never>;
  session_shutdown: { readonly reason?: string };
};

type Pi = {
  on<K extends keyof PiEvents>(event: K, handler: (event: PiEvents[K], ctx: Context) => void): void;
  getThinkingLevel?: () => string;
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

function parseIdentity(ctx: Context): SessionIdentity | undefined {
  const sessionId = quietly(() => ctx.sessionManager.getSessionId());
  if (typeof sessionId !== "string" || sessionId.trim().length === 0) return undefined;
  if (typeof ctx.cwd !== "string" || !ctx.cwd.startsWith("/")) return undefined;
  if (ctx.mode !== undefined && typeof ctx.mode !== "string") return undefined;
  return ctx.mode !== undefined
    ? { sessionId, cwd: ctx.cwd, mode: ctx.mode }
    : { sessionId, cwd: ctx.cwd };
}

function sameIdentity(a: SessionIdentity, b: SessionIdentity): boolean {
  return a.sessionId === b.sessionId && a.cwd === b.cwd && a.mode === b.mode;
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

function serializeReport(report: Report): OutboundReport {
  switch (report.status) {
    case "stopped":
      return { status: "stopped" };
    case "done":
      return { status: "done" };
    case "busy":
      return { status: "busy" };
    case "waiting":
      return { status: "waiting" };
    case "error":
      return report.message !== undefined
        ? { status: "error", statusMessage: report.message.text }
        : { status: "error" };
    default:
      return assertNever(report);
  }
}

function serializeOutbound(
  identity: SessionIdentity,
  snapshot: Snapshot,
  since: number,
  started: string | undefined,
): OutboundFile {
  const report = serializeReport(currentReport(snapshot.phase, snapshot.promptOpen));
  return {
    sessionId: identity.sessionId,
    cwd: identity.cwd,
    ...report,
    statusSince: since,
    ...(identity.mode !== undefined ? { mode: identity.mode } : {}),
    ...(started !== undefined ? { procStart: started } : {}),
    phase: serializePhase(snapshot.phase),
    promptOpen: snapshot.promptOpen,
    ...(snapshot.telemetry !== undefined ? { telemetry: snapshot.telemetry } : {}),
  };
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
  if (typeof raw !== "string") return undefined;
  const trimmed = raw.trim();
  if (trimmed.length === 0) return undefined;
  switch (trimmed) {
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

function sameTelemetryContext(
  a: TelemetryContext | undefined,
  b: TelemetryContext | undefined,
): boolean {
  if (a === b) return true;
  if (a === undefined || b === undefined) return false;
  return (
    a.usedPercent === b.usedPercent &&
    a.windowTokens === b.windowTokens &&
    a.usedTokens === b.usedTokens
  );
}

function sameTelemetry(
  a: TelemetrySnapshot | undefined,
  b: TelemetrySnapshot | undefined,
): boolean {
  if (a === b) return true;
  if (a === undefined || b === undefined) return false;
  return (
    a.model === b.model &&
    a.effort === b.effort &&
    a.costUsd === b.costUsd &&
    sameTelemetryContext(a.context, b.context)
  );
}

function sameSnapshot(a: Snapshot, b: Snapshot): boolean {
  return (
    a.promptOpen === b.promptOpen &&
    samePhase(a.phase, b.phase) &&
    sameTelemetry(a.telemetry, b.telemetry)
  );
}

const SESSION_ENTRIES_MAX = 4096;

function nonEmptyTrimmed(raw: unknown): string | undefined {
  if (typeof raw !== "string") return undefined;
  const trimmed = raw.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

function finiteNonNegative(raw: unknown): number | undefined {
  if (typeof raw !== "number" || !Number.isFinite(raw) || raw < 0) return undefined;
  return raw;
}

function readSessionCost(ctx: Context): number | undefined {
  const entries = quietly(() => ctx.sessionManager.getEntries?.());
  if (!Array.isArray(entries)) return undefined;
  let total = 0;
  let found = false;
  const count = Math.min(entries.length, SESSION_ENTRIES_MAX);
  for (let i = 0; i < count; i += 1) {
    const entry = entries[i];
    if (!isRecord(entry) || entry["type"] !== "message") continue;
    const message = entry["message"];
    if (!isRecord(message) || message["role"] !== "assistant") continue;
    const usage = message["usage"];
    if (!isRecord(usage)) continue;
    const cost = usage["cost"];
    if (!isRecord(cost)) continue;
    const amount = finiteNonNegative(cost["total"]);
    if (amount !== undefined) {
      total += amount;
      found = true;
    }
  }
  return found ? total : undefined;
}

function readTelemetry(
  pi: Pi,
  ctx: Context,
  costUsd: number | undefined,
): TelemetrySnapshot | undefined {
  const model = nonEmptyTrimmed(ctx.model?.name) ?? nonEmptyTrimmed(ctx.model?.id);
  const effort = nonEmptyTrimmed(quietly(() => pi.getThinkingLevel?.()))?.toLowerCase();
  const rawContext = quietly(() => ctx.getContextUsage?.());
  const windowTokens = finiteNonNegative(rawContext?.contextWindow ?? ctx.model?.contextWindow);
  const usedTokens = finiteNonNegative(rawContext?.tokens);
  const usedPercent = finiteNonNegative(rawContext?.percent);
  let context: TelemetryContext | undefined;
  if (
    usedPercent !== undefined ||
    (usedTokens !== undefined && windowTokens !== undefined && windowTokens > 0)
  ) {
    context = {
      ...(usedPercent !== undefined ? { usedPercent } : {}),
      ...(windowTokens !== undefined && windowTokens > 0
        ? { windowTokens: Math.floor(windowTokens) }
        : {}),
      ...(usedTokens !== undefined ? { usedTokens: Math.floor(usedTokens) } : {}),
    };
  }
  const cost = finiteNonNegative(costUsd);
  if (model === undefined && effort === undefined && context === undefined && cost === undefined) {
    return undefined;
  }
  return {
    ...(model !== undefined ? { model } : {}),
    ...(effort !== undefined ? { effort } : {}),
    ...(context !== undefined ? { context } : {}),
    ...(cost !== undefined ? { costUsd: cost } : {}),
  };
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
  const identity = parseIdentity(ctx);
  if (identity === undefined) return undefined;
  const file = fileOfThisProcess();
  if (!file) return undefined;
  const stats = lstatSync(file);
  if (!stats.isFile() || stats.size > FILE_BYTES_MAX) return undefined;
  const parsed: unknown = JSON.parse(readFileSync(file, "utf8"));
  if (!isRecord(parsed)) return undefined;
  if (parsed["sessionId"] !== identity.sessionId) return undefined;
  if (parsed["cwd"] !== identity.cwd) return undefined;
  if (identity.mode !== undefined ? parsed["mode"] !== identity.mode : parsed["mode"] !== undefined) {
    return undefined;
  }
  if (started !== undefined ? parsed["procStart"] !== started : parsed["procStart"] !== undefined) {
    return undefined;
  }
  const since = parsed["statusSince"];
  if (typeof since !== "number" || !Number.isSafeInteger(since) || since < 0) return undefined;
  const report = parseReport(parsed["status"], parsed["statusMessage"]);
  if (report === undefined) return undefined;

  if ("phase" in parsed || "promptOpen" in parsed) {
    const phase = parsePersistedPhase(parsed["phase"]);
    const promptOpen = parsed["promptOpen"];
    if (phase === undefined || typeof promptOpen !== "boolean") return undefined;
    const projected = currentReport(phase, promptOpen);
    if (!sameReport(report, projected)) return undefined;
    const kind = isCanonicalReportFields(parsed, projected) ? "exact" : "needsRewrite";
    return { kind, identity, snapshot: { phase, promptOpen }, since };
  }

  switch (report.status) {
    case "busy":
      return {
        kind: "needsRewrite",
        identity,
        snapshot: { phase: { kind: "running", outcome: { status: "done" } }, promptOpen: false },
        since,
      };
    case "waiting": {
      const running = ctx.isIdle !== undefined && !ctx.isIdle();
      const phase: Phase = running
        ? { kind: "running", outcome: { status: "done" } }
        : { kind: "resting", resting: { status: "stopped" } };
      return { kind: "needsRewrite", identity, snapshot: { phase, promptOpen: true }, since };
    }
    case "stopped":
    case "done":
    case "error":
      return {
        kind: "needsRewrite",
        identity,
        snapshot: { phase: { kind: "resting", resting: report }, promptOpen: false },
        since,
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
  const keptFlags = parsePiLaunchFlags(process.argv.slice(2));
  let reportedResume: { readonly sessionId: string; readonly cwd: string } | undefined;
  let sessionCostUsd: number | undefined;
  let lifecycle: Lifecycle = {
    kind: "active",
    phase: { kind: "resting", resting: { status: "stopped" } },
    promptOpen: false,
    disk: { kind: "unwritten" },
  };

  function clearReportedResume(): void {
    if (OSC7577_TOKEN === undefined || reportedResume === undefined) return;
    if (clearOsc7577(OSC7577_TOKEN)) {
      reportedResume = undefined;
    }
  }

  function syncResume(identity: SessionIdentity): void {
    if (OSC7577_TOKEN === undefined) return;
    if (keptFlags === undefined) {
      clearReportedResume();
      return;
    }
    if (
      reportedResume !== undefined &&
      reportedResume.sessionId === identity.sessionId &&
      reportedResume.cwd === identity.cwd
    ) {
      return;
    }
    const body = buildOsc7577Body(identity, keptFlags);
    if (body === undefined) {
      clearReportedResume();
      return;
    }
    if (writeOsc7577(OSC7577_TOKEN, body)) {
      reportedResume = { sessionId: identity.sessionId, cwd: identity.cwd };
    } else {
      clearReportedResume();
    }
  }

  function sync(state: ActiveState, ctx: Context): void {
    const identity = parseIdentity(ctx);
    if (identity === undefined) return;
    syncResume(identity);
    const telemetry = readTelemetry(pi, ctx, sessionCostUsd);
    const next: Snapshot = {
      phase: state.phase,
      promptOpen: state.promptOpen,
      ...(telemetry !== undefined ? { telemetry } : {}),
    };
    if (
      state.disk.kind === "synced" &&
      sameIdentity(state.disk.identity, identity) &&
      sameSnapshot(state.disk.snapshot, next)
    ) {
      return;
    }
    const now = currentReport(next.phase, next.promptOpen);
    let previousReport: Report | undefined;
    let previousSince: number | undefined;
    switch (state.disk.kind) {
      case "synced":
        previousReport = currentReport(state.disk.snapshot.phase, state.disk.snapshot.promptOpen);
        previousSince = state.disk.since;
        break;
      case "needsRewrite":
        previousReport = state.disk.report;
        previousSince = state.disk.since;
        break;
      case "unwritten":
        break;
      default:
        assertNever(state.disk);
    }
    const since =
      previousSince !== undefined && sameReport(previousReport, now) ? previousSince : Date.now();
    const said = serializeOutbound(identity, next, since, started);
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
      state.disk = { kind: "synced", identity, snapshot: next, since };
    });
  }

  pi.on("session_start", (event, ctx) => {
    if (lifecycle.kind !== "active") return;
    sessionCostUsd = readSessionCost(ctx);
    const restored =
      event.reason === "reload" ? quietly(() => snapshotInFile(ctx, started)) : undefined;
    if (restored === undefined) {
      lifecycle.phase = { kind: "resting", resting: { status: "stopped" } };
      lifecycle.promptOpen = false;
      lifecycle.disk = { kind: "unwritten" };
      sync(lifecycle, ctx);
      return;
    }
    lifecycle.phase = restored.snapshot.phase;
    lifecycle.promptOpen = restored.snapshot.promptOpen;
    switch (restored.kind) {
      case "exact":
        lifecycle.disk = {
          kind: "synced",
          identity: restored.identity,
          snapshot: restored.snapshot,
          since: restored.since,
        };
        syncResume(restored.identity);
        break;
      case "needsRewrite":
        lifecycle.disk = {
          kind: "needsRewrite",
          report: currentReport(restored.snapshot.phase, restored.snapshot.promptOpen),
          since: restored.since,
        };
        sync(lifecycle, ctx);
        break;
      default:
        assertNever(restored);
    }
  });

  pi.on("agent_start", (_event, ctx) => {
    if (lifecycle.kind !== "active") return;
    lifecycle.phase = { kind: "running", outcome: { status: "done" } };
    sync(lifecycle, ctx);
  });

  pi.on("message_end", (event, ctx) => {
    if (lifecycle.kind !== "active") return;
    const message = event.message;
    if (!message || message.role !== "assistant") return;
    const messageCost = finiteNonNegative(message.usage?.cost?.total);
    if (messageCost !== undefined) {
      sessionCostUsd = (sessionCostUsd ?? 0) + messageCost;
    }
    if (lifecycle.phase.kind !== "running") {
      sync(lifecycle, ctx);
      return;
    }
    const stop = parseStopReason(message.stopReason);
    if (stop === undefined) {
      sync(lifecycle, ctx);
      return;
    }
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

  pi.on("turn_end", (_event, ctx) => {
    if (lifecycle.kind !== "active") return;
    sync(lifecycle, ctx);
  });

  pi.on("model_select", (_event, ctx) => {
    if (lifecycle.kind !== "active") return;
    sync(lifecycle, ctx);
  });

  pi.on("thinking_level_select", (_event, ctx) => {
    if (lifecycle.kind !== "active") return;
    sync(lifecycle, ctx);
  });

  pi.on("session_compact", (_event, ctx) => {
    if (lifecycle.kind !== "active") return;
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
    clearReportedResume();
    quietly(() => {
      const file = fileOfThisProcess();
      if (file) rmSync(file, { force: true });
    });
  });
}
