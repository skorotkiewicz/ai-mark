import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { promisify } from "node:util";

const exec = promisify(execFile);

const plugin = fileURLToPath(new URL("../integrations/codex-plugin/", import.meta.url));
const binary = resolve(process.env.AIMARK_BIN || join("target", "debug", process.platform === "win32" ? "aimark.exe" : "aimark"));

async function setup(t) {
  const root = mkdtempSync(join(tmpdir(), "aimark-codex-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  await exec("git", ["init", "--quiet"], { cwd: root });
  const bin = async (args) => (await exec(binary, ["--root", root, ...args], { cwd: root })).stdout;
  const manifest = JSON.parse(readFileSync(join(plugin, "plugin.json"), "utf8"));
  const config = JSON.parse(readFileSync(join(plugin, manifest.extensions["com.openai"].hooks), "utf8"));
  const hook = (event, id, patch, cwd = root) => {
    const handler = config.hooks[event].find(({ matcher }) => new RegExp(matcher).test("apply_patch")).hooks[0];
    return new Promise((resolve, reject) => {
      const child = spawn(handler.command, {
        cwd, shell: true, timeout: 15000,
        env: { ...process.env, PLUGIN_ROOT: plugin, PATH: [dirname(binary), process.env.PATH].join(delimiter) },
      });
      let stdout = "";
      let stderr = "";
      child.stdout.setEncoding("utf8").on("data", (chunk) => { stdout += chunk; });
      child.stderr.setEncoding("utf8").on("data", (chunk) => { stderr += chunk; });
      child.on("error", reject);
      child.on("close", (status) => resolve({ status, stdout, stderr }));
      child.stdin.on("error", reject);
      child.stdin.end(JSON.stringify({
        hook_event_name: event, tool_name: "apply_patch", tool_use_id: id,
        session_id: "session-test", turn_id: "turn-test", model: "example-model",
        cwd, tool_input: { command: patch },
      }));
    });
  };
  return { root, bin, hook, blame: async (path) => JSON.parse(await bin(["blame", path, "--json"])) };
}

function success(result) {
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, "");
}

test("bundled hooks attribute multi-file patches and resolve renames from a subdirectory", async (t) => {
  const { root, bin, hook, blame } = await setup(t);
  const src = join(root, "src");
  mkdirSync(src);
  const baseline = "first human line\nsecond human line\nthird human line\n";
  writeFileSync(join(src, "old.txt"), baseline);
  writeFileSync(join(root, "unrelated.txt"), "human baseline\n");
  await bin(["checkpoint"]);
  writeFileSync(join(root, "unrelated.txt"), "pending human edit\n");
  const patch = [
    "*** Begin Patch", "*** Update File: old.txt", "*** Move to: renamed.txt",
    "@@", " first human line", "+robot line", "*** Add File: comma,name.txt",
    "+*** Delete File: unrelated.txt", "*** End Patch",
  ].join("\n");
  success(await hook("PreToolUse", "multi", patch, src));
  renameSync(join(src, "old.txt"), join(src, "renamed.txt"));
  writeFileSync(join(src, "renamed.txt"), baseline.replace("second", "robot line\nsecond"));
  writeFileSync(join(src, "comma,name.txt"), "*** Delete File: unrelated.txt\n");
  success(await hook("PostToolUse", "multi", patch, src));
  const lines = await blame("src/renamed.txt");
  assert.equal(lines[0].ai, false);
  assert.equal(lines[1].author, "codex/example-model");
  assert.equal((await blame("src/comma,name.txt"))[0].ai, true);
  assert.equal((await blame("unrelated.txt"))[0].pending, true);
  const commit = await exec("git", ["--git-dir=.aimark/repo", "log", "-1", "--format=%B"], { cwd: root });
  assert.match(commit.stdout, /AI-Session: session-test/);
});

test("bundled hooks reject an unmatched post event and reserve overlapping file scopes", async (t) => {
  const { root, bin, hook, blame } = await setup(t);
  writeFileSync(join(root, "a.txt"), "human\n");
  await bin(["checkpoint"]);
  const patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n-human\n+robot\n*** End Patch";
  const unmatched = await hook("PostToolUse", "first", patch);
  assert.match(unmatched.stderr, /matching checkpoint/);
  assert.equal(unmatched.status, 2);
  success(await hook("PreToolUse", "first", patch));
  writeFileSync(join(root, "a.txt"), "robot\n");
  const overlapping = await hook("PreToolUse", "second", patch);
  assert.match(overlapping.stderr, /overlapping patch/);
  assert.equal(overlapping.status, 2);
  success(await hook("PostToolUse", "first", patch));
  assert.equal((await blame("a.txt"))[0].ai, true);
  writeFileSync(join(root, "a.txt"), "pending human edit\n");
  assert.match((await hook("PostToolUse", "first", patch)).stderr, /matching checkpoint/);
  assert.equal((await blame("a.txt"))[0].pending, true);
});
