# aimark

See which lines came from a human and which came from an AI harness, without adding markers to your code.

aimark keeps snapshots in a separate Git ledger under `.aimark/`. The CLI and local browser dashboard show attribution for the current working tree. Nothing is uploaded.

![Local dashboard showing human and AI line attribution](docs/dashboard.png)

## Install

Requires Rust 1.89 or newer and Git 2.28 or newer.

```sh
cargo install --path . --locked
```

Or run directly from this checkout with `cargo run -- <command>`.

## Quick start

In the project you want to track:

```sh
aimark checkpoint
aimark run --harness codex -- codex
aimark dashboard
```

Open the printed address, normally `http://127.0.0.1:4242`. The dashboard shows project totals, searchable files, and a source viewer with human, AI, and pending line filters. Hover an attribution label for its commit. Use Refresh after edits. Press `/` to search files.

`aimark dashboard --port 0` chooses an available port. The server binds only to loopback and embeds all its assets. It needs no Node.js installation.

To inspect the example project without touching your own source:

```sh
just demo
just demo-dashboard
```

## Record edits

Every AI edit needs a human snapshot before it and an AI snapshot after it:

```sh
aimark checkpoint --files src/main.rs
# The agent edits src/main.rs here.
aimark record --harness pi --model example-model --files src/main.rs
aimark blame src/main.rs
aimark stat
```

Use `--files` repeatedly for multiple files or directories. Each argument is a literal path. Commas, spaces, and brackets belong to the filename and are not separators or patterns. A scoped snapshot leaves unrelated pending changes alone. Omitting `--files` snapshots the entire eligible project.

The root is the nearest ancestor containing `.aimark/` or `.git`, falling back to the current directory. Pass `--root .` to start an independent project inside another tracked directory. With an explicit `--root`, file arguments are relative to that root. Otherwise they are relative to your current directory. Absolute paths within the root also work.

## Commands

| Command | Purpose |
| --- | --- |
| `init` | Create an empty ledger without snapshotting existing files |
| `checkpoint [--files PATH]` | Snapshot human changes before AI work |
| `record --harness NAME [--model NAME] [--session ID] [--files PATH]` | Snapshot AI changes |
| `run --harness NAME [--model NAME] [--session ID] -- COMMAND ...` | Snapshot before and after a command |
| `blame FILE [--json]` | Inspect current line attribution |
| `stat [--json]` | Show AI percentage per file and overall |
| `dashboard [--port PORT]` | Serve the local browser dashboard |
| `hook claude` / `hook codex` | Handle native file-edit hook JSON from stdin |

`run` starts the command in your current directory and preserves its exit code. It records changes even after failure. On Unix it forwards `SIGINT`, `SIGTERM`, and `SIGHUP` to the direct child and records partial edits before returning. Uncatchable termination, such as `SIGKILL`, cannot trigger a final snapshot. On Windows, normal command completion and failure are supported; interruption capture is Unix-only.

## Harness integrations

### Codex

Native integration is available for local Codex sessions with `PreToolUse` and `PostToolUse` hooks for `apply_patch`. The adapter follows the Codex 0.169 hook contract.

1. Install the current aimark binary on `PATH`.
2. Merge [integrations/codex/hooks.json](integrations/codex/hooks.json) into your project's `.codex/hooks.json`. Preserve existing hooks and use one configuration source per layer to avoid duplicate invocation.
3. Start Codex and open `/hooks` to review and trust the added definitions. Local desktop sessions that load these hooks can use the app's hook review UI instead.

Codex runs the hook command from the session working directory. Aimark discovers the project root and resolves patch paths relative to the `cwd` in the hook payload. For a project without Git, run `aimark init` at its intended root before launching Codex from a subdirectory.

The adapter checkpoints the affected files before each patch and records them afterward with the model and session metadata. It supports additions, updates, deletions, and moves across multiple files. Unrelated pending human edits stay pending. Codex patches to different files can overlap; another patch to the same file is blocked until the earlier patch is recorded. Explicit remote-environment patches are rejected by this local adapter.

Successful pre-edit checkpoints save a receipt under `.aimark/pending/`. A post event must match that receipt's session, turn, tool-call id, and file paths. Missing or mismatched receipts return an error and leave changes unrecorded. The receipt is claimed before recording and removed after success. Failed or interrupted recording keeps the file scope blocked, and replayed post events cannot reuse the old baseline. Inspect pending changes and use the manual `record --files` workflow to recover attribution after a failed record.

If a tool never emits its post event, its receipt can remain and block later patches to those files. Confirm the original tool has stopped, inspect and record any surviving AI changes, then remove that tool's receipt from `.aimark/pending/` before retrying. Receipts include the session and tool-call ids so you can identify the affected call.

Exit code 2 blocks supported Codex pre-tool calls. Codex can still proceed after a hook launch failure or external timeout; aimark's receipt check prevents the later post hook from guessing that a baseline exists. Hooks skipped because they are disabled or untrusted cannot record provenance.

These hooks cover native `apply_patch` calls in local sessions. For shell writes, custom tools, or older local clients, use the wrapper:

```sh
aimark run --harness codex --model example-model -- codex
```

Choose native hooks or the wrapper for a session. The wrapper attributes the entire run's diff, including shell writes. Native hook coverage follows the [official Codex hooks reference](https://learn.chatgpt.com/docs/hooks).

Hosted and cloud-orchestrated sessions do not use these local command hooks.

### Claude Code

Install aimark on `PATH`, then merge [integrations/claude/settings.json](integrations/claude/settings.json) into your project's `.claude/settings.json`. Preserve any hooks already configured there. The configuration requires Claude Code 2.1.295 or newer for `onFailure: "block"`. The supplied commands use the POSIX shell syntax supported by Claude Code on macOS, Linux, and Git Bash.

The same `aimark hook claude` command handles before, after, and failed file-tool events. It reads the target path and session from the payload and the latest model from the transcript when available. Missing model information becomes `unknown`. A malformed payload fails instead of recording the entire project.

Hook errors return exit code 2, so Claude blocks a file edit when its before-edit checkpoint fails. The supplied before-edit hook also sets `onFailure: "block"` and a 30-second timeout, so launch failures and external timeouts block the edit too. Aimark waits up to five seconds to acquire a ledger lock before returning an error.

These hooks cover `Edit`, `Write`, `MultiEdit`, and `NotebookEdit`. Shell commands and custom tools that write files need wrapper coverage instead:

```sh
aimark run --harness claude-code -- claude
```

Choose hooks for file-tool attribution or the wrapper for session-wide attribution. Avoid combining them in one session, because a wrapper's final snapshot also includes pending human edits.

The older `record --harness claude-code --stdin claude` form remains available for existing two-phase hook configurations. Use the supplied scoped `hook claude` configuration for new setups.

### pi

Copy [integrations/pi/aimark.ts](integrations/pi/aimark.ts) into `.pi/extensions/aimark.ts` in the project you want to track, or load it directly:

```sh
pi -e ./integrations/pi/aimark.ts
```

The extension checkpoints before `write` and `edit`, records afterward using the current model and session, and adds `/aimark` for a quick percentage summary. It blocks a file edit if its checkpoint cannot be saved. It reports recording failures through pi's UI, or stderr without a UI. The extension's type import uses the current `@earendil-works/pi-coding-agent` package name; pi loads the TypeScript module directly.

For shell writes, custom tools, or older pi versions, use the universal wrapper instead:

```sh
aimark run --harness pi -- pi
```

The adapter follows the [official pi extension API](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/extensions.md). Claude configuration follows the [official hook reference](https://code.claude.com/docs/en/hooks).

### Other harnesses

The wrapper works with any local command. You can also call `checkpoint --files PATH` before a tool and `record --files PATH` afterward from a harness plugin. Pass the actual model name if the harness exposes it.

## Attribution rules

- Unchanged lines keep their previous attribution. Whitespace-only changes are ignored by blame. Git also detects some moves and renames.
- Unrecorded lines count as human and have `pending: true`. This means their origin has not been recorded. It is not a claim that a human wrote them.
- Reports never commit pending changes. Looking at the dashboard during an edit cannot turn that edit into a human checkpoint.
- Reports capture one ledger revision and release their capture lock before blaming lines. Slow reports do not hold up edit hooks. Working files remain live, so different files can reflect different moments during an active edit.
- Snapshots honor worktree `.gitignore` files. Files already in the ledger stay tracked until removed, following Git's usual behavior. The real repository's local excludes and global ignore settings are separate from the ledger.
- Reports inspect UTF-8 text files up to 1,000,000 bytes. Binary files, symlinks, and larger files are skipped and counted. Empty text files appear with zero lines. Snapshots can still contain binary or large files.
- Ledger operations use an OS file lock. Scoped hooks can track overlapping edits to different files. Concurrent edits to the same file, simultaneous wrappers, and human edits during a wrapper run cannot be separated reliably.
- Nested Git repositories and submodules are separate tracking boundaries. Initialize and inspect them independently with `--root`; the parent's report does not include their source lines.

The ledger has its own history and index. aimark adds `/.aimark/` to the real repository's local exclude file when one exists, including linked worktrees. Source files, the real Git index, and tracked ignore files stay untouched. Keep files you do not want snapshotted in `.gitignore` before the first checkpoint.

Reports use temporary read views inside `.aimark/` that share the ledger's Git objects but have their own detached HEAD and no shared index. The views are removed when each report finishes. The dashboard uses four request workers, so one slow report does not stall asset requests.

This is cooperative provenance. It cannot detect AI code pasted from a chat, reconstruct authorship before tracking started, or prevent someone from deleting or rewriting the ledger. Structural formatter rewrites may change attribution. The dashboard is read-only and has no account system; other users and processes on the same machine may reach its loopback port while it is running.

## JSON output

`blame --json` returns one object per line:

```json
{"line": 3, "ai": true, "pending": false, "author": "pi/example-model", "text": "    robot();", "commit": "..."}
```

`stat --json` returns `files`, `ai`, `human`, `pending`, `total`, `pct`, and `skipped`. Each file includes its path and the same line counts. Pending is a subset of human, so `human + ai = total`. Percentages refer to current lines, not historical effort.

## Development

```sh
just check
just test
just build
```

Rust tests exercise the real binary and Git in temporary projects. Dashboard tests need permission to bind loopback sockets.

Optional adapter and browser checks require Node.js 24 or newer:

```sh
cargo build
npm ci
npm run check
npm run test:pi
npx playwright install chromium
npm run test:browser
```

Browser tests cover attribution filters, file and source search, source escaping, mobile layout, and refresh. The app itself has no npm runtime dependencies.
