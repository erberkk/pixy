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
            "command": "IN=$(cat); echo \"$IN\" | curl -s -X POST http://127.0.0.1:47623/decide -H \"Content-Type: application/json\" -d @-",
            "timeout": 5
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
    ]
  }
}
```

## Design decisions (and why)

**Notification-only, no in-widget answering.** An earlier version tried to let
the widget itself answer permission prompts (blocking the `PermissionRequest`
hook until you clicked Allow/Deny in the widget, returning
`{"decision":"approve"|"block"}` on the hook's stdout). That contract is real
— confirmed directly from the installed Claude Code binary's embedded
strings — but it only appears to affect **headless/auto-mode** execution.
For a normal interactive VS Code/CLI session, the human-facing prompt is the
actual approval mechanism regardless of what the hook returns. So `/decide`
responds immediately with `{"decision":"ask"}` (defer to normal behavior) and
never blocks — it exists purely to trigger the widget's notification without
adding any latency to your real workflow.

**Fixed notice text, not the actual command/tool content.** Showing the real
`tool_name`/`tool_input` from the hook payload looked cramped and
inconsistent in practice. The widget just shows a fixed
"Claude is waiting for your approval" style message for every
permission/notification event instead.

**Instant collapse on approve, best-effort on deny.** `PreToolUse` fires
unconditionally right as an approved tool is about to run (confirmed
unconditional in the Claude Code binary — not gated by auto-mode/classifier
like the two hooks below), so approving in your editor/terminal collapses
the pill almost immediately. `PermissionDenied`, however, **only fires for
auto-mode/classifier-driven denials** — clicking "No" in the interactive
prompt does not trigger it at all (confirmed the same way: its call site is
wrapped in `if (decisionReason.type === "classifier" && decisionReason.classifier === "auto-mode")`).
So there is no reliable "the human just clicked Deny" signal — the widget's
own 6-second safety timeout is what actually closes it in that case, not a
hook.

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

| State                | Fired by                                    | Mascot behavior                                          |
|----------------------|------------------------------------------------|-------------------------------------------------------------|
| `waiting_permission` | `PermissionRequest`                            | Expanded pill, "waiting for your approval" text, double beep |
| `waiting_input`      | `Notification`                                 | Expanded pill, different text, softer single chime         |
| `idle`               | `PreToolUse` / `PermissionDenied` / default    | Collapses immediately back to the small idle pill          |
| `turn_done`          | `Stop`                                          | Brief brightness flash + quiet tick, then settles to idle   |

## Manual test (without Claude Code)

With the widget running (`npm run tauri dev`), verify each state from a shell:

```bash
curl -X POST http://127.0.0.1:47623/event -d "{\"state\":\"waiting_input\"}"
curl -X POST http://127.0.0.1:47623/decide -d "{\"tool_name\":\"Bash\"}"
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

- The port (`47623`) is currently hardcoded in `src-tauri/src/lib.rs`. Change
  it there and in the hooks above together if it conflicts with something
  else on your machine.
- This wiring only covers the Claude Code CLI/VS Code extension. There is no
  publicly documented equivalent hook system for Codex CLI, Cursor, or
  Antigravity yet — those would need separate research before they could
  drive the same mascot states.
