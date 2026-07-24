import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { chmod, mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { join } from "node:path";

function processStartTicks(stat: string): number {
  const end = stat.lastIndexOf(")");
  if (end < 0) throw new Error("invalid /proc/self/stat");
  const fields = stat.slice(end + 1).trim().split(/\s+/);
  const value = Number(fields[19]);
  if (!Number.isSafeInteger(value)) throw new Error("invalid process start time");
  return value;
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
      version: 1,
      instance_id: instanceId,
      pid: process.pid,
      process_start_ticks: startTicks,
      session_id: ctx.sessionManager.getSessionId(),
      session_file: sessionFile,
      cwd: ctx.cwd,
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

  pi.on("session_shutdown", async () => {
    currentContext = undefined;
    if (heartbeat) clearInterval(heartbeat);
    heartbeat = undefined;
    await removeRegistry();
  });
}
