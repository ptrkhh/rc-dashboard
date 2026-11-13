# Claude Code 2.1.323 facts (research agent, 2026-11-13; binary + docs, no session)

Extracts: scratchpad/claude-re/.

1. PermissionRequest hook
- Input: session_id, transcript_path, cwd, permission_mode, prompt_id, effort.level, hook_event_name, tool_name, tool_input, permission_suggestions[], mcp_server (MCP tools). NO tool_use_id.
- Output: hookSpecificOutput:{hookEventName:"PermissionRequest", decision:{behavior:"allow", updatedInput?, updatedPermissions?} | {behavior:"deny", message?, interrupt?}}. updatedPermissions entries: addRules/replaceRules/removeRules (rules:[{toolName,ruleContent?}], behavior), setMode (mode), addDirectories/removeDirectories; destination session|localSettings|projectSettings|userSettings|cliArg. interrupt:true aborts turn. Exit code 2 ignored on this event.
- Fires for AskUserQuestion and ExitPlanMode. Allow WITHOUT updatedInput ignored for requiresUserInteraction tools; allow WITH updatedInput accepted for exactly ExitPlanMode + AskUserQuestion. AskUserQuestion answer: echo questions + answers:{"<question text>":"Label"}; multi-select: array or ", "-joined; annotations:{q:{notes,preview}} optional. ExitPlanMode tool_input has plan + planFilePath; sending plan back overwrites plan file. If mode still plan when tool runs, reverts to pre-plan mode (bypass for dashboard) unless hook sends setMode.
- Timeout default 600 s (command/http/mcp_tool), prompt 30 s, agent 60 s. `timeout` seconds, no max. Agent hooks unsupported on this event.

2. Notification types: permission_prompt (after ~6 s no typing), idle_prompt (~60 s after reply), auth_success, auth_storage_failure, elicitation_dialog, elicitation_url_dialog, elicitation_complete, elicitation_response, agent_needs_input, agent_completed, worker_permission_prompt, push_notification, computer_use_enter/exit, quota_auto_resume_*, model_refusal_fallback, plugin_notification. Events exist: Stop, StopFailure, SubagentStart, SubagentStop, Elicitation, ElicitationResult, PreModelSwitch, PostModelSwitch, PermissionDenied.

3. HTTP hooks {type:"http", url, headers?, allowedEnvVars?, timeout?, onFailure?:"continue"|"block", if?, statusMessage?}; POST JSON; 2xx JSON parsed like command output; empty = no decision; non-2xx/conn fail = non-blocking (dialog stays) unless onFailure block. Loopback allowed ("Loopback (127.0.0.1, ::1) is allowed for local dev"); private/link-local refused. allowedHttpHookUrls / httpHookAllowedEnvVars allowlists may restrict. `--settings` can carry hooks; hooks MERGE across levels; identical handler runs once.

4. TUI typed settings:
- `/model <alias>` switches, no picker, BUT saves default to ~/.claude/settings.json ("Set model to X and saved as your default for new sessions"); mid-conversation with warm cache shows "Switch model?" confirmation (dialog!). Only picker `s` key = session-only.
- `/effort <level>` exists: low|medium|high|xhigh|max (session-only)|auto, ultracode on|off; saved as default; may show "Change effort level?" confirmation; no arg = slider. shift+up/down step effort.
- `/fast [on|off]`; meta+o toggles; meta+p opens model picker.
- Shift+Tab cycle: default → acceptEdits → plan → bypassPermissions (if avail) → auto (if avail) → default. With --dangerously-skip-permissions: bypass → auto → default → acceptEdits → plan → bypass. Shift+Tab in dialog acts on dialog (at plan approval: approves plan).
- Only `/plan` sets a mode by command ("Enabled plan mode"); `/plan <text>` enters plan + starts task. `/config permissionMode=…` = default for new sessions only.
- Recorded: `permission-mode` record re-appended with metadata (lags shift+tab); each `user` record has `permissionMode`; `assistant` has message.model, requestedModel, effort, perTurnEffort; `model` attachment identity.modelId; `/model` writes system local_command records. Session state file has NO model/mode.

5. Images: bracketed paste of path(s) ending .png/.jpg/.jpeg/.gif/.webp attaches as [Image #N]; content checked; BMP converted; shrunk to 2000×2000 px & 5 MB base64. Pasted line must be ONLY paths (text+path = plain text). Enter arriving while image read is dropped: wait for chip before Enter. Transcript user content: {type:"image", source:{type:"base64", media_type, data}} before text, plus imagePasteIds (unconfirmed locally). `@path` adds attachment record {type:"file", filename, content}; typing @ opens suggestion menu (Enter accepts path).

6. Rewind (Esc Esc, /rewind): cuts in-memory conversation, puts original prompt back in input box; nothing written until next prompt; next prompt's chain branches from last kept message uuid. Exit before next prompt restores rewound messages. last-prompt.leafUuid picks resume branch; SDK/RC rewind writes last-prompt {leafUuid, explicit:true, rewound:true, cutUuid}. Code checkpoints: file-history-snapshot records, backups ~/.claude/file-history/<session>/; hidden flag --rewind-files <user-msg-uuid> (requires --resume).

7. Commands: -p stream-json initialize → commands:[{name, description, argumentHint, aliases?, builtin?}] + models with effort levels; only headless-capable (prompt-type or local with supportsNonInteractive): custom commands + skills appear; TUI-only built-ins (/plan, /permissions, /rewind, /resume, /btw) absent. Open picker/dialog without args: /model, /effort, /config, /permissions, /resume, /rewind, /usage, /status, /tasks, /mcp, /plugin, /hooks, /agents, /memory, /export.

8. /btw overlay, no tools, mid-turn OK, never in transcript; unavailable over RC. /usage dialog (cost, plan limits, stats); /cost /stats aliases. /context colored grid. /diff fullscreen side panel.

9. assistant message.usage: input, output, cache_read, cache_creation, thinking tokens, speed; same message.id lines repeat usage (dedupe). Context ≈ input+cache_read+cache_creation of last main-chain reply. compact_boundary.compactMetadata.preTokens. Rate limit: synthetic assistant isApiErrorMessage:true, error:"rate_limit", apiErrorStatus:429. 5h/7d % only in statusLine JSON (rate_limits, context_window).
