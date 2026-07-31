// Chat conversations for the Workspace window's Chat mode — one JSON file per chat
// (mirrors content/notes.rs's file-per-note approach), in app_data_dir/Chats by
// default and in whatever folder config.chats_dir names otherwise. Unlike
// notes, there's no meaningful "raw file a user would open elsewhere" here,
// so the whole conversation (messages included) lives in one JSON blob
// rather than a text file + sidecar.
use std::fs;
use std::path::{Path, PathBuf};

use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;

use crate::config::{read_config, write_config};

/// A file the user attached to a message, kept beside the message rather than
/// pasted into it.
///
/// Inlining the text into `content` was the obvious thing and it was wrong: a
/// 150-line file became the visible message, burying what the user actually
/// asked underneath it. Held separately, the transcript shows a card and the
/// model still gets the whole file — the two are assembled at send time (see
/// chat.js's inlineAttachment) instead of being the same string.
#[derive(Serialize, Deserialize, Clone)]
pub struct MessageAttachment {
    pub name: String,
    /// Fenced-code language for `text`, or empty.
    #[serde(default)]
    pub lang: String,
    /// The extracted text. Empty for an image, whose bytes are deliberately not
    /// persisted — see the note on display vs wire content in chat.js.
    #[serde(default)]
    pub text: String,
    /// "text" or "image".
    #[serde(default)]
    pub kind: String,
    /// Characters the file had before any cap, so the card can say it was cut.
    #[serde(default)]
    pub full_chars: usize,
    /// Where an attached image was written (images::save_attached_image). Empty
    /// for text attachments, whose content is in `text`, and for images attached
    /// before this existed — which are the ones whose cards open nothing.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    pub ts: u64,
    /// Files attached to this message. Absent in every chat saved before this
    /// existed, which serde's default covers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<MessageAttachment>,
    // Where the message came from: empty (the default, and what every chat
    // saved before this existed carries) means typed in the chat window;
    // "voice" means it was spoken to the voice assistant. Kept per-message
    // rather than per-chat because it is a property of the individual turn.
    #[serde(default)]
    pub source: String,
    /// A picture this turn produced (ai/images.rs), stored as the path it was
    /// written to rather than as its bytes.
    ///
    /// The bytes deliberately do not go in here: attachments already drop image
    /// data for exactly this reason, and a conversation with a dozen pictures
    /// inlined as base64 would be several megabytes of JSON to open. The file
    /// outlives the app, so the path is enough to show it again.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub image_path: String,
    /// Size and how long it took, shown under the picture. Recorded at
    /// generation time because it cannot be recovered from the file later.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub image_meta: String,
    /// The web pages this turn was answered from, shown as links under the reply.
    ///
    /// Saved, unlike the recall note beside it in the UI. The two look alike but
    /// point at different things: a recall note names an earlier conversation,
    /// which is still in the app and findable by search, so losing the note loses
    /// little. These name pages outside it, and nothing else records which ones
    /// were read — so a reopened chat showed factual claims with their citations
    /// stripped off, which is the exact "did the model make this up" problem the
    /// sources exist to answer. A few short URLs per answer is a cheap thing to
    /// carry for that.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<MessageSource>,
}

/// One page shown under a reply.
///
/// Structurally the same as tools::ToolSource today, and deliberately not that
/// type: this one is part of the on-disk chat format, and reusing the tool
/// module's would mean a change made for tooling reasons silently rewrote what
/// every saved conversation is expected to contain. Nothing converts between
/// them — the round trip is through the frontend as JSON.
#[derive(Serialize, Deserialize, Clone)]
pub struct MessageSource {
    pub title: String,
    pub url: String,
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

fn default_chats_dir(app: &tauri::AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .expect("app data dir must be resolvable")
        .join("Chats")
}

fn chats_dir(app: &tauri::AppHandle) -> PathBuf {
    let dir = read_config(app)
        .chats_dir
        .filter(|d| !d.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| default_chats_dir(app));
    let _ = fs::create_dir_all(&dir);
    dir
}

#[derive(Serialize)]
pub struct ChatsDir {
    dir: String,
    // Whether that path is the built-in location rather than one the user
    // picked — the UI offers "reset" only when it isn't.
    is_default: bool,
}

#[tauri::command]
pub fn get_chats_dir(app: tauri::AppHandle) -> ChatsDir {
    let dir = chats_dir(&app);
    ChatsDir {
        is_default: dir == default_chats_dir(&app),
        dir: dir.to_string_lossy().to_string(),
    }
}

/// Result of moving the conversation files to a new folder. Reported rather
/// than assumed: a partial move leaves history split across two directories,
/// and the user is the only one who can decide what to do about that.
#[derive(Serialize)]
pub struct ChatsDirChange {
    dir: String,
    moved: usize,
    /// Files left behind because the target already had a file of that name —
    /// never overwritten, since the one at the target may be the newer copy.
    skipped: usize,
    failed: Vec<String>,
}

/// Moves every conversation file from one folder to another.
///
/// An existing file at the target is never overwritten — it may be the newer
/// copy, and this has no way to know — so it is counted as skipped and left
/// where it is at both ends.
fn move_chat_files(source: &Path, target: &Path) -> ChatsDirChange {
    let mut change = ChatsDirChange {
        dir: target.to_string_lossy().to_string(),
        moved: 0,
        skipped: 0,
        failed: Vec::new(),
    };
    if source == target {
        return change;
    }
    let _ = fs::create_dir_all(target);

    for entry in fs::read_dir(source).into_iter().flatten().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(name) = path.file_name() else { continue };
        let destination = target.join(name);
        if destination.exists() {
            change.skipped += 1;
            continue;
        }
        // rename() fails across volumes on Windows, which is exactly what a user
        // picking "my synced drive" hits — so fall back to copy + delete.
        let moved = fs::rename(&path, &destination).is_ok()
            || (fs::copy(&path, &destination).is_ok() && fs::remove_file(&path).is_ok());
        if moved {
            change.moved += 1;
        } else {
            change.failed.push(name.to_string_lossy().to_string());
        }
    }
    change
}

/// Points chat storage at another folder, taking the existing conversations
/// with it.
///
/// The files move rather than being left behind: "change where my chats live"
/// means the chats, not just future ones. Filenames are `<id>.json` and the id
/// is inside the file too, so moving them changes nothing the recall index
/// keys on — it is the same conversations at a new path.
#[tauri::command]
pub async fn choose_chats_dir(app: tauri::AppHandle) -> Option<ChatsDirChange> {
    crate::offload(move || {
        let picked = app.dialog().file().blocking_pick_folder()?;
        let target = picked.into_path().ok()?;
        let change = move_chat_files(&chats_dir(&app), &target);

        let mut cfg = read_config(&app);
        cfg.chats_dir = Some(target.to_string_lossy().to_string());
        write_config(&app, &cfg);

        // The index keys on chat ids, not paths, so nothing above invalidated it —
        // but a skipped or failed file means the folder's contents are not what the
        // index thinks, and reconcile is what notices.
        crate::ai::recall::start_indexer(app.clone());
        Some(change)
})
    .await
}

/// Puts conversations back in the built-in location, bringing them along.
#[tauri::command]
pub async fn reset_chats_dir(app: tauri::AppHandle) -> ChatsDirChange {
    crate::offload(move || {
        let change = move_chat_files(&chats_dir(&app), &default_chats_dir(&app));

        let mut cfg = read_config(&app);
        cfg.chats_dir = None;
        write_config(&app, &cfg);
        crate::ai::recall::start_indexer(app.clone());
        change
})
    .await
}

fn chat_path(app: &tauri::AppHandle, id: &str) -> PathBuf {
    chats_dir(app).join(format!("{id}.json"))
}

#[tauri::command]
pub async fn list_chats(app: tauri::AppHandle) -> Vec<ChatSummary> {
    crate::offload(move || {
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
})
    .await
}

#[tauri::command]
pub fn load_chat(app: tauri::AppHandle, id: String) -> Option<Chat> {
    let contents = fs::read_to_string(chat_path(&app, &id)).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Every conversation, messages included — the source ai/recall.rs builds its
/// search index from. Unlike list_chats this deliberately reads the bodies:
/// the index is a derived cache, so it has to be rebuildable from these files
/// alone.
pub(crate) fn all_chats(app: &tauri::AppHandle) -> Vec<Chat> {
    fs::read_dir(chats_dir(app))
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("json"))
        .filter_map(|path| serde_json::from_str(&fs::read_to_string(&path).ok()?).ok())
        .collect()
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
    // The file is the source of truth and is already written; the search index
    // catches up behind it (see ai/recall.rs) so a later conversation can find
    // this one.
    crate::ai::recall::index_chat_soon(&app, chat.clone());
    chat
}

#[tauri::command]
pub fn delete_chat(app: tauri::AppHandle, id: String) {
    let _ = fs::remove_file(chat_path(&app, &id));
    // Dropped from the index too, or a deleted conversation stays findable —
    // which is worse than never having indexed it. Cheap because it only ever
    // touches rows for this one chat.
    crate::ai::recall::forget_chat_soon(&app, id);
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

/// The id spoken turns are being appended to right now — so ai/recall.rs can
/// leave the running conversation out of what it recalls.
pub(crate) fn todays_voice_chat_id() -> String {
    voice_chat_id(&chrono::Local::now())
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
        // A spoken turn has no files attached to it, no picture and no sources:
        // image generation is a typed command in the chat window, and the voice
        // assistant answers without the web tools.
        attachments: Vec::new(),
        image_path: String::new(),
        image_meta: String::new(),
        sources: Vec::new(),
    });
    chat.messages.push(ChatMessage {
        role: "assistant".to_string(),
        content: reply,
        ts,
        source: "voice".to_string(),
        attachments: Vec::new(),
        image_path: String::new(),
        image_meta: String::new(),
        sources: Vec::new(),
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
    // "image" (`data` is base64) | "text" (`data` is the file's text, inlined
    // into the message as a fenced code block) | "failed" (a readable format
    // this particular file defeated — `problem` says how) | "unsupported"
    // (nothing here reads this format at all).
    pub kind: String,
    pub mime: String,
    pub data: String,
    /// Why a readable format produced nothing, for the user. Empty otherwise.
    pub problem: String,
    /// The fenced-code language token for `data`, derived from the extension.
    /// Empty when there is no sensible one.
    ///
    /// Carried from here rather than worked out in the frontend because the
    /// extension is what maps to a language, and by the time the frontend has
    /// the attachment it only has a filename. The old code used the filename
    /// itself as the fence token, which produced ```` ```script.py ```` — a
    /// language no highlighter has ever heard of.
    pub lang: String,
    /// Characters the file actually had, before any cap was applied. Equal to
    /// `data`'s length when nothing was dropped; larger when it was truncated,
    /// which is the only way the user can tell that happened.
    pub full_chars: usize,
}

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];

/// Reads a file as text, whatever its encoding, capped at `max_chars`.
///
/// Lossy rather than strict: `read_to_string` rejects anything that is not
/// valid UTF-8, and a Turkish .txt saved by Notepad in the Windows-1254 code
/// page is not. That used to abort the whole attach with no message at all —
/// the picker closed and nothing happened. A file with a few replacement
/// characters in it is far better than silence.
///
/// Capped because the text goes straight into the prompt: a 20 MB log would
/// either be refused by the model or cost a fortune, and truncating with a note
/// is the only outcome that leaves the user informed.
fn read_text_capped(path: &Path, max_chars: usize) -> Option<(String, usize)> {
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let full_chars = text.chars().count();
    if full_chars <= max_chars {
        return Some((text.into_owned(), full_chars));
    }
    Some((text.chars().take(max_chars).collect(), full_chars))
}

// The chat composer's attach button — opens a native file picker and reads
// the result directly (rather than just returning a path) so the frontend
// never needs its own filesystem access. Images are read as attachable
// base64 (a vision-capable model can look at them; a text-only one will
// just ignore or error on the image part). Anything readable as text is read
// and inlined into the message as a fenced code block — any model can read
// that, no special capability needed. Structured binary formats (PDF, Excel,
// Word, PowerPoint) come back "unsupported" until a parser exists for them.
#[tauri::command]
pub async fn pick_chat_attachment(app: tauri::AppHandle) -> Option<ChatAttachment> {
    crate::offload(move || {
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
                lang: String::new(),
                full_chars: 0,
                problem: String::new(),
            });
        }
        let max_chars = crate::tunables::int(&app, crate::tunables::CHAT_ATTACHMENT_MAX_CHARS).max(0) as usize;

        if let Some(lang) = crate::content::documents::text_language(&name, &ext) {
            let (data, full_chars) = read_text_capped(&path, max_chars)?;
            return Some(ChatAttachment {
                name,
                kind: "text".to_string(),
                mime: format!("text/{ext}"),
                data,
                lang: lang.to_string(),
                full_chars,
                problem: String::new(),
            });
        }
        if crate::content::documents::is_document(&ext) {
            return Some(match crate::content::documents::extract(&path, &ext, max_chars) {
                Ok(extracted) => ChatAttachment {
                    name,
                    kind: "text".to_string(),
                    mime: format!("application/{ext}"),
                    data: extracted.text,
                    // No language: this is a spreadsheet or a document rendered as
                    // plain text, and tagging it as a language would have a
                    // highlighter guessing at prose.
                    lang: String::new(),
                    full_chars: extracted.full_chars,
                    problem: String::new(),
                },
                // A format we can read that this particular file defeated — a
                // scanned PDF, an empty workbook. Distinct from "unsupported"
                // because the reason is specific and the user can act on it.
                Err(problem) => ChatAttachment {
                    name,
                    kind: "failed".to_string(),
                    mime: ext,
                    data: String::new(),
                    lang: String::new(),
                    full_chars: 0,
                    problem,
                },
            });
        }
        Some(ChatAttachment {
            name,
            kind: "unsupported".to_string(),
            mime: ext,
            data: String::new(),
            lang: String::new(),
            full_chars: 0,
            problem: String::new(),
        })
})
    .await
}


#[cfg(test)]
mod tests {
    use super::read_text_capped;

    #[test]
    fn a_file_that_is_not_utf8_still_attaches() {
        // Windows-1254 "şğü" — invalid UTF-8. read_to_string used to return Err
        // here, which aborted the attach with no message at all.
        let dir = std::env::temp_dir().join("widget-attach-test-encoding");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("turkish.txt");
        std::fs::write(&path, [0xFE, 0xF0, 0xFC, b'!']).unwrap();

        let (text, full) = read_text_capped(&path, 1000).expect("should read despite the encoding");
        assert!(text.ends_with('!'), "got {text:?}");
        assert_eq!(full, text.chars().count());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_long_file_is_cut_and_says_how_long_it_was() {
        let dir = std::env::temp_dir().join("widget-attach-test-cap");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.txt");
        std::fs::write(&path, "x".repeat(5000)).unwrap();

        let (text, full) = read_text_capped(&path, 100).unwrap();
        assert_eq!(text.chars().count(), 100);
        // The original length is what lets the UI say "of 5000" rather than
        // silently handing the model a fifth of the file.
        assert_eq!(full, 5000);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_cap_counts_characters_not_bytes() {
        // A Turkish or CJK file would otherwise be cut mid-character, and
        // slicing a String by byte index on a char boundary panics.
        let dir = std::env::temp_dir().join("widget-attach-test-chars");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tr.txt");
        std::fs::write(&path, "ığüşöçİĞÜŞÖÇ".repeat(50)).unwrap();

        let (text, full) = read_text_capped(&path, 10).unwrap();
        assert_eq!(text.chars().count(), 10);
        assert_eq!(full, 600);
        std::fs::remove_dir_all(&dir).ok();
    }
}
