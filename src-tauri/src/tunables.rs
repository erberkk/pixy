// Every value in the app a user might legitimately need to change on their own
// machine, in one declarative registry.
//
// Why a registry instead of ~30 more fields on AppConfig: each of these is read
// from exactly one place, and the settings window has to render an input for
// every one of them. Done by hand that would be four things to keep in sync per
// number (a config field, a reader, an HTML input, a JS binding) — and the
// frontend would need its own copy of every default, which is precisely how two
// copies of a number drift apart. Here the schema below is the only place a
// tunable is described: the Rust readers, the settings form and the frontend's
// values all derive from it.
//
// What deliberately does NOT belong here: anything that is a property of a file
// or a protocol rather than of the user's machine. The mel/embedding geometry in
// mascot/voice/wakeword.js is fixed by the bundled ONNX models, and
// TERMINAL_POOL in ui/windows.rs is fixed by the window list in
// tauri.conf.json — exposing either would only hand the user a way to break the
// app in a way that looks like a bug rather than a setting.
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use serde::Serialize;
use serde_json::Value;
use tauri::Emitter;

use crate::config::{read_config, write_config};

#[derive(Clone, Copy)]
pub enum Kind {
    Int {
        min: i64,
        max: i64,
        default: i64,
    },
    Float {
        min: f64,
        max: f64,
        step: f64,
        default: f64,
    },
    /// A comma-separated list of case-insensitive substrings, matched against
    /// names the OS gives us. A list rather than the regex this replaced: a
    /// user-supplied regex can fail to compile, and "what do I type here" has a
    /// much better answer for a list than for a pattern.
    Names {
        default: &'static str,
    },
    /// On or off. Rendered as a switch rather than a 0/1 box — a setting whose
    /// only two values are yes and no should not ask the user to type a number.
    Toggle {
        default: bool,
    },
    /// Free text, stored exactly as typed. Distinct from `Names`, which folds
    /// and reorders what it is given: a URL or a model name has to survive
    /// unchanged.
    Text {
        default: &'static str,
        placeholder: &'static str,
    },
}

pub struct Tunable {
    pub id: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    /// Shown after the input ("ms", "%", …). Empty when the label already says it.
    pub unit: &'static str,
    pub kind: Kind,
    /// True when the running app cannot pick a new value up on its own, so the
    /// settings form has to say so instead of implying it took effect.
    pub restart: bool,
}

// The macro exists for one reason: it emits the `&'static str` id constant and
// the registry entry from the same literal, so a caller using MIC_SILENCE_TO_END
// cannot be reading a key that no longer exists in the schema. Referring to
// tunables by bare string literals would make that a silent
// wrong-value-at-runtime instead of a compile error.
macro_rules! tunables {
    ($(
        $const_name:ident = $id:literal, $group:literal, $label:literal, $unit:literal,
        $kind:expr, restart: $restart:literal, $help:literal;
    )*) => {
        // Roughly half of these are read only by the frontend, which looks them
        // up by id in the get_tunables payload rather than through a Rust
        // constant — so "never used" here means "used from JavaScript", not
        // "dead". Dropping the unused ones would give the settings form entries
        // Rust has no name for, which is worse.
        $( #[allow(dead_code)] pub const $const_name: &str = $id; )*

        pub const TUNABLES: &[Tunable] = &[$(
            Tunable {
                id: $id,
                group: $group,
                label: $label,
                unit: $unit,
                kind: $kind,
                restart: $restart,
                help: $help,
            }
        ),*];
    };
}

tunables! {
    // --- Network -------------------------------------------------------------
    EVENT_PORT = "network.event_port", "Network", "Claude Code event port", "",
        Kind::Int { min: 1024, max: 65535, default: 47623 },
        restart: true,
        "The local port Claude Code's hooks post to. Only worth changing if \
         something else on this machine already uses it — and the hook commands \
         in your Claude Code settings have to be updated to match.";

    // --- Microphone ----------------------------------------------------------
    MIC_SPEECH_OVER_FLOOR = "mic.speech_over_floor", "Microphone",
        "Speech loudness over room noise", "×",
        Kind::Float { min: 1.2, max: 6.0, step: 0.1, default: 2.5 },
        restart: false,
        "How much louder than the measured room noise a sound has to be to count \
         as speech. Lower it in a quiet room if the assistant cuts you off \
         mid-sentence; raise it in a noisy one if it never stops recording.";

    MIC_MIN_FLOOR_RMS = "mic.min_floor_rms", "Microphone",
        "Noise floor lower limit", "",
        Kind::Float { min: 0.0002, max: 0.02, step: 0.0002, default: 0.002 },
        restart: false,
        "The room's noise level is never treated as quieter than this, so in a \
         near-silent room breathing doesn't get promoted to speech.";

    MIC_SPEECH_TO_START = "mic.speech_to_start_ms", "Microphone",
        "Speech needed to start", "ms",
        Kind::Int { min: 40, max: 1000, default: 160 },
        restart: false,
        "How much continuous speech has to arrive before recording is treated as \
         really under way. Rejects a door slam, which is loud but not a sentence.";

    MIC_SILENCE_TO_END = "mic.silence_to_end_ms", "Microphone",
        "Pause that ends your turn", "ms",
        Kind::Int { min: 200, max: 3000, default: 900 },
        restart: false,
        "Silence this long ends the recording. Raise it if you pause to think \
         mid-sentence and get cut off; lower it if the assistant feels slow to \
         start answering.";

    MIC_MIN_UTTERANCE = "mic.min_utterance_ms", "Microphone",
        "Shortest usable recording", "ms",
        Kind::Int { min: 100, max: 2000, default: 400 },
        restart: false,
        "Anything shorter than this is thrown away rather than transcribed — it \
         is a cough or a click, not a request.";

    MIC_MAX_UTTERANCE = "mic.max_utterance_ms", "Microphone",
        "Longest recording", "ms",
        Kind::Int { min: 3000, max: 60000, default: 15000 },
        restart: false,
        "A hard stop, so a microphone that never goes quiet can't record forever.";

    MIC_NO_SPEECH_TIMEOUT = "mic.no_speech_timeout_ms", "Microphone",
        "Give up if nobody speaks", "ms",
        Kind::Int { min: 500, max: 10000, default: 2500 },
        restart: false,
        "If the wake word fired but no speech follows within this, the turn is \
         abandoned instead of sitting in recording state.";

    MIC_HOPS_TO_CONFIRM = "mic.hops_to_confirm", "Microphone",
        "Agreeing frames to accept the wake word", "× 80ms",
        Kind::Int { min: 1, max: 5, default: 2 },
        restart: false,
        "Consecutive 80ms frames that must clear the sensitivity threshold. \
         Raising it rejects more single-frame false alarms at the cost of a \
         little delay.";

    MIC_COOLDOWN = "mic.cooldown_ms", "Microphone",
        "Deaf period after a reply", "ms",
        Kind::Int { min: 0, max: 10000, default: 1200 },
        restart: false,
        "The wake word is ignored for this long after a turn ends. Without it a \
         reply that happens to contain a wake-word-shaped sound starts a second \
         turn by itself.";

    // --- Speech servers ------------------------------------------------------
    SPEECH_MIN_SENTENCE_CHARS = "speech.min_sentence_chars", "Speech servers",
        "Smallest chunk sent to speak", "characters",
        Kind::Int { min: 20, max: 400, default: 100 },
        restart: false,
        "The reply is spoken in pieces as the model writes it. This is a property \
         of your speech server's speed, not of language: every request costs a \
         fixed amount, so chunks shorter than this make the reply slower overall \
         rather than faster. Lower it only if your server has little fixed cost.";

    SPEECH_SYNTH_CONCURRENCY = "speech.synth_concurrency", "Speech servers",
        "Sentences synthesized at once", "",
        Kind::Int { min: 1, max: 8, default: 2 },
        restart: false,
        "How many pieces of the reply are sent to the speech server in parallel. \
         More hides the wait before a later sentence, until the server itself \
         becomes the bottleneck — 1 turns parallelism off entirely.";

    SPEECH_STT_TIMEOUT = "speech.stt_timeout_secs", "Speech servers",
        "Transcription timeout", "s",
        Kind::Int { min: 5, max: 600, default: 60 },
        restart: false,
        "How long to wait for the speech-to-text server before giving up.";

    SPEECH_TTS_TIMEOUT = "speech.tts_timeout_secs", "Speech servers",
        "Speech timeout", "s",
        Kind::Int { min: 5, max: 600, default: 60 },
        restart: false,
        "How long to wait for the text-to-speech server before giving up. Worth \
         raising on a machine where synthesis runs on the CPU.";

    LLM_REPLY_TIMEOUT = "llm.reply_timeout_secs", "Chat",
        "Give up on a reply after", "seconds",
        Kind::Int { min: 30, max: 3600, default: 900 },
        restart: false,
        "How long one reply may take from start to finish. This is a total, not a \
         gap between tokens, so a long answer counts against it even while it is \
         arriving normally. It was fixed at 120 and that was too short for images: \
         measured here, asking a 9B vision model about one screenshot took 166 \
         seconds and the reply was cut off with an unhelpful stream error. Lower it \
         only if you would rather a stuck server gave up sooner — the Stop button \
         already ends a reply you do not want.";

    // --- Spoken replies ------------------------------------------------------
    VOICE_MAX_TOKENS = "voice.max_tokens", "Spoken replies",
        "Reply length ceiling", "tokens",
        Kind::Int { min: 40, max: 2000, default: 220 },
        restart: false,
        "A spoken answer is asked to be short; this is the hard limit behind that \
         request. Raising it lets the assistant ramble at you.";

    VOICE_HISTORY_MESSAGES = "voice.history_messages", "Spoken replies",
        "Spoken turns remembered", "messages",
        Kind::Int { min: 0, max: 40, default: 8 },
        restart: false,
        "How much of today's spoken conversation is sent back to the model, so \
         follow-up questions make sense. 0 makes every turn standalone.";

    VOICE_HISTORY_CHARS = "voice.history_chars", "Spoken replies",
        "Spoken history size limit", "characters",
        Kind::Int { min: 0, max: 20000, default: 3000 },
        restart: false,
        "A second cap on the same history, so a few long turns can't crowd out \
         the actual question.";

    VOICE_TRANSCRIPT_LINGER = "voice.transcript_linger_ms", "Spoken replies",
        "Keep the transcript visible", "ms",
        Kind::Int { min: 500, max: 20000, default: 4000 },
        restart: false,
        "How long what you said stays on the widget after the reply finishes. It \
         is the most useful thing to see when an answer was strange.";

    VOICE_ERROR_NOTICE = "voice.error_notice_ms", "Spoken replies",
        "Keep errors visible", "ms",
        Kind::Int { min: 500, max: 30000, default: 6000 },
        restart: false,
        "How long a voice error message stays on the widget.";

    // --- Presence ------------------------------------------------------------
    PRESENCE_POLL = "presence.poll_secs", "Presence",
        "How often to re-read your state", "s",
        Kind::Int { min: 1, max: 60, default: 5 },
        restart: false,
        "How often the widget checks terminals, audio, battery and idle time to \
         pick its mood. Lower reacts faster and costs a little more CPU.";

    PRESENCE_BREAK = "presence.break_secs", "Presence",
        "Idle before \"stepped away\"", "s",
        Kind::Int { min: 30, max: 3600, default: 240 },
        restart: false,
        "No keyboard or mouse for this long and the widget shows a short break \
         rather than assuming you are still there.";

    PRESENCE_SLEEP = "presence.sleep_secs", "Presence",
        "Idle before sleeping", "s",
        Kind::Int { min: 60, max: 14400, default: 900 },
        restart: false,
        "The deeper version of the setting above. Keep it longer than the break: \
         this one is checked first, so setting it lower means the short-break \
         pose never appears at all.";

    PRESENCE_FORGOTTEN = "presence.forgotten_secs", "Presence",
        "Unanswered prompt before nagging", "s",
        Kind::Int { min: 30, max: 7200, default: 300 },
        restart: false,
        "How long a Claude Code session can sit waiting on your answer before \
         the widget stops being polite about it.";

    PRESENCE_LOW_BATTERY = "presence.low_battery_percent", "Presence",
        "Low battery warning at", "%",
        Kind::Int { min: 5, max: 50, default: 20 },
        restart: false,
        "Battery level below which the widget shows a low-power mood. Ignored on \
         a desktop.";

    PRESENCE_CALL_APPS = "presence.call_apps", "Presence",
        "Call apps", "",
        Kind::Names { default: "teams,zoom,discord,slack" },
        restart: false,
        "Comma-separated. Any app whose name contains one of these counts as a \
         call when it is making sound. Browser-based calls are usually detected \
         without this — the list is the fallback for when the microphone is \
         already in use by the wake word.";

    PRESENCE_STREAM_APPS = "presence.stream_apps", "Presence",
        "Streaming apps", "",
        Kind::Names { default: "obs" },
        restart: false,
        "Comma-separated, same matching as above — these put the widget in its \
         streaming mood instead.";

    // --- Chat ----------------------------------------------------------------
    CHAT_ATTACHMENT_MAX_CHARS = "chat.attachment_max_chars", "Chat",
        "Largest attached file sent", "characters",
        Kind::Int { min: 1000, max: 500_000, default: 60_000 },
        restart: false,
        "An attached file's text goes straight into your question, so a long one \
         eats the model's whole context before it reads what you asked. Anything \
         past this is cut off and the chat says so. Roughly four characters to a \
         token: the default is about 15,000 tokens, which a 32k-context model can \
         still answer around.";

    // --- Memory --------------------------------------------------------------
    RECALL_ENABLED = "recall.enabled", "Memory",
        "Remember earlier conversations", "",
        Kind::Toggle { default: true },
        restart: false,
        "When you ask about something discussed before, the relevant part of that \
         earlier conversation is quietly added to the question — typed or spoken, \
         either one can reach the other. Turn this off and every conversation \
         starts from nothing.";

    RECALL_INDEX_NOTES = "recall.index_notes", "Memory",
        "Also search Claude Code's own notes", "",
        Kind::Toggle { default: true },
        restart: false,
        "Claude Code keeps short notes of its own about your projects — the ones \
         the Memory graph in the workspace shows. With this on they are searched \
         alongside your conversations, which is usually worth it: a note was kept \
         on purpose, so it says more per line than a chat message. They cover \
         every project, not just the one you are asking about, so turn this off \
         if another project's notes start turning up.";

    RECALL_MAX_TURNS = "recall.max_turns", "Memory",
        "Earlier turns to bring back", "at most",
        Kind::Int { min: 1, max: 10, default: 3 },
        restart: false,
        "How many earlier exchanges may be added at once. This sits in front of \
         your actual question, so more is not better — a few too many and the \
         model is answering the old conversation instead of the new one.";

    RECALL_MAX_CHARS = "recall.max_chars", "Memory",
        "Total size of what is brought back", "characters",
        Kind::Int { min: 100, max: 4000, default: 600 },
        restart: false,
        "A ceiling on the whole recalled block, whatever the count above allows. \
         Matters most for spoken replies, where the entire budget is a couple of \
         hundred tokens.";

    RECALL_MIN_COVERAGE = "recall.min_coverage", "Memory",
        "How closely it has to match", "",
        Kind::Float { min: 0.1, max: 1.0, step: 0.05, default: 0.5 },
        restart: false,
        "The share of your question's meaningful words an old conversation has to \
         contain before it counts as related. Raise it if it keeps dragging in \
         things you did not mean; lower it if it forgets things you know you \
         discussed. At 1.0 every word has to match.";

    RECALL_EMBEDDING_URL = "recall.embedding_url", "Memory",
        "Embedding server", "",
        Kind::Text { default: "", placeholder: "http://localhost:11434/v1" },
        restart: false,
        "Optional, and empty by default. Without it, an old conversation is found \
         by the words it used — ask about \"caching\" and it finds the one that \
         said \"cache\". Point this at any OpenAI-compatible /v1/embeddings \
         endpoint (Ollama serves one) and it can also find one that made the same \
         point in different words. Only local addresses are used: this sends your \
         conversations to whatever is at this URL.";

    RECALL_EMBEDDING_MODEL = "recall.embedding_model", "Memory",
        "Embedding model", "",
        Kind::Text { default: "", placeholder: "bge-m3" },
        restart: false,
        "The model name to ask that server for. Both this and the URL above have \
         to be filled in before anything changes. Pick a multilingual one if you \
         work in more than one language — an English-only model will not connect \
         a Turkish conversation to an English question.";

    RECALL_SHARE_WITH_CLOUD = "recall.share_with_cloud", "Memory",
        "Send remembered history to non-local models", "",
        Kind::Toggle { default: false },
        restart: false,
        "Off by default, and deliberately. Recall pulls text out of your past \
         conversations and puts it in the next request — which is harmless while \
         the model runs on this machine, and is sending your history to someone \
         else's server the moment you point a profile at a hosted API. Local \
         models are unaffected either way.";

    // --- Images --------------------------------------------------------------
    //
    // The model itself is not a setting here: it is chosen when the server is
    // started (sd-server's own -m argument, in the start command below). That is
    // what keeps swapping SD 1.5 for SDXL or Flux a change of configuration
    // rather than of code — every one of them answers the same request.
    IMAGE_BASE_URL = "image.base_url", "Images",
        "Image server address", "",
        Kind::Text { default: "http://127.0.0.1:7801", placeholder: "http://127.0.0.1:7801" },
        restart: false,
        "Where a local image server is listening. It must speak the OpenAI images \
         API (POST /v1/images/generations) — stable-diffusion.cpp's sd-server does, \
         and needs no account or key. Only local addresses are accepted: what you \
         ask it to draw is your own words, and this app does not send those to \
         somebody else's machine.";

    IMAGE_START_COMMAND = "image.start_command", "Images",
        "Start command", "",
        Kind::Text { default: "", placeholder: "C:\\sd\\sd-server.exe -m C:\\sd\\model.safetensors --listen-port 7801 --diffusion-fa --vae-tiling" },
        restart: false,
        "Run at startup if nothing is already listening at the address above. \
         The two flags in the example are not optional on an AMD card: measured on \
         this machine, without --diffusion-fa a 768x768 image exceeded the Vulkan \
         driver's 2 GB single-allocation limit and fell back to roughly CPU speed \
         (300s and still unfinished, against 5.7s with it).";

    IMAGE_SIZE = "image.size", "Images",
        "Default image size", "px",
        Kind::Int { min: 256, max: 2048, default: 512 },
        restart: false,
        "Width and height of a generated picture. Match this to the model the \
         server loaded: SD 1.5 was trained at 512 and goes soft above it, SDXL was \
         trained at 1024 and is wasted below it. Bigger costs time — measured with \
         SD 1.5 here: 512 in 3.9s, 768 in 5.7s, 1024 in 10.5s.";

    IMAGE_STEPS = "image.steps", "Images",
        "Denoising steps", "",
        Kind::Int { min: 1, max: 100, default: 24 },
        restart: false,
        "How many passes the model refines the picture over. More is slower and \
         better only up to a point — around 20-30 for most models, and as few as 4 \
         for the 'turbo' and 'schnell' variants, which will look burnt at 24.";

    IMAGE_SEED = "image.seed", "Images",
        "Seed", "",
        Kind::Int { min: -1, max: 2147483647, default: -1 },
        restart: false,
        "The number the picture is built from. -1 draws a different one every \
         time; any other value pins it, so the same prompt gives back the same \
         picture. Set this to a seed printed under a result you liked to get that \
         result again, or to explore what one word changes while everything else \
         stays put. Only servers with stable-diffusion.cpp's own API take a seed \
         — the seed is not shown when the picture came from a generic one.";

    IMAGE_NEGATIVE_PROMPT = "image.negative_prompt", "Images",
        "Always avoid", "",
        Kind::Text { default: "blurry, low detail, deformed, extra limbs, watermark, text", placeholder: "blurry, watermark" },
        restart: false,
        "Added to every picture as things not to draw. This matters more than it \
         sounds: measured on the same model and the same seed, adding a negative \
         prompt and a more specific description was the difference between a flat, \
         soft image and a sharp one — a bigger difference than any other setting \
         on this page.";

    // --- Web -----------------------------------------------------------------
    WEB_TOOLS_ENABLED = "web.tools_enabled", "Web",
        "Let the model read web pages and search", "",
        Kind::Toggle { default: true },
        restart: false,
        "On by default: without it a local model can only answer from what it was \
         trained on, and has no way to look at a link you paste. With it on, the \
         model decides when to fetch a page or run a search — so the address it \
         reads, and the words it searches for, leave this machine. Nothing else \
         does: your conversation is not sent anywhere by this. Sources used are \
         free ones that need no account, and turning this off removes the tools \
         entirely rather than just hiding them.";

    // --- Mail ----------------------------------------------------------------
    //
    // There is deliberately no "mail.enabled" switch: the feature is on exactly
    // when an account is connected in Settings, the same rule the GitHub
    // integration already follows with its token. A second switch would only
    // create a state where everything is configured and nothing happens.
    MAIL_POLL = "mail.poll_secs", "Mail",
        "Check for new mail every", "s",
        Kind::Int { min: 60, max: 3600, default: 120 },
        restart: false,
        "How often the inbox is polled. Gmail's quota is generous enough that \
         this is really a question of how soon you want to know, not of cost.";

    MAIL_NOTIFY_NEW = "mail.notify_new", "Mail",
        "Announce new mail as it arrives", "",
        Kind::Toggle { default: true },
        restart: false,
        "Turn this off to keep the morning summary but stop the individual \
         pop-ups — the connection and the daily card are unaffected.";

    MAIL_MAX_NOTICES = "mail.max_notices_per_poll", "Mail",
        "Most pop-ups at once", "",
        Kind::Int { min: 1, max: 20, default: 3 },
        restart: false,
        "When more mail than this arrives between two checks, it collapses into \
         one \"12 new messages\" notice instead of queueing twelve. Without a cap, \
         coming back to a full inbox means the widget beeps at you for a minute.";

    MAIL_MUTE_SENDERS = "mail.mute_senders", "Mail",
        "Never announce mail from", "",
        Kind::Names { default: "noreply,no-reply,newsletter,notifications" },
        restart: false,
        "Comma-separated. Any sender address containing one of these is skipped \
         for pop-ups. A reply to something you sent is announced anyway — you \
         asked someone a question, so their answer matters even if their address \
         looks automated.";

    MAIL_SUMMARIZE_MIN_CHARS = "mail.summarize_min_chars", "Mail",
        "Summarize mail longer than", "characters",
        Kind::Int { min: 200, max: 20000, default: 800 },
        restart: false,
        "Below this a message goes through untouched — its subject line already \
         says what a summary would. Only longer mail is worth a model's time.";

    MAIL_SUMMARY_TIMEOUT = "mail.summary_timeout_secs", "Mail",
        "Give up summarizing after", "s",
        Kind::Int { min: 5, max: 120, default: 25 },
        restart: false,
        "The notice waits for the summary rather than appearing and then changing \
         under you, so this is also how late a notice can be. Past it, the message \
         is shown with its opening lines instead — never nothing.";

    MAIL_LOCAL_MODELS_ONLY = "mail.local_models_only", "Mail",
        "Only summarize mail with a local model", "",
        Kind::Toggle { default: true },
        restart: false,
        "Summarizing means sending the message body to whichever LLM profile is \
         active, and that profile can be switched to a hosted one at any time — \
         so this guards against what a later change might do, not against today. \
         With it on, a non-local profile simply skips the summary: mail notices \
         and the daily card still work, showing the message's opening lines \
         instead of a summary.";

    MAIL_BRIEF_DAYS = "mail.brief_days", "Mail",
        "Morning card looks back", "days",
        Kind::Int { min: 1, max: 90, default: 7 },
        restart: false,
        "How far back the morning card counts as still waiting for you. Both the \
         listed messages and the unread total obey it, so the number and the rows \
         describe the same week. Without a window the total is whatever Gmail's \
         own all-time counter says — 18,600 in one mailbox here, almost all of it \
         years old, which answers a question nobody asked. Raise it if you go \
         through mail less often than weekly.";

    MAIL_BRIEF_HOUR = "mail.brief_hour", "Mail",
        "Morning summary after", "o'clock",
        Kind::Int { min: 0, max: 23, default: 9 },
        restart: false,
        "Local hour the once-a-day mail and meeting card is allowed to appear. If \
         the GitHub digest is already on screen this one waits its turn rather \
         than being lost.";

    // --- Calendar ------------------------------------------------------------
    CALENDAR_POLL = "calendar.poll_secs", "Calendar",
        "Check the calendar every", "s",
        Kind::Int { min: 60, max: 3600, default: 300 },
        restart: false,
        "Keep this comfortably shorter than the reminder lead time below, or an \
         event can be added and then start again between two checks.";

    CALENDAR_REMIND_MINUTES = "calendar.remind_minutes", "Calendar",
        "Warn before a meeting starts", "min",
        Kind::Int { min: 1, max: 120, default: 10 },
        restart: false,
        "How far ahead the widget says something is about to start. Each \
         occurrence is announced once, so a daily standup does not re-fire every \
         time the calendar is polled.";

    CALENDAR_INCLUDE_ALL_DAY = "calendar.include_all_day", "Calendar",
        "Also warn about all-day entries", "",
        Kind::Toggle { default: false },
        restart: false,
        "Off, because an all-day entry starts at midnight — with this on, every \
         birthday and public holiday on your calendar wakes the widget up in the \
         middle of the night. They still appear in the morning summary either way.";

    // --- GitHub --------------------------------------------------------------
    GITHUB_DIGEST_HOUR = "github.digest_hour", "GitHub",
        "Daily digest after", "o'clock",
        Kind::Int { min: 0, max: 23, default: 9 },
        restart: false,
        "Local hour the once-a-day summary of your issues and pull requests is \
         allowed to run.";

    GITHUB_ISSUE_POLL = "github.issue_poll_secs", "GitHub",
        "Check GitHub every", "s",
        Kind::Int { min: 30, max: 3600, default: 90 },
        restart: false,
        "How often assigned issues and review requests are polled. Lower means \
         faster notifications and more of your API rate limit.";

    GITHUB_STALE_DAYS = "github.stale_days", "GitHub",
        "Call a pull request stale after", "days",
        Kind::Int { min: 1, max: 365, default: 30 },
        restart: false,
        "Age at which one of your open pull requests is flagged as stale in the \
         digest.";

    GITHUB_MERGE_FRESHNESS = "github.merge_freshness_minutes", "GitHub",
        "Only celebrate merges from the last", "min",
        Kind::Int { min: 1, max: 1440, default: 10 },
        restart: false,
        "Merges older than this are ignored, so restarting the widget doesn't \
         replay a week of them at you.";

    GITHUB_ISSUE_FRESHNESS = "github.issue_freshness_minutes", "GitHub",
        "Only announce issue activity from the last", "min",
        Kind::Int { min: 1, max: 1440, default: 10 },
        restart: false,
        "The same guard for assignments and comments.";
}

fn spec(id: &str) -> &'static Tunable {
    TUNABLES
        .iter()
        .find(|t| t.id == id)
        // Unreachable via the id constants above, which is the whole point of
        // generating them from the same literal as the entry.
        .expect("tunable id must exist in TUNABLES")
}

// Overrides are cached because some of these are read on paths that run often
// (the presence poll, every streamed reply chunk), and re-reading config.json
// from disk for a single number would be absurd. Invalidated on save, which is
// the only thing that can change them.
fn cache() -> &'static RwLock<Option<HashMap<String, Value>>> {
    static CACHE: OnceLock<RwLock<Option<HashMap<String, Value>>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(None))
}

fn override_value(app: &tauri::AppHandle, id: &str) -> Option<Value> {
    if let Ok(guard) = cache().read() {
        if let Some(map) = guard.as_ref() {
            return map.get(id).cloned();
        }
    }
    let map = read_config(app).tunables;
    let found = map.get(id).cloned();
    if let Ok(mut guard) = cache().write() {
        if guard.is_none() {
            *guard = Some(map);
        }
    }
    found
}

pub fn int(app: &tauri::AppHandle, id: &str) -> i64 {
    let spec = spec(id);
    let default = match spec.kind {
        Kind::Int { default, .. } => default,
        _ => 0,
    };
    override_value(app, id)
        .and_then(|v| v.as_i64())
        .unwrap_or(default)
}

/// Convenience for the many durations stored in seconds and used as a Duration.
pub fn secs(app: &tauri::AppHandle, id: &str) -> std::time::Duration {
    std::time::Duration::from_secs(int(app, id).max(0) as u64)
}

pub fn float(app: &tauri::AppHandle, id: &str) -> f64 {
    let default = match spec(id).kind {
        Kind::Float { default, .. } => default,
        _ => 0.0,
    };
    override_value(app, id)
        .and_then(|v| v.as_f64())
        .unwrap_or(default)
}

pub fn toggle(app: &tauri::AppHandle, id: &str) -> bool {
    let default = match spec(id).kind {
        Kind::Toggle { default } => default,
        _ => false,
    };
    override_value(app, id)
        .and_then(|v| v.as_bool())
        .unwrap_or(default)
}

pub fn text(app: &tauri::AppHandle, id: &str) -> String {
    let default = match spec(id).kind {
        Kind::Text { default, .. } => default,
        _ => "",
    };
    override_value(app, id)
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| default.to_string())
        .trim()
        .to_string()
}

/// A `Names` list, already split and lowercased, ready to match against.
///
/// This reader did not exist while both lists of that kind were read only by the
/// frontend; the mail watcher's muted-senders list is the first one Rust itself
/// has to match on. Values are normalized on save too (see `validate`), so this
/// re-splits an already-clean string — cheap, and it keeps the reader correct for
/// a config.json edited by hand.
pub fn names(app: &tauri::AppHandle, id: &str) -> Vec<String> {
    let default = match spec(id).kind {
        Kind::Names { default } => default,
        _ => "",
    };
    let raw = override_value(app, id)
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| default.to_string());
    split_names(&raw)
}

/// Splits a `Names` value into lowercase substrings, dropping empties so a stray
/// trailing comma can't produce a pattern that matches everything.
fn split_names(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|part| part.trim().to_lowercase())
        .filter(|part| !part.is_empty())
        .collect()
}

// --- the settings window's side ---------------------------------------------

#[derive(Serialize)]
pub struct TunableInfo {
    id: &'static str,
    group: &'static str,
    label: &'static str,
    help: &'static str,
    unit: &'static str,
    kind: &'static str,
    min: Option<f64>,
    max: Option<f64>,
    step: Option<f64>,
    default: Value,
    /// Example text for an optional setting whose default is empty, where the
    /// default itself would show the user nothing.
    placeholder: &'static str,
    restart: bool,
}

#[derive(Serialize)]
pub struct TunablesPayload {
    /// Group names in declaration order — the settings form renders sections in
    /// this order rather than inventing one of its own.
    groups: Vec<&'static str>,
    settings: Vec<TunableInfo>,
    /// Effective values (default merged with the user's override) keyed by id.
    /// The frontend reads these and therefore never carries its own defaults.
    values: HashMap<&'static str, Value>,
    /// Ids the user has actually changed, so the form can mark them and offer a
    /// reset without having to compare floats itself.
    overridden: Vec<&'static str>,
}

fn default_value(kind: &Kind) -> Value {
    match *kind {
        Kind::Int { default, .. } => Value::from(default),
        Kind::Float { default, .. } => Value::from(default),
        Kind::Names { default } => Value::from(default),
        Kind::Toggle { default } => Value::from(default),
        Kind::Text { default, .. } => Value::from(default),
    }
}

#[tauri::command]
pub fn get_tunables(app: tauri::AppHandle) -> TunablesPayload {
    let overrides = read_config(&app).tunables;

    let mut groups: Vec<&'static str> = Vec::new();
    for tunable in TUNABLES {
        if !groups.contains(&tunable.group) {
            groups.push(tunable.group);
        }
    }

    let settings = TUNABLES
        .iter()
        .map(|t| {
            let (kind, min, max, step) = match t.kind {
                Kind::Int { min, max, .. } => ("int", Some(min as f64), Some(max as f64), Some(1.0)),
                Kind::Float {
                    min, max, step, ..
                } => ("float", Some(min), Some(max), Some(step)),
                Kind::Names { .. } => ("names", None, None, None),
                Kind::Toggle { .. } => ("toggle", None, None, None),
                Kind::Text { .. } => ("text", None, None, None),
            };
            TunableInfo {
                id: t.id,
                group: t.group,
                label: t.label,
                help: t.help,
                unit: t.unit,
                kind,
                min,
                max,
                step,
                default: default_value(&t.kind),
                placeholder: match t.kind {
                    Kind::Text { placeholder, .. } => placeholder,
                    _ => "",
                },
                restart: t.restart,
            }
        })
        .collect();

    let mut values = HashMap::new();
    let mut overridden = Vec::new();
    for tunable in TUNABLES {
        match overrides.get(tunable.id) {
            Some(value) => {
                overridden.push(tunable.id);
                values.insert(tunable.id, value.clone());
            }
            None => {
                values.insert(tunable.id, default_value(&tunable.kind));
            }
        }
    }

    TunablesPayload {
        groups,
        settings,
        values,
        overridden,
    }
}

/// Validates a single incoming value against its schema entry, returning the
/// normalized value to store. Kept separate from the command so it is testable
/// without an AppHandle.
fn validate(tunable: &Tunable, value: &Value) -> Result<Value, String> {
    match tunable.kind {
        Kind::Int { min, max, .. } => {
            // as_i64 alone rejects 5.0, which is what a number input in the
            // webview produces for an integer field.
            let number = value
                .as_i64()
                .or_else(|| value.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
                .ok_or_else(|| format!("{} needs a whole number.", tunable.label))?;
            if number < min || number > max {
                return Err(format!(
                    "{} has to be between {min} and {max}.",
                    tunable.label
                ));
            }
            Ok(Value::from(number))
        }
        Kind::Float { min, max, .. } => {
            let number = value
                .as_f64()
                .filter(|f| f.is_finite())
                .ok_or_else(|| format!("{} needs a number.", tunable.label))?;
            if number < min || number > max {
                return Err(format!(
                    "{} has to be between {min} and {max}.",
                    tunable.label
                ));
            }
            Ok(Value::from(number))
        }
        Kind::Names { .. } => {
            let text = value
                .as_str()
                .ok_or_else(|| format!("{} needs a comma-separated list.", tunable.label))?;
            // Stored normalized so what the user typed and what the matching
            // actually uses can't diverge.
            Ok(Value::from(split_names(text).join(",")))
        }
        Kind::Toggle { .. } => value
            .as_bool()
            .map(Value::from)
            .ok_or_else(|| format!("{} is on or off.", tunable.label)),
        Kind::Text { .. } => value
            .as_str()
            .map(|s| Value::from(s.trim()))
            .ok_or_else(|| format!("{} needs text.", tunable.label)),
    }
}

/// Saves the given values. A null value removes the override, putting that
/// setting back on its compiled default — which is also why absent-means-default
/// is the storage rule: resetting leaves no trace in config.json instead of
/// writing the default out as if the user had chosen it.
#[tauri::command]
pub fn save_tunables(app: tauri::AppHandle, values: HashMap<String, Value>) -> Result<(), String> {
    let mut validated: Vec<(String, Option<Value>)> = Vec::new();
    for (id, value) in &values {
        let tunable = TUNABLES
            .iter()
            .find(|t| t.id == id.as_str())
            .ok_or_else(|| format!("Unknown setting: {id}"))?;
        if value.is_null() {
            validated.push((id.clone(), None));
        } else {
            validated.push((id.clone(), Some(validate(tunable, value)?)));
        }
    }

    // Nothing is written until every value passed, so a form with one bad field
    // doesn't half-apply.
    let mut cfg = read_config(&app);
    for (id, value) in validated {
        match value {
            Some(value) => {
                cfg.tunables.insert(id, value);
            }
            None => {
                cfg.tunables.remove(&id);
            }
        }
    }
    write_config(&app, &cfg);

    if let Ok(mut guard) = cache().write() {
        *guard = Some(cfg.tunables.clone());
    }
    // The mascot window holds its own copy of these; it reloads on this rather
    // than polling the config.
    let _ = app.emit("tunables-changed", ());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for tunable in TUNABLES {
            assert!(seen.insert(tunable.id), "duplicate tunable id {}", tunable.id);
        }
    }

    #[test]
    fn every_default_is_inside_its_own_range() {
        for tunable in TUNABLES {
            match tunable.kind {
                Kind::Int { min, max, default } => {
                    assert!(min <= max, "{}: min above max", tunable.id);
                    assert!(
                        (min..=max).contains(&default),
                        "{}: default {default} outside {min}..{max}",
                        tunable.id
                    );
                }
                Kind::Float {
                    min,
                    max,
                    step,
                    default,
                } => {
                    assert!(min <= max, "{}: min above max", tunable.id);
                    assert!(step > 0.0, "{}: step must be positive", tunable.id);
                    assert!(
                        default >= min && default <= max,
                        "{}: default {default} outside {min}..{max}",
                        tunable.id
                    );
                }
                Kind::Names { default } => {
                    assert!(
                        !split_names(default).is_empty(),
                        "{}: default list is empty",
                        tunable.id
                    );
                }
                Kind::Toggle { .. } => {}
                // A Text default may legitimately be empty — that is how an
                // optional connection setting says "not configured".
                Kind::Text { .. } => {}
            }
        }
    }

    #[test]
    fn every_tunable_is_described() {
        for tunable in TUNABLES {
            assert!(!tunable.label.is_empty(), "{}: no label", tunable.id);
            // The help text is what makes the difference between a settings form
            // and a wall of numbers, so an entry without one is a mistake.
            assert!(tunable.help.len() > 30, "{}: help too thin", tunable.id);
            assert!(
                tunable.id.contains('.'),
                "{}: ids are group-prefixed",
                tunable.id
            );
        }
    }

    // Guards the one ordering relationship in the registry that a user can get
    // wrong from the form: a sleep threshold below the break threshold means the
    // widget can never reach the sleeping state.
    #[test]
    fn sleep_default_is_longer_than_break_default() {
        let value = |id: &str| match spec(id).kind {
            Kind::Int { default, .. } => default,
            _ => panic!("expected an int"),
        };
        assert!(value(PRESENCE_SLEEP) > value(PRESENCE_BREAK));
    }

    #[test]
    fn int_validation_rejects_out_of_range_and_accepts_whole_floats() {
        let port = spec(EVENT_PORT);
        assert!(validate(port, &Value::from(80)).is_err());
        assert!(validate(port, &Value::from(8080)).is_ok());
        assert!(validate(port, &Value::from(70000)).is_err());
        // A webview number input yields 8080.0, not 8080.
        assert_eq!(validate(port, &Value::from(8080.0)).unwrap(), Value::from(8080));
        assert!(validate(port, &Value::from(8080.5)).is_err());
        assert!(validate(port, &Value::from("8080")).is_err());
    }

    #[test]
    fn float_validation_rejects_non_finite() {
        let floor = spec(MIC_SPEECH_OVER_FLOOR);
        assert!(validate(floor, &Value::from(2.5)).is_ok());
        assert!(validate(floor, &Value::from(0.5)).is_err());
        assert!(serde_json::from_str::<Value>("1e999")
            .map(|v| validate(floor, &v).is_err())
            .unwrap_or(true));
    }

    #[test]
    fn names_are_stored_normalized() {
        let apps = spec(PRESENCE_CALL_APPS);
        assert_eq!(
            validate(apps, &Value::from("Teams, ZOOM ,, jitsi,")).unwrap(),
            Value::from("teams,zoom,jitsi")
        );
        assert!(validate(apps, &Value::from(3)).is_err());
    }

    #[test]
    fn a_toggle_takes_only_a_boolean() {
        let toggle = spec(RECALL_ENABLED);
        assert_eq!(validate(toggle, &Value::from(false)).unwrap(), Value::from(false));
        // A checkbox that sent 0/1 or "true" would otherwise be stored as a
        // value the reader can't interpret, silently falling back to the default.
        assert!(validate(toggle, &Value::from(1)).is_err());
        assert!(validate(toggle, &Value::from("true")).is_err());
    }

    // The one default in the registry that is a privacy decision rather than a
    // preference: recall lifts text out of the user's own past conversations, so
    // sending it to somebody else's server has to be something they turned on.
    #[test]
    fn sharing_recalled_history_with_hosted_models_is_off_by_default() {
        match spec(RECALL_SHARE_WITH_CLOUD).kind {
            Kind::Toggle { default } => assert!(!default),
            _ => panic!("expected a toggle"),
        }
    }

    #[test]
    fn splitting_names_drops_empties() {
        assert_eq!(split_names(" A , ,b,"), vec!["a".to_string(), "b".to_string()]);
        assert!(split_names(" , ").is_empty());
    }
}
