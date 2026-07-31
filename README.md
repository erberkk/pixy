# Pixy

A little robot that sits on your desktop and keeps an eye on your work.

Pixy answers Claude Code's permission prompts, chats with a language model
running on your own machine, remembers what you talked about last week, reads
your unread mail out loud if you want, and tells you when the build breaks. Then
it goes quiet until it has something to say.

**Everything runs on your machine.** No account, no telemetry, no server of ours
— there isn't one. Your chats, notes and search index never leave the disk they
were written to.

<!--
  SCREENSHOT GOES HERE. This is a visual application and no prose substitutes
  for one — a reader deciding whether to try this will decide from the picture.
  Suggested: the mascot with a permission card open, plus one of the workspace
  chat. Put files in docs/ and reference them as:
      ![Pixy answering a Claude Code permission prompt](docs/mascot.png)
-->

> **Status:** used daily by its author on one machine. Windows only for now.
> Interfaces are not stable yet.

---

## Using it

Pixy is a small pill that floats above your other windows. Everything starts
from there.

### The mascot

| Gesture | What happens |
|---|---|
| **Drag** | Move it anywhere on screen |
| **Single click** | Opens the music panel — play/pause, skip, what's playing |
| **Double click** | Opens a terminal |
| **Right click** | Opens the quick menu: **Workspace**, **Settings**, **Hide** |
| **Esc** | Closes the quick menu |

Clicks pass straight through to whatever is underneath unless the cursor is
actually on Pixy, so it can sit on top of your editor without getting in the way.

### When a card appears

Pixy expands into a card when something needs you, and shrinks back when it's
handled.

| Card | What you do |
|---|---|
| **Permission request** | **Approve** or **Deny** — the answer goes straight back to Claude Code. **×** dismisses it (counts as deny) |
| **A question from Claude** | Tap the option chips, then **Submit** |
| **Morning brief / digest** | Read it, click **×** when done |
| **Mail, meetings, GitHub** | Click through to open the relevant thing |

### The tray icon

Right-click it for everything the mascot can't reach:

**Show mascot** · **Hide mascot** · **Open Workspace** · **Open Settings** ·
**Open Terminal** · **Run GitHub Digest Now** · **Run Morning Brief Now** ·
**Quit** · **Quit and stop local servers**

### The workspace window

Three tabs: **Chat**, **Notes**, **Memory**.

| Shortcut | Where | What |
|---|---|---|
| `Ctrl+E` | Notes | Toggle markdown preview |
| `Ctrl+F` | Notes | Find & replace |
| `Ctrl+.` / `Esc` | Notes | Focus mode |

### Talking to it

Say **"hey pixy"** and it listens. Everything — the wake word, the
transcription, the reply, the voice — runs locally against servers you point it
at. Nothing is recorded or sent anywhere before the wake word fires.

---

## What it can do

### Claude Code
Claude Code's hooks talk to a small local server, so permission prompts show up
as a card you can answer from anywhere — no need to find the right terminal. The
real command, file path and diff are on the card, straight from the hook. Several
sessions can be waiting at once; they stack. Multiple-choice questions become
tappable chips. Pixy's mood follows along: thinking, working, waiting on you.

### Chat
Point it at any OpenAI-compatible server and chat with streaming replies, syntax
highlighting and several named model profiles you can switch between mid-thought.
Attach documents (`pdf` `docx` `pptx` `xlsx` `csv` `md` `txt` `json` `html`) or
images. Edit a message and send it again, or retry just one reply. The model can
search the web and read pages when it needs to.

### Memory
Full-text search over every past conversation, plus a retrieval pass that quietly
brings the relevant bit of an old chat into a new question. Ask "what did we
decide about caching?" and it finds the conversation that said *cache*. Add an
embedding server and it will also find the one that made the same point in
different words. Claude Code's own project notes can be searched alongside.

### Voice
An on-device wake word, then transcription and speech through whatever local
STT/TTS servers you like.

### Pictures
Text-to-image through a local stable-diffusion server, started when you ask for a
picture and shut down when you stop — because it and the chat model together want
more VRAM than most cards have.

### Mail and calendar
Unread Gmail announced with a one-line summary written by your local model, a
morning brief once a day, and a nudge before a meeting starts. Works across
several accounts. You bring your own OAuth client.

### GitHub
Merged pull requests, failing CI, issues assigned to you, and reviews someone is
waiting on — as notices, not another tab to check.

### Notes and memory graph
A markdown notes pane with wiki-links, pinning, trash, find & replace and a focus
mode. Notes are real files in a real folder, editable by anything. Alongside it, a
graph view of Claude Code's own memory files.

### Your machine
Music controls, per-app volume, speaker and microphone mute, and enough awareness
of whether you're on a call, on battery or away that Pixy can stay out of the way.

---

## Running it

```sh
git clone https://github.com/erberkk/pixy && cd pixy
npm install          # just the Tauri CLI
npm run tauri dev
```

Or grab an installer from [Releases](../../releases) — it's unsigned, so Windows
will warn you once.

**Requirements:** Windows 10/11 and [Rust](https://tauri.app/start/prerequisites/).
Node is only needed for the CLI and tests; there is no frontend build step.

Everything else is optional and found at runtime. Pixy starts fine with no model,
no accounts and no tokens — each feature simply stays quiet until you set it up.

Open **Settings** from the tray and fill in what you want, in any order.
**[SETUP.md](SETUP.md)** walks through each one, including the Claude Code hooks
you'll need to paste into `.claude/settings.json`.

```sh
npm test                    # 10 frontend tests, no dependencies
cd src-tauri && cargo test  # 210 backend tests
```

> **If you're changing the code:** the frontend is compiled into the binary, so
> editing a `.js` or `.css` file needs a rebuild before you'll see it. That
> surprises everyone once.

---

## Local by default

Pixy reads web pages a language model chose, runs a local HTTP server and holds
OAuth tokens. That deserves a straight answer rather than a reassurance, so it
has its own document: **[SECURITY.md](SECURITY.md)**.

- Chat history, notes, memory and the search index **never leave the machine**.
  Remembered history isn't sent to a hosted model unless you turn that on.
- You supply **your own OAuth client and tokens**. There is no shared application
  and no secret compiled into the binary — a desktop app can't keep one.
- Secrets are stored **in plaintext** in `%APPDATA%\com.widget.mascot\config.json`.
  A deliberate trade-off for a local-first tool, explained rather than hidden.
- Web content is treated as **hostile**: addresses are checked and then pinned so
  DNS can't change its mind, downloads are capped, and the renderer refuses to
  emit a link whose scheme it doesn't recognise.

---

## Platform support

**Windows only today** — not by design. The platform-specific parts sit in four
files: `system/media.rs` (audio sessions, mute, media keys), `ai/process.rs`
(finding a local server by the port it listens on), `system/power.rs` (idle and
battery) and `ui/clickthrough.rs`. Roughly half a day of `#[cfg]` work plus a
machine to test on. Everything above them is already platform-neutral.

## Under the hood

[Tauri 2](https://tauri.app) — Rust backend, vanilla-JS frontend, **no bundler**.
About 17k lines each side, 90 commands, 220 tests.

```
src/                  one directory per window, native ES modules
├── mascot/           the overlay: sprite, notices, voice, quick menu
├── workspace/        chat, notes, memory
├── settings/         settings window
├── shared/           markdown, Tauri bindings, the sprite renderer
└── vendor/           highlight.js, ONNX Runtime

src-tauri/src/        one module group per feature area
├── ai/               chat, models, images, recall, speech, voice, tools
├── google/           OAuth, Gmail, Calendar — mechanism only
├── mail/ calendar/   …and the policy over them: what's worth announcing
├── github/  web/     API clients, and the untrusted-input side
├── agent/            the Claude Code hook server
├── content/          notes, memory, document parsing
└── system/  ui/      OS integration, windows, tray
```

The decisions too large for a comment live in
**[docs/DESIGN-NOTES.md](docs/DESIGN-NOTES.md)** — why the hook answers Claude
Code directly, why the window never resizes, and a list of bugs whose symptoms
were nothing like their causes.

## Contributing

See **[CONTRIBUTING.md](CONTRIBUTING.md)** — the build, the no-bundler asset step
that catches everyone out once, and what the tests expect.

## License

MIT — see [LICENSE](LICENSE). Third-party components redistributed here are
listed in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
