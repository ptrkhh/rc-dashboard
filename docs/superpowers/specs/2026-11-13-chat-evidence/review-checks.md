# Review checks, 2026-11-13

Checks run during the review of the Part 2 to 5 specs. Each settles a claim
that the specs now state. Claude Code 2.1.323, on this host.

## Headless `initialize` with hooks off (Part 2)

`claude -p --input-format stream-json --output-format stream-json --verbose --model haiku`,
one `initialize` request, run under `env -i` in an empty folder.

| Settings | Reply after | Commands | Not built-in | Hooks started |
|---|---|---|---|---|
| none | 2.57 s | 153 | 99 | 4 |
| `--settings '{"disableAllHooks":true}'` | 0.57 s | 153 | 99 | 0 |

No model call was made.

## Image path with spaces (Part 3)

A live TUI (`--dangerously-skip-permissions --model haiku`, private tmux
server, the spike's trusted folder). A bracketed paste of
`…/up/abc/Screenshot 2026-11-13 100000.png`, with no Enter, showed
`❯ [Image #1]` within 1.5 s. Spaces in a stored name do not stop the attach.
The input box was cleared and the server killed; nothing was submitted.

## Branch cuts on real transcripts (Part 5)

Part 5's rule (a prompt whose `parentUuid` is known but is not the last
main-chain `uuid`) run over the owner's 253 latest transcripts:
922 prompts, 17 cuts. Every cut skipped at least one assistant record or
prompt. No cut came from a record written between turns (`bridge_status`,
`informational`, `local_command` and others were present).

## Clean filters and `git diff` (Part 5)

A repository with `.gitattributes` `* filter=x` and
`git config filter.x.clean "sh -c 'touch <marker>; cat'"`, one modified file.
`git -c core.fsmonitor=false diff --no-color --no-ext-diff --no-textconv HEAD`
created the marker: the clean filter ran.

## Context sizes per model (Part 5)

Largest `input + cache_read + cache_creation` of a main-chain reply in the
same 253 transcripts: opus-5-5 965,673; fable-5-2 825,913; sonnet-5-5
823,267; haiku-5-5 160,650.

## API-error turns (Part 4)

30 assistant records with `isApiErrorMessage: true` (rate_limit 12,
server_error 10, authentication_failed 7, invalid_request 1). In 29 a
`turn_duration` record followed; the 30th ended the file. Part 4's `error`
note, which waits for the turn's end, therefore fires.

## Build facts (Part 4)

`cargo tree -i ring -p cdash-agent`: `ring` 0.17.14 comes through
`jsonwebtoken` 9.3.1. rustls 0.23 uses aws-lc-rs. `ring` exposes no way to
load a fixed ECDH private key outside its deprecated `ring::test` module.

## Agent to supervisor socket (Part 3)

`chat-feed.mjs` destroys the agent's connection when a line passes
`LINE_MAX`, 64 KiB.
