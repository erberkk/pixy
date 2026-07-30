// The voice assistant's network side: turn recorded speech into text (STT),
// get an answer for it (LLM), and turn that answer back into audio (TTS).
//
// Wake-word detection, microphone capture and playback are all in the mascot
// window instead (src/mascot/voice/) — WebView2 grants getUserMedia and runs
// the ONNX wake-word chain in ~3.5ms per 80ms hop, so putting capture here
// would mean adding a native audio stack and an ONNX runtime to the build for
// no gain. What genuinely has to be on this side is exactly the part below:
// HTTP to servers whose CORS config we don't control.
//
// That CORS point is the whole reason these three commands exist rather than
// the webview calling the speech servers directly — same reasoning as
// ai/llm.rs's chat_completion. whisper.cpp's server and Kokoro-FastAPI (the
// two this was built against) send no Access-Control-Allow-Origin at all, so
// a fetch() from the webview fails before it reaches them. reqwest is not a
// browser and is not subject to CORS.
//
// URL handling is messier than it looks, and the two functions near the bottom
// of this file (api_base / transcribe_endpoints) are where that lives. Local
// speech servers do not agree on where their endpoints are: Kokoro serves
// /v1/audio/speech, while whisper.cpp's server has no OpenAI-compatible route at
// all and only answers on /inference. Both were verified by probing the actual
// servers, not assumed.
use base64::Engine;
use std::sync::Mutex;
use serde::Serialize;
use serde_json::json;

use crate::config::read_config;

// The speech-server timeouts (tunables.rs) default to long enough for a slow
// local model on a cold cache, and short enough that a wedged server gives the
// pill its idle face back instead of hanging in "thinking" forever. The reply
// has no entry there because the streaming path (run_chat_stream) carries its
// own timeout.
//
// Spoken replies are read aloud start to finish — there is no skimming and no
// scrollback — so length matters far more here than in the chat window. The
// prompt below caps it at one end and the reply-length ceiling at the other: an
// instruction the model can follow, and a hard limit for when it doesn't.
//
// Split into an identity line and the delivery contract so the user's own chat
// instructions can replace the former without touching the latter — see
// build_system_prompt. The delivery rules always come last: they are what makes
// a spoken reply usable at all, so nothing should be able to talk over them.
const VOICE_IDENTITY: &str = "You are a voice assistant.";
const VOICE_DELIVERY_RULES: &str = "Your reply will be read aloud, \
so answer in at most two short sentences of plain spoken language. \
No markdown, no lists, no code blocks, no emoji, no stage directions. \
If a question genuinely needs a long answer, give the single most useful sentence \
and offer to go deeper. \
Always reply in the same language the user spoke to you in. \
The text you receive came from speech recognition and may be garbled: if it does not \
read like a sensible request, say you did not catch that and ask them to repeat it, \
rather than answering the words literally.";

// How much of today's spoken conversation to carry as context is a setting
// (tunables.rs), defaulting small on purpose: replies are capped at two
// sentences, so a handful of exchanges is all a follow-up ever refers to, and
// every extra token is latency on a reply the user is waiting to hear.

// The user's custom instructions (Settings' chat personality) take the place of
// the generic identity line rather than being appended after it — two competing
// "you are X" statements is how a persona gets ignored. The delivery rules are
// always appended last.
//
// Applies to voice at all because it previously did not: the instructions were
// only ever read by the chat window, so anyone who set a personality found the
// voice assistant silently ignoring it.
fn build_system_prompt(instructions: &str) -> String {
    let identity = instructions.trim();
    let identity = if identity.is_empty() { VOICE_IDENTITY } else { identity };
    format!("{identity}\n\n{VOICE_DELIVERY_RULES}")
}

#[derive(Serialize)]
pub struct VoiceAudio {
    // base64 rather than Vec<u8> because Tauri's IPC serializes a byte vector
    // as a JSON array of numbers — one JSON integer per sample byte, which for
    // a few seconds of speech is megabytes of text to parse in the webview.
    pub audio_base64: String,
    pub mime: String,
}

#[derive(Serialize)]
pub struct VoiceReadiness {
    pub enabled: bool,
    pub threshold: f32,
    pub stt_configured: bool,
    pub tts_configured: bool,
    pub llm_configured: bool,
}

// Default wake-word score to fire at. 0.5 is openWakeWord's own default and
// the value the bundled hey_pixy model was validated at.
const DEFAULT_THRESHOLD: f32 = 0.5;

// One place that decides whether the voice loop may run, so the mascot doesn't
// have to reason about four separate settings sections to answer "should I open
// the microphone".
#[tauri::command]
pub fn get_voice_readiness(app: tauri::AppHandle) -> VoiceReadiness {
    let cfg = read_config(&app);
    let stt = active_stt(&app).is_some();
    let tts = active_tts(&app).is_some();
    let llm = {
        let p = crate::ai::llm::get_active_llm_profile(app.clone());
        !p.model.trim().is_empty() && !p.base_url.trim().is_empty()
    };
    VoiceReadiness {
        enabled: cfg.voice_enabled,
        threshold: cfg
            .voice_threshold
            // A stored 0 would silence the assistant by firing on everything,
            // and a stored 1 by never firing — treat out-of-range as unset
            // rather than trusting a hand-edited config.json.
            .filter(|t| *t > 0.0 && *t < 1.0)
            .unwrap_or(DEFAULT_THRESHOLD),
        stt_configured: stt,
        tts_configured: tts,
        llm_configured: llm,
    }
}

// Settings and the microphone live in different webviews, so a change made in
// one has to be announced for the other to act on it — the mascot can't be
// expected to poll the config file to notice it was switched on.
fn announce_change(app: &tauri::AppHandle) {
    use tauri::Emitter;
    let _ = app.emit("voice-settings-changed", ());
}

#[tauri::command]
pub fn set_voice_enabled(app: tauri::AppHandle, enabled: bool) {
    let mut cfg = read_config(&app);
    cfg.voice_enabled = enabled;
    crate::config::write_config(&app, &cfg);
    announce_change(&app);
}

#[tauri::command]
pub fn set_voice_threshold(app: tauri::AppHandle, threshold: f32) {
    let mut cfg = read_config(&app);
    cfg.voice_threshold = Some(threshold.clamp(0.05, 0.95));
    crate::config::write_config(&app, &cfg);
    announce_change(&app);
}

// Resolves the profile the user marked active, falling back to the first one —
// same precedence as ai/speech.rs's get_stt_settings, so what Settings shows as
// active is what actually gets called.
fn active_stt(app: &tauri::AppHandle) -> Option<crate::config::SttProfile> {
    let cfg = read_config(app);
    cfg.stt_active_profile_id
        .as_ref()
        .and_then(|id| cfg.stt_profiles.iter().find(|p| &p.id == id).cloned())
        .or_else(|| cfg.stt_profiles.first().cloned())
        .filter(|p| !p.base_url.trim().is_empty())
}

fn active_tts(app: &tauri::AppHandle) -> Option<crate::config::TtsProfile> {
    let cfg = read_config(app);
    cfg.tts_active_profile_id
        .as_ref()
        .and_then(|id| cfg.tts_profiles.iter().find(|p| &p.id == id).cloned())
        .or_else(|| cfg.tts_profiles.first().cloned())
        .filter(|p| !p.base_url.trim().is_empty())
}

// Where the OpenAI-compatible routes live for a configured server.
//
// The LLM section's convention is that the user pastes a base URL including the
// version prefix ("http://localhost:11434/v1"), but Settings' STT and TTS
// placeholders show a bare host:port — so those profiles were filled in without
// one, and appending /audio/speech to them lands on nothing. Rather than
// migrating the user's saved profiles or making them re-read a placeholder, the
// prefix is added here when the configured URL carries no path of its own, and
// left alone when it does (so someone who *did* type /v1 isn't sent to /v1/v1).
fn api_base(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    let authority_end = trimmed.find("://").map(|i| i + 3).unwrap_or(0);
    let has_path = trimmed[authority_end..].contains('/');
    if has_path {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1")
    }
}

// Transcription endpoints to try, in order. Two entries because whisper.cpp's
// server — the most common local choice, and the one this was tested against —
// implements no OpenAI-compatible route whatsoever and answers only on
// /inference at the root. Verified by probing it: /v1/audio/transcriptions
// returns 404 there, /inference returns 200.
fn transcribe_endpoints(base_url: &str) -> Vec<String> {
    let root = base_url.trim().trim_end_matches('/').to_string();
    vec![
        format!("{}/audio/transcriptions", api_base(base_url)),
        format!("{root}/inference"),
    ]
}

// Pulls the human-readable message out of an OpenAI-style error body, so the
// pill can show "model not found" instead of "HTTP 404".
fn describe_http_error(status: reqwest::StatusCode, body: &str) -> String {
    let msg = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message").and_then(|m| m.as_str()).map(str::to_string))
                .or_else(|| v.get("detail").map(|d| d.to_string()))
        })
        .unwrap_or_else(|| body.chars().take(200).collect());
    format!("HTTP {status}: {msg}")
}

// The transcription endpoint that last worked. Without this, every utterance
// spoken to a whisper.cpp server would pay for a wasted 404 against the
// OpenAI-compatible path first, since that server doesn't implement it.
static STT_ENDPOINT: Mutex<Option<String>> = Mutex::new(None);

/// Transcribes recorded speech. `audio_base64` is a 16kHz mono WAV built by the
/// mascot's voice/recorder.js.
#[tauri::command]
pub async fn voice_transcribe(app: tauri::AppHandle, audio_base64: String) -> Result<String, String> {
    crate::offload(move || {
        let profile = active_stt(&app).ok_or("No speech-to-text server is configured (Settings -> STT).")?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(audio_base64.as_bytes())
            .map_err(|e| format!("Couldn't decode the recording: {e}"))?;

        // Try the endpoint that worked last time first, then the rest. On the very
        // first call that's just the full candidate list in preference order.
        let remembered = STT_ENDPOINT.lock().unwrap().clone();
        let mut candidates = transcribe_endpoints(&profile.base_url);
        if let Some(url) = remembered {
            candidates.retain(|c| c != &url);
            candidates.insert(0, url);
        }

        let timeout = crate::tunables::secs(&app, crate::tunables::SPEECH_STT_TIMEOUT);
        let mut last_error = None;
        for url in candidates {
            match post_transcription(&url, &profile, bytes.clone(), timeout) {
                Ok(text) => {
                    *STT_ENDPOINT.lock().unwrap() = Some(url);
                    return Ok(clean_transcript(&text));
                }
                // Only a missing route is worth trying the next candidate for. A 500,
                // a timeout or a bad model name means we found the right endpoint and
                // it failed for a reason the user needs to see, not a reason to go
                // knocking on another URL.
                Err(TranscribeError::NotFound) => {
                    last_error = Some(format!(
                        "No transcription endpoint found on {}. Tried the OpenAI-compatible path and whisper.cpp's /inference.",
                        profile.base_url.trim_end_matches('/')
                    ));
                }
                Err(TranscribeError::Other(message)) => return Err(message),
            }
        }
        Err(last_error.unwrap_or_else(|| "Couldn't transcribe the recording.".into()))
})
    .await
}

enum TranscribeError {
    // 404 specifically: this server doesn't have this route, so a different one
    // is worth trying.
    NotFound,
    Other(String),
}

// Takes the timeout rather than an AppHandle so this stays a plain HTTP
// function, testable and free of Tauri.
fn post_transcription(
    url: &str,
    profile: &crate::config::SttProfile,
    bytes: Vec<u8>,
    timeout: std::time::Duration,
) -> Result<String, TranscribeError> {
    let part = reqwest::blocking::multipart::Part::bytes(bytes)
        // whisper.cpp's server picks its decoder from the filename extension
        // and rejects the upload outright without one.
        .file_name("speech.wav")
        .mime_str("audio/wav")
        .map_err(|e| TranscribeError::Other(e.to_string()))?;
    let mut form = reqwest::blocking::multipart::Form::new().part("file", part);
    if !profile.model.trim().is_empty() {
        // whisper.cpp ignores this (its model is fixed at launch); the
        // OpenAI-compatible servers require it.
        form = form.text("model", profile.model.clone());
    }
    if !profile.language.trim().is_empty() {
        // Both whisper.cpp and the OpenAI-compatible servers take this, and it
        // is the difference between a Turkish sentence being transcribed and
        // being guessed at in English. See SttProfile::language.
        form = form.text("language", profile.language.trim().to_string());
    }
    // Ask for bare text so there is no JSON shape to guess at across servers.
    form = form.text("response_format", "text");

    let mut req = reqwest::blocking::Client::new()
        .post(url)
        .timeout(timeout)
        .multipart(form);
    if !profile.api_key.trim().is_empty() {
        req = req.bearer_auth(&profile.api_key);
    }

    let resp = req
        .send()
        .map_err(|e| TranscribeError::Other(format!("Couldn't reach the speech-to-text server: {e}")))?;
    let status = resp.status();
    let body = resp.text().map_err(|e| TranscribeError::Other(e.to_string()))?;
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(TranscribeError::NotFound);
    }
    if !status.is_success() {
        return Err(TranscribeError::Other(describe_http_error(status, &body)));
    }

    // response_format=text should give plain text, but some servers ignore it
    // and answer with {"text": "..."} anyway — accept either rather than
    // handing the model a transcript with JSON punctuation in it.
    Ok(serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("text").and_then(|t| t.as_str()).map(str::to_string))
        .unwrap_or(body))
}

// Whisper emits bracketed annotations for non-speech audio ("[BLANK_AUDIO]",
// "(silence)", "[ Music ]"). Left in, they get sent to the model as if the user
// had said them; stripped, an accidental wake on a quiet room becomes an empty
// transcript, which the caller already treats as "never mind".
fn clean_transcript(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut depth = 0usize;
    for ch in raw.chars() {
        match ch {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

// --- sentence boundaries in a token stream ------------------------------------
//
// The point of splitting at all is latency: the first sentence can be spoken
// while the rest is still being generated, so the user hears an answer starting
// roughly a sentence-worth of tokens in rather than after the whole reply.
//
// Accumulates deltas and hands back each complete sentence. Getting a boundary
// slightly wrong costs a small pause in the middle of a phrase; getting it wrong
// on a decimal or an abbreviation makes the speech stutter oddly, so both are
// guarded against explicitly.
struct SentenceSplitter {
    buf: String,
    // Carried per splitter rather than read from the config here, so this stays
    // a pure function of its input — the tests pin an explicit value instead of
    // moving whenever the product default does.
    min_chars: usize,
}

// Below this, a sentence rides along with whatever follows instead of becoming
// its own utterance — so a reply either gets a head start with continuous audio,
// or no head start at all, but never a stall in the middle.
//
// Derived from the measured speech server rather than picked for tidiness.
// Splitting only pays if the first chunk takes longer to *play* than the second
// takes to *synthesize*; otherwise playback runs dry and the listener hears the
// assistant stop mid-answer, which is worse than having waited. Against the
// local Kokoro measured here — about 1.2s fixed cost plus 42ms per character to
// synthesize, against 55ms per character of resulting audio — a 250-character
// reply needs a first sentence of roughly 90 characters to break even, and is
// comfortable from 100. Measured sentences land at 107-129 characters, so the
// normal case still splits.
//
// This number is therefore a property of the speech server's throughput, not of
// language: a faster one lowers it, and if voice ever runs against a
// substantially different server this is the thing to re-derive.
// A model that never emits a terminator (a long run-on, or a list it was told
// not to produce) must still start speaking rather than buffering to the end.
// Never below twice the minimum: a run-on flush point under the minimum can
// never fire (see find_boundary's word-boundary filter), which would silently
// turn a raised minimum into "buffer the whole reply".
const RUNON_FLUSH_CHARS: usize = 220;

impl SentenceSplitter {
    fn new(min_chars: usize) -> Self {
        Self {
            buf: String::new(),
            min_chars,
        }
    }

    fn runon_flush_at(&self) -> usize {
        RUNON_FLUSH_CHARS.max(self.min_chars * 2)
    }

    /// Feeds one streamed delta, returning any sentences it completed.
    fn push(&mut self, delta: &str) -> Vec<String> {
        self.buf.push_str(delta);
        let mut out = Vec::new();
        while let Some(cut) = self.find_boundary() {
            let rest = self.buf.split_off(cut);
            let sentence = std::mem::replace(&mut self.buf, rest);
            let sentence = sentence.trim().to_string();
            if !sentence.is_empty() {
                out.push(sentence);
            }
        }
        out
    }

    /// Whatever is left when the stream ends — the last sentence usually has no
    /// trailing whitespace to have triggered a boundary.
    fn finish(&mut self) -> Option<String> {
        let tail = std::mem::take(&mut self.buf).trim().to_string();
        (!tail.is_empty()).then_some(tail)
    }

    // Byte index just past the end of the first complete sentence, if any.
    fn find_boundary(&self) -> Option<usize> {
        let bytes: Vec<(usize, char)> = self.buf.char_indices().collect();
        for (i, (idx, ch)) in bytes.iter().enumerate() {
            let terminator = matches!(ch, '.' | '!' | '?' | '…' | '\n');
            if !terminator {
                continue;
            }
            let end = idx + ch.len_utf8();
            if end < self.min_chars {
                continue;
            }
            // A '.' between two digits is a decimal point ("3.5"), not an end.
            if *ch == '.' {
                let prev = i.checked_sub(1).map(|p| bytes[p].1);
                let next = bytes.get(i + 1).map(|(_, c)| *c);
                if prev.is_some_and(|c| c.is_ascii_digit()) && next.is_some_and(|c| c.is_ascii_digit()) {
                    continue;
                }
                // A single capital letter before the dot is an initial ("J. Smith").
                if prev.is_some_and(|c| c.is_uppercase())
                    && i.checked_sub(2).map(|p| bytes[p].1).is_none_or(|c| !c.is_alphabetic())
                {
                    continue;
                }
            }
            // Only a terminator actually followed by a break ends a sentence —
            // otherwise "e.g" would split mid-word. End-of-buffer is not a
            // boundary yet: the next delta may continue the word.
            match bytes.get(i + 1).map(|(_, c)| *c) {
                Some(next) if next.is_whitespace() => return Some(end),
                // Closing punctuation may sit between the terminator and the space.
                Some('"') | Some('\'') | Some(')') | Some('»') | Some('”') => {
                    if bytes.get(i + 2).map(|(_, c)| c.is_whitespace()) == Some(true) {
                        return Some(bytes[i + 2].0);
                    }
                }
                _ => {}
            }
        }
        // No terminator, but too much buffered to keep waiting — break at the
        // last word boundary so a word is never cut in half.
        if self.buf.len() > self.runon_flush_at() {
            return self.buf.rfind(char::is_whitespace).filter(|i| *i >= self.min_chars);
        }
        None
    }
}

/// Answers a transcript with the active LLM profile and emits each sentence as
/// soon as it is complete, so the caller can start speaking the first one while
/// the rest is still being generated.
///
/// Streams (rather than returning one string) purely for that latency, and
/// carries today's spoken conversation as context so a follow-up question has
/// something to refer to — without it, every turn started from nothing, so
/// "and what about tomorrow?" was unanswerable.
///
/// Events, all carrying `turn_id` so a reply from an abandoned turn can be
/// ignored: `voice-reply-sentence` { turn_id, index, text },
/// `voice-reply-done` { turn_id, full_text }, `voice-reply-error` { turn_id, error }.
#[tauri::command]
pub async fn voice_reply_stream(window: tauri::Window, app: tauri::AppHandle, turn_id: String, transcript: String) {
    crate::offload(move || {
        use tauri::Emitter;

        let profile = crate::ai::llm::get_active_llm_profile(app.clone());
        if profile.model.trim().is_empty() || profile.base_url.trim().is_empty() {
            let _ = window.emit(
                "voice-reply-error",
                json!({ "turn_id": turn_id, "error": "No language model is configured (Settings -> LLM)." }),
            );
            return;
        }

        let instructions = read_config(&app).chat_instructions.unwrap_or_default();
        let mut messages = vec![json!({
            "role": "system",
            "content": build_system_prompt(&instructions),
        })];

        // Anything said in an earlier conversation that bears on this question —
        // usually nothing, which is the point. Added as its own system message,
        // ahead of today's spoken history, so the model can tell "something you were
        // told weeks ago" from "what we have been saying just now".
        // Today's spoken conversation is excluded: it is already carried as history
        // just below, and recalling it would repeat it back at the model.
        let recalled = crate::ai::recall::context_for(
            &app,
            &transcript,
            &profile.base_url,
            &crate::ai::chat::todays_voice_chat_id(),
        );
        if !recalled.block.is_empty() {
            messages.push(json!({"role": "system", "content": recalled.block}));
        }
        let history = crate::ai::chat::recent_voice_turns(
            &app,
            crate::tunables::int(&app, crate::tunables::VOICE_HISTORY_MESSAGES) as usize,
            crate::tunables::int(&app, crate::tunables::VOICE_HISTORY_CHARS) as usize,
        );
        for past in history {
            messages.push(json!({"role": past.role, "content": past.content}));
        }
        messages.push(json!({"role": "user", "content": transcript.trim()}));

        let mut splitter = SentenceSplitter::new(
            crate::tunables::int(&app, crate::tunables::SPEECH_MIN_SENTENCE_CHARS) as usize,
        );
        let mut index = 0usize;
        let endpoint = crate::ai::llm::ChatEndpoint {
            base_url: &profile.base_url,
            model: &profile.model,
            api_key: &profile.api_key,
            think: profile.think,
            max_tokens: crate::tunables::int(&app, crate::tunables::VOICE_MAX_TOKENS) as u32,
        };
        let result = crate::ai::llm::run_chat_stream(
            &endpoint,
            &messages,
            // No tools for the spoken path, deliberately. A tool round is a
            // second full request — measured at ~19s on this machine's model —
            // and a voice assistant that goes silent that long has failed at
            // the one thing it is for. Typed chat pays that cost willingly.
            &[],
            &mut |delta| {
                for sentence in splitter.push(delta) {
                    let _ = window.emit(
                        "voice-reply-sentence",
                        json!({ "turn_id": &turn_id, "index": index, "text": sentence }),
                    );
                    index += 1;
                }
            },
        );

        match result {
            Ok(full) => {
                if let Some(tail) = splitter.finish() {
                    let _ = window.emit(
                        "voice-reply-sentence",
                        json!({ "turn_id": &turn_id, "index": index, "text": tail }),
                    );
                }
                let cleaned = full.text.trim().to_string();
                if cleaned.is_empty() {
                    // An empty answer would otherwise end the turn silently, which
                    // is indistinguishable from the assistant ignoring you.
                    let _ = window.emit(
                        "voice-reply-error",
                        json!({ "turn_id": turn_id, "error": "The model returned an empty answer." }),
                    );
                    return;
                }
                let _ = window.emit("voice-reply-done", json!({ "turn_id": turn_id, "full_text": cleaned }));
            }
            Err(error) => {
                let _ = window.emit("voice-reply-error", json!({ "turn_id": turn_id, "error": error }));
            }
        }
})
    .await
}

/// Renders text to speech and hands the audio back for the webview to play.
#[tauri::command]
pub async fn voice_speak(app: tauri::AppHandle, text: String) -> Result<VoiceAudio, String> {
    crate::offload(move || {
        let profile = active_tts(&app).ok_or("No text-to-speech server is configured (Settings -> TTS).")?;
        let url = format!("{}/audio/speech", api_base(&profile.base_url));

        let mut payload = json!({
            "input": text,
            // WAV rather than the OpenAI default of MP3: every local server in
            // this space can emit WAV, and it needs no decoder work on our side.
            "response_format": "wav",
        });
        if !profile.model.trim().is_empty() {
            payload["model"] = json!(profile.model);
        }
        if !profile.voice.trim().is_empty() {
            payload["voice"] = json!(profile.voice);
        }

        let mut req = reqwest::blocking::Client::new()
            .post(&url)
            .timeout(crate::tunables::secs(&app, crate::tunables::SPEECH_TTS_TIMEOUT))
            .json(&payload);
        if !profile.api_key.trim().is_empty() {
            req = req.bearer_auth(&profile.api_key);
        }

        let resp = req
            .send()
            .map_err(|e| format!("Couldn't reach the text-to-speech server: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(describe_http_error(status, &body));
        }

        // Content-Type is read before consuming the body, and defaulted rather
        // than trusted — some servers answer WAV bytes with a JSON content type.
        let mime = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .filter(|v| v.starts_with("audio/"))
            .unwrap_or("audio/wav")
            .to_string();
        let bytes = resp
            .bytes()
            .map_err(|e| format!("Couldn't read the audio back: {e}"))?;
        if bytes.is_empty() {
            return Err("The text-to-speech server returned no audio.".into());
        }

        Ok(VoiceAudio {
            audio_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
            mime,
        })
})
    .await
}

#[cfg(test)]
mod tests {
    use super::{api_base, build_system_prompt, clean_transcript, transcribe_endpoints, SentenceSplitter};

    // The minimum the fixtures below were written against. Pinned rather than
    // read from the settings default so these tests keep testing the splitter's
    // boundary rules, instead of turning into a test of whatever the product
    // default happens to be.
    const TEST_MIN_CHARS: usize = 100;

    // Feeds text one character at a time, which is the worst case a token stream
    // can present: every boundary check runs with the buffer cut mid-word.
    fn split_char_by_char(text: &str) -> Vec<String> {
        let mut sp = SentenceSplitter::new(TEST_MIN_CHARS);
        let mut out = Vec::new();
        for ch in text.chars() {
            out.extend(sp.push(&ch.to_string()));
        }
        out.extend(sp.finish());
        out
    }

    // Sentence lengths here match what a real spoken reply looks like (the model
    // is told to answer in two sentences, which lands around 100-140 characters
    // each). Short fixtures would only exercise the merge path below.
    #[test]
    fn speaks_the_first_sentence_before_the_rest_arrives() {
        let first = "The build passed on every platform we support, so the release branch is ready to go whenever you are.";
        let second = "I can start the deploy now, or hold off until you have had a chance to read through the changelog.";
        let out = split_char_by_char(&format!("{first} {second}"));
        assert_eq!(out, vec![first, second]);
    }

    #[test]
    fn merges_a_short_leading_sentence_into_the_next() {
        // Splitting here would buy a fragment too brief to cover the next
        // chunk's synthesis, which the listener hears as a stall — see the
        // minimum-chunk setting in tunables.rs.
        let out = split_char_by_char("Sure. I will run the tests and tell you what breaks.");
        assert_eq!(out, vec!["Sure. I will run the tests and tell you what breaks."]);
    }

    // The run-on flush point has to stay above the minimum. A fixed 220 would
    // mean a user-raised minimum could never produce a boundary at all (see
    // find_boundary's word-boundary filter), silently turning "longer chunks"
    // into "buffer the entire reply and speak it as one block".
    #[test]
    fn the_run_on_flush_point_stays_above_the_minimum() {
        assert_eq!(SentenceSplitter::new(TEST_MIN_CHARS).runon_flush_at(), 220);
        assert_eq!(SentenceSplitter::new(300).runon_flush_at(), 600);
    }

    #[test]
    fn a_raised_minimum_still_flushes_a_run_on() {
        let run_on = "and then ".repeat(80); // 720 characters, no terminator anywhere
        let mut sp = SentenceSplitter::new(300);
        let out = sp.push(&run_on);
        assert!(!out.is_empty(), "a terminator-free reply never started speaking");
        assert!(out[0].len() >= 300);
    }

    #[test]
    fn does_not_break_on_a_decimal_point() {
        // "3.5" mid-sentence must not become its own utterance — that is what
        // makes streamed speech stutter.
        let out = split_char_by_char("The version you want is 3.5 for now.");
        assert_eq!(out, vec!["The version you want is 3.5 for now."]);
    }

    #[test]
    fn does_not_break_on_an_initial() {
        let out = split_char_by_char("That commit was by J. Smith yesterday.");
        assert_eq!(out, vec!["That commit was by J. Smith yesterday."]);
    }

    #[test]
    fn keeps_closing_punctuation_with_its_sentence() {
        // Built from two named halves rather than one wrapped literal: Rust's
        // line continuation would fold the indentation into the string, which
        // silently changes the very whitespace this boundary rule depends on.
        let first = "The reviewer went through the whole diff this morning and wrote \"this looks fine to me, go ahead and ship it.\"";
        let second = "Nobody has raised anything against it since then, so as far as I can tell we are clear to merge it now.";
        let out = split_char_by_char(&format!("{first} {second}"));
        assert_eq!(out.len(), 2, "{out:?}");
        // The quote belongs to the sentence it closes, not to the next one.
        assert_eq!(out[0], first);
        assert_eq!(out[1], second);
    }

    #[test]
    fn never_leaves_the_tail_unspoken() {
        // No trailing space or terminator: finish() must still emit it, or the
        // last thing the assistant says is silently dropped.
        let out = split_char_by_char("Everything is ready to go");
        assert_eq!(out, vec!["Everything is ready to go"]);
    }

    #[test]
    fn starts_speaking_even_without_a_terminator() {
        // A run-on reply must not buffer to the end; it breaks at a word
        // boundary so no word is cut in half.
        let long = "word ".repeat(80);
        let out = split_char_by_char(&long);
        assert!(out.len() > 1, "expected an early flush, got {} chunk(s)", out.len());
        assert!(out.iter().all(|s| !s.is_empty()));
        assert_eq!(out.join(" ").split_whitespace().count(), 80, "no words lost or split");
    }

    #[test]
    fn does_not_emit_a_fragment_for_a_stray_terminator() {
        let out = split_char_by_char("Hi. The deployment finished a moment ago.");
        assert_eq!(out.len(), 1, "{out:?}");
    }

    #[test]
    fn custom_instructions_replace_the_identity_but_never_the_delivery_rules() {
        let plain = build_system_prompt("");
        assert!(plain.starts_with("You are a voice assistant."));
        assert!(plain.contains("read aloud"));

        let persona = build_system_prompt("  You are Pixy, blunt and very brief.  ");
        assert!(persona.starts_with("You are Pixy"), "{persona}");
        // The generic identity must be gone, so the two don't contradict.
        assert!(!persona.contains("You are a voice assistant."), "{persona}");
        // ...but the delivery contract survives, and comes last.
        assert!(persona.contains("read aloud"));
        assert!(persona.find("read aloud").unwrap() > persona.find("You are Pixy").unwrap());
    }

    #[test]
    fn adds_the_version_prefix_only_when_the_url_has_no_path() {
        // The two shapes the user's own saved profiles actually have.
        assert_eq!(api_base("http://127.0.0.1:8091"), "http://127.0.0.1:8091/v1");
        assert_eq!(api_base("http://127.0.0.1:8091/"), "http://127.0.0.1:8091/v1");
        // Already versioned (the LLM section's convention) — must not double up.
        assert_eq!(api_base("http://localhost:11434/v1"), "http://localhost:11434/v1");
        assert_eq!(api_base("http://localhost:11434/v1/"), "http://localhost:11434/v1");
        // A server mounted under a path keeps it.
        assert_eq!(api_base("https://host/openai"), "https://host/openai");
        // A bare host with no scheme still shouldn't have its host read as a path.
        assert_eq!(api_base("localhost:8091"), "localhost:8091/v1");
    }

    #[test]
    fn falls_back_to_whisper_cpp_native_route() {
        assert_eq!(
            transcribe_endpoints("http://127.0.0.1:8090"),
            vec![
                "http://127.0.0.1:8090/v1/audio/transcriptions".to_string(),
                "http://127.0.0.1:8090/inference".to_string(),
            ]
        );
    }

    #[test]
    fn strips_whisper_non_speech_annotations() {
        assert_eq!(clean_transcript("[BLANK_AUDIO]"), "");
        assert_eq!(clean_transcript(" (silence) "), "");
        assert_eq!(clean_transcript("[ Music ] what time is it"), "what time is it");
        assert_eq!(clean_transcript("what time\n is it?"), "what time is it?");
    }
}
