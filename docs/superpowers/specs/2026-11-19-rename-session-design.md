# Rename a session from the dashboard — design

## Intent and the why

The dashboard mints every session name itself (`collect/names.rs`:
`[Tool] folder · animal`, animal derived from the session id). Once several
sessions exist, those names carry no meaning. The user wants to rename a
session from the dashboard — e.g. `[OpenCode] rc-dashboard · sheep` →
`Brainstorming: Rename Session` — and have the name stick.

## Goals

- A **Rename** action on running, resumable, and pinned session cards, for all
  three tools (Claude Code, Codex, OpenCode), including external Claude panes.
- The new name shows in the dashboard on the next poll (resumable cards
  already read the per-sid name stores that rename writes; running
  Codex/OpenCode cards are taught to, see Constraints).
- The name survives resume, and for Claude it lands in the same file Claude
  Code itself reads, so renames in either direction stay visible.

## Non-goals

- No instant sync of the name *inside* a live tool session: a running Codex
  thread, OpenCode server, or the Remote Control name of any running session
  keeps its old name until the next resume (see Approaches). OpenCode's own
  session title is never changed; only the dashboard and Remote Control names
  are.
- No rename from the chat overlay (its header updates on next open).
- No batch or auto rename.

## Anti-goals

- A rename that silently washes out on resume because the generated animal
  name was re-minted.

## Constraints

- Claude resume re-mints `--remote-control <generated name>`
  (`collect/spawn.rs:200`). Rename must make it reuse an existing custom
  title, or the rename is lost.
- A running Codex/OpenCode bridge holds its state in memory and rewrites
  `<pane>.json` (worker) and `<pane>.native` (supervisor) on every state
  change; the running card reads `.native`
  (`codex.rs` / `opencode.rs` `snapshot`). Writing `display_name` into either
  file is overwritten within seconds, and a restarted Codex worker re-writes
  `<sid>.name` from `<pane>.json` (`codex-remote.mjs` `save`). So rename must
  write **only the per-sid sidecar** (`<sid>.name`, `<sid>.owned.json`), and
  the sidecar must win everywhere: resume already reads it (`codex::threads`,
  OpenCode `threads`), and the running card must prefer it over the
  snapshot's name.
- Session ids must pass the existing `assert_valid_sid` / `assert_opencode_sid`
  checks, and `dir` must pass `assert_path`, before touching the filesystem.

## Approaches

- **A. Rename in the Rust backend (chosen).** `POST /api/rename`
  `{tool, sid, dir, name}` writes the per-sid name store directly. Cheap,
  works for running *and* resumable sessions; the only bridge change is one
  guard in `codex-remote.mjs` `save` (write `<sid>.name` only when absent).
  Trade-off: a live session keeps its old name inside the tool until the next
  resume, where existing code carries the new one into the state json (Codex
  then sends `thread/name/set`; both bridges pass it to
  `claude remote-control --name`).
- B. A plus live sync through the running bridges (RPC to Codex,
  `PATCH /api/session/:id` to the live OpenCode server). Instant tool-side
  names, but new Rust→bridge plumbing per tool that must degrade gracefully
  mid-restart — roughly double the surface for tool-side cosmetics. Rejected.
- C. Rename through each tool's native rename API. Rejected: much larger, and
  nothing is reachable for resumable (stopped) sessions.

## Per-tool behavior (decided)

- **Claude**: write `customTitle` into
  `~/.claude/projects/<project-slug>/<sid>/custom-title.json` — the file
  `collect/lookup.rs` already reads and that already beats the generated name.
  Plus the one-line `spawn.rs` fix so resume reuses an existing custom title.
  Claude Code 2.1.327 reads this file with the schema `{customTitle: string}`
  (checked in its binary), so write exactly that.
- **Codex**: write `~/.claude/cdash-codex/<sid>.name`. The running Codex card
  reads that file by sid before falling back to the snapshot's name, and the
  bridge's `save` stops overwriting an existing `<sid>.name`.
- **OpenCode**: update `display_name` in `<sid>.owned.json` (atomic
  read-modify-write that keeps `sid`/`dir`; the bridge drops an owned file
  whose `sid` doesn't match). `owned.display_name` already wins over the
  server title for resumable rows; the running OpenCode card reads it by sid
  the same way.
- Codex/OpenCode: clear that tool's cached thread list (`codex_threads` /
  `opencode_threads`, 10 s TTL) so the next poll shows the new name.

## UI (decided)

- **Rename** button on the cards next to Pin/Purge (resumable/pinned) and the
  running card's actions; opens a small dialog (`<dialog class="dialog">`, the
  folder-picker pattern) with one text input pre-filled with the current name.
  `window.prompt` is unreliable in the Android webview.
- Validation: trimmed, non-empty, ≤ 120 chars, no control characters; output
  already goes through `esc()` in the cards. Accepted risk: on Claude resume
  the name is a tmux argument, and tmux strips one trailing `;` from the
  Remote Control name (tested on tmux 3.3a); the dashboard name is unaffected.
- Submit calls `api('/api/rename', …)` like the other card actions; the next
  poll shows the new name everywhere.

## Left to the builder

- Where the rename logic lives (a `collect` helper vs. in `routes.rs`) and
  exact test placement, following the existing naming tests.

## Tests

- Rust: each per-tool store gets written and the name wins over the generated
  one in collection, for resumable rows *and* running cards (a sidecar name
  beats a `.native` snapshot carrying the old name); the Claude resume path
  reuses a custom title; an OpenCode rename keeps `owned.json`'s `sid`/`dir`.
- Bridge: Codex `save` leaves an existing `<sid>.name` alone.
- JS (`test/frontend-guards.test.mjs`): the Rename button renders on the cards
  and submits the right payload, mirroring the Pin/Purge assertions.
