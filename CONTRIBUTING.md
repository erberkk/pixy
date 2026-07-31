# Contributing

Contributions are welcome — this is a small project built by one person on one
machine, which means the rough edges are real and finding them is genuinely
useful.

- **Found something broken?** Open an issue. See [Filing an issue](#filing-an-issue)
  below for what to include — and what never to paste.
- **Want to add something?** Open an issue first if it's substantial, so nobody
  writes the same thing twice. Small fixes can go straight to a pull request.
- **Ported it to macOS or Linux?** That is the single most useful thing anyone
  could send. The platform-specific code sits in four files; the rest is already
  neutral.
- **Not a programmer?** Telling us what was confusing in [SETUP.md](SETUP.md) is
  worth as much as a patch. Every instruction in it was written by someone who
  already knew the answer.

No CLA, no style bikeshedding, no minimum size. The one hard rule is the privacy
one further down: never paste `config.json` anywhere.

## Build and run

```sh
npm install                 # just @tauri-apps/cli, nothing at runtime
npm run tauri dev
```

You need Rust (stable) and the [Tauri 2
prerequisites](https://tauri.app/start/prerequisites/). Windows only for now — see
the platform table in the [README](README.md#platform-support) for what stands
between here and a port.

```sh
npm test                    # frontend tests — Node's built-in runner, no deps
cd src-tauri && cargo test  # backend tests
cd src-tauri && cargo clippy
```

`cargo clippy` is not clean at zero warnings today; there is a small standing
baseline. Don't add to it.

## Two things that will catch you out

**The frontend is compiled into the binary.** `frontendDist` points at `src/`, and
Tauri embeds it. Editing a `.js` or `.css` file and reloading the window does
nothing — you need a rebuild. Everyone loses ten minutes to this once.

**There is no bundler, so assets are declared by hand.** Adding a feature folder
means adding its `<link>` and `<script type="module">` to the right window's HTML:

| Window | File |
|---|---|
| Mascot overlay | `src/mascot/index.html` |
| Workspace | `src/workspace/workspace.html` |
| Settings | `src/settings/settings.html` |

A missing entry fails silently — no console is attached in a release build — and a
stale one 404s. The stylesheet link order in `index.html` and `workspace.html` is
load-bearing: those files were split out of single stylesheets, and the order
preserves the original cascade. There are comments saying so; keep them true.

No bundler is a deliberate choice, not an omission. It is what makes
`src/vendor/` reasonable and what keeps a clone working offline.

## Where code goes

The backend groups by feature area, with one rule that resolves the apparent
inconsistency between `github/` and `google/`:

> A group owns both its client and its consumers **unless** the client is shared
> across more than one feature domain, in which case the client is promoted to its
> own layer.

So `github/` holds its API client next to the watchers that poll it, while
`google/` is mechanism only — "given a signed-in account, hand back typed data" —
and the decisions about what is worth announcing live in `mail/` and `calendar/`.
One OAuth grant serving two unrelated features is what earns that split.

The frontend organises by window. `src/shared/` is for what genuinely crosses all
three windows; `src/mascot/lib/` and `src/workspace/lib/` are for helpers scoped to
one. There is currently one cross-window import (`workspace/chat/chat.js` reaching
into `mascot/pip/`) and it is a known wart, not a precedent.

`config.rs` and `tunables.rs` sit at the crate root because every group reads
them, so they belong to none.

## Comments

This codebase's comments explain **decisions**, not mechanics. A comment that says
what the next line does is noise; a comment that records a measurement, or an
alternative that was tried and rejected, is the reason the code is shaped that way.

Real examples to calibrate against: why `offload()` exists (`lib.rs`, with the
observed `IsHungAppWindow()` symptom), why `readability` was rejected as a
dependency (`Cargo.toml`), why Turkish dotless `ı` is folded by hand
(`ai/recall.rs`), why the clipboard goes through Rust instead of
`navigator.clipboard`.

If you change code that carries such a comment, the comment is the argument you
have to answer — either your change is consistent with the measurement, or the
measurement no longer holds and the comment needs updating in the same commit.
Please don't leave a comment describing a world that no longer exists.

## Tests

Rust tests live in a `#[cfg(test)] mod tests` at the bottom of the file they
cover. Names are sentences: `a_utf8_bom_does_not_lose_the_config`,
`only_a_long_message_with_a_usable_model_is_summarized`. The point is that a
failure reads as a broken promise rather than a broken function.

Prefer testing the pure core over the plumbing. Several modules were shaped
specifically to make that possible — `mail::summary::plan` decides what to do
before anything slow happens, so the rule can be tested without a model, a token
or an `AppHandle`; `config::parse_config` is separate from the file handling for
the same reason.

Two tests are `#[ignore]`d because they need the network. Run them deliberately:

```sh
cd src-tauri && cargo test -- --ignored
```

The reasoning is in the code: a suite that fails because the network is down is a
suite people stop trusting, but real pages look nothing like hand-written
fixtures, so both kinds are kept and only one runs by default.

### If you touch security-relevant code

- `src/shared/markdown.js` — run `npm test`. Those tests exist because of a real
  vulnerability and they fail against the version that had it.
- `src-tauri/src/web/fetch.rs` — the address checks are a security boundary, not a
  convenience check. Adding an allowed case needs a test showing what it does *not*
  allow.
- `src-tauri/src/agent/server.rs` — network-facing. See [SECURITY.md](SECURITY.md)
  for what the browser-rejection check does and does not claim.

## Logs and privacy

The mail watcher writes to `mail-watcher-debug.log` in the app data directory.
**It records counts, ids and reasons only — never a sender, a subject or a body.**
That constraint is stated at the top of `src-tauri/src/mail/mod.rs` and it is not
negotiable: a debug log that quotes mail is a debug log nobody can share when
asking for help.

The same applies to anything you add. If a log line would be awkward to paste
into a public issue, it is the wrong log line.

## Commits and pull requests

- Small and self-contained beats large and thorough.
- Say *why* in the commit message. The diff already says what.
- If you found something by measuring it, put the measurement in the commit
  message or the comment. That is the most valuable thing you can leave behind.
- Please don't reformat code you are not otherwise changing.

## Filing an issue

Include what you configured (which features are on — not their credentials), what
you expected, and what happened. For anything voice- or model-related, the exact
server and model matter: "a local model" covers a hundred behaviours.

**Do not paste `config.json` or any part of it into an issue.** It contains your
API keys, OAuth client secret, refresh tokens and GitHub PAT in plaintext. If you
think a config value is relevant, name the setting rather than quoting the file.

For a suspected vulnerability, don't open a public issue at all — see
[SECURITY.md](SECURITY.md).
