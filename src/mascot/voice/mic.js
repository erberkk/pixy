// Owns the microphone: one AudioContext + one MediaStream + one worklet node,
// opened when the voice assistant is switched on and closed when it is switched
// off. Everything downstream (wake-word scoring, utterance recording) is a
// subscriber to the single hop stream this produces, because opening a second
// capture graph to record after the wake word fired would drop the first ~200ms
// of speech while the device spins up — and would ask the OS for the mic twice.
//
// The 16kHz sample rate is requested from the AudioContext itself rather than
// resampled by hand: WebView2 honours it exactly (verified), and its resampler
// is better than anything worth writing here.
import { SAMPLE_RATE } from "./wakeword.js";

const WORKLET_URL = new URL("./capture-worklet.js", import.meta.url);

let audioContext = null;
let stream = null;
let node = null;
const subscribers = new Set();

/** Adds a hop listener. Returns an unsubscribe function. */
export function onHop(fn) {
  subscribers.add(fn);
  return () => subscribers.delete(fn);
}

export function isOpen() {
  return !!node;
}

export async function openMic() {
  if (node) return;
  // Ask for the raw-ish signal: the browser's own noise suppression and AGC are
  // tuned for speech intelligibility on a call, and both distort the quiet
  // frames the wake-word model was trained on. Echo cancellation matters for a
  // different reason — without it, the assistant's own spoken reply coming out
  // of the speakers is heard as input and can re-trigger the wake word.
  stream = await navigator.mediaDevices.getUserMedia({
    audio: {
      channelCount: 1,
      echoCancellation: true,
      noiseSuppression: false,
      autoGainControl: false,
    },
  });

  audioContext = new AudioContext({ sampleRate: SAMPLE_RATE });
  await audioContext.audioWorklet.addModule(WORKLET_URL);
  node = new AudioWorkletNode(audioContext, "capture-processor");
  node.port.onmessage = (event) => {
    for (const fn of subscribers) fn(event.data);
  };
  audioContext.createMediaStreamSource(stream).connect(node);
  // Not connected to destination: that would play the microphone back through
  // the speakers. An AudioWorkletNode still runs with no output connected as
  // long as something feeds it.

  // A context created before any user gesture can start suspended; capture
  // would then silently never produce a hop.
  if (audioContext.state === "suspended") await audioContext.resume();
}

export async function closeMic() {
  node?.port && (node.port.onmessage = null);
  node?.disconnect();
  node = null;
  // Stopping the tracks is what actually releases the device and turns off the
  // OS microphone indicator — closing the AudioContext alone does not.
  stream?.getTracks().forEach((track) => track.stop());
  stream = null;
  const ctx = audioContext;
  audioContext = null;
  await ctx?.close().catch(() => {});
}
