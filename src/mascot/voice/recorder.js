// Records one utterance after the wake word fires, and decides when the person
// has stopped talking.
//
// End-of-speech is detected by short-term energy (RMS) against a noise floor
// measured from the room, not a fixed threshold: a fixed one is either deaf in a
// noisy room or never closes in a quiet one. The floor comes from the hops
// *before* the wake word, which are the best available sample of "this room with
// nobody addressing the assistant".
//
// Deliberately not webrtcvad or Silero: this runs after a wake word, on a
// single known-active speaker, where the only question is "still talking?".
// Energy answers that, and it costs nothing next to another ONNX model.
import { SAMPLE_RATE } from "./wakeword.js";

// Speech has to exceed the measured floor by this factor to count as voiced.
// 2.5x is roughly 8dB — comfortably above the drift of a room's own noise, and
// below the margin of even quiet speech.
const SPEECH_OVER_FLOOR = 2.5;
// A floor is never treated as lower than this, so in a near-silent room the
// factor above doesn't turn breathing into speech.
const MIN_FLOOR_RMS = 0.002;
const SILENCE_TO_END_MS = 900; // pause that ends the utterance
const MIN_UTTERANCE_MS = 400; // shorter than this is a click or a cough
const MAX_UTTERANCE_MS = 15000; // hard stop so a stuck-open mic can't record forever
// Speech is only accepted as *started* once this much voiced audio has arrived;
// a single loud hop is a door slam, not a sentence.
const SPEECH_TO_START_MS = 160;
// If the wake word fired but nobody actually said anything, give up rather than
// sit in recording state.
const NO_SPEECH_TIMEOUT_MS = 2500;

const rms = (hop) => {
  let sum = 0;
  for (let i = 0; i < hop.length; i++) sum += hop[i] * hop[i];
  return Math.sqrt(sum / hop.length);
};

// Rolling noise floor, fed continuously by voice.js while it listens for the
// wake word so a recording always starts with a current estimate.
let floorRms = MIN_FLOOR_RMS;

export function observeIdleHop(hop) {
  const level = rms(hop);
  // Asymmetric: adapt down quickly (the fan stopped, someone left the room) and
  // up slowly, so a burst of speech doesn't raise the floor to the point that
  // the rest of the sentence falls below it.
  const alpha = level < floorRms ? 0.2 : 0.02;
  floorRms = floorRms * (1 - alpha) + level * alpha;
}

function currentFloor() {
  return Math.max(floorRms, MIN_FLOOR_RMS);
}

const msToHops = (ms) => Math.round(ms / ((hopSamples() / SAMPLE_RATE) * 1000));
// Read from the hop the caller actually gives us rather than re-deriving the
// constant, so this stays correct if the hop size ever changes.
let observedHopSamples = 1280;
function hopSamples() {
  return observedHopSamples;
}

/**
 * Collects hops until the speaker stops. Returns a recorder handle whose
 * `push(hop)` is fed from the same mic stream the wake word uses, and whose
 * `done` promise resolves with a 16kHz mono WAV Blob — or null if there was
 * nothing worth transcribing.
 */
export function startRecording() {
  const hops = [];
  const threshold = currentFloor() * SPEECH_OVER_FLOOR;

  let resolveDone;
  const done = new Promise((resolve) => {
    resolveDone = resolve;
  });

  let finished = false;
  let voicedHops = 0;
  let silentHops = 0;
  let started = false;
  let totalHops = 0;

  function finish(accept) {
    if (finished) return;
    finished = true;
    if (!accept || hops.length * ((hopSamples() / SAMPLE_RATE) * 1000) < MIN_UTTERANCE_MS) {
      resolveDone(null);
      return;
    }
    resolveDone(encodeWav(hops));
  }

  return {
    done,
    push(hop) {
      if (finished) return;
      observedHopSamples = hop.length;
      totalHops++;
      hops.push(hop);

      const voiced = rms(hop) > threshold;
      if (voiced) {
        voicedHops++;
        silentHops = 0;
        if (voicedHops >= msToHops(SPEECH_TO_START_MS)) started = true;
      } else {
        silentHops++;
      }

      if (started && silentHops >= msToHops(SILENCE_TO_END_MS)) {
        // Trailing silence is trimmed: it is dead weight in the upload and,
        // for whisper, an invitation to hallucinate filler into the gap.
        hops.length = Math.max(0, hops.length - silentHops);
        finish(true);
      } else if (!started && totalHops >= msToHops(NO_SPEECH_TIMEOUT_MS)) {
        finish(false);
      } else if (totalHops >= msToHops(MAX_UTTERANCE_MS)) {
        finish(started);
      }
    },
    cancel() {
      finish(false);
    },
  };
}

// 16-bit PCM WAV. Written by hand rather than via MediaRecorder because
// MediaRecorder produces webm/opus from its own separate capture of the stream —
// we already have the exact samples the wake word ran on, and whisper servers
// take WAV without transcoding.
function encodeWav(hops) {
  const sampleCount = hops.reduce((n, hop) => n + hop.length, 0);
  const dataBytes = sampleCount * 2;
  const buffer = new ArrayBuffer(44 + dataBytes);
  const view = new DataView(buffer);

  const ascii = (offset, text) => {
    for (let i = 0; i < text.length; i++) view.setUint8(offset + i, text.charCodeAt(i));
  };

  ascii(0, "RIFF");
  view.setUint32(4, 36 + dataBytes, true);
  ascii(8, "WAVE");
  ascii(12, "fmt ");
  view.setUint32(16, 16, true); // fmt chunk size
  view.setUint16(20, 1, true); // PCM
  view.setUint16(22, 1, true); // mono
  view.setUint32(24, SAMPLE_RATE, true);
  view.setUint32(28, SAMPLE_RATE * 2, true); // byte rate
  view.setUint16(32, 2, true); // block align
  view.setUint16(34, 16, true); // bits per sample
  ascii(36, "data");
  view.setUint32(40, dataBytes, true);

  let offset = 44;
  for (const hop of hops) {
    for (let i = 0; i < hop.length; i++) {
      // Clamp before scaling: a sample slightly outside [-1,1] (possible after
      // any gain in the capture chain) would otherwise wrap to full-scale
      // opposite-sign noise.
      const s = Math.max(-1, Math.min(1, hop[i]));
      view.setInt16(offset, s < 0 ? s * 0x8000 : s * 0x7fff, true);
      offset += 2;
    }
  }
  return new Blob([buffer], { type: "audio/wav" });
}
