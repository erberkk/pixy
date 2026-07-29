// The voice assistant's turn loop, and the only module here that decides
// anything. The pieces it drives are each ignorant of the others:
//
//   mic.js       one microphone, one hop stream, many subscribers
//   wakeword.js  hop -> score, no policy
//   recorder.js  hop -> WAV, decides when you stopped talking
//   ai/voice.rs  WAV -> text -> answer -> audio
//
// One turn: wake word heard -> record until you stop -> transcribe -> answer ->
// speak. The microphone stays open across turns; only the subscriber changes,
// because re-opening the device per turn loses the first fraction of a second
// of speech and re-asks the OS for the mic each time.
import { invoke, listen } from "../../shared/tauri.js";
import { loadTunables, t } from "../../shared/tunables.js";
import { pushEvent, clearEvent } from "../pip/pipstate.js";
import { showTransientNotice } from "../notice/notice.js";
import { beep } from "../lib/sound.js";
import { closeMic, isOpen, onHop, openMic } from "./mic.js";
import { observeIdleHop, startRecording } from "./recorder.js";
import { feed, initWakeWord, resetBuffers } from "./wakeword.js";

// The timings this module reads (agreeing hops before a wake counts, the deaf
// period after a reply, how long a transcript or an error stays up) are all
// settings — see src-tauri/src/tunables.rs for what each one trades off.
//
// `idle` here means "listening for the wake word" — the assistant is on, it just
// isn't in a turn. `off` is the only state in which the microphone is closed.
let phase = "off";
let unsubscribe = null;
let confirmedHops = 0;
let threshold = 0.5;
let cooldownUntil = 0;
let activeRecorder = null;
// Serializes wake-word scoring: see handleHop for why hops must not overlap.
let scoreQueue = Promise.resolve();

function isVoiceRunning() {
  return phase !== "off";
}

// Two short rising notes on wake, one falling note when the turn ends without
// an answer. Audible feedback matters more here than anywhere else in the app:
// the wake word is probabilistic, so without a sound you cannot tell "it didn't
// hear me" from "it heard me and is thinking".
function chirpWake() {
  beep({ freq: 660, duration: 0.07, gain: 0.04 });
  beep({ freq: 990, duration: 0.09, gain: 0.04, delay: 0.07 });
}

function chirpGiveUp() {
  beep({ freq: 500, duration: 0.12, gain: 0.035 });
}

function showVoiceError(message) {
  clearEvent();
  showTransientNotice("state-voice_error", message, t("voice.error_notice_ms"));
}

/** Starts listening for the wake word. Safe to call when already running. */
async function startVoice() {
  if (phase !== "off") return;
  phase = "starting";
  try {
    await initWakeWord();
    await openMic();
  } catch (err) {
    phase = "off";
    // Closes the device again if getUserMedia succeeded but something after it
    // didn't — otherwise the mic indicator stays lit with nothing listening.
    await closeMic().catch(() => {});
    showVoiceError(err?.message || "Couldn't start the voice assistant.");
    return;
  }
  threshold = await readThreshold();
  resetBuffers();
  confirmedHops = 0;
  unsubscribe = onHop(handleHop);
  phase = "idle";
}

async function stopVoice() {
  if (phase === "off") return;
  phase = "off";
  unsubscribe?.();
  unsubscribe = null;
  activeRecorder?.cancel();
  activeRecorder = null;
  resetBuffers();
  clearEvent();
  await closeMic();
}

async function readThreshold() {
  const readiness = await invoke("get_voice_readiness").catch(() => null);
  return readiness?.threshold ?? 0.5;
}

// The single hop subscriber. Which branch it takes is the state machine: while
// idle it scores hops for the wake word, and during a turn it forwards them to
// the recorder instead. Scoring is deliberately *not* run during recording —
// your own speech would keep re-triggering it.
function handleHop(hop) {
  if (phase === "recording") {
    activeRecorder?.push(hop);
    return;
  }
  if (phase !== "idle") return;

  // The noise floor recorder.js compares speech against is measured here, from
  // the hops nobody is addressing the assistant in.
  observeIdleHop(hop);

  if (Date.now() < cooldownUntil) return;

  // feed() is async (ONNX), so hops can arrive while one is in flight. Dropping
  // the overlapping hop would corrupt the streaming buffers — every hop has to
  // go through in order — so instead the promise chain below is allowed to run
  // behind, which it comfortably can: 3.5ms of work per 80ms hop.
  scoreQueue = scoreQueue.then(() => scoreHop(hop)).catch(() => {});
}

async function scoreHop(hop) {
  if (phase !== "idle") return;
  const score = await feed(hop);
  if (score === null || phase !== "idle") return;

  if (score < threshold) {
    confirmedHops = 0;
    return;
  }
  confirmedHops++;
  if (confirmedHops < t("mic.hops_to_confirm")) return;
  confirmedHops = 0;
  runTurn();
}

async function runTurn() {
  phase = "recording";
  chirpWake();
  pushEvent("hearing", 30000);

  const recorder = startRecording();
  activeRecorder = recorder;
  const wav = await recorder.done;
  activeRecorder = null;
  // stopVoice() may have run while we were waiting on the recording.
  if (phase === "off") return;

  if (!wav) {
    chirpGiveUp();
    endTurn();
    return;
  }

  phase = "thinking";
  pushEvent("thinking", 120000, { title: "Thinking", sub: "working out an answer" });

  try {
    const transcript = await invoke("voice_transcribe", { audioBase64: await blobToBase64(wav) });
    if (phase === "off") return;
    if (!transcript.trim()) {
      // Whisper heard nothing intelligible — treat it the same as a wake with
      // no speech rather than sending an empty prompt to the model.
      chirpGiveUp();
      endTurn();
      return;
    }

    pushEvent("thinking", 120000, { title: "Thinking", sub: truncate(transcript) });
    const reply = await speakStreamingReply(transcript);
    if (phase === "off") return;

    // Recorded once the whole reply is known, and not awaited: the pill has
    // nothing to show for it, and a failure to log must not cost the user the
    // answer they just heard.
    invoke("record_voice_turn", { transcript, reply }).catch(() => {});

    // The transcript stays up briefly after the audio ends so there is a record
    // of what it thought you said — the most common thing to get wrong, and
    // otherwise invisible once the sound has stopped.
    pushEvent("speaking", t("voice.transcript_linger_ms"), { title: "You said", sub: truncate(transcript) });
    endTurn({ keepEvent: true });
  } catch (err) {
    if (phase === "off") return;
    showVoiceError(typeof err === "string" ? err : err?.message || "Voice assistant failed.");
    endTurn({ keepEvent: true });
  }
}

// Streams the reply and speaks it sentence by sentence, resolving with the full
// text once the last one has finished playing.
//
// Synthesis and playback are deliberately decoupled: a sentence's audio is
// requested the moment its text arrives, but sentences are played strictly in
// order.
//
// Requesting in parallel rather than one-at-a-time is measured, not assumed —
// the speech server turns two concurrent requests around in 6.3s versus 9.5s
// sequentially. That matters because synthesis is only barely faster than
// speech itself here (about 1.2s fixed cost plus 42ms per character, against
// 55ms per character of audio), so a later sentence that waits for the previous
// one to finish synthesizing can easily not be ready in time, leaving a silent
// gap mid-reply. Starting it early is what closes that gap.
//
// Bounded because the concurrency gain flattens out and an unbounded fan-out
// would only thrash what is already the slowest link in the turn. How far it is
// worth going is a property of the user's speech server, so it is a setting.
function speakStreamingReply(transcript) {
  const turnId = String(Date.now()) + "-" + Math.random().toString(36).slice(2, 8);
  // Read once per turn: changing it mid-reply would let more requests in flight
  // than the semaphore below has accounted for.
  const concurrency = t("speech.synth_concurrency");

  return new Promise((resolve, reject) => {
    let inFlight = 0;
    const waitingForSlot = [];
    const acquireSlot = () => {
      if (inFlight < concurrency) {
        inFlight++;
        return Promise.resolve();
      }
      return new Promise((r) => waitingForSlot.push(r));
    };
    const releaseSlot = () => {
      const next = waitingForSlot.shift();
      // Hand the slot straight over rather than decrementing and re-acquiring,
      // so the count can't drift.
      if (next) next();
      else inFlight--;
    };

    let playChain = Promise.resolve();
    let spokenAnything = false;
    let settled = false;
    const unlisteners = [];

    const cleanup = () => {
      // Awaited nowhere on purpose: listen() resolves with its unlistener, and a
      // turn must not be held open waiting for teardown.
      for (const p of unlisteners) p.then((off) => off()).catch(() => {});
      unlisteners.length = 0;
    };
    const finish = (err, text) => {
      if (settled) return;
      settled = true;
      cleanup();
      err ? reject(err) : resolve(text);
    };

    const enqueue = (text) => {
      // Started as soon as the text exists, not when the previous sentence's
      // audio is done, so synthesis overlaps both the generation still in
      // progress and whatever is currently playing.
      const audio = acquireSlot()
        .then(() => invoke("voice_speak", { text }))
        .finally(releaseSlot);
      // A rejection here is handled on the playback chain below; swallowing it
      // separately keeps one failed sentence from surfacing as an unhandled
      // rejection while the rest of the reply carries on.
      audio.catch(() => {});
      playChain = playChain
        .then(() => audio)
        .then((a) => {
          if (phase === "off") return;
          if (!spokenAnything) {
            spokenAnything = true;
            phase = "speaking";
          }
          pushEvent("speaking", 120000, { title: "Answering", sub: truncate(text) });
          // Field names come back exactly as ai/voice.rs declares them: Tauri
          // camelCases arguments on the way *in*, not values on the way out.
          return playAudio(a.audio_base64, a.mime);
        })
        .catch((err) => finish(err instanceof Error ? err : new Error(String(err))));
    };

    unlisteners.push(
      listen("voice-reply-sentence", (event) => {
        if (event.payload?.turn_id !== turnId || phase === "off") return;
        enqueue(event.payload.text);
      })
    );
    unlisteners.push(
      listen("voice-reply-done", (event) => {
        if (event.payload?.turn_id !== turnId) return;
        const full = event.payload.full_text;
        // Resolve only once the queued audio has actually finished, so the pill
        // doesn't drop out of "speaking" while it is still talking.
        playChain.then(() => finish(null, full)).catch(() => finish(null, full));
      })
    );
    unlisteners.push(
      listen("voice-reply-error", (event) => {
        if (event.payload?.turn_id !== turnId) return;
        finish(new Error(event.payload.error));
      })
    );

    // Fired after the listeners are registered. The events are emitted from a
    // Tauri command on its own thread, so anything emitted before this point
    // would have nobody listening.
    Promise.all(unlisteners)
      .then(() => invoke("voice_reply_stream", { turnId, transcript }))
      .catch((err) => finish(err instanceof Error ? err : new Error(String(err))));
  });
}

// Returns to listening for the wake word. resetBuffers() is the important part:
// the ~2s of audio the wake word was detected in is still in the streaming
// buffers, and would score above the threshold again on the very next hop.
function endTurn({ keepEvent = false } = {}) {
  resetBuffers();
  confirmedHops = 0;
  cooldownUntil = Date.now() + t("mic.cooldown_ms");
  if (!keepEvent) clearEvent();
  phase = "idle";
}

const truncate = (text, max = 60) => {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > max ? flat.slice(0, max - 1) + "…" : flat;
};

function blobToBase64(blob) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    // readAsDataURL rather than manually walking an ArrayBuffer: the built-in
    // encoder is native, and the "," split is safe because a data URL's header
    // never contains one.
    reader.onload = () => resolve(String(reader.result).split(",")[1]);
    reader.onerror = () => reject(new Error("Couldn't read the recording."));
    reader.readAsDataURL(blob);
  });
}

// Plays the spoken reply. Rejects if it could not actually be played, rather
// than resolving anyway: silently treating "no sound came out" as a completed
// answer is indistinguishable, from the outside, from the assistant ignoring
// you — which is exactly how the undefined-field bug above stayed invisible.
// The turn's own catch turns a rejection here into a visible notice.
function playAudio(base64, mime) {
  if (!base64) return Promise.reject(new Error("The speech server returned no audio to play."));
  return new Promise((resolve, reject) => {
    const audio = new Audio(`data:${mime};base64,${base64}`);
    let settled = false;
    // A hard ceiling so a stalled decode can't leave the pill in "speaking"
    // forever. Generous: it only has to beat the real duration, and the reply is
    // capped at a couple of sentences.
    const guard = setTimeout(() => finish(new Error("The reply didn't finish playing.")), 120000);
    function finish(err) {
      if (settled) return;
      settled = true;
      clearTimeout(guard);
      err ? reject(err) : resolve();
    }

    audio.onended = () => finish();
    audio.onerror = () => finish(new Error("Couldn't play the reply — the audio format wasn't accepted."));
    audio.play().catch((err) =>
      // The likely one here is the autoplay policy (NotAllowedError) — worth
      // naming, because it is a permission problem rather than a broken file.
      finish(
        new Error(
          err?.name === "NotAllowedError"
            ? "The system blocked audio playback for this window."
            : `Couldn't play the reply: ${err?.message || err}`
        )
      )
    );
  });
}

// Settings toggles voice_enabled in the config, and the mascot window is where
// the microphone lives — so it has to be told, rather than polling the config.
async function syncVoiceFromSettings() {
  const readiness = await invoke("get_voice_readiness").catch(() => null);
  if (!readiness) return;
  threshold = readiness.threshold;
  if (readiness.enabled && !isVoiceRunning()) {
    if (!readiness.stt_configured || !readiness.llm_configured || !readiness.tts_configured) {
      // Enabled but unusable: say so once here rather than opening the
      // microphone and failing at the end of the user's first sentence.
      showVoiceError("Voice needs an STT, LLM and TTS profile configured first.");
      return;
    }
    await startVoice();
  } else if (!readiness.enabled && isVoiceRunning()) {
    await stopVoice();
  }
}

window.addEventListener("DOMContentLoaded", () => {
  // Awaited before anything else: every threshold in this feature is a setting,
  // and opening the microphone with them unloaded would throw somewhere deep in
  // the hop handler instead of here.
  loadTunables()
    .then(syncVoiceFromSettings)
    .catch((err) =>
      // Deliberately not showVoiceError: that reads a tunable to decide how long
      // to stay up, and the one thing that can bring us here is tunables being
      // unreadable. This duration is a local choice for this one message, not a
      // second copy of that setting's default.
      showTransientNotice(
        "state-voice_error",
        err?.message || "Couldn't read the voice settings.",
        6000
      )
    );
  // Settings lives in its own webview, so flipping the toggle there can't call
  // into this one — the backend re-broadcasts the change and we react to it.
  // Same shape as signals.js: this module has no exports and is loaded purely
  // for these listeners.
  listen("voice-settings-changed", () => {
    syncVoiceFromSettings();
  });
});

// Releases the device on window close/reload. Without this, a reload leaves the
// old AudioContext holding the microphone until the webview is torn down.
window.addEventListener("beforeunload", () => {
  if (isOpen()) closeMic();
});
