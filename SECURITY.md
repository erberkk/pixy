# Security

This application reads web pages that a language model chose, runs an HTTP
server on loopback, and holds OAuth refresh tokens for Gmail and Calendar. That
combination deserves a straight description rather than a reassurance, so this
document states what is defended, what is deliberately not, and what is still
open.

## Reporting a vulnerability

Please use **GitHub's private vulnerability reporting** on this repository
(Security → Report a vulnerability). It creates a private thread, so nothing is
disclosed by the act of reporting.

<!-- Add a fallback contact here if you want one. A work address is a poor choice
     for this; a dedicated address or GitHub-only reporting is better. -->

There is no bounty, and no formal response-time commitment — this is one person's
project. Expect a reply within a week or so.

## What this is, in threat-model terms

| Boundary | What crosses it |
|---|---|
| **Untrusted → prompt** | Web pages the model fetches. Anyone can write these |
| **Model output → DOM** | Replies are rendered as markdown into `innerHTML` |
| **Network → app** | The Claude Code hook server, on `127.0.0.1` |
| **App → network** | The URLs the model asks to fetch |
| **Disk** | `config.json`, holding every credential |

The interesting one is the first two together: a page the model merely *read* can
try to steer the model into emitting output that does something when rendered.
That is prompt injection terminating on a DOM sink, and the model is the delivery
mechanism rather than the attacker.

## The renderer

`src/shared/markdown.js` is hand-rolled, and the external-link branch is the
sharp edge. It now:

- **Allow-lists schemes.** Only `http(s):`, `mailto:`, `#` and `/` are emitted as
  links. Anything else — `javascript:`, `data:`, `vbscript:`, `file:` — is left as
  literal markdown text, so the reader sees the source instead of a link whose
  destination they cannot judge.
- **Escapes quotes in the attribute.** `escapeHtml` deliberately does not touch
  quotes (a quote in text position needs no escaping), which is exactly wrong for
  `href="…"` — one quote ends the attribute and the rest is parsed as markup.
- Emits `rel="noopener noreferrer"`.

This is covered by [`src/shared/markdown.test.js`](src/shared/markdown.test.js),
which exists for this reason and nothing else. Its cases include a payload that
needs no user interaction at all (`" autofocus onfocus="…`). Run it with
`npm test`.

**If you change that regex, run those tests.** They fail against the previous
version of the file, which is the only property that makes them worth having.

## Content Security Policy

`src-tauri/tauri.conf.json` sets a real CSP. It is the second, independent layer:
it blocks inline event handlers and inline script outright, so the whole class
above is dead even if the renderer regresses.

```
default-src 'self'; script-src 'self' 'wasm-unsafe-eval';
style-src 'self' 'unsafe-inline'; img-src 'self' data:; media-src 'self' data:;
font-src 'self'; connect-src 'self' ipc: http://ipc.localhost;
object-src 'none'; base-uri 'self'; frame-src 'none'
```

Every relaxation is load-bearing and was verified against the running app rather
than guessed:

| Directive | Why it is not tighter |
|---|---|
| `'wasm-unsafe-eval'` | The wake word runs ONNX Runtime. Without this, `WebAssembly.compile` throws and voice does not start |
| `style-src 'unsafe-inline'` | Two `style="background:…"` attributes in the memory graph legend. CSP hashes do not apply to style attributes |
| `img-src data:` | Pasted image attachments and generated pictures are handed to the webview as data URLs |
| `media-src data:` | TTS playback constructs `new Audio("data:audio/…")` |
| `connect-src ipc:` / `ipc.localhost` | Tauri's own IPC transport |

Tauri appends hashes for its injected bootstrap scripts automatically; those
appear in the effective policy at runtime and are not written above.

### `withGlobalTauri` is deliberately left on

A tempting hardening is `withGlobalTauri: false`, to remove the ambient
`window.__TAURI__` that injected script could call. **It is not applicable here,
and the reason is worth recording:** `src/shared/tauri.js` does not *import* the
Tauri API — it reads the injected global and re-exports it, because this project
has no bundler and therefore no way to resolve `@tauri-apps/api`. Turning the
flag off would break every window, not just harden one.

The alternative is vendoring the Tauri JS API bundle. That is real work with real
maintenance, and the CSP above already prevents injected script from running at
all, so the global is unreachable by the path that motivated the change. Revisit
if a bundler ever arrives.

## Fetching from the web

`src-tauri/src/web/fetch.rs` treats "fetch this URL" as an instruction from a
stranger, because it is:

- **Public addresses only.** Loopback, private ranges, link-local (including
  `169.254.169.254`, where cloud metadata lives), carrier-grade NAT, unique-local
  and reserved ranges are refused. IPv4-mapped IPv6 is unwrapped first, so
  `::ffff:127.0.0.1` does not slip past a v4-only check.
- **All addresses, not the first.** A hostname is refused if *any* address it
  resolves to is non-public.
- **The approved address is pinned.** Validating and connecting are two separate
  lookups, and a resolver may answer differently for each — public for the check,
  `127.0.0.1` for the connection. The validated addresses are handed to the client
  via `resolve_to_addrs`, so no second lookup happens. TLS still verifies the
  hostname; only DNS is overridden.
- **Every redirect hop is re-validated,** which is why the chain is walked by hand
  instead of letting `reqwest` follow it.
- **The download is capped** at 512 KB, because everything read ends up in a
  prompt and the size of a page is the size of the injection surface.

Non-`http(s)` schemes are refused outright.

## The Claude Code hook server

`src-tauri/src/agent/server.rs` binds `127.0.0.1` on a configurable port. Binding
loopback is commonly mistaken for a boundary; it is not, because **any page in any
browser can POST to loopback cross-origin.** The response is unreadable to that
page under CORS, but these are side-effect endpoints: `POST /decide` puts a
permission card on screen that looks exactly like a real one, which is a phishing
primitive.

What is in place: requests carrying `Origin` or any `Sec-Fetch-*` header are
rejected with 403. Both are forbidden header names, so page script cannot remove
them, and browsers attach `Sec-Fetch-*` to every request including same-origin
ones. Claude Code and `curl` send neither, so no configuration changes.

**What is not in place, stated plainly: this is not authentication.** It stops a
browser. It does not stop another program running as the same user. The real fix
is a shared token generated at startup and carried in the hook URL — which
invalidates every existing hook configuration, so it is a deliberate follow-up
rather than something folded into the same change. See
[Open items](#open-items).

## Where your data goes

**Local by default, and the defaults matter here.**

| Data | Where it goes |
|---|---|
| Chat history, notes, memory, search index | Stay on disk. Never uploaded |
| Recalled conversation history | Not sent to a non-local endpoint unless `recall.share_with_cloud` is turned on. **Default: off** |
| Mail bodies, for summarising | Not sent to a non-local model when `mail.local_models_only` is on. **Default: on** — and it degrades to showing the message's own opening lines rather than turning the feature off |
| Image prompts | Local servers only, enforced in code, not a setting |
| Chat messages | Whichever endpoint you configured. If you point a profile at a hosted API, that is where they go |

There is no telemetry, no crash reporting, and no network call this app makes on
its own behalf.

The web-search tool uses keyless public endpoints (DuckDuckGo, Wikipedia) — no
account, no API key, and no identifier that ties a query to you beyond your IP.
The User-Agent is deliberately browser-shaped rather than naming this app, so
that visiting a page does not put a record of who runs this widget in that site's
logs.

## Stored credentials

`%APPDATA%\com.widget.mascot\config.json` holds, **in cleartext**: LLM/STT/TTS
API keys, the Google OAuth client secret, Google refresh tokens, and the GitHub
personal access token.

This is a deliberate, accepted trade-off, not an oversight — and it is documented
here rather than left to be discovered:

- Any process running as your user can read it. So can any backup, and any sync
  client pointed at `%APPDATA%`.
- Windows DPAPI (`CryptProtectData`) would raise the bar. It would not change the
  fundamental position — a local tool has to be able to use these credentials
  unattended, so anything that can run as you can obtain them — but it would stop
  casual reads and stray backups.
- **A refresh token is the whole grant.** It is durable Gmail and Calendar access,
  not a session. Revoke from your Google Account's third-party access page, not by
  deleting the file.

There is no shared OAuth application and no secret compiled into the binary: you
create your own client. A desktop binary cannot keep a secret, so pretending
otherwise would be the worse design.

If you do not want a credential on disk, do not configure that feature — every
one of them stays off until it is set up.

## The config file itself

The most likely real data-loss path in a program like this is a settings file
that gets half-written and then silently replaced with defaults, taking every
account with it. So:

- Writes go to a temp file and are **renamed** over the target, which is atomic on
  one volume. The file is either the old config or the new one, never a truncated
  hybrid.
- A file that cannot be parsed is **moved aside** to
  `config.json.corrupt-<timestamp>` and reported, instead of being overwritten by
  the defaults that had to be loaded in its place.
- A UTF-8 BOM is tolerated. Editing the file with a tool that adds one (PowerShell
  `-Encoding utf8`, among others) used to make the whole config invisible.

## Open items

Honest list of what a reviewer would find and what is known:

| Item | Status |
|---|---|
| Token authentication for the hook server | **Open.** Origin/Sec-Fetch rejection is in place; token auth is not, because it breaks existing hook configs |
| Settings commands return secrets to the renderer | **Open.** `get_llm_settings` and friends return API keys, so the UI can show them. A `has_secret: bool` shape is better, but the blank-field-means-unset ambiguity it creates once wiped every connected account, so it needs care rather than speed |
| `start_command` is unvalidated | **Open.** A caller that can invoke `save_llm_settings` can set a command that runs at next launch. Reachable only from the renderer, which the CSP now protects |
| DPAPI for stored secrets | **Won't do for now**, documented above instead |
| Mutex poisoning latches some state | **Open, low.** A panic while holding one of ~20 `lock().unwrap()` sites disables that feature until restart |

## Scope

This is a single-user desktop application. It has no multi-user model, no
privilege separation between its own components, and it assumes the machine it
runs on is not already compromised. If an attacker is running code as your user,
nothing here is a defence against them — and no local-first tool holding usable
credentials can be.
