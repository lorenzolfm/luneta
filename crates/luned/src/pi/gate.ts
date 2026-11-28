const ALLOWED_RAW: unknown = JSON.parse("LUNE_ALLOWED");
const DENIED = "LUNE_DENIED";
const ENTRY = "LUNE_ENTRY";
const COMMAND_BYTES_MAX = 16 * 1024;
const SHELL = /[;&|<>`$\\\x00-\x1f\x7f]/;

type ToolCallBlock = {
  readonly block: true;
  readonly reason: string;
};

type Pi = {
  appendEntry(customType: string, data: Record<string, never>): void;
  on(event: "session_start", handler: () => Promise<void> | void): void;
  on(
    event: "tool_call",
    handler: (event: unknown) => Promise<ToolCallBlock | undefined> | ToolCallBlock | undefined,
  ): void;
};

function invariant(condition: boolean, message: string): asserts condition {
  if (!condition) {
    throw new Error(message);
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function parseAllowed(raw: unknown): readonly string[] {
  invariant(Array.isArray(raw), "allowed command list must be an array");
  invariant(raw.length > 0, "allowed command list must not be empty");
  const prefixes: string[] = [];
  for (const item of raw) {
    invariant(typeof item === "string", "allowed command prefix must be a string");
    invariant(item.length > 0, "allowed command prefix must not be empty");
    invariant(item === item.trim(), "allowed command prefix must be trimmed");
    invariant(!SHELL.test(item), "allowed command prefix must hold no shell characters");
    prefixes.push(item);
  }
  return prefixes;
}

const ALLOWED: readonly string[] = parseAllowed(ALLOWED_RAW);
invariant(DENIED.length > 0, "denied reason must not be empty");
invariant(ENTRY.length > 0, "gate entry name must not be empty");

function allowed(tool: unknown, input: unknown): boolean {
  if (tool !== "bash") {
    return false;
  }
  if (!isRecord(input)) {
    return false;
  }
  const said = input["command"];
  if (typeof said !== "string") {
    return false;
  }
  if (said.length > COMMAND_BYTES_MAX) {
    return false;
  }
  if (Buffer.byteLength(said, "utf8") > COMMAND_BYTES_MAX) {
    return false;
  }
  const command = said.trim();
  if (command === "") {
    return false;
  }
  if (SHELL.test(command)) {
    return false;
  }
  return ALLOWED.some((prefix) => command === prefix || command.startsWith(`${prefix} `));
}

export default function (pi: Pi): void {
  pi.on("session_start", async () => {
    pi.appendEntry(ENTRY, {});
  });
  pi.on("tool_call", async (event: unknown): Promise<ToolCallBlock | undefined> => {
    try {
      if (!isRecord(event)) {
        return { block: true, reason: DENIED };
      }
      if (allowed(event["toolName"], event["input"])) {
        return undefined;
      }
      return { block: true, reason: DENIED };
    } catch {
      return { block: true, reason: DENIED };
    }
  });
}
