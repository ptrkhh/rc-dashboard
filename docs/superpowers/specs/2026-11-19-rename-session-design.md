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
- The new name shows in the dashboard on the next poll (the cards already read
  the per-tool name stores that rename writes).
- The name survives resume, and for Claude it lands in the same file Claude
  Code itself reads, so renames in either direction stay visible.

## Non-goals

- No instant sync of the name *inside* a live Codex thread or OpenCode server
  (they converge on the next resume, see Approaches).
- No rename from the chat overlay (its header updates on next open).
- No batch or auto rename.

## Anti-goals

- A rename that silently washes out on resume because the generated animal
  name was re-minted.

## Constraints

- Claude resume re-mints `--remote-control <generated name>`
  (`collect/spawn.rs:200`). Rename must make it reuse an existing custom
  title, or the rename is lost.
- The bridges keep two stores: sidecar files (`<sid>.name`, `<sid>.owned.json`)
  drive display; the state json's `display_name` is what the bridge pushes to
  the tool on resume. Rename must update both.
- Session ids must pass the existing `assert_valid_sid` / `assert_opencode_sid`
  checks before touching the filesystem.

## Approaches

- **A. Rename in the Rust backend (chosen).** `POST /api/rename`
  `{tool, sid, dir, name}` writes the per-tool stores directly. Cheap, works
  for running *and* resumable sessions, no bridge (`.mjs`) changes. Trade-off:
  a live Codex/OpenCode thread keeps its old name inside the tool until the
  next resume, where existing code re-pushes the new one (Codex sends
  `thread/name/set` on every resume; OpenCode's title syncs at next launch).
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
- **Codex**: write `~/.claude/cdash-codex/<sid>.name` and update `display_name`
  in the matching session state json.
- **OpenCode**: update `display_name` in the matching state json and in
  `<sid>.owned.json` (`owned.display_name` already wins over the server title
  in display).

## UI (decided)

- **Rename** button on the cards next to Pin/Purge (resumable/pinned) and the
  running card's actions; opens a small dialog (`<dialog class="dialog">`, the
  folder-picker pattern) with one text input pre-filled with the current name.
  `window.prompt` is unreliable in the Android webview.
- Validation: trimmed, non-empty, ≤ 120 chars, no control characters; output
  already goes through `esc()` in the cards.
- Submit calls `api('/api/rename', …)` like the other card actions; the next
  poll shows the new name everywhere.

## Left to the builder

- Where the rename logic lives (a `collect` helper vs. in `routes.rs`), how to
  locate the state json for a sid, and exact test placement, following the
  existing naming tests.

## Tests

- Rust: each per-tool store gets written and the name wins over the generated
  one in collection; the Claude resume path reuses a custom title.
- JS (`test/frontend-guards.test.mjs`): the Rename button renders on the cards
  and submits the right payload, mirroring the Pin/Purge assertions.
