import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { chmod, mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { join } from "node:path";

const MAX_REFERENCE_CHARS = 12_000;

function processStartTicks(stat: string): number {
  const end = stat.lastIndexOf(")");
  if (end < 0) throw new Error("invalid /proc/self/stat");
  const fields = stat.slice(end + 1).trim().split(/\s+/);
  const value = Number(fields[19]);
  if (!Number.isSafeInteger(value)) throw new Error("invalid process start time");
  return value;
}

function capReference(value: string): string {
  const characters = Array.from(value);
  if (characters.length <= MAX_REFERENCE_CHARS) return value;
  const headLength = Math.floor(MAX_REFERENCE_CHARS * 2 / 3);
  const tailLength = MAX_REFERENCE_CHARS - headLength - 3;
  return `${characters.slice(0, headLength).join("")}\n…\n${characters.slice(-tailLength).join("")}`;
}

function latestCompletedAssistantMessage(ctx: ExtensionContext): string | undefined {
  const branch = ctx.sessionManager.getBranch();
  for (let index = branch.length - 1; index >= 0; index -= 1) {
    const entry = branch[index];
    if (entry.type !== "message") continue;
    const message = entry.message;
    if (message.role !== "assistant" || message.stopReason !== "stop") continue;
    const text = message.content
      .filter((block) => block.type === "text")
      .map((block) => block.text)
      .join("\n")
      .trim();
    if (text) return capReference(text);
  }
  return undefined;
}

export default function (pi: ExtensionAPI) {
  const runtime = process.env.XDG_RUNTIME_DIR;
  const directory = runtime ? join(runtime, "voice-input", "agent-sessions") : undefined;
  const registry = directory ? join(directory, `pi-${process.pid}.json`) : undefined;
  const instanceId = randomUUID();
  let heartbeat: ReturnType<typeof setInterval> | undefined;
  let currentContext: ExtensionContext | undefined;

  async function removeRegistry(): Promise<void> {
    if (!registry) return;
    try {
      const current = JSON.parse(await readFile(registry, "utf8"));
      if (current.instance_id !== instanceId) return;
      await rm(registry, { force: true });
    } catch {
      // Missing, malformed, or already replaced by another extension instance.
    }
  }

  async function publish(ctx: ExtensionContext): Promise<void> {
    if (!directory || !registry) return;
    const sessionFile = ctx.sessionManager.getSessionFile();
    if (!sessionFile) {
      await removeRegistry();
      return;
    }

    const startTicks = processStartTicks(await readFile("/proc/self/stat", "utf8"));
    const payload = JSON.stringify({
      version: 2,
      instance_id: instanceId,
      pid: process.pid,
      process_start_ticks: startTicks,
      session_id: ctx.sessionManager.getSessionId(),
      session_file: sessionFile,
      cwd: ctx.cwd,
      latest_completed_assistant_message: latestCompletedAssistantMessage(ctx),
      updated_at_ms: Date.now(),
    });
    const temporary = `${registry}.tmp-${Date.now()}-${Math.random().toString(16).slice(2)}`;

    await mkdir(directory, { recursive: true, mode: 0o700 });
    await chmod(directory, 0o700);
    await writeFile(temporary, payload, { encoding: "utf8", mode: 0o600 });
    await rename(temporary, registry);
    await chmod(registry, 0o600);
  }

  function startHeartbeat(): void {
    if (heartbeat) clearInterval(heartbeat);
    heartbeat = setInterval(() => {
      if (currentContext) void publish(currentContext).catch(() => {});
    }, 5000);
    heartbeat.unref();
  }

  pi.on("session_start", async (_event, ctx) => {
    currentContext = ctx;
    startHeartbeat();
    await publish(ctx).catch(() => {});
  });

  pi.on("before_agent_start", async (_event, ctx) => {
    currentContext = ctx;
    await publish(ctx).catch(() => {});
  });

  pi.on("agent_end", async (_event, ctx) => {
    currentContext = ctx;
    await publish(ctx).catch(() => {});
  });

  pi.on("agent_settled", async (_event, ctx) => {
    currentContext = ctx;
    await publish(ctx).catch(() => {});
  });

  pi.on("session_tree", async (_event, ctx) => {
    currentContext = ctx;
    await publish(ctx).catch(() => {});
  });

  pi.on("session_shutdown", async () => {
    currentContext = undefined;
    if (heartbeat) clearInterval(heartbeat);
    heartbeat = undefined;
    await removeRegistry();
  });
}
