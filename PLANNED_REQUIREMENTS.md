# Planned Requirements — Next Features

Captured from planning discussion, not yet implemented. v1 (Claude Code
session watcher / Dynamic Island notification pill) is done and working —
see `SETUP.md`. These are the next three feature areas under consideration.

## 1. Notepad (self-contained, no external API)

- Opens via right-click menu on the widget, or via left-click / double-click.
- Sidebar with multiple tabs/notes, search across notes.
- Autosave — no manual save step.
- Notes stored locally on the PC.
- Customizable (exact scope TBD — styling, organization, etc.).
- Local LLM can analyze notes later (e.g. re-organize, summarize, extract
  reminders) — analysis should be able to revisit and edit/improve existing
  notes, not just read them once.

## 2. GitHub integration

- Open PRs overview.
- Issues assigned to the user.
- Notifications for state changes the user cares about, e.g.:
  - A PR you have open gets merged → "PR merged" notification.
  - You get assigned to an issue → notification.
- Needs periodic polling of the GitHub API (a personal access token is
  sufficient — no OAuth consent flow required, simpler than Gmail).

## 3. Mail integration (Gmail)

- Daily unread mail count.
- Mail summaries, presented reminder-style (not full inbox replication).
- Needs real Gmail API access independent of this conversation's Claude.ai
  Gmail connection — the standalone widget exe cannot reuse that connection,
  so it needs its own registered OAuth client and local token storage/refresh
  flow. This is the heaviest of the three to build.

## Suggested build order

**Notepad → GitHub → Mail**, each one a self-contained increment on a
working foundation:

1. Notepad has no external dependencies — good next step, fully local.
2. GitHub needs only a static PAT + polling — moderate complexity.
3. Mail needs a full OAuth flow — most complex, tackle last once the
   polling/notification/local-LLM patterns are already proven by the other
   two.

## Open questions (not yet decided)

- Where do PR/issue/mail polling loops live — a background thread in the
  existing Rust backend, or a separate process?
- How does the local LLM (Ollama) get wired in for summarization — same
  event/notification pipeline as the Claude Code hooks, or a separate path?
- Notepad customization scope — what exactly is configurable?
- Notification fatigue — with mail + GitHub + Claude Code hooks all feeding
  the same widget, may need a way to prioritize/mute certain sources.
