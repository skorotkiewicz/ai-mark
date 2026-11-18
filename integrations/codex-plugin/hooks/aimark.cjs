const { execFile } = require("node:child_process");
const { createHash } = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const { promisify } = require("node:util");

const exec = promisify(execFile);

const maxInputBytes = 4 * 1024 * 1024;
const toolHeaders = /^\*\*\* (Add|Update|Delete) File: (.+)$/u;
const moveHeader = "*** Move to: ";

function check(condition, message) {
  if (!condition) throw new Error(message);
}

async function readEvent() {
  const chunks = [];
  let size = 0;
  for await (const chunk of process.stdin) {
    size += chunk.length;
    check(size <= maxInputBytes, "hook payload exceeds 4 MiB");
    chunks.push(chunk);
  }
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

function metadata(value, name) {
  check(typeof value === "string" && value.trim() && Buffer.byteLength(value) <= 512
    && !/[<>\p{Cc}]/u.test(value), `invalid ${name} in hook payload`);
  return value;
}

function patchTargets(command) {
  check(typeof command === "string", "missing tool_input.command");
  let lines = command.trim().split(/\r?\n/u);
  if (["<<EOF", "<<'EOF'", '<<"EOF"'].includes(lines[0]) && lines.at(-1)?.endsWith("EOF")) {
    lines = lines.slice(1, -1);
  }
  check(lines[0]?.trim() === "*** Begin Patch" && lines.at(-1)?.trim() === "*** End Patch",
    "expected a complete apply_patch patch");

  const targets = [];
  let operation;
  let canMove = false;
  for (const line of lines.slice(1, -1)) {
    // A leading space is meaningful inside an update. It marks a context line.
    const marker = operation === "Update" ? line.trimEnd() : line.trim();
    check(!marker.startsWith("*** Environment ID:"), "environment-scoped patches require a local path");
    const header = toolHeaders.exec(marker);
    if (header) {
      operation = header[1];
      targets.push(header[2]);
      canMove = operation === "Update";
    } else if (canMove && marker.startsWith(moveHeader)) {
      targets.push(marker.slice(moveHeader.length));
      canMove = false;
    } else {
      const addition = operation === "Add" && line.startsWith("+");
      const update = operation === "Update" &&
        (line === "" || /^[ +\-]/u.test(line) || marker === "@@" ||
          marker.startsWith("@@ ") || marker === "*** End of File");
      check(addition || update, "invalid line in apply_patch patch");
      canMove = false;
    }
  }
  check(targets.length > 0, "patch has no file targets");
  return targets;
}

function existing(pathname) {
  try {
    return fs.lstatSync(pathname);
  } catch (error) {
    if (error.code === "ENOENT") return undefined;
    throw error;
  }
}

function projectRoot(cwd) {
  let directory = cwd;
  while (true) {
    if (existing(path.join(directory, ".aimark")) || existing(path.join(directory, ".git"))) {
      return directory;
    }
    const parent = path.dirname(directory);
    if (parent === directory) return cwd;
    directory = parent;
  }
}

function contained(root, filename) {
  const relative = path.relative(root, filename);
  return relative && relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative);
}

function projectFile(root, cwd, target) {
  check(target && !target.includes("://") && !target.includes("\0"), "patch target must be a local file");
  let filename = path.resolve(cwd, target);
  if (!contained(root, filename) && path.isAbsolute(target)) {
    // Resolve aliases outside the project, including macOS temporary-directory aliases.
    // Preserve the suffix so internal symlinks are still checked below.
    const ancestors = [];
    for (let parent = path.dirname(filename); ; parent = path.dirname(parent)) {
      ancestors.push(parent);
      if (parent === path.dirname(parent)) break;
    }
    for (const ancestor of ancestors.reverse()) {
      if (existing(ancestor) && path.relative(root, fs.realpathSync(ancestor)) === "") {
        filename = path.join(root, path.relative(ancestor, filename));
        break;
      }
    }
  }
  check(contained(root, filename), "patch target is outside the project");
  const parts = path.relative(root, filename).split(path.sep);
  let current = root;
  for (const [index, part] of parts.entries()) {
    check(![".git", ".aimark"].includes(part.toLowerCase()), "patch target is internal metadata");
    current = path.join(current, part);
    const info = existing(current);
    if (info) {
      check(index === parts.length - 1 ? info.isFile() : info.isDirectory(),
        "patch target must be a regular file with no symlink components");
    }
  }
  check(!parts[0].startsWith(":"), "Git pathspec syntax is not accepted");
  return parts.join("/");
}

function ensureDirectory(directory) {
  fs.mkdirSync(directory, { recursive: true });
  check(fs.lstatSync(directory).isDirectory(), "checkpoint directory must not be a symlink");
}

function overlaps(left, right) {
  if (process.platform === "win32" || process.platform === "darwin") {
    left = left.toLowerCase();
    right = right.toLowerCase();
  }
  return left === right || left.startsWith(`${right}/`) || right.startsWith(`${left}/`);
}

async function runAimark(root, command, files, ...options) {
  const args = ["--root", root, command, ...options, ...files.map((file) => `--files=${file}`)];
  try {
    await exec("aimark", args, { cwd: root, timeout: 20000, windowsHide: true });
  } catch (error) {
    throw new Error(error.stderr?.trim() || `aimark ${command} failed (${error.code ?? error.signal ?? "unknown error"})`);
  }
}

function readReceipt(directory) {
  const file = path.join(directory, "receipt.json");
  const info = existing(file);
  check(info?.isFile() && info.size <= maxInputBytes, "invalid or incomplete checkpoint receipt");
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

async function checkpoint(root, pending, reservation, receipt) {
  fs.mkdirSync(reservation);
  try {
    // Publish the reservation before checking other scopes. Concurrent pre hooks
    // can both decline an overlap, but cannot both take its baseline.
    fs.writeFileSync(path.join(reservation, "receipt.json"), JSON.stringify(receipt), { mode: 0o600 });
    for (const entry of fs.readdirSync(pending, { withFileTypes: true })) {
      if (!entry.isDirectory() || !/^call-[a-f0-9]{64}$/u.test(entry.name)) continue;
      const other = path.join(pending, entry.name);
      if (other === reservation) continue;
      let previous;
      try {
        previous = readReceipt(other);
      } catch (error) {
        if (!existing(other)) continue;
        throw error;
      }
      check(!receipt.files.some((file) => previous.files.some((otherFile) => overlaps(file, otherFile))),
        "an overlapping patch is pending. Let it finish before editing the same file");
    }
    await runAimark(root, "checkpoint", receipt.files);
    fs.writeFileSync(path.join(reservation, "ready"), "");
  } catch (error) {
    fs.rmSync(reservation, { recursive: true, force: true });
    throw error;
  }
}

async function record(root, reservation, files) {
  check(existing(reservation)?.isDirectory() && existing(path.join(reservation, "ready"))?.isFile(),
    "no matching checkpoint for this Codex patch");
  const receipt = readReceipt(reservation);
  check(JSON.stringify(receipt.files) === JSON.stringify(files), "patch files do not match the checkpoint");
  // Claim once. Keep the reservation on failure so a later checkpoint cannot
  // absorb the unrecorded Codex edit into a human baseline.
  fs.writeFileSync(path.join(reservation, "recording"), "", { flag: "wx" });
  await runAimark(root, "record", files, "--harness=codex", `--model=${receipt.model}`, `--session=${receipt.session}`);
  fs.rmSync(reservation, { recursive: true });
}

async function main() {
  const event = await readEvent();
  check(event?.tool_name === "apply_patch", "expected an apply_patch hook");
  check(["PreToolUse", "PostToolUse"].includes(event.hook_event_name), "unsupported Codex hook event");
  const session = metadata(event.session_id, "session_id");
  const turn = metadata(event.turn_id, "turn_id");
  const call = metadata(event.tool_use_id, "tool_use_id");
  const model = metadata(event.model ?? "unknown", "model");
  check(typeof event.cwd === "string" && path.isAbsolute(event.cwd), "hook cwd must be an absolute path");
  const cwd = fs.realpathSync(event.cwd);
  check(fs.statSync(cwd).isDirectory(), "hook cwd must be a directory");
  const root = projectRoot(cwd);
  const files = [...new Set(patchTargets(event.tool_input?.command)
    .map((target) => projectFile(root, cwd, target)))].sort();
  const ledger = path.join(root, ".aimark");
  const pending = path.join(ledger, "codex-pending");
  const id = createHash("sha256").update(JSON.stringify([session, turn, call])).digest("hex");
  const reservation = path.join(pending, `call-${id}`);

  if (event.hook_event_name === "PreToolUse") {
    ensureDirectory(ledger);
    ensureDirectory(pending);
    await checkpoint(root, pending, reservation, { session, turn, call, model, files });
  } else {
    check(existing(ledger)?.isDirectory() && existing(pending)?.isDirectory(),
      "no matching checkpoint for this Codex patch");
    await record(root, reservation, files);
  }
}

main().catch((error) => {
  process.stderr.write(`aimark Codex hook: ${error.message}\n`);
  process.exitCode = 2;
});
