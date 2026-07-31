import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { chmod, mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { join } from "node:path";

const MAX_REFERENCE_CHARS = 12_000;

type RegistrySnapshot = {
  sessionId: string;
  sessionFile: string;
  cwd: string;
  latestCompletedAssistantMessage: string | undefined;
};

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

function captureSnapshot(ctx: ExtensionContext): RegistrySnapshot | undefined {
  const sessionFile = ctx.sessionManager.getSessionFile();
  if (!sessionFile) return undefined;
  return {
    sessionId: ctx.sessionManager.getSessionId(),
    sessionFile,
    cwd: ctx.cwd,
    latestCompletedAssistantMessage: latestCompletedAssistantMessage(ctx),
  };
}

export default function (pi: ExtensionAPI) {
  const runtime = process.env.XDG_RUNTIME_DIR;
  const directory = runtime ? join(runtime, "voice-input", "agent-sessions") : undefined;
  const registry = directory ? join(directory, `pi-${process.pid}.json`) : undefined;
  const instanceId = randomUUID();
  let heartbeat: ReturnType<typeof setInterval> | undefined;
  let currentContext: ExtensionContext | undefined;
  let generation = 0;
  let stopped = true;
  let pendingWrite: Promise<void> = Promise.resolve();
  let cachedStartTicks: number | undefined;

  function isCurrent(expectedGeneration: number): boolean {
    return !stopped && generation === expectedGeneration;
  }

  async function getProcessStartTicks(): Promise<number> {
    if (cachedStartTicks !== undefined) return cachedStartTicks;
    const value = processStartTicks(await readFile("/proc/self/stat", "utf8"));
    // Cache only a successful read so a transient filesystem error does not
    // permanently poison every later heartbeat publication.
    cachedStartTicks = value;
    return value;
  }

  async function removeOwnedRegistry(expectedGeneration?: number): Promise<void> {
    if (!registry) return;
    if (expectedGeneration !== undefined && !isCurrent(expectedGeneration)) return;
    try {
      const current = JSON.parse(await readFile(registry, "utf8"));
      if (current.instance_id !== instanceId) return;
      if (expectedGeneration !== undefined && !isCurrent(expectedGeneration)) return;
      await rm(registry, { force: true });
    } catch {
      // Missing, malformed, or already replaced by another extension instance.
    }
  }

  async function publishSnapshot(
    snapshot: RegistrySnapshot | undefined,
    expectedGeneration: number,
  ): Promise<void> {
    if (!directory || !registry || !isCurrent(expectedGeneration)) return;
    if (!snapshot) {
      await removeOwnedRegistry(expectedGeneration);
      return;
    }

    const startTicks = await getProcessStartTicks();
    if (!isCurrent(expectedGeneration)) return;

    const payload = JSON.stringify({
      version: 2,
      instance_id: instanceId,
      pid: process.pid,
      process_start_ticks: startTicks,
      session_id: snapshot.sessionId,
      session_file: snapshot.sessionFile,
      cwd: snapshot.cwd,
      latest_completed_assistant_message: snapshot.latestCompletedAssistantMessage,
      updated_at_ms: Date.now(),
    });
    const temporary = `${registry}.tmp-${Date.now()}-${Math.random().toString(16).slice(2)}`;

    try {
      await mkdir(directory, { recursive: true, mode: 0o700 });
      await chmod(directory, 0o700);
      if (!isCurrent(expectedGeneration)) return;

      await writeFile(temporary, payload, { encoding: "utf8", mode: 0o600 });
      // session_shutdown invalidates the generation before waiting for this
      // operation. Never let a stale heartbeat/session write replace the file.
      if (!isCurrent(expectedGeneration)) return;

      await rename(temporary, registry);
      await chmod(registry, 0o600);
    } finally {
      // Harmless after a successful rename; cleans up cancelled/failed writes.
      await rm(temporary, { force: true }).catch(() => {});
    }
  }

  function queuePublish(ctx: ExtensionContext): Promise<void> {
    const expectedGeneration = generation;
    let snapshot: RegistrySnapshot | undefined;
    try {
      snapshot = captureSnapshot(ctx);
    } catch (error) {
      return Promise.reject(error);
    }

    const operation = pendingWrite.then(() =>
      publishSnapshot(snapshot, expectedGeneration),
    );
    // Keep the queue usable after a failed write while returning the original
    // operation to event handlers that want to observe its failure.
    pendingWrite = operation.catch(() => {});
    return operation;
  }

  async function refresh(ctx: ExtensionContext): Promise<void> {
    if (stopped) return;
    currentContext = ctx;
    await queuePublish(ctx).catch(() => {});
  }

  function startHeartbeat(expectedGeneration: number): void {
    if (heartbeat) clearInterval(heartbeat);
    heartbeat = setInterval(() => {
      const ctx = currentContext;
      if (ctx && isCurrent(expectedGeneration)) {
        void queuePublish(ctx).catch(() => {});
      }
    }, 5000);
    heartbeat.unref();
  }

  pi.on("session_start", async (_event, ctx) => {
    generation += 1;
    stopped = false;
    currentContext = ctx;
    startHeartbeat(generation);
    await queuePublish(ctx).catch(() => {});
  });

  pi.on("before_agent_start", async (_event, ctx) => {
    await refresh(ctx);
  });

  pi.on("agent_end", async (_event, ctx) => {
    await refresh(ctx);
  });

  pi.on("agent_settled", async (_event, ctx) => {
    await refresh(ctx);
  });

  pi.on("session_tree", async (_event, ctx) => {
    await refresh(ctx);
  });

  pi.on("session_shutdown", async () => {
    stopped = true;
    generation += 1;
    currentContext = undefined;
    if (heartbeat) clearInterval(heartbeat);
    heartbeat = undefined;

    // Pi awaits session_shutdown before binding the replacement session. Wait
    // for every queued write to observe the invalid generation, then remove
    // only the file still owned by this extension instance.
    await pendingWrite;
    await removeOwnedRegistry();
  });
}
