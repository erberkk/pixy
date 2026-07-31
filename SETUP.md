# Using Pixy

A guide to what Pixy can do and how to make it do it. Nothing here is required
in order — the app runs with nothing configured, and each capability turns
itself on when you set it up.

For *why* things work the way they do — the measurements, the bugs, the rejected
alternatives — see [docs/DESIGN-NOTES.md](docs/DESIGN-NOTES.md).

**Contents**

1. [First launch](#1-first-launch)
2. [Chat with a local model](#2-chat-with-a-local-model) — everything else builds on this
3. [Claude Code permission cards](#3-claude-code-permission-cards)
4. [Voice](#4-voice)
5. [Pictures](#5-pictures)
6. [Gmail and Calendar](#6-gmail-and-calendar)
7. [GitHub](#7-github)
8. [Day to day](#8-day-to-day)
9. [When something doesn't work](#9-when-something-doesnt-work)

---

## 1. First launch

```sh
npm install
npm run tauri dev
```

A small pill appears at the top-centre of your screen. That is the mascot. It
stays on top of other windows, ignores your clicks unless you are pointing at
it, and shrinks back to a dot when it has nothing to say.

**Everything is reached from the tray icon** (bottom-right, near the clock):

| Menu item | What it does |
|---|---|
| Show / Hide mascot | Gets the pill out of the way without quitting |
| Open Workspace | The main window: chat, notes, memory |
| Open Settings | Where all configuration lives |
| Open Terminal | A system terminal, for convenience |
| Run GitHub Digest Now | Fires the daily digest immediately — useful for testing |
| Run Morning Brief Now | Same, for the mail brief |
| Quit and stop local servers | Quits **and** shuts down the model servers it started |

That last one matters: Pixy can start a local model server for you, and a plain
quit would leave several gigabytes of model resident. This item stops them.

> **If you are developing:** the frontend is compiled into the binary. Editing a
> `.js` or `.css` file does nothing until you rebuild.

---

## 2. Chat with a local model

This is the foundation — mail summaries, voice replies and the web-search tool
all use whatever model you configure here.

**Settings → LLM.** Add a profile:

| Field | What to put |
|---|---|
| Name | Anything — it labels the picker in the chat window |
| Base URL | e.g. `http://127.0.0.1:11434/v1` for Ollama, `http://127.0.0.1:8080/v1` for llama.cpp |
| Model | The model name the server reports |
| API key | Only if your server wants one. Leave blank for local servers |

Anything OpenAI-compatible works. Press **Test connection** — it tells you what
it found rather than just going green.

You can add several profiles and switch between them **mid-conversation** from
the picker at the top of the chat. Useful when one model is good at code and
another is fast.

**Autostart.** If you give it a start command, Pixy launches the server when it
starts and can stop it again on quit. Leave it empty if you run the server
yourself.

### What you can do in chat

- **Attach documents** — PDF, DOCX, TXT, code files. Their text is extracted and
  goes into the conversation.
- **Attach or paste images.** Ctrl+V a screenshot straight into the composer. If
  the model can't see images, Pixy says so instead of silently ignoring it. Click
  a sent image to open it full size.
- **Edit any message you sent** and re-run from there.
- **Retry the last answer** without retyping.
- **Stop a reply mid-stream** with the same button that sent it.
- **Ask it to search the web** — it has a keyless search-and-read tool, and shows
  the sources it used under the answer.
- **`/image <prompt>`** — see [Pictures](#5-pictures).

### Recall across conversations

Pixy indexes every past chat and quietly pulls in relevant earlier exchanges
when they help. When it does, the message says so — "Added from your earlier
conversations" — so an answer never leans on context you can't see.

Search history yourself from the workspace search box. **Settings → Memory**
controls how eagerly recall fires, and whether recalled history may be sent to a
non-local model (**off by default**).

---

## 3. Claude Code permission cards

The original reason this app exists. Claude Code asks permission before running a
tool; with hooks wired up, that question appears as a card on your mascot and
your answer goes straight back to Claude Code. You never have to find the
terminal window.

`AskUserQuestion` shows up as tappable option chips instead of Approve/Deny.

### Wiring it up

Add this to `~/.claude/settings.json` (global) or a project's
`.claude/settings.json`. **Merge it into any existing `hooks` key** rather than
replacing the file.

```json
{
  "hooks": {
    "PermissionRequest": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "IN=$(cat); echo \"$IN\" | curl -s -X POST \"http://127.0.0.1:47623/decide\" -H \"Content-Type: application/json\" -d @-",
            "timeout": 3600
          }
        ]
      }
    ],
    "PreToolUse": [
      { "matcher": "", "hooks": [ { "type": "command", "command": "curl -s -X POST http://127.0.0.1:47623/event -H \"Content-Type: application/json\" -d \"{\\\"state\\\":\\\"idle\\\"}\"", "timeout": 5 } ] }
    ],
    "PermissionDenied": [
      { "matcher": "", "hooks": [ { "type": "command", "command": "curl -s -X POST http://127.0.0.1:47623/event -H \"Content-Type: application/json\" -d \"{\\\"state\\\":\\\"idle\\\"}\"", "timeout": 5 } ] }
    ],
    "Notification": [
      { "matcher": "", "hooks": [ { "type": "command", "command": "curl -s -X POST http://127.0.0.1:47623/event -H \"Content-Type: application/json\" -d \"{\\\"state\\\":\\\"waiting_input\\\"}\"", "timeout": 5 } ] }
    ],
    "Stop": [
      { "matcher": "", "hooks": [ { "type": "command", "command": "curl -s -X POST http://127.0.0.1:47623/event -H \"Content-Type: application/json\" -d \"{\\\"state\\\":\\\"turn_done\\\"}\"", "timeout": 5 } ] }
    ],
    "UserPromptSubmit": [
      { "matcher": "", "hooks": [ { "type": "command", "command": "curl -s -X POST http://127.0.0.1:47623/event -H \"Content-Type: application/json\" -d \"{\\\"state\\\":\\\"thinking\\\"}\"", "timeout": 5 } ] }
    ]
  }
}
```

**The `timeout: 3600` on `PermissionRequest` is not a typo.** That connection
stays open while you decide, so the timeout has to cover a human, not a network.
If it does expire before you click, Claude Code just shows its own prompt as
usual — annoying, not dangerous.

### What you will see

| Hook | On the mascot |
|---|---|
| `PermissionRequest` | A pinned card with the real tool name, command and diff, plus Approve/Deny |
| `Notification` | Expands, different text, a soft chime — Claude is waiting on you |
| `PreToolUse` | Collapses back to the idle pill; also drives the "coding" mood |
| `PermissionDenied` | Same as `PreToolUse`. Only fires for auto-mode denials, not for you clicking Deny |
| `Stop` | A brief flash and a quiet tick, then settles |
| `UserPromptSubmit` | Ambient only — shows "thinking" briefly. Never opens a card |

If a card sits unanswered longer than the threshold in **Settings → Advanced**,
the mascot moves to a "forgotten" pose so you can tell at a glance that something
has been waiting a while.

### Testing without Claude Code

From a terminal you opened yourself:

```bash
curl -X POST http://127.0.0.1:47623/event -d '{"state":"waiting_input"}'
curl -X POST http://127.0.0.1:47623/event -d '{"state":"turn_done"}'
curl -X POST http://127.0.0.1:47623/event -d '{"state":"idle"}'

# A permission card. This will SIT THERE until you click Approve or Deny —
# that is correct, not a hang. The decision JSON prints when you answer.
curl -X POST http://127.0.0.1:47623/decide \
  -d '{"tool_name":"Bash","tool_input":{"command":"ls -la"}}'

# Option chips instead of Approve/Deny:
curl -X POST http://127.0.0.1:47623/decide \
  -d '{"tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Which approach?","header":"Approach","multiSelect":false,"options":[{"label":"Option A"},{"label":"Option B"}]}]}}'
```

Two things that will confuse you if you don't know them:

- **Run these outside Claude Code.** Inside a session, your `curl` *is itself* a
  tool call, so it fires the real hooks on top of whatever you sent.
- **This endpoint refuses browser requests.** Anything carrying an `Origin` or
  `Sec-Fetch-*` header gets `403`, because otherwise any web page you had open
  could put a fake permission card on your screen. `curl` and Claude Code send
  neither, so they are unaffected — but a browser, a REST client, or a fetch from
  devtools will be rejected. Use `curl`.

---

## 4. Voice

Say **"hey pixy"** and it starts listening. No key press, and nothing leaves the
machine until the wake word actually fires — the detection runs in the app.

**Settings → Voice** to turn it on, plus:

- **Settings → STT** — a transcription server. whisper.cpp's server works, as does
  anything OpenAI-compatible; Pixy tries both routes and tells you if it finds
  neither.
- **Settings → TTS** — a speech server for replies out loud. Kokoro-FastAPI or
  anything OpenAI-compatible.

The wake word costs an 11 MB download the first time you enable it, and nothing
at all if you never do.

**Settings → Advanced** has the knobs that depend on your room rather than on the
app: detection threshold, how much silence ends a sentence, how long to keep
listening. If it triggers on the TV, raise the threshold there.

---

## 5. Pictures

Type **`/image a cat wearing a hat`** in the chat.

**Settings → Images** points at a local [stable-diffusion.cpp](https://github.com/leejet/stable-diffusion.cpp)
server and sets size, steps and seed. Prompts only ever go to a local server —
that is enforced in code, not a setting you can get wrong.

**The server starts when you ask for a picture and stops when you stop asking.**
It is deliberately not autostarted: the picture model and the chat model together
want more VRAM than a 16 GB card has, and Windows answers that by paging to system
memory rather than refusing — so both *look* loaded while everything crawls. The
first picture after a pause therefore takes longer (the model has to load); the
next few are quick. The idle timeout is in **Settings → Advanced**.

Generated images are saved to disk and clicking one opens it full size.

---

## 6. Gmail and Calendar

What you get: unread mail announced as a notice with a one-line summary written
by *your* local model, a once-a-day morning brief card, and a nudge before a
meeting starts. Across every account you connect.

### You need your own Google OAuth client

There is no shared Pixy application — a desktop app cannot keep a client secret,
so you make your own. Once, in about five minutes:

1. Go to [Google Cloud Console](https://console.cloud.google.com/) and create a
   project (or pick one).
2. **Enable the APIs you want.** APIs & Services → Library → enable **Gmail API**
   and **Google Calendar API**. This step is easy to skip and the failure is
   confusing — see the troubleshooting note below.
3. APIs & Services → OAuth consent screen → set it up. Add your own email under
   **Test users** if the app is in testing mode.
4. Credentials → Create credentials → **OAuth client ID**.
   **Application type must be "Desktop app".**
5. Copy the client ID and client secret into **Settings → Google**.
6. Press **Add account** and complete the browser sign-in. Repeat for each
   mailbox.

> **⚠ "Desktop app", not "Web application".** This is the single most common
> mistake. Pixy signs you in on a temporary local port, and only Desktop-app
> clients are allowed to do that — a Web application client requires every
> redirect URL to be registered in advance, so you get
> `Error 400: redirect_uri_mismatch` and no amount of retrying helps.

### Tuning what you see

**Settings → Advanced**, Mail and Calendar sections:

- **How far back unread counts** — default 1 day. Set it to 7 and the brief covers
  a week. The unread count on the card follows the same window.
- **How many messages the brief shows** — it is not capped at a handful; all
  connected accounts are included, newest first, with replies to you first.
- **Summarise with local models only** — **on by default.** With it on, a message
  is never sent to a hosted model; if no local model is available the notice shows
  the message's own opening lines instead of turning the feature off.
- **Minimum length worth summarising** — short mail explains itself.

Use **tray → Run Morning Brief Now** to see the result immediately instead of
waiting for tomorrow.

---

## 7. GitHub

Notices for merged PRs, failing CI, and issues assigned to you.

**Settings → GitHub**: paste a personal access token (`repo` scope is enough for
private repositories; public-only needs less) and add the repositories to watch.
**Test connection** tells you whether the token works and what it can see.

**tray → Run GitHub Digest Now** fires the daily digest on demand.

---

## 8. Day to day

**The mascot** sits at the top of the screen and is click-through except where it
actually has something — so it does not steal clicks from the window behind it.
Notices stack; cards pin until answered. Click the mascot for a quick menu.

**The workspace** (tray → Open Workspace) has three panes:

- **Chat** — as above.
- **Notes** — markdown with `[[wiki-links]]` between notes. Pick where they live
  in Settings, so they can sit in a synced folder.
- **Memory** — a browser over Claude Code's own memory files, with a graph of how
  they link.

**Settings → Advanced** deserves one look. Everything in it is a value that
depends on your machine rather than on the app — room noise, how fast your speech
server is, how long away from the keyboard counts as away. Each field explains
what it trades off, and the defaults are sensible; you do not have to touch any of
it.

---

## 9. When something doesn't work

**"All my settings vanished."** Look in
`%APPDATA%\com.widget.mascot\` for a `config.json.corrupt-<timestamp>` file. If
one is there, the config could not be parsed and was kept rather than
overwritten — the settings are in that file and can be recovered by hand.

**Google: "Gmail API has not been used in project … or it is disabled."** Step 2
above was skipped. The message includes a link straight to the page that enables
it. Pixy passes Google's own wording through rather than guessing, so believe the
message.

**`Error 400: redirect_uri_mismatch`** when adding an account. The OAuth client is
a "Web application". Make a new one as **Desktop app**.

**A mail notice shows the raw message instead of a summary.** The model was asked
and could not answer in time — usually because something else is using the GPU. If
the picture server is running, that is the likely culprit. `mail-watcher-debug.log`
in the app data folder records why, in counts and reasons only — never the
contents of your mail, so it is safe to paste into an issue.

**Chat: "stream read error".** The model took longer than the reply timeout.
Vision models on long images are the usual cause. Raise **Settings → Advanced →
reply timeout**.

**The mascot stopped reacting to Claude Code.** In order:

1. Is the port free? If something else took `47623`, Pixy logs the bind failure
   and the hooks silently do nothing. Change it in **Settings → Advanced → Claude
   Code event port**, then **restart the app** — the socket is bound at launch —
   and update the port in your hook commands by hand. Nothing here can edit your
   Claude Code settings for you.
2. Are you testing from a browser or REST client? Those get `403` by design. Use
   `curl`.
3. Does `curl -X POST http://127.0.0.1:47623/event -d '{"state":"waiting_input"}'`
   move the mascot? If yes, the app is fine and the problem is in your hook
   configuration.

**A model server is still running after quitting.** Use **Quit and stop local
servers** from the tray rather than closing the window. Pixy identifies the
servers it started by which port they are listening on, so it can stop them even
when they have outlived their parent process.

**Sound plays but nothing looks different**, or vice versa. These are two
independent paths (`notice/notice.js` for the pill, `lib/sound.js` for audio).
See [docs/DESIGN-NOTES.md](docs/DESIGN-NOTES.md) — this exact split has bitten
before and the diagnosis is written down.

---

Pixy is Claude Code only by design. The hook system it relies on has no
equivalent in Codex, Cursor or Antigravity, and the screen-scraping approach that
used to half-support them was removed in its favour.
