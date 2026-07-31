# Third-party notices

This project's own code is under the MIT License (see [LICENSE](LICENSE)). It
also **redistributes** third-party files inside this repository, and ships a
binary that links a large number of Rust crates. Those carry their own licenses,
listed here.

Full license texts are in [`licenses/`](licenses/).

## Redistributed in this repository

These are the ones that matter most, because the files are physically here — a
clone contains them, so their license terms travel with the clone.

| Component | Where | Version | License | Text |
|---|---|---|---|---|
| highlight.js | `src/vendor/highlight/highlight.bundle.js` | 11.11.1 | BSD-3-Clause | [licenses/highlight.js.BSD-3-Clause.txt](licenses/highlight.js.BSD-3-Clause.txt) |
| ONNX Runtime Web | `src/vendor/onnxruntime/` | 1.19.2 | MIT | [licenses/onnxruntime-web.MIT.txt](licenses/onnxruntime-web.MIT.txt) |
| openWakeWord models | `src/mascot/voice/models/` | — | Apache-2.0 | [licenses/openWakeWord.Apache-2.0.txt](licenses/openWakeWord.Apache-2.0.txt) |

Each of those three has a provenance note recording where the files came from
and how to regenerate them: `src/vendor/onnxruntime/README.md`, the header
comment of `highlight.bundle.js`, and the header of the openWakeWord notice.

Two files that *look* third-party but are not, and are covered by this
project's LICENSE:

- `src/vendor/highlight/highlight.css` — highlight.js's class names, this app's
  colours. None of an upstream theme's CSS is in it.
- `src/mascot/voice/wakeword.js` — a JavaScript reimplementation of
  openWakeWord's inference chain. The constants in it are dictated by what the
  models expect; the code is this project's.

### Why these are vendored rather than installed

This frontend has no bundler and no runtime `node_modules` — the webview loads
files directly from disk. Vendoring is what makes a clone work offline and keeps
the build hermetic. It also means the ~1.7k lines of `highlight.bundle.js` are
plainly *not* this project's code, which is easier to see under `src/vendor/`
than in a lockfile.

## Rust dependencies

The binary links 590 crates (20 direct, the rest transitive). Every one is under
a permissive license; there is no GPL or AGPL anywhere in the graph. The exact
set and versions are pinned in [`src-tauri/Cargo.lock`](src-tauri/Cargo.lock),
which is committed for this reason.

Distribution by license, as reported by `cargo metadata`:

| License | Crates |
|---|---:|
| MIT OR Apache-2.0 (in its various spellings) | 352 |
| MIT | 136 |
| Unicode-3.0 | 18 |
| Zlib OR Apache-2.0 OR MIT | 17 |
| MPL-2.0 | 8 |
| ISC | 5 |
| Unlicense OR MIT | 5 |
| Apache-2.0 | 4 |
| BSD-3-Clause | 3 |
| other permissive combinations | remainder |

To regenerate this survey:

```sh
cd src-tauri && cargo metadata --format-version 1
```

### The eight MPL-2.0 crates

Worth calling out because MPL-2.0 is weak copyleft rather than permissive:
`cssparser`, `cssparser-macros`, `selectors`, `dtoa-short` (all pulled in by
`scraper`, used for HTML extraction) and `option-ext` (pulled in by `dirs`).

MPL-2.0's copyleft is per-file and applies to modifications of *those* files.
This project uses all of them unmodified, as ordinary crates.io dependencies, so
the obligation is attribution — which this section is — and source availability,
which crates.io provides. It places no condition on this project's own license.

### Full attribution for binary distribution

The table above is a survey, not a complete notice file. Publishing **installers**
rather than source would mean shipping the individual notices too. The
conventional tool for that is `cargo about`:

```sh
cargo install cargo-about
cd src-tauri && cargo about generate about.hbs > ../THIRD-PARTY-BINARY.html
```

That step is deliberately not done here: this repository distributes source, and
`Cargo.lock` plus this file is the honest scope for that.

## Models and services this app talks to but does not ship

Nothing below is redistributed — the app connects to whatever the user runs or
configures, and the choice of model is theirs. Listed so the dependency surface
is not misread as smaller than it is.

| What | Reached how |
|---|---|
| Local LLM server (Ollama, llama.cpp, LM Studio, …) | OpenAI-compatible HTTP, address from Settings |
| whisper.cpp / any OpenAI-compatible STT | HTTP |
| Kokoro-FastAPI / any OpenAI-compatible TTS | HTTP |
| stable-diffusion.cpp | HTTP, started on demand |
| Gmail and Google Calendar | Google APIs, with an OAuth client the user creates |
| GitHub | REST API, with a token the user creates |
| DuckDuckGo / Wikipedia | Keyless HTTP, for the web-search tool |

Their licenses are the user's concern for their own installs, not this project's
to restate — but note that a model's weights often have terms of their own, and
those are worth reading before pointing this app at one.
