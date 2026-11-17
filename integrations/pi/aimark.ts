import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { resolve } from "node:path";
import { homedir } from "node:os";

type Edit = { path: string; model: string; session: string; cwd: string };

/** Scoped file-tool provenance. Shell writes require `aimark run --harness pi -- pi`. */
export default function aimark(pi: ExtensionAPI) {
  const pending = new Map<string, Edit>();
  pi.on("tool_call", async (event, ctx) => {
    if (event.toolName !== "write" && event.toolName !== "edit") return;
    const input = event.input as { path?: unknown };
    if (typeof input.path !== "string") return;
    const path = input.path.startsWith("~/") ? resolve(homedir(), input.path.slice(2)) : resolve(ctx.cwd, input.path);
    try {
      const result = await pi.exec("aimark", ["checkpoint", "--files", path], { cwd: ctx.cwd });
      if (result.code !== 0) throw new Error(result.stderr || "checkpoint failed");
      pending.set(event.toolCallId, { path, model: ctx.model?.id ?? "unknown", session: ctx.sessionManager.getSessionId(), cwd: ctx.cwd });
    } catch (error) {
      return { block: true, reason: `aimark could not checkpoint this edit: ${String(error)}` };
    }
  });
  pi.on("tool_execution_end", async (event, ctx) => {
    const edit = pending.get(event.toolCallId);
    if (!edit) return;
    pending.delete(event.toolCallId);
    try {
      // A failed tool can still have changed its file; record any surviving diff.
      const result = await pi.exec("aimark", ["record", "--harness", "pi", "--model", edit.model, "--session", edit.session, "--files", edit.path], { cwd: edit.cwd });
      if (result.code !== 0) throw new Error(result.stderr || "record failed");
    } catch (error) {
      if (ctx.hasUI) ctx.ui.notify(`aimark could not record ${edit.path}: ${String(error)}`, "error");
      else console.error(`aimark: ${String(error)}`);
    }
  });
  pi.registerCommand("aimark", {
    description: "Show the project's current AI line contribution",
    handler: async (_args, ctx) => {
      try {
        const result = await pi.exec("aimark", ["stat", "--json"], { cwd: ctx.cwd });
        if (result.code !== 0) throw new Error(result.stderr || "stat failed");
        const stats = JSON.parse(result.stdout);
        const summary = `${stats.pct.toFixed(1)}% AI · ${stats.ai}/${stats.total} lines · ${stats.pending} pending`;
        if (ctx.hasUI) ctx.ui.notify(summary, "info"); else console.log(summary);
      } catch (error) { if (ctx.hasUI) ctx.ui.notify(String(error), "error"); else console.error(String(error)); }
    },
  });
}
