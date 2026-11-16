# Dashboard-native chat, part 3: attachments

Status: approved by the owner, 2026-11-13.
Code references are to `master` at `e3385c4` (Part 1 merged).

## Intent

Screenshots and files are part of how the owner talks to a coding agent: an
error dialog, a mock-up, a log file. Today chat can send neither, and worse,
a prompt that carries an image (pasted in Terminal, or sent from the Claude
app) disappears from chat entirely, because the Claude tap drops any `user`
record whose content is a list of blocks. Part 3 makes attachments work in
both directions, for Claude Code, Codex and OpenCode.

## Program context

Part 3 of the program in `2026-11-12-chat-core-design.md`, shipped in one
pull request with Parts 2, 4 and 5 (owner decision, 2026-11-13). Sibling
specs: `2026-11-13-chat-prompts-menus-design.md`,
`2026-11-13-chat-push-reliability-design.md`,
`2026-11-13-chat-later-tier-design.md`.

## Decisions made by the owner

From the Part 3 request:

- Sending works from a picker, paste and drag-and-drop in the composer.
- How an image reaches Claude's TUI is worked out and verified on a real
  session.
- History shows the text and a thumbnail or chip, for all three tools.
- Anything rendered stays escaped; no path traversal; uploads never leave the
  host.

Size limits, storage location, cleanup and permissions are the author's
calls, listed below.

## Goals

1. Attach images and files in the composer by button, paste and drop. Each
   shows as a chip (a thumbnail for images) that can be removed before
   sending.
2. Send text plus attachments as one message to all three tools, including
   as a held message and with Send now.
3. Show attachments in history, whichever door the message came from:
   thumbnails for images, a chip with the name for other files.
4. Tap a thumbnail to see the image full size.

## Non-goals

- Thumbnails made on the server. The browser scales the original, loaded
  lazily (see "Author's calls" 3).
- Audio, video and folders.
- Images inside tool results (for example Claude reading a PNG with its Read
  tool). Tool rows keep showing text only.
- Previews of PDFs or other files, and downloading a file back from history.
- Attaching something already in history to a new message.

## Anti-goals

- A client-supplied path reaching the file system, anywhere.
- Rendering a name, MIME type or file content as HTML.
- An image prompt that breaks the chat stream for a tool (see the OpenCode
  limits under "How it works").
- Uploads readable by other users on the host.

## User-visible behaviour

### Composer

- A paperclip button opens the file picker (several files allowed). Pasting
  an image or file from the clipboard and dropping files on the chat pane
  attach them too.
- Each attachment shows as a chip above the text box: a small thumbnail for
  images, an icon with name and size for other files, each with a remove
  control. Upload starts at once and the chip shows progress. Send waits for
  uploads to finish; a failed upload shows on its chip with Retry.
- Limits: 20 MB per file, 10 attachments per message. Going over shows a
  message on the chip and does not upload.
- A message may be attachments only (empty text is allowed when at least one
  attachment is present).
- Held messages keep their attachments. Edit puts text and attachments back
  in the composer.

### History

- A prompt bubble shows its images as thumbnails above the text (up to three
  per row), then file chips, then the text.
- Tapping a thumbnail opens it full screen; back or Escape closes it.
- Messages that carried an image from Terminal or the Claude app now appear,
  with their thumbnail.

## How it works

### Upload

- **Uploads travel on the chat WebSocket,** not over a separate HTTP route:
  - the client sends `{"t":"upload","ref":<client id>,"name":…,"size":…}`;
  - then the bytes, as binary frames of at most 1 MiB;
  - the agent answers `{"t":"uploaded","ref":…,"id":…,"name":…,"size":…,"kind":…,"mime":…}`,
    or `{"t":"uploaded","ref":…,"error":…}`.

  Every client already reaches that socket, the desktop and Android apps
  included, and it already passes the agent's origin check. An HTTP upload
  from the apps would be a cross-origin request, which the agent
  deliberately refuses: it sends no CORS headers, and a test pins that.
  Progress is the share of frames sent.
- The agent refuses a declared size over 20 MB, counts the bytes as they
  arrive, and aborts and deletes the file if they exceed the declared size.
- The desktop app sets `dragDropEnabled: false` on its window
  (`tauri.conf.json`), so a dropped file reaches the page instead of
  Tauri's own drop handler.
- The type is sniffed from the first bytes: PNG, JPEG, GIF and WebP are
  images; everything else is a file. The client's MIME type is ignored.
- The file is stored at
  `$XDG_CACHE_HOME/cdash/uploads/<session>/<id>/<name>` (default
  `~/.cache/cdash/uploads/…`):
  - `<id>` is 16 random hex characters;
  - `<name>` is the base name of the upload frame's `name` with every character outside
    `A–Z a–z 0–9 . _ -` and space replaced by `_`, at most 100 bytes, never
    starting with `.`, `file` if nothing is left. Images get the extension of
    their sniffed type.
  - Folders are created 0700 and files 0600, set explicitly rather than left
    to the umask.
- The hub records `id → (path, kind, name, size)`. The response is
  `{id, name, size, kind, mime}`.
- Cleanup: when the hub sees the session end it deletes
  `uploads/<session>`. At agent start and once a day, upload folders older
  than 7 days are deleted.

### Send

- `ClientFrame::Send` gains `files: [id]` (optional, default empty). The hub
  resolves ids through its record; an unknown id refuses the message with
  "attachment expired". No path ever comes from the client.
- `Engine::send` receives the resolved attachments.
- **Claude Code:** see "Claude: how an image reaches the TUI" below. Files
  that are not images are added to the end of the text as a line
  `Attached: <absolute path>` each, and Claude reads them with its own tools.
- **Codex and OpenCode get paths, never bytes,** from the agent. The chat
  socket to their supervisors drops any line over 64 KiB (`LINE_MAX` in
  `chat-feed.mjs`), so the agent sends each attachment's absolute path, and
  the supervisor reads the file itself.
- **Codex:** `turn/start` (or `turn/steer`) input gets the text, then one
  `image {url: "data:<mime>;base64,…"}` per image, the way Codex's own
  terminal sends them. Codex's app server takes at most 64 MiB per message,
  and its terminal budgets 32 MiB of images, so chat refuses a Codex message
  whose images exceed 24 MB in total ("too many images for one Codex
  message"). Files that are not images go in the text as for Claude.
- **OpenCode:** the prompt's `files` gets `{uri: "file://<absolute path>",
  name}` for images and PDFs only. OpenCode reads them at admission (20 MiB
  limit per file, images normalised by OpenCode). Other files go in the text
  as for Claude. OpenCode 2.2.6 pastes a text file's whole content into the
  prompt, which can overflow the context, and silently drops other types,
  such as `.zip`.

### Claude: how an image reaches the TUI

Verified on Claude Code 2.1.323 on 2026-11-13, on a live TUI driven the way
the dashboard drives it:

- **Paste a line holding only the image's absolute path,** as a bracketed
  paste, and Claude attaches the image. The input box shows `[Image #N]`
  within 32 ms, and the raw path is never drawn. `N` counts up across the
  session.
- A path followed by text on the same line stays plain text.
- **An Enter sent before the chip is drawn is dropped,** and nothing is
  submitted.
- The model sees the image (asked the colour of a red square, it answered
  "Red.").
- The record:
  - `message.content` is
    `[{type:"text", text:"[Image #N] <text>"}, {type:"image", source:{type:"base64", media_type, data}}]`,
    text first, with `imagePasteIds: [N]`;
  - it is followed by an `isMeta` record `[Image: source: <path>]`, which
    the tap already skips.

The engine, for a message with images:

1. reads the input line with `capture-pane` and notes the `[Image #…]` chips
   already in it;
2. for each image, pastes its path alone, then polls every 50 ms, for up to
   5 s, until exactly one new chip appears;
3. pastes a space and the text, waits Part 1's 400 ms, and presses Enter.

If a chip does not appear within 5 s, the engine clears the input box
(Part 1's `clear_input`) and refuses the message with "image did not
attach", so a half-built prompt is never sent. The tap removes the
`[Image #N]` tokens from the text, so the prompt both shows and matches the
pending message as the text chat sent.

### Showing attachments

**Media cache.** Images seen in history are written once to
`$XDG_CACHE_HOME/cdash/media/<sha256>.<ext>` (folder 0700, files 0600), named
by the SHA-256 of their bytes. Writing skips an existing file and refreshes
its modification time. Files untouched for 7 days are deleted by the same
daily sweep. The cache is shared by all sessions, so one screenshot sent
twice is stored once. The agent passes the media folder to each supervisor
at launch, as `CDASH_MEDIA_DIR` in the pane's environment (`tmux new-session
-e`, as Part 2 passes its hook token), and the supervisors write there. They
never work the folder out themselves: a pane otherwise gets the tmux
server's environment, not the agent's. A supervisor rewrites messages before
any agent connects, so the folder cannot wait for the chat socket's hello.

**File lines.** A prompt's trailing `Attached: <absolute path>` lines whose
path contains `/cdash/uploads/` are removed from its text. Each becomes a
`{kind:"file", name}` entry, where `name` is the path's base name. The Claude
tap and both supervisors apply this rule. Without it, the recorded text never
equals the text chat sent. The hub matches a sent message by exact text, so
the message would never be confirmed, and it would show twice, once with a
cache path. Part 5's `raw` text keeps these lines.

**Event shape.** A prompt event gains
`files: [{kind:"image", ref, mime} | {kind:"file", name, size?}]`. `ref` is the
SHA-256 in lowercase hex. Text keeps no image placeholders.

**Route.** `GET /api/chat/media/<ref>`, behind the existing auth:
- `ref` must match `^[0-9a-f]{64}$`, otherwise 400;
- the file is looked up under the media folder only, with one of the four
  image extensions;
- the response sets the image MIME type from the extension,
  `X-Content-Type-Options: nosniff`,
  `Content-Security-Policy: default-src 'none'` and
  `Cache-Control: private, max-age=31536000, immutable`.

**Per tool.**

- **Claude Code.** The tap stays pure: a `user` record whose content is a
  list without `tool_result` blocks becomes a prompt event with the text
  blocks joined (`[Image #N]` tokens removed) and the image blocks passed
  through as `{mime, data}`. The
  engine (`claude.rs`) writes each image to the media cache and replaces it
  with its `ref` before the event reaches the hub. This replaces today's
  rule 6 path that drops such records.
- **Codex.** The supervisor (`codex-remote.mjs`) rewrites user messages before
  they reach the chat socket:
  - `image {url: "data:…"}`: decode, write to the media cache, emit `ref`.
    A data URL from the native `--remote` TUI can be tens of MB; it never
    reaches the agent socket.
  - `localImage {path}`: read the file, write it to the media cache, emit
    `ref`; if the file is gone, emit a file chip with its base name.
  - Delivery to the Remote Control worker is unchanged.
- **OpenCode.** The supervisor does the same for a user row's
  `files[{data, mime, name}]`: images go to the media cache, other files
  become chips with name and decoded size. Two limits in
  `opencode-api.mjs` must not be hit by an image prompt:
  - the event stream is dropped on a frame over 1 MiB (line 63), and
    `session.inbox.enqueued` always carries the full base64;
  - REST bodies over 8 MiB fail (line 31), and a history page with one large
    image can exceed that.
  How to avoid them is left to the builder (raise the caps, filter event
  types on the server with `?types=`, or read rows one at a time); the
  requirement is that a prompt with up to 10 attachments of 20 MiB each
  neither drops the stream nor fails a history read. That includes PDFs,
  which OpenCode stores unshrunk (about 26.7 MiB of base64 each).

## Security

- Uploads use the chat socket, which already passes auth and the origin
  check. The media route sits behind the existing auth.
- The upload size is checked against the declared size before any byte is
  written, and again as bytes arrive.
- Stored paths are built from a random id and a sanitised base name under a
  fixed root. Send takes upload ids, never paths. The media route takes a
  64-character hex string, never a path.
- In the apps, image URLs are prefixed with `agent_base`. A plain `<img>`
  load needs no CORS headers, so the agent's no-CORS rule stays.
- Media is served only as one of four image types, with `nosniff` and a
  CSP that forbids everything; names and sizes are rendered as text.
- Folders 0700, files 0600.
- Nothing is sent anywhere by the dashboard. The coding tool on the same host
  reads the file and sends it to its model provider, which is the purpose of
  attaching it.

## Author's calls

1. **Location `$XDG_CACHE_HOME/cdash/`,** not `~/.claude` (Claude Code treats
   its own folder specially) and not the session's folder (it would show up
   in `git status`).
2. **20 MB per file and 10 per message.** OpenCode's own limit is 20 MiB per
   file; Claude and Codex shrink images themselves.
3. **No server thumbnails.** Making them needs an image library; the browser
   scales the original and loads it only when scrolled into view.
   `ponytail:` if phones on mobile data suffer, add a thumbnail route.
4. **Files that are not images go as paths in the text** for Claude and Codex.
   Both read files with their own tools; neither has a better input for
   arbitrary files.
5. **Upload when attached, not when sent,** so Send is instant once chips
   are ready.
6. **One media cache keyed by content hash,** shared and swept by age, so
   replaying a transcript re-creates what the sweep removed.
7. **Codex gets data URLs, as from its own terminal,** not `localImage`
   paths. A path dies when the upload is cleaned up, so thumbnails would
   break after a resume, and Codex's terminal refuses its own rewind on a
   prompt with local images. The supervisor strips the data before it
   reaches the agent socket.
8. **Uploads over the chat socket,** in 1 MiB binary frames, instead of an
   HTTP route. The apps cannot make a cross-origin upload, and opening CORS
   for one route, or adding a binary command to the app crate, is more code
   and more to secure.
9. **OpenCode gets only images and PDFs as file parts;** other files go as
   paths, as for Claude and Codex (see Send).
10. **Rewind puts back a prompt's text only** (Part 5). When the prompt had
    attachments, the composer says "This prompt had N attachments; attach
    them again". Turning history images back into uploads would contradict
    the non-goal on reusing history attachments.

## Left to the builder

- Composer and viewer layout details.
- How the OpenCode stream and REST limits are avoided (see above).
- Whether the hub's upload record survives an agent restart (it need not:
  an unknown id is refused with "attachment expired").

## Acceptance checks

- Unit tests:
  - the name sanitiser, including `../x`, `..`, `.hidden`, NUL bytes and
    names of 300 bytes;
  - type sniffing for the four image types and a text file named `.png`;
  - the 20 MB limit: a declared size of 20 MB + 1 byte is refused, and a
    stream that sends more than it declared is aborted and its file
    deleted;
  - unknown upload ids refused;
  - the media route refusing `..`, `%2e%2e`, uppercase hex, 63 and 65
    characters, and serving `nosniff` and the CSP;
  - the Claude tap turning an `[image, text]` record into a prompt with
    `files`, and the fixture test updated;
  - a prompt with trailing `Attached:` lines becoming its text plus file
    entries, and a message with a PDF confirmed by the hub, not
    `unconfirmed`.
- Real sessions on this host:
  - Claude Code: image plus text from chat; the model describes the image;
    chat shows the thumbnail; Terminal shows the prompt with its image. An
    image pasted in Terminal shows in chat.
  - Codex and OpenCode: the same, and an image typed in their native
    terminals shows in chat.
  - A 15 MB PNG and a 15 MB PDF to OpenCode do not drop its event stream.

## Evidence

Collected on 2026-11-13 for Parts 2 to 5, in `2026-11-13-chat-evidence/`:

- `claude-2.1.323-live-spike.md`: live tests on Claude Code 2.1.323 in a
  private tmux server. Covers the permission hook (questions, plans,
  permissions, answering in the TUI first), image paste, status-file
  timing, the rewind dialog, the headless command list, side questions and
  background-task records.
- `claude-2.1.323-static-notes.md`: what the docs and the shipped CLI say
  about hooks, settings, commands, images, rewind and usage.
- `codex-opencode-source-notes.md`: Codex 0.167 app-server and OpenCode 2.2.6
  source and schema research (approvals, questions, settings, images,
  rewind, usage, diffs, side chat, background work), with file and line
  references.
- `review-checks.md`: checks run during the review (headless `initialize`
  with hooks off, image paths with spaces, branch cuts on real transcripts,
  clean filters, context sizes, API-error turns, build facts).
