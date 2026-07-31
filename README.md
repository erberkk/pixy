# Pixy — a desktop mascot that watches your work

An always-on-top mascot overlay for Windows that bridges Claude Code hooks, a
local LLM, voice, Gmail, Calendar and GitHub into one place you can glance at.
It answers Claude Code's permission prompts, tells you when a build breaks or an
issue lands on you, summarises your unread mail, and gets out of the way when it
has nothing to say.

Built with [Tauri 2](https://tauri.app) — Rust backend, vanilla-JS frontend, no
bundler.

<!--
  SCREENSHOT GOES HERE. This is a visual application and no prose substitutes
  for one — a reader deciding whether to try this will decide from the picture.
  Suggested: the mascot overlay with a permission card open, plus one of the
  workspace chat. Put files in docs/ and reference them as:
      ![The mascot answering a Claude Code permission prompt](docs/mascot.png)
-->

> **Status:** used daily by its author on one machine. Windows-only today (see
> [Platform support](#platform-support)). Interfaces are not stable.

## What it does

**Claude Code integration** — the reason it exists. Claude Code hooks POST to a
small local HTTP server, and the mascot renders the permission request as a card
you approve or deny; the answer goes back as the hook's own response. Tool use,
thinking and waiting all show as mascot moods. `AskUserQuestion` renders as
tappable chips. See [SETUP.md](SETUP.md) for the hook configuration.

**Chat with a local model** — streaming replies, code blocks with syntax
highlighting, document and image attachments, editable messages, per-message
retry, and several named model profiles you can switch between mid-conversation.
Points at anything OpenAI-compatible.

**Recall across conversations** — full-text search (SQLite FTS5) over every past
chat, plus a retrieval pass that pulls relevant earlier exchanges into a new
question automatically. Question and answer are indexed as one unit, because a
question on its own loses what was decided.

**Voice** — an on-device wake word ("hey pixy") running in the webview via ONNX,
then transcription and speech through whatever local STT/TTS servers you point it
at. Nothing is sent anywhere until the wake word fires.

**Pictures** — text-to-image through a local stable-diffusion.cpp server, started
on demand when you ask for a picture and shut down again when you stop, because
it and the chat model together want more VRAM than a 16 GB card has.

**Mail and calendar** — unread Gmail announced as notices with a one-line summary
written by the local model, a once-a-day morning brief card, and a nudge before a
meeting starts. Across all connected accounts.

**GitHub** — merged PRs, failing CI, and issues assigned to you, as notices.

**Notes and memory** — a markdown notes pane with wiki-links, and a browser over
Claude Code's own memory files.

## Requirements

- **Windows 10/11.** WebView2 is required and ships with Windows 11.
- **Rust** (stable) and the [Tauri 2 prerequisites](https://tauri.app/start/prerequisites/).
- **Node** only for `npm test` and the Tauri CLI. There is no frontend build.

Everything else is optional and discovered at runtime — the app starts fine with
no model, no accounts and no tokens, and each feature stays quiet until it is
configured.

## Running it

```sh
git clone <this repo> && cd Widget
npm install                 # just @tauri-apps/cli
npm run tauri dev
```

Then open Settings from the tray icon and configure what you want. Nothing needs
to be set up in a particular order.

```sh
npm test                    # frontend tests (Node's built-in runner, no deps)
cd src-tauri && cargo test  # backend tests
```

> **A note on iterating:** the frontend is compiled into the binary, so editing a
> `.js` or `.css` file needs a rebuild to take effect. That surprises everyone
> once.

## Security model

This app reads web pages a language model chose, runs a local HTTP server, and
holds OAuth refresh tokens. That combination deserves a straight answer rather
than a reassurance, so it has its own document: **[SECURITY.md](SECURITY.md)**.

The short version:

- Everything is **local by default**. Chat history, notes, memory and the search
  index never leave the machine. Recall results are not sent to a non-local
  endpoint unless you explicitly turn that on.
- You supply **your own OAuth client** and your own tokens. There is no shared
  application and no secret compiled into the binary — a desktop binary cannot
  keep one.
- Secrets are stored **in plaintext** in `%APPDATA%\com.widget.mascot\config.json`.
  That is a deliberate trade-off for a local-first tool, and SECURITY.md explains
  it rather than hiding it.
- Content fetched from the web is treated as **hostile input**: the URL is checked
  against a public-address allow-list and then pinned so DNS cannot change its
  mind between the check and the connection, the download is capped, and the
  renderer refuses to emit a link whose scheme it does not recognise.

## Platform support

**Windows only, today.** Not by design — the platform-specific parts are
concentrated rather than spread out, and none of them are architectural:

| What | Where | Porting cost |
|---|---|---|
| Audio sessions, mic/speaker mute | `src-tauri/src/system/media.rs` | Rewrite against the platform mixer |
| Process lookup by listening port | `src-tauri/src/ai/process.rs` | `netstat` parsing → `lsof` |
| Idle time and power status | `src-tauri/src/system/power.rs` | Platform API |
| Spotify control | `src-tauri/src/system/media.rs` | Platform API |
| Click-through overlay behaviour | `src-tauri/src/ui/clickthrough.rs` | Per-platform window flags |

Roughly half a day of `#[cfg]` work plus a machine to test on. Everything above
those files — chat, recall, voice, mail, calendar, GitHub, the hook server — is
platform-neutral already.

## How it is put together

```
src/                       Frontend — one directory per window, native ES modules
├── mascot/                The overlay: sprite, notices, voice, quick menu
├── workspace/             Main window: chat, notes, memory
├── settings/              Settings window
├── shared/                Cross-window: markdown, tauri bindings, formatting
└── vendor/                highlight.js, ONNX Runtime — see THIRD-PARTY-NOTICES.md

src-tauri/src/             Backend — one module group per feature area
├── ai/                    chat, llm, images, recall, speech, voice, tools, process
├── google/                OAuth, Gmail and Calendar clients — mechanism only
├── mail/  calendar/       …and the policy over them: what is worth announcing
├── github/                API client and its watchers
├── web/                   search, fetch, extraction — the untrusted-input side
├── agent/                 the Claude Code hook HTTP server
├── content/               notes, memory, document parsing
├── system/  ui/           OS integration, window and tray behaviour
├── config.rs              the one settings file
└── tunables.rs            per-machine values, so nothing is hardcoded twice
```

Around 17k lines of Rust and 17k of frontend (plus ~1.7k vendored), 88 Tauri
commands, 204 Rust tests and 10 frontend tests.

Two conventions worth knowing before reading the code:

- **`google/` is a mechanism layer; `mail/` and `calendar/` are policy.** One
  OAuth grant is shared by two unrelated features, so the transport was promoted
  to its own group. `github/` keeps its client and watchers together because
  nothing outside GitHub uses them. The rule: a group owns both client and
  consumers *unless* the client is shared across feature domains.
- **Comments explain decisions, not mechanics.** Where one records a measurement
  ("`IsHungAppWindow()` returned true within two seconds") that measurement is
  why the code is shaped the way it is. If you change such code, the comment is
  the argument you have to answer.

The decisions too large to fit in a comment are in
[docs/DESIGN-NOTES.md](docs/DESIGN-NOTES.md) — why the hook answers Claude Code's
prompt directly, why the window never resizes at runtime, and a list of bugs whose
symptoms were nothing like their causes.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) — the build, the no-bundler asset step
that catches everyone out once, and what the tests expect.

## License

MIT — see [LICENSE](LICENSE). Third-party components redistributed in this
repository are listed in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
