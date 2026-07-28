// Chat conversations for the Workspace window's Chat mode — one JSON file per chat
// (mirrors content/notes.rs's file-per-note approach), in app_data_dir/Chats. Unlike
// notes, there's no meaningful "raw file a user would open elsewhere" here,
// so the whole conversation (messages included) lives in one JSON blob
// rather than a text file + sidecar.
use std::fs;
use std::path::PathBuf;

use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;

use crate::config::{read_config, write_config};

#[derive(Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    pub ts: u64,
    // Where the message came from: empty (the default, and what every chat
    // saved before this existed carries) means typed in the chat window;
    // "voice" means it was spoken to the voice assistant. Kept per-message
    // rather than per-chat because it is a property of the individual turn.
    #[serde(default)]
    pub source: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Chat {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub profile_id: String,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
}

// Lightweight listing for the sidebar — avoids reading every message body
// of every chat just to render the history list.
#[derive(Serialize, Clone)]
pub struct ChatSummary {
    pub id: String,
    pub title: String,
    pub updated_at: u64,
    pub message_count: usize,
}

fn current_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn chats_dir(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .expect("app data dir must be resolvable")
        .join("Chats");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn chat_path(app: &tauri::AppHandle, id: &str) -> PathBuf {
    chats_dir(app).join(format!("{id}.json"))
}

#[tauri::command]
pub fn list_chats(app: tauri::AppHandle) -> Vec<ChatSummary> {
    let dir = chats_dir(&app);
    let mut chats: Vec<ChatSummary> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("json"))
        .filter_map(|path| {
            let contents = fs::read_to_string(&path).ok()?;
            let chat: Chat = serde_json::from_str(&contents).ok()?;
            Some(ChatSummary {
                id: chat.id,
                title: chat.title,
                updated_at: chat.updated_at,
                message_count: chat.messages.len(),
            })
        })
        .collect();

    chats.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    chats
}

#[tauri::command]
pub fn load_chat(app: tauri::AppHandle, id: String) -> Option<Chat> {
    let contents = fs::read_to_string(chat_path(&app, &id)).ok()?;
    serde_json::from_str(&contents).ok()
}

#[tauri::command]
pub fn save_chat(app: tauri::AppHandle, mut chat: Chat) -> Chat {
    let now = current_millis();
    if chat.id.is_empty() {
        chat.id = uuid::Uuid::new_v4().to_string();
    }
    if chat.created_at == 0 {
        chat.created_at = now;
    }
    chat.updated_at = now;

    if let Ok(json) = serde_json::to_string_pretty(&chat) {
        let _ = fs::write(chat_path(&app, &chat.id), json);
    }
    chat
}

#[tauri::command]
pub fn delete_chat(app: tauri::AppHandle, id: String) {
    let _ = fs::remove_file(chat_path(&app, &id));
}

/// Appends one spoken exchange to the voice log so it is readable afterwards in
/// the Chat window's history, tagged as spoken rather than typed.
///
/// One conversation per calendar day, with the date as the chat id so
/// find-or-create is just "does this file exist". A single ever-growing "Voice"
/// chat would be simpler still, but it would become unopenable over months and
/// "what did I ask it yesterday" is the actual question people have.
///
/// Writes the transcript alongside the reply on purpose: what the recognizer
/// *thought* you said is the most common thing to go wrong in a voice turn, and
/// without it a strange answer looks like the model misbehaving.
// One voice conversation per calendar day — see record_voice_turn.
fn voice_chat_id(now: &chrono::DateTime<chrono::Local>) -> String {
    format!("voice-{}", now.format("%Y-%m-%d"))
}

/// The tail of today's spoken conversation, oldest first, for the voice
/// assistant to send as context so a follow-up question makes sense.
///
/// Bounded twice on purpose. `max_messages` keeps a long day from growing the
/// prompt without limit, and `max_chars` is the one that actually protects the
/// turn's latency: prompt processing is fast but not free, and a spoken reply
/// that arrives late is worse than one that has forgotten something.
/// Trimming from the front (oldest first) keeps the most recent exchange, which
/// is what a follow-up almost always refers to.
pub(crate) fn recent_voice_turns(
    app: &tauri::AppHandle,
    max_messages: usize,
    max_chars: usize,
) -> Vec<ChatMessage> {
    let id = voice_chat_id(&chrono::Local::now());
    let Some(chat) = load_chat(app.clone(), id) else {
        return Vec::new();
    };

    let mut kept: Vec<ChatMessage> = chat
        .messages
        .into_iter()
        .rev()
        .take(max_messages)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    while kept.len() > 1 && kept.iter().map(|m| m.content.len()).sum::<usize>() > max_chars {
        kept.remove(0);
    }
    // A conversation must not start on an assistant turn — some servers reject
    // that outright, and it reads to the model as if it spoke unprompted.
    while kept.first().is_some_and(|m| m.role != "user") {
        kept.remove(0);
    }
    kept
}

#[tauri::command]
pub fn record_voice_turn(app: tauri::AppHandle, transcript: String, reply: String) -> Result<String, String> {
    let today = chrono::Local::now();
    let id = voice_chat_id(&today);

    let mut chat = load_chat(app.clone(), id.clone()).unwrap_or_else(|| Chat {
        id: id.clone(),
        title: format!("Voice — {}", today.format("%d %b %Y")),
        profile_id: String::new(),
        created_at: 0,
        updated_at: 0,
        messages: Vec::new(),
    });

    // Recorded against whichever profile answered, so the conversation reopens
    // with the right model selected in the chat window's picker.
    chat.profile_id = crate::ai::llm::get_active_llm_profile(app.clone()).id;

    let ts = current_millis();
    chat.messages.push(ChatMessage {
        role: "user".to_string(),
        content: transcript,
        ts,
        source: "voice".to_string(),
    });
    chat.messages.push(ChatMessage {
        role: "assistant".to_string(),
        content: reply,
        ts,
        source: "voice".to_string(),
    });

    let saved = save_chat(app.clone(), chat);
    // The Chat window may be open and showing a now-stale history list; it has
    // no other way to learn a conversation grew behind its back.
    {
        use tauri::Emitter;
        let _ = app.emit("voice-turn-recorded", serde_json::json!({ "chat_id": &saved.id }));
    }
    Ok(saved.id)
}

// Custom instructions ("personality") — global, applies to every chat, not
// scoped to one conversation. Sent as a leading system-role message ahead
// of each request (see chat.js's sendChatMessage), never stored inside
// any single chat's `messages` so editing it doesn't rewrite history.
#[tauri::command]
pub fn get_chat_instructions(app: tauri::AppHandle) -> String {
    read_config(&app).chat_instructions.unwrap_or_default()
}

#[tauri::command]
pub fn save_chat_instructions(app: tauri::AppHandle, instructions: String) {
    let mut cfg = read_config(&app);
    cfg.chat_instructions = Some(instructions);
    write_config(&app, &cfg);
}

#[derive(Serialize, Clone)]
pub struct ChatAttachment {
    pub name: String,
    // "image" (base64-encoded, `data` is the base64 body) | "text" (`data`
    // is the raw file text, inlined into the message as a fenced code
    // block) | "unsupported" (`data` empty — e.g. PDF/Excel, which would
    // need a real parser we don't have yet).
    pub kind: String,
    pub mime: String,
    pub data: String,
}

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];
const TEXT_EXTS: &[&str] = &[
    "txt", "md", "py", "js", "jsx", "ts", "tsx", "json", "csv", "html", "htm", "css", "rs", "go", "java", "c", "cpp",
    "h", "hpp", "sh", "yaml", "yml", "toml", "xml", "log", "sql",
];

// The chat composer's attach button — opens a native file picker and reads
// the result directly (rather than just returning a path) so the frontend
// never needs its own filesystem access. Images are read as attachable
// base64 (a vision-capable model can look at them; a text-only one will
// just ignore or error on the image part). Plain-text-ish files are read
// as-is and inlined into the message as a fenced code block — any model can
// read that, no special capability needed. PDF/Excel and other binary
// structured formats aren't parsed (would need a real parser crate); they
// come back as "unsupported" so the frontend can say so instead of silently
// sending nothing useful.
#[tauri::command]
pub fn pick_chat_attachment(app: tauri::AppHandle) -> Option<ChatAttachment> {
    let picked = app.dialog().file().blocking_pick_file()?;
    let path = picked.into_path().ok()?;
    let name = path.file_name()?.to_string_lossy().to_string();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();

    if IMAGE_EXTS.contains(&ext.as_str()) {
        let bytes = fs::read(&path).ok()?;
        let mime = format!("image/{}", if ext == "jpg" { "jpeg" } else { ext.as_str() });
        return Some(ChatAttachment {
            name,
            kind: "image".to_string(),
            mime,
            data: general_purpose::STANDARD.encode(bytes),
        });
    }
    if TEXT_EXTS.contains(&ext.as_str()) {
        let text = fs::read_to_string(&path).ok()?;
        return Some(ChatAttachment {
            name,
            kind: "text".to_string(),
            mime: format!("text/{ext}"),
            data: text,
        });
    }
    Some(ChatAttachment {
        name,
        kind: "unsupported".to_string(),
        mime: ext,
        data: String::new(),
    })
}
