# Design notes

Why parts of Pixy are shaped the way they are — the measurements, the bugs that
produced a decision, and the alternatives that were tried and rejected. Split out
of [SETUP.md](../SETUP.md), which is now a usage guide.

Nothing here is needed to *use* the app. It is here because the reasoning is the
expensive part: the code can be re-derived from it, and a decision without its
reason gets reverted by the next person who finds it inconvenient.

---

## The Claude Code integration

### The hook itself answers the prompt — no PTY keystrokes

An earlier version of this project believed the `PermissionRequest` hook's JSON
decision (`{"hookSpecificOutput":{"decision":{"behavior":"allow"|"deny",…}}}`)
only took effect in headless/auto mode, and that an interactive terminal session
could only be answered by writing real keystrokes into its PTY.

**That was wrong**, confirmed against Claude Code's own hook documentation: the
decision is honoured in interactive sessions too, answering the prompt before it
is shown.

So `/decide`'s HTTP request is held open rather than answered immediately, for as
long as the human takes. Claude Code blocks the tool call on that response, which
means approving from the widget *is* the decision. The machinery that used to type
digits into a PTY — `highest_numbered_option`, `wait_for_prompt_visible`,
`write_keys` — is gone, and so is the PTY.

### The timeout has to span a human, not a network

Because the connection stays open while someone decides, the hook's own `timeout`
in `settings.json` must be generous — 3600s, not the 5s that was fine when
`/decide` answered instantly. If it does expire first, Claude Code falls back to
its own interactive prompt as if no hook existed: annoying, not unsafe.

### `AskUserQuestion` gets its own rendering

When `tool_name` is `AskUserQuestion`, its `tool_input.questions` array
(`question` / `header` / `options[].label` / `multiSelect` — the shape Claude
Code's own multi-choice prompt renders from) is parsed out and shown as tappable
chips. See `buildQuestionUI` in `src/mascot/agent/agent-notice.js`.

Submitting sends the answer back as the *same* hook response, with
`updatedInput: {questions, answers}`. Claude Code does not document that specific
shape, so it was matched against AgentGlance's answer contract.

`tool_name` and `tool_input` are real structured data from the hook payload — not
text scraped off a rendered terminal.

### The app no longer runs terminals of its own

It used to keep a pool of sixteen PTY-backed windows so it could watch Claude by
reading what they rendered, with `WIDGET_TERMINAL_LABEL` telling it which window a
hook came from. The hooks made that watching unnecessary and they fire wherever
you actually run Claude — so the pool was spending sixteen `cmd.exe` and sixteen
`conhost.exe` at every launch on a case that no longer existed, and closing one of
its windows could take the widget down with it.

**Removing it fixed a bug rather than causing one.** "Somebody is waiting on you"
was a flag on one of *our* sessions, keyed by that label — so running Claude in
your own terminal showed the permission card but never moved the mascot into its
`waiting`/`forgotten` pose, because there was no session of ours to flag. It is
now counted straight from the held-open requests (`pending_permissions` in
`agent/server.rs`), which works for any terminal.

`?label=` on `/decide` survives as pure decoration: whatever it contains captions
the card, and an absent one reads "Claude Code". What genuinely went away is
"click a session to focus its window", which is unanswerable for a terminal this
app did not launch.

### `PreToolUse` / `PermissionDenied` are about mood, not the card

The permission card needs no safety timeout: it closes the instant
Approve/Deny/Submit resolves the held-open request, deterministically.

`PreToolUse` fires unconditionally as an approved tool starts.
`PermissionDenied` fires only for auto-mode/classifier denials — confirmed by its
call site being wrapped in `if (decisionReason.type === "classifier" &&
decisionReason.classifier === "auto-mode")`. Clicking "No" in a genuinely
interactive prompt does not trigger it at all.

Both exist only to drive the ambient "coding" vs "idle" mood in
`src/mascot/pixy/signals.js`, which is a separate and much lower-stakes concern
than the card's own state. `UserPromptSubmit` never touches `mascot-state` or
`body.className` for the same reason — `signals.js` listens for it directly and
shows "thinking" briefly, backing off to "coding" the moment a real `PreToolUse`
arrives.

### The event server refuses browser requests

Binding `127.0.0.1` is routinely mistaken for a security boundary. It is not: any
page in any browser can POST to loopback. Since `/decide` puts a permission card
on screen, that would be a phishing primitive — a fake card indistinguishable
from a real one.

Requests carrying `Origin` or any `Sec-Fetch-*` header are rejected with 403.
Both are forbidden header names, so page script cannot strip them, and browsers
attach `Sec-Fetch-*` to every request including same-origin ones. `curl` and
Claude Code send neither.

This is not authentication, and [SECURITY.md](../SECURITY.md) says so plainly —
it stops a browser, not another program running as you.

---

## Windows and rendering

### The window never resizes at runtime — only CSS does

The OS window is created once at a fixed size, large enough for the biggest
expanded state, and never touched again.

An earlier version called `window.set_size()` on every state change. The outer
window measurably grew — confirmed via `outer_size()` — but the embedded WebView2
surface kept rendering at the old size, leaving most of the "expanded" window
blank. All growing and shrinking is now a pure CSS `width`/`height` transition on
the inner `#mascot` div, anchored to the top of the fixed window, which sidesteps
the native-resize/webview-repaint mismatch entirely.

### Secondary windows are hidden, not destroyed

Closing one hides it. Destroying it would mean rebuilding it dynamically later,
which hangs — see `ui/windows.rs`.

---

## Debugging notes

Real bugs found during development, kept in case the symptoms come back.

**1. Wrong hook event names.** `Notification` with `permission_prompt` /
`idle_prompt` matchers does not exist in Claude Code. The real events are
top-level `PermissionRequest` and plain `Notification` with no matcher.

Confirmed by grepping the installed VS Code extension's
`claude-code-settings.schema.json` and the bundled `claude.exe` for literal
strings — `grep -a -o ".\{100\}PermissionRequest.\{100\}" claude.exe` — rather
than trusting docs or memory. The same technique confirmed the decision contract.

**2. Per-window ACL blocks Tauri APIs silently.** Tauri v2 scopes frontend
permissions per window label in `src-tauri/capabilities/default.json`
(`"windows": [...]`). If that list does not match the actual window label in
`tauri.conf.json`, `listen()` and `invoke()` silently receive nothing — while the
backend still returns `200 ok`, which is easy to misread as a hook problem when it
is a frontend permissions problem.

**3. AudioContext autoplay policy.** Web Audio is often blocked from making sound
until a user gesture happens in the page, and this widget is designed never to be
clicked.

This was originally solved with
`additionalBrowserArgs: "--autoplay-policy=no-user-gesture-required"` on the
window. **That flag is no longer set**, because `additionalBrowserArgs` turned out
to break WebView2 initialisation for every secondary window regardless of its
value (see `lib.rs` and git history). The current fix lives in
`src/mascot/lib/sound.js`: it checks `ctx.state === "suspended"` and calls
`ctx.resume()` before each beep. If chimes ever go silent again, look there first.

**4. Native window resize does not repaint the webview to match.** See above.

**5. Concurrent hook events racing each other.** Every hook fires on its own
thread and several can land within milliseconds — `PreToolUse` fires once per tool
call. Unsynchronised concurrent writes were briefly a real bug here, before the
architecture moved to pure event emission with no shared mutable window state. If
backend state ever needs mutating across concurrent requests again, guard it.

**6. CSS class name typo (underscore vs hyphen)** — the root cause of the
longest-lived "nothing visually updates" bug.

JS state strings use underscores (`waiting_permission`, `waiting_input`, matching
`VALID_STATES` in `src/mascot/notice/notice.js`), so
``document.body.className = `state-${state}` `` produces
`state-waiting_permission`. A selector written `.state-waiting-permission` with a
hyphen silently never matches.

Every other part of the chain — hook → backend → emit → JS handler → sound — was
working the whole time; only the selector was wrong, which is why sound played and
nothing looked different. Diagnosed by bypassing CSS entirely with
`element.style.background = "lime"` to prove JS→DOM was fine, which narrowed it to
class matching.

---

## Elsewhere

Some decisions are documented where they apply rather than here:

| Topic | Where |
|---|---|
| The security posture as a whole | [SECURITY.md](../SECURITY.md) |
| Why dependencies were chosen or rejected | `src-tauri/Cargo.toml`, per dependency |
| Why blocking commands go through `offload()` | `src-tauri/src/lib.rs` |
| Why the picture server is not autostarted | `src-tauri/src/lib.rs`, `ai/images.rs` |
| Why local servers are found by port, not PID | `src-tauri/src/ai/process.rs` |
| Why recall uses a coverage floor, not a BM25 threshold | `src-tauri/src/ai/recall.rs` |
| Why the OAuth client is not compiled in | `src-tauri/src/config.rs` |
| Why the three notice modules are not shared | `src/mascot/mail/mail-notice.js` |
| Why the renderer lives in `shared/` but its state machine does not | `src/mascot/pixy/pixystate.js` |
