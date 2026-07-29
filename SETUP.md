# Mascot Widget — Claude Code Hook Setup

The widget is a Dynamic-Island-style pill docked at the top-center of the
screen. It listens for events on `http://127.0.0.1:47623/`. Wire your Claude
Code hooks to POST to it so the mascot reacts when a session needs you.

Add this to your `~/.claude/settings.json` (global) or a project's
`.claude/settings.json` (per-project). Merge it into any existing `hooks` key
rather than replacing the whole file.

```json
{
  "hooks": {
    "PermissionRequest": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "IN=$(cat); echo \"$IN\" | curl -s -X POST \"http://127.0.0.1:47623/decide?label=$WIDGET_TERMINAL_LABEL\" -H \"Content-Type: application/json\" -d @-",
            "timeout": 3600
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "curl -s -X POST http://127.0.0.1:47623/event -H \"Content-Type: application/json\" -d \"{\\\"state\\\":\\\"idle\\\"}\"",
            "timeout": 5
          }
        ]
      }
    ],
    "PermissionDenied": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "curl -s -X POST http://127.0.0.1:47623/event -H \"Content-Type: application/json\" -d \"{\\\"state\\\":\\\"idle\\\"}\"",
            "timeout": 5
          }
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "curl -s -X POST http://127.0.0.1:47623/event -H \"Content-Type: application/json\" -d \"{\\\"state\\\":\\\"waiting_input\\\"}\"",
            "timeout": 5
          }
        ]
      }
    ],
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "curl -s -X POST http://127.0.0.1:47623/event -H \"Content-Type: application/json\" -d \"{\\\"state\\\":\\\"turn_done\\\"}\"",
            "timeout": 5
          }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "curl -s -X POST http://127.0.0.1:47623/event -H \"Content-Type: application/json\" -d \"{\\\"state\\\":\\\"thinking\\\"}\"",
            "timeout": 5
          }
        ]
      }
    ]
  }
}
```

## Design decisions (and why)

**The hook itself answers the prompt — no PTY keystrokes involved.** An
earlier version of this file claimed the `PermissionRequest` hook's JSON
decision (`{"hookSpecificOutput":{"decision":{"behavior":"allow"|"deny",
...}}}`) only takes effect in headless/auto-mode, and that a normal
interactive terminal session could only be answered by writing real
keystrokes into its PTY. That claim was **wrong** — confirmed against
Claude Code's own hook documentation (code.claude.com/docs/en/hooks): the
hook's decision is honored in interactive sessions too, auto-answering the
prompt before it's even shown. This is exactly how AgentGlance (a comparable
macOS tool for Claude Code) does it, and this app now works the same way.

So `/decide`'s HTTP request is held open — not responded to immediately —
for as long as it takes the human to click Approve/Deny (or, for
`AskUserQuestion`, pick option chips and hit Submit) in the mascot. Claude
Code itself blocks the tool call on that response, so approving/denying from
the widget IS the decision. No keystroke is written anywhere — the mechanism
that used to type digits into a PTY (`highest_numbered_option`,
`wait_for_prompt_visible`, `write_keys`) is gone, and so is the PTY.

**Because the timeout must span the human, not the network.** Holding the
hook's HTTP connection open for potentially minutes (however long the
person takes to decide) means the hook's own `timeout` in `settings.json`
must be generous — set to 3600s above, not the 5s that was fine when
`/decide` used to respond instantly. If this hook ever times out before you
click, Claude Code falls back to showing its own interactive prompt as if no
hook existed — annoying but not unsafe.

**`AskUserQuestion` gets its own rendering, not a generic Approve/Deny.**
When the hook's `tool_name` is `AskUserQuestion`, its `tool_input.questions`
array (`question`/`header`/`options[].label`/`multiSelect` per question — the
exact shape Claude Code's own multi-choice prompt renders from) is parsed
out and shown as tappable option chips (see `main.js`'s `buildQuestionUI`).
Submitting sends the answer back as the *same* hook response, with
`updatedInput: {questions, answers}` instead of `updatedInput` left unset —
matching AgentGlance's own answer contract, since Claude Code doesn't
document this specific shape itself. No keystrokes here either.

**What stayed the same:** `tool_name`/`tool_input` are still real, structured
data parsed straight from the hook payload, not text scraped off a rendered
terminal screen. This app is still Claude Code only — no Codex/Cursor/
Antigravity support.

**The app no longer runs terminals of its own.** It used to keep a pool of
sixteen PTY-backed windows so it could watch Claude by reading what they
rendered, and `WIDGET_TERMINAL_LABEL` (set per pooled shell, inherited down to
the hook's own child process) told it which window a hook came from. The hooks
made the watching unnecessary, and they fire wherever you actually run Claude —
so the pool was spending sixteen `cmd.exe` and sixteen `conhost.exe` at every
launch to support a case that no longer existed, and closing one of its windows
could take the widget with it.

That removal fixed a bug rather than causing one. "Somebody is waiting on you"
was a flag on one of *our* sessions, keyed by that label — so running Claude in
your own terminal showed the permission card but never moved the mascot into its
`waiting`/`forgotten` pose, because there was no session of ours to flag. It is
now counted straight from the held-open requests (`pending_permissions` in
`agent/server.rs`), which works for any terminal.

`?label=` in the `/decide` URL is now optional and purely cosmetic — whatever it
contains captions the card, and an absent one just reads "Claude Code". Leaving
`$WIDGET_TERMINAL_LABEL` in your hook command is harmless: it expands to
nothing. What genuinely went away is "click a session to focus its window",
which is not answerable for a terminal this app didn't launch.

**`PreToolUse`/`PermissionDenied` are about ambient mood, not the permission
card.** The permission card itself never needs a safety timeout anymore — it
closes the instant Approve/Deny/Submit resolves the held-open hook request,
deterministically, every time. `PreToolUse` (fires unconditionally right as
an approved tool is about to run) and `PermissionDenied` (fires only for
auto-mode/classifier-driven denials, confirmed by its call site being
wrapped in `if (decisionReason.type === "classifier" && decisionReason.classifier === "auto-mode")`
— clicking "No" in a genuinely interactive prompt doesn't trigger it at all)
exist solely to drive the pip ambient mood's "coding" vs "idle" signal (see
`signals.js`), which is a separate, lower-stakes concern from the permission
card's own state.

**The window never resizes at runtime — only CSS does.** The OS window is
created once at a fixed size (large enough for the biggest expanded state,
see `tauri.conf.json`) and never touched again. An earlier version called
`window.set_size()` on every state change; the outer window measurably grew
(confirmed via `outer_size()`), but the embedded WebView2 surface kept
rendering at the old size, leaving most of the "expanded" window blank. All
growing/shrinking is now a pure CSS `width`/`height` transition on the inner
`#mascot` div, anchored to the top of the fixed window — this sidesteps that
native-resize/webview-repaint mismatch entirely.

## States the mascot reacts to

| State               | Fired by                                    | Mascot behavior                                                    |
|---------------------|------------------------------------------------|---------------------------------------------------------------------|
| `agent_permission`  | `PermissionRequest` (any — `label` resolvable or not) | Pinned card: real tool name/command/diff + Approve/Deny, or (for `AskUserQuestion`) tappable option chips + Submit. Answering resolves the held-open hook request directly — no PTY keystrokes. |
| `waiting_input`     | `Notification`                                 | Expanded pill, different text, softer single chime                  |
| `idle`              | `PreToolUse` / `PermissionDenied` / default     | Collapses immediately back to the small idle pill                   |
| `turn_done`         | `Stop`                                          | Brief brightness flash + quiet tick, then settles to idle            |

`UserPromptSubmit` is NOT in this table on purpose — it never touches
`mascot-state`/body.className at all (see agent/server.rs's `handle_event_request`).
It's purely an ambient pip-mood signal instead: `signals.js` listens for it
directly and shows "thinking" for a short window right after the human
submits a prompt, backing off in favor of "coding" the moment a real
`PreToolUse` fires. See `src/mascot/signals.js`'s own header comment for how
the ambient system and this notice/card system stay deliberately decoupled.

## Manual test (without Claude Code)

With the widget running (`npm run tauri dev`), verify each state from a shell:

```bash
curl -X POST http://127.0.0.1:47623/event -d "{\"state\":\"waiting_input\"}"

# Ambient-only — never touches the notice/card system, just the pip mood
# (signals.js): watch #mascot's data-pip-state switch to "thinking" for
# ~25s, or immediately back to "coding"/whatever else if you also fire a
# real hook event in that window.
curl -X POST http://127.0.0.1:47623/event -d "{\"state\":\"thinking\"}"

# /decide now HOLDS THE CONNECTION OPEN until you click Approve/Deny in the
# widget — curl will just sit there (that's correct, not a hang). Its
# response body is Claude Code's own decision JSON, printed once you answer:
curl -X POST http://127.0.0.1:47623/decide -d "{\"tool_name\":\"Bash\"}"

# Rich card. `label` is optional and cosmetic — whatever you pass captions the
# card; omit it and it reads "Claude Code". Either way the mascot goes to its
# "waiting" pose while this sits open, and "forgotten" once it has waited longer
# than the threshold in Settings > Advanced:
curl -X POST "http://127.0.0.1:47623/decide?label=my-shell" \
  -d "{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"ls -la\"}}"

# AskUserQuestion — renders as tappable option chips instead of Approve/Deny;
# submitting sends the answer back as this same curl's response body:
curl -X POST http://127.0.0.1:47623/decide -d '{"tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Which approach?","header":"Approach","multiSelect":false,"options":[{"label":"Option A"},{"label":"Option B"}]}]}}'

curl -X POST http://127.0.0.1:47623/event -d "{\"state\":\"idle\"}"
curl -X POST http://127.0.0.1:47623/event -d "{\"state\":\"turn_done\"}"
```

**Important:** if you're testing from inside a live Claude Code session
(rather than a plain terminal), remember that the `curl`/PowerShell command
you use to test *is itself* a tool call — it will trigger the real
`PermissionRequest`/`PreToolUse` hooks for its own execution, on top of
whatever payload you're sending. For a clean, uncontaminated test, run the
curl command from a terminal window you opened yourself, completely outside
Claude Code.

## Debugging notes from getting this working

A few real bugs surfaced during development, in case similar symptoms show
up again after future changes:

1. **Wrong hook event names.** `Notification` with `permission_prompt`/
   `idle_prompt` matchers doesn't exist in Claude Code. The real events are
   the top-level `PermissionRequest` and plain `Notification` (no matcher).
   Confirmed by grepping the installed VS Code extension's
   `claude-code-settings.schema.json` and the bundled
   `resources/native-binary/claude.exe` for literal strings — e.g.
   `grep -a -o ".\{100\}PermissionRequest.\{100\}" claude.exe` — rather than
   trusting docs/memory. Same technique confirmed the `/decide` hook's real
   output contract: `{"decision":"approve"|"block"|"ask"}` on stdout.
2. **Per-window ACL blocks Tauri APIs silently.** Tauri v2 scopes frontend
   permissions per window label in `src-tauri/capabilities/*.json`
   (`"windows": [...]`). If that list doesn't match the actual window label
   in `tauri.conf.json`, `listen()`/`invoke()` calls silently receive
   nothing — the backend still returns `200 ok`, which is easy to mistake
   for a hook problem when it's actually a frontend permissions problem.
3. **AudioContext autoplay policy.** Web Audio is often blocked from making
   sound until a user gesture happens inside the page. Since this widget
   never gets clicked, `additionalBrowserArgs:
   "--autoplay-policy=no-user-gesture-required"` is set on the window in
   `tauri.conf.json`.
4. **Native window resize doesn't repaint the webview to match.** See "The
   window never resizes at runtime" above.
5. **Concurrent hook events racing each other.** Every hook fires on its own
   thread; several can land within milliseconds during normal usage (e.g.
   `PreToolUse` fires once per tool call). If backend state ever needs to be
   mutated across concurrent requests again, guard it with a mutex —
   unsynchronized concurrent writes were briefly a real bug here before the
   architecture moved to pure event emission with no shared mutable window
   state.
6. **CSS class name typo (underscore vs. hyphen) — the actual root cause of
   the "nothing visually updates" bug that took the longest to find.** JS
   state strings use underscores (`waiting_permission`, `waiting_input`,
   matching `VALID_STATES` in `main.js`), so `document.body.className =
   \`state-${state}\`` produces classes like `state-waiting_permission`.
   CSS selectors must match **exactly** — `.state-waiting-permission` (hyphen)
   silently never matches `state-waiting_permission` (underscore). Every
   other part of the pipeline (hook → backend → emit → JS state handler →
   sound) was working the entire time; only the CSS selector was wrong,
   which is why sound played but nothing ever looked different. Diagnosed by
   bypassing CSS entirely with a temporary inline-style test
   (`element.style.background = "lime"`) to prove JS→DOM was fine, which
   narrowed it down to CSS class matching specifically.

## Notes

- The port (`47623`) is a setting: **Settings → Advanced → Claude Code event
  port**. Only worth changing if something else on your machine already uses
  it. Two things to know when you do: the widget has to be restarted (the
  socket is bound at launch), and the hook commands above have to be updated
  to the new port by hand — nothing here can edit your Claude Code settings
  for you. The Advanced section spells the new URLs out next to the field.
  If the port is already taken, the widget logs the failure to stderr and the
  hooks simply do nothing, so check there first if the mascot stops reacting.
- Everything else in **Settings → Advanced** is the same kind of value: it
  depends on your machine rather than on the widget (room noise, speech-server
  speed, how long away from the keyboard counts as away). The defaults and the
  explanation of what each one trades off live in one place,
  `src-tauri/src/tunables.rs`; the form is generated from it, and the frontend
  reads its values from there too, so there is no second copy to keep in sync.
- This app is Claude Code only by design now — the terminal-screen-scraping
  approach that used to also (best-effort) support Codex/Cursor/Antigravity
  has been removed in favor of Claude Code's own hook system, which those
  other CLIs don't expose an equivalent of. See `agent/server.rs`'s
  `resolve_decision`/`handle_decide_request` and this file's design-decisions
  section above.
