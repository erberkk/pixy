// openWakeWord's inference chain for the bundled "hey pixy" model, running in
// the webview on onnxruntime-web. Three models in series:
//
//   audio (16kHz mono) -> melspectrogram -> embedding_model -> hey_pixy -> score
//
// Everything here is streaming: feed() takes one 80ms hop and returns that
// hop's score, so the caller never assembles windows itself. The shapes and
// hop arithmetic below were measured against these exact model files rather
// than taken from the openWakeWord docs — see the constants for what each one
// actually is.
//
// Cost is ~3.5ms per 80ms hop (measured), so this comfortably runs forever on
// the main thread; the audio thread only does the regrouping
// (capture-worklet.js).
//
// Deliberately free of any Tauri dependency: this file is pure inference, so
// the score threshold and everything else policy-shaped lives in voice.js.

// --- measured model geometry -------------------------------------------------
// The melspectrogram model emits (samples/160 - 3) frames of 32 mel bins, i.e.
// a 160-sample (10ms) hop with a 3-frame warm-up it cannot produce. So to get
// exactly the 8 new frames one 80ms audio hop is worth, it has to be fed the
// hop plus 3 hops of lookback: 1280 + 480 = 1760 samples -> [1,1,8,32].
const HOP_SAMPLES = 1280;
const MEL_LOOKBACK_SAMPLES = 480;
const MEL_WINDOW_SAMPLES = HOP_SAMPLES + MEL_LOOKBACK_SAMPLES;
const MEL_FRAMES_PER_HOP = 8;
const MEL_BINS = 32;
// The embedding model takes 76 mel frames (760ms) and returns one 96-value
// vector; the classifier takes 16 of those vectors.
const EMBED_MEL_FRAMES = 76;
const EMBED_FEATURES = 96;
const CLASSIFIER_FRAMES = 16;
// Total mel frames that must be retained to produce a full classifier window:
// 15 hops of stride plus the 76-frame window the oldest embedding needs.
const MEL_RING_FRAMES = (CLASSIFIER_FRAMES - 1) * MEL_FRAMES_PER_HOP + EMBED_MEL_FRAMES;

// openWakeWord scales the raw melspectrogram before the embedding model. Not a
// normalization we chose — it is baked into what the embedding model was
// trained on, so the value it expects is not negotiable.
const melTransform = (v) => v / 10 + 2;

// Resolved against this module's own URL, not the document's. fetch() and
// wasmPaths both resolve relative strings against the *document* (which is
// /mascot/index.html), so a plain "./models/" here would silently mean
// something different from what the directory layout suggests.
const MODEL_DIR = new URL("./models/", import.meta.url);
const ORT_DIR = new URL("../../vendor/onnxruntime/", import.meta.url);

let ort = null;
let melSession = null;
let embedSession = null;
let wakeSession = null;

// Ring buffers, kept as flat Float32Arrays with a write cursor rather than
// arrays-of-arrays: every consumer wants a contiguous tensor anyway, so
// shifting a JS array each hop would only add garbage to collect.
let audioLookback = new Float32Array(MEL_LOOKBACK_SAMPLES);
let melRing = new Float32Array(MEL_RING_FRAMES * MEL_BINS);
let melWritten = 0; // total frames ever written — also tells us when it's warm
let embedRing = new Float32Array(CLASSIFIER_FRAMES * EMBED_FEATURES);
let embedWritten = 0;

// The mel window is rebuilt per hop from lookback + new hop; allocated once.
const melInput = new Float32Array(MEL_WINDOW_SAMPLES);

function loadOrtRuntime() {
  if (window.ort) return Promise.resolve(window.ort);
  // onnxruntime-web ships as a classic script that assigns window.ort, so it is
  // injected rather than imported — the same reason vendor/xterm is a <script>
  // tag in the HTML. It is loaded here instead of in index.html so a user who
  // never turns the voice assistant on never pays the 11MB wasm download.
  return new Promise((resolve, reject) => {
    const script = document.createElement("script");
    script.src = new URL("ort.wasm.min.js", ORT_DIR).href;
    script.onload = () => resolve(window.ort);
    script.onerror = () => reject(new Error("Couldn't load the ONNX runtime."));
    document.head.appendChild(script);
  });
}

async function createSession(name) {
  const response = await fetch(new URL(name + ".onnx", MODEL_DIR));
  if (!response.ok) throw new Error(`Couldn't load ${name}.onnx (HTTP ${response.status}).`);
  const bytes = new Uint8Array(await response.arrayBuffer());
  return ort.InferenceSession.create(bytes);
}

let initPromise = null;

// Idempotent and safe to call from more than one place: the models take a
// moment to compile, and both the settings toggle and the app's own startup
// path can ask for them at once.
export function initWakeWord() {
  if (!initPromise) {
    initPromise = (async () => {
      ort = await loadOrtRuntime();
      ort.env.wasm.wasmPaths = ORT_DIR.href;
      // Threads need SharedArrayBuffer, which needs COOP/COEP headers that the
      // Tauri dev server and the production asset protocol don't send. At
      // 3.5ms/hop single-threaded there is nothing to gain by fighting that.
      ort.env.wasm.numThreads = 1;
      ort.env.logLevel = "error";
      [melSession, embedSession, wakeSession] = await Promise.all([
        createSession("melspectrogram"),
        createSession("embedding_model"),
        createSession("hey_pixy"),
      ]);
    })().catch((err) => {
      // Don't cache a failed init — a retry after the user fixes whatever
      // broke (or simply a transient fetch failure) should be able to succeed.
      initPromise = null;
      throw err;
    });
  }
  return initPromise;
}

// Drops all accumulated context. Called after a detection and whenever capture
// stops: without it, the ~2s of audio that contained the wake word is still
// sitting in the ring buffers and would immediately re-trigger on the next
// hop, and the tail of a conversation would leak into the next detection.
export function resetBuffers() {
  audioLookback.fill(0);
  melRing.fill(0);
  embedRing.fill(0);
  melWritten = 0;
  embedWritten = 0;
}

function appendMelFrames(data, frameCount) {
  for (let f = 0; f < frameCount; f++) {
    const slot = (melWritten % MEL_RING_FRAMES) * MEL_BINS;
    for (let b = 0; b < MEL_BINS; b++) {
      melRing[slot + b] = melTransform(data[f * MEL_BINS + b]);
    }
    melWritten++;
  }
}

// Copies the newest `EMBED_MEL_FRAMES` frames out of the ring in chronological
// order. The ring wraps, so the newest window is usually split across the end
// and the start of the array — this is why the models are fed a copy rather
// than a view.
function newestMelWindow() {
  const out = new Float32Array(EMBED_MEL_FRAMES * MEL_BINS);
  const firstFrame = melWritten - EMBED_MEL_FRAMES;
  for (let f = 0; f < EMBED_MEL_FRAMES; f++) {
    const src = ((firstFrame + f) % MEL_RING_FRAMES) * MEL_BINS;
    out.set(melRing.subarray(src, src + MEL_BINS), f * MEL_BINS);
  }
  return out;
}

function newestEmbedWindow() {
  const out = new Float32Array(CLASSIFIER_FRAMES * EMBED_FEATURES);
  const firstFrame = embedWritten - CLASSIFIER_FRAMES;
  for (let f = 0; f < CLASSIFIER_FRAMES; f++) {
    const src = ((firstFrame + f) % CLASSIFIER_FRAMES) * EMBED_FEATURES;
    out.set(embedRing.subarray(src, src + EMBED_FEATURES), f * EMBED_FEATURES);
  }
  return out;
}

/**
 * Feeds one 1280-sample (80ms) hop of 16kHz mono audio.
 * Returns the wake-word score for this hop, or null while the pipeline is
 * still filling up — the first ~2 seconds after a reset produce no score at
 * all, because a classifier window doesn't exist yet.
 */
export async function feed(hop) {
  if (!wakeSession || hop.length !== HOP_SAMPLES) return null;

  melInput.set(audioLookback, 0);
  melInput.set(hop, MEL_LOOKBACK_SAMPLES);
  audioLookback.set(hop.subarray(HOP_SAMPLES - MEL_LOOKBACK_SAMPLES));

  const melOut = await melSession.run({
    input: new ort.Tensor("float32", melInput, [1, MEL_WINDOW_SAMPLES]),
  });
  appendMelFrames(melOut.output.data, MEL_FRAMES_PER_HOP);
  if (melWritten < EMBED_MEL_FRAMES) return null;

  const embedOut = await embedSession.run({
    input_1: new ort.Tensor("float32", newestMelWindow(), [1, EMBED_MEL_FRAMES, MEL_BINS, 1]),
  });
  const slot = (embedWritten % CLASSIFIER_FRAMES) * EMBED_FEATURES;
  embedRing.set(embedOut.conv2d_19.data, slot);
  embedWritten++;
  if (embedWritten < CLASSIFIER_FRAMES) return null;

  const wakeOut = await wakeSession.run({
    "onnx::Flatten_0": new ort.Tensor("float32", newestEmbedWindow(), [1, CLASSIFIER_FRAMES, EMBED_FEATURES]),
  });
  return wakeOut["39"].data[0];
}

// The capture rate the models were trained at — mic.js opens the AudioContext
// at exactly this, and recorder.js writes it into its WAV header.
export const SAMPLE_RATE = 16000;
