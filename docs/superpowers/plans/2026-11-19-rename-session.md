# Rename a Session Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rename action on every dashboard session card (running, resumable, pinned, external) that writes the new name into the per-tool sidecar store, where it wins over generated and snapshot names everywhere.

**Architecture:** New `POST /api/rename` in the Rust agent writes only the per-sid store each tool already reads (Claude `custom-title.json`, Codex `<sid>.name`, OpenCode `<sid>.owned.json` read-modify-write) and clears that tool's thread cache. Read paths are taught to prefer the sidecar over state-snapshot names, and the Codex bridge stops overwriting an existing `<sid>.name`. The frontend adds a Rename button per card and one `<dialog>` for the new name.

**Tech Stack:** Rust (axum, tokio::fs), vanilla JS frontend, Node test harness (`npm test`), cargo.

**Spec:** `docs/superpowers/specs/2026-11-19-rename-session-design.md`

## Global Constraints

- Validation (both backend and frontend): trimmed, non-empty, ≤ 120 chars (chars, not bytes), no control characters (`char::is_control`).
- Session ids pass `assert_valid_sid` / `assert_opencode_sid` (`collect/validate.rs`, dispatched like `purge_session`: `ses_` prefix → OpenCode check) and `dir` passes `assert_path` before any filesystem write.
- Rename never writes a bridge state file (`<pane>.json`, `<pane>.native`) or the OpenCode server title; only per-sid stores.
- `body` is `{tool, sid, dir, name}` with `tool` ∈ `"claude" | "codex" | "opencode"`.
- Commit style: Conventional Commits, as in `git log`.

## Review Focus

- Rename while a Codex worker is live: its next `save()` must not rewrite `<sid>.name` back to the old in-memory name (Task 3).
- Rename of an OpenCode sid with missing/corrupt `owned.json` or a sid mismatch: 400, the file is left untouched (Task 1).
- Rename of a Claude sid with no transcript in `projects/<project_dir_name(dir)>/<sid>.jsonl`: 400, no directory or title file created (Task 1).
- A name that slips past the dialog (stale tab, Tauri webview): the backend still rejects control chars, whitespace-only, and > 120 chars with 400 (Task 1).
- Right after a Codex/OpenCode rename, the next poll (≤ 2 s) must show the new name, not the ≤ 10 s thread-cache row (Task 1 clears both caches).

---

### Task 1: POST /api/rename

**Files:**
- Create: `crates/agent/src/collect/rename.rs`
- Modify: `crates/agent/src/collect/mod.rs` (add `mod rename;`)
- Modify: `crates/agent/src/collect/opencode.rs:67` (`fn root` → `pub(crate) fn root`)
- Modify: `crates/agent/src/http/routes.rs` (body struct + handler, next to `post_pin` ~:189)
- Modify: `crates/agent/src/http/serve.rs:56` (route table, after `post "/api/pin"`)
- Test: `crates/agent/src/collect/rename.rs` (`#[cfg(test)]`, tempdir ctx like `pins`/`spawn` tests)

**Interfaces:**
- Consumes: `validate::{assert_valid_sid, assert_opencode_sid, assert_path, Refused}`, `fsio::write_atomic`, `parse::paths::project_dir_name`, `lookup::custom_title`, `codex::root`-style roots, `ctx.codex_threads` / `ctx.opencode_threads`.
- Produces: `collect::rename::rename(ctx: &Ctx, tool: &str, sid: &str, dir: &str, name: &str) -> Result<(), Refused>`; `POST /api/rename` accepting `{"tool":…, "sid":…, "dir":…, "name":…}` and answering `{"ok":true}`.

- [ ] **Step 1: Write the failing tests** in `rename.rs` `#[cfg(test)]`, one per store and rule:

```rust
// claude: rename writes custom-title.json readable by lookup::custom_title
rename(&ctx, "claude", SID, dir, "Brainstorming: Rename Session").await.unwrap();
assert_eq!(lookup::custom_title(&ctx.claude_dir, dir, SID).await.as_deref(),
    Some("Brainstorming: Rename Session"));

// codex: the sidecar holds the raw name, nothing else
rename(&ctx, "codex", SID, dir, "Renamed").await.unwrap();
assert_eq!(read(&ctx.claude_dir.join("cdash-codex").join(format!("{SID}.name"))), "Renamed");

// opencode: read-modify-write keeps sid and dir
// (pre-write <sid>.owned.json as {"sid":…,"dir":…,"display_name":"Old"})
rename(&ctx, "opencode", "ses_x1", dir, "Renamed").await.unwrap();
// parsed owned.json: sid and dir unchanged, display_name == "Renamed"

// opencode without an owned.json (or with a mismatched sid inside) → BadRequest, no file created
// claude without projects/<project_dir_name(dir)>/<sid>.jsonl → BadRequest, custom-title.json absent
// "  " → BadRequest; 121 chars → BadRequest; "ok\u{7}" → BadRequest; "ses_x!" with tool "opencode" → BadRequest
```

Name the tests after these behaviors (e.g. `claude_rename_lands_in_custom_title_json`, `opencode_rename_keeps_owned_sid_and_dir`, `rename_rejects_blank_too_long_control_and_bad_sid`).

- [ ] **Step 2: Run to verify they fail** — `cargo test -p cdash-agent rename:: 2>&1 | tail` (compile error is the "fail").

- [ ] **Step 3: Implement** `rename.rs`. One `match tool` arm per store, `assert_path(dir)` and the sid assert first; then:
  - `claude`: reject unless `claude_dir/projects/<project_dir_name(dir)>/<sid>.jsonl` exists (the transcript gate, Review Focus); `create_dir_all` the sid dir; `write_atomic` `custom-title.json` with exactly `{"customTitle": <name>}` (schema checked against Claude Code 2.1.327).
  - `codex`: `create_dir_all(claude_dir/cdash-codex)`; `write_atomic` the `<sid>.name` file with the bare name.
  - `opencode`: read `<opencode::root(ctx)>/<sid>.owned.json`; absent or `sid` field ≠ sid → `Refused::BadRequest`; write back keeping `sid` and `dir`, only `display_name` changed (serde_json::Value edit, then `write_atomic`).
  - Afterwards clear the poll cache: `*ctx.codex_threads.lock().await = None` for codex, `*ctx.opencode_threads.lock().await = None` for opencode.

- [ ] **Step 4: Run tests, verify pass** — `cargo test -p cdash-agent rename:: 2>&1 | tail`.

- [ ] **Step 5: Add route + handler.** In `routes.rs`: `RenameBody { #[serde(default)] tool/sid/dir/name: String }` (defaults like `SidBody` so a missing field is a validation error, not a 400-desync) and `post_rename` calling `rename::rename(&ctx, …)` and answering `{"ok":true}`, both shaped exactly like `post_pin`. Register `post "/api/rename" => routes::post_rename,` in `serve.rs`.

- [ ] **Step 6: Run the full Rust suite, commit.**

```bash
cargo test -p cdash-agent 2>&1 | tail
git add crates/agent/src/collect/rename.rs crates/agent/src/collect/mod.rs crates/agent/src/collect/opencode.rs crates/agent/src/http/routes.rs crates/agent/src/http/serve.rs
git commit -m "feat: POST /api/rename writes per-tool session names"
```

### Task 2: The sidecar wins on every read path

**Files:**
- Modify: `crates/agent/src/collect/codex.rs` (new `sid_name` helper; reuse it in `threads()` ~:153)
- Modify: `crates/agent/src/collect/opencode.rs` (new `owned_name` helper reading `display_name` from `<root>/<sid>.owned.json`)
- Modify: `crates/agent/src/collect/sessions.rs:121` (OpenCode running card) and `:136` (Codex running card)
- Modify: `crates/agent/src/collect/spawn.rs:200` (Claude launch/reuse)
- Test: `sessions.rs` and `spawn.rs` test modules

**Interfaces:**
- Consumes: Task 1 stores (files already written by rename).
- Produces: `codex::sid_name(ctx: &Ctx, sid: &str) -> Option<String>`; `opencode::owned_name(ctx: &Ctx, sid: &str) -> Option<String>` — later tasks and any future reader use these instead of touching the files.

- [ ] **Step 1: Failing tests.**
  - In `sessions.rs` tests (pattern: existing test at :707 seeds a `.native` snapshot with `display_name:"Live"`): seed a Codex `.native` snapshot carrying the old name plus a `<sid>.name` sidecar with a newer one; assert the running row shows the sidecar name. Same for OpenCode with `<sid>.owned.json`.
  - In `spawn.rs` tests (pattern: `stub_tmux` records args to `tmux-args`): with `projects/<slug(dir)>/<sid>/custom-title.json` present and the session resuming (`--resume <sid>` in args), assert the recorded `--remote-control` argument is the custom title, not the generated `[Claude] … · animal` name.

- [ ] **Step 2: Run, verify fail** — `cargo test -p cdash-agent 2>&1 | tail`.

- [ ] **Step 3: Implement.** Add the two helpers (each: validate sid, read file, non-empty check). Precedence at the two running-card construction sites becomes **sidecar → snapshot → generated** (today it is snapshot → generated; the Claude site at :204 already prefers `custom_title`). `spawn.rs:200` becomes `custom_title(&ctx.claude_dir, dir, &sid).await` falling back to `session_name("Claude", dir, &sid)` — new sessions have no title file, so the generated name still stands at first launch.

- [ ] **Step 4: Run suite, verify pass** — `cargo test -p cdash-agent 2>&1 | tail`.

- [ ] **Step 5: Commit** — `git commit -m "fix: renamed session names win over snapshot and generated names"` (files as above).

### Task 3: The Codex bridge stops clobbering the sidecar

**Files:**
- Modify: `crates/agent/src/collect/codex-remote.mjs` (`save()` ~:357-369)
- Test: `test/codex-remote.test.mjs`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: worker behavior — `save()` creates `<sid>.name` when absent and never rewrites an existing one.

- [ ] **Step 1: Failing test** in `test/codex-remote.test.mjs` (pattern: existing `:187` asserting `.name` is written): pre-create `<sid>.name` containing `Renamed` before starting the supervised flow; after the worker saves state at least once, assert the file still reads `Renamed`. The existing "writes `.name`" test must still pass unchanged (fresh session → absent → written).

- [ ] **Step 2: Run, verify fail** — `npm test test/codex-remote.test.mjs`.

- [ ] **Step 3: Implement.** In `save()`, replace the `namePath && namePath !== savedNamePath` condition with a check that the file does not exist yet (import an existence helper from `node:fs`; keep the `saved` promise chain and `atomic` write). One-line comment: dashboard renames own the sidecar once it exists; resume carries the new name back into state.

- [ ] **Step 4: Run, verify pass** — `npm test test/codex-remote.test.mjs test/codex-native.test.mjs`.

- [ ] **Step 5: Commit** — `git commit -m "fix(codex bridge): a dashboard rename owns <sid>.name once written"`.

### Task 4: Rename button and dialog on the cards

**Files:**
- Modify: `crates/agent/src/collect/sessions.rs` (add `pub provider: Option<&'static str>` with `skip_serializing_if` to `Resumable`, set at every `Resumable` construction — `"claude"` / `"codex"` / `"opencode"`)
- Modify: `public/index.html` (new `<dialog>` after `#picker`, ~:97)
- Modify: `public/app.js` (cards, click handler, dialog submit; `ICONS` ~:26)
- Test: `test/frontend-guards.test.mjs`

**Interfaces:**
- Consumes: `POST /api/rename {tool, sid, dir, name}` → `{ok:true}` (Task 1).
- Produces: card buttons with `data-rename="<sid>"`; `provider` on serialized resumable rows.

- [ ] **Step 1: Failing tests** in `test/frontend-guards.test.mjs` (pattern: `:1118-1127` card-render assertions): rendered running and resumable card HTML contains `data-rename="<sid>"` with a title/aria label "Rename session"; a terminal-provider card does not. Extend an existing resumable-render case to assert `s.provider` reaches the card path (payload carries `provider:"codex"`).

- [ ] **Step 2: Run, verify fail** — `npm test test/frontend-guards.test.mjs`.

- [ ] **Step 3: Implement backend field.** Add `provider` to `Resumable` and set it at each construction site (Claude rows, `codex.rs` history map, `opencode.rs` history map). The frontend needs it to fill `tool`; running cards already carry `provider`.

- [ ] **Step 4: Implement frontend.**
  - `index.html`: `<dialog id="rename-dialog" class="dialog">` (folder-picker pattern) with `dialog-head`, a form containing `<input id="rename-name" maxlength="120">`, Cancel and Save buttons; label "Rename session".
  - `app.js`: add a pencil `ICONS` entry; `resumableCard` gets `<button class="action" type="button" data-rename="${esc(s.sid)}" aria-label="Rename session">` in the actions row; `runningCard` gets the same button in its last row (skip `provider === 'terminal'`; render it for external panes — they have a sid). Click handler joins the delegation selector at `:531` (`[data-rename]`): look the row up in `lastRunning` by `sid`, else `[...lastPinned, ...lastRes]`; store `{sid, tool: row.provider || 'claude', dir: row.dir}` on the dialog, prefill the input with `row.displayName || ''`, `showModal()`. Submit: client-side validation (trim, non-empty, ≤ 120, `/[\u0000-\u001f\u007f]/` reject), `await api('/api/rename', {tool, sid, dir, name})`, close, `poll()`; errors `toast(err.message)` like the other actions.
- [ ] **Step 5: Run suite, verify pass** — `npm test && cargo test -p cdash-agent 2>&1 | tail` (resumable serialization changed).

- [ ] **Step 6: Commit** — `git commit -m "feat: rename sessions from the dashboard cards"`.
