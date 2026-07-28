# onnxruntime-web (vendored)

Runs the wake-word models in the mascot window — see `src/mascot/voice/wakeword.js`.

- **Version:** 1.19.2
- **Source:** `npm pack onnxruntime-web@1.19.2`, files taken from `package/dist/`
- **License:** MIT — Microsoft, <https://github.com/microsoft/onnxruntime>

Vendored rather than installed because this project has no bundler and no
runtime `node_modules`: the webview loads these files directly, and a clone has
to work offline.

## Which files, and why these three

| file | why |
|---|---|
| `ort.wasm.min.js` | The wasm-only build. Loaded as a classic script (it assigns `window.ort`), which is why `wakeword.js` injects a `<script>` tag instead of importing it. The full `ort.min.js` also carries WebGL/WebGPU backends this doesn't use. |
| `ort-wasm-simd-threaded.mjs` | The loader glue for the wasm. 1.19 fetches this by name at runtime, so leaving it out fails with a bare "no available backend found". |
| `ort-wasm-simd-threaded.wasm` | The runtime itself. |

Despite the `-threaded` name, `wakeword.js` sets `ort.env.wasm.numThreads = 1`:
threads need `SharedArrayBuffer`, which needs COOP/COEP headers that neither the
Tauri dev server nor the production asset protocol sends. Single-threaded costs
about 3.5 ms per 80 ms of audio, so there is nothing to gain by fighting that.

## Updating

Re-run the `npm pack` above and copy the same three filenames from
`package/dist/`. If a future version renames the wasm or its glue file, the
names in `wakeword.js` (`ORT_DIR`, and `ort.env.wasm.wasmPaths`) have to move
with it — a mismatch only shows up at runtime, as a failure to create any
inference session.
