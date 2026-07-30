// Cross-conversation memory: makes an earlier conversation findable from a
// later one, whether it was typed or spoken.
//
// The index is a DERIVED CACHE, never the source of truth. Chats/*.json stays
// authoritative — save_chat rewrites a whole file and delete_chat unlinks one,
// so this mirrors them by content hash and reconciles at startup. That is what
// makes changing the embedding model later a DELETE plus a rebuild rather than
// a migration, and it means a corrupted or deleted index costs nothing.
//
// What goes to the model is never the index. It is at most a few earlier turns,
// chosen per question, and often nothing at all — see recall_context. Handing a
// model everything it has ever been told makes its answers worse, not better,
// quite apart from not fitting.
//
// This is the retrieval half. Ranking here is BM25 over SQLite's FTS5, which
// finds turns that share words with the question. It does NOT find a turn that
// said the same thing in different words — that is what the embedding column
// this schema leaves room for is for, and it is deliberately a later step: the
// pipeline below (units, incremental indexing, the relevance floor, injection)
// is the same either way, and building it first means we can measure how much
// the embeddings actually add instead of assuming.
use std::collections::HashSet;
use std::path::PathBuf;

use rusqlite::{params, Connection};
use serde::Serialize;
use tauri::Manager;

use crate::ai::chat::{Chat, ChatMessage};

/// Longest single indexed unit. A long assistant reply is split into several
/// units rather than indexed whole: BM25 scores a term against the whole
/// document, so one long turn matches everything weakly and drowns out short
/// turns that are actually about the question.
const MAX_UNIT_CHARS: usize = 1200;

/// Fraction of the question's content words a result has to actually contain.
///
/// The floor is what stops recall from injecting something merely because it
/// was the best of a bad set — with OR-matching, one incidental word in common
/// is enough to rank. Expressed as coverage rather than as a BM25 threshold
/// because BM25 scores are corpus-relative and have no meaning to compare
/// against a constant, while "the question had four content words and this turn
/// contains one of them" is the same judgement in any corpus or language.
///
/// The default only; the effective value is a setting, because how eagerly this
/// should reach for old conversations is a matter of taste.
const DEFAULT_MIN_COVERAGE: f32 = 0.5;

// Deliberately tiny, and covering both languages. A long stopword list starts
// making decisions about meaning; this one only removes words that would
// otherwise let any two sentences look related.
//
// The greetings and pleasantries at the end are here for a measured reason:
// "hello" recalled an unrelated conversation whose reply happened to open with
// "Hello!". They are function words in the same sense as "the" — they carry
// conversational form, never subject matter — so nobody's question is ever
// *about* them. Without them the coverage floor cannot help either: a query of
// one content word scores 0 or 1 and nothing in between, so the floor has
// nothing to discriminate with.
//
// The general version of that problem is a term that is common across the whole
// index carrying no information wherever it appears, which is what document
// frequency measures. Not implemented: with a handful of conversations indexed,
// every frequency is either 0% or 100% and the statistic means nothing. It is
// the right rule to add once there is a corpus to compute it over.
const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "of", "to", "in", "on", "for", "with", "is", "are", "was",
    "were", "be", "been", "it", "this", "that", "we", "i", "you", "what", "how", "why", "when",
    "did", "do", "does", "can", "could", "would", "should", "about",
    // Pronouns, and the phrasing people use to ask about the conversation
    // itself. "remind me what we decided about X" is a question about X — but
    // counted literally, three of its five content words are asking rather than
    // naming, which drags the coverage below the floor and loses the answer.
    "me", "my", "us", "our", "your", "remind", "remember", "recall", "again",
    "bana", "bize", "bizim", "benim", "senin", "hatirla", "hatirlat", "hatirliyor",
    "yine", "tekrar",
    "ve", "veya", "ama", "ile", "icin", "bir", "bu", "su", "o", "ne", "nasil", "neden", "ni̇ye",
    "mi", "mu", "ya", "de", "da", "ki", "biz", "ben", "sen", "yaptik", "yapti",
    // Greetings and pleasantries — see the note above.
    "hello", "hi", "hey", "thanks", "thank", "please", "sorry", "ok", "okay", "yes", "no", "sure",
    "merhaba", "selam", "tesekkur", "tesekkurler", "tesekkurederim", "lutfen", "tamam", "evet",
    "hayir", "gunaydin", "iyi", "gunler",
];

// --- text normalization -------------------------------------------------------

/// Folds text the way both the stored column and the query are folded, so a
/// match is decided on the same shape on both sides.
///
/// The one thing done here rather than left to the tokenizer is Turkish dotless
/// i. `remove_diacritics 2` already folds ç ğ ö ş ü and dotted İ (measured
/// against this exact SQLite build), but `ı` is not a diacritic — it is its own
/// letter, so a query typed `igdir` never matches a stored `ığdır`. That was the
/// single most common way Turkish silently failed to be findable, and it costs
/// five lines to fix. English is unaffected: no English word contains ı.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            'ı' | 'İ' | 'I' => out.push('i'),
            _ => out.extend(ch.to_lowercase()),
        }
    }
    out
}

/// Content words of a query, normalized and stopword-free.
fn content_terms(query: &str) -> Vec<String> {
    let normalized = normalize(query);
    let mut seen = HashSet::new();
    normalized
        .split(|c: char| !c.is_alphanumeric())
        .filter(|term| term.chars().count() > 1)
        .filter(|term| !STOPWORDS.contains(term))
        .filter(|term| seen.insert(term.to_string()))
        .map(str::to_string)
        .collect()
}

/// Builds the FTS5 MATCH expression: any term, not all of them.
///
/// FTS5 ANDs bare terms by default, which for a natural question means a result
/// has to contain every word of it — in practice, nothing matches. OR plus BM25
/// ranking plus the coverage floor gets the same precision without the
/// all-or-nothing failure.
fn match_expression(terms: &[String]) -> Option<String> {
    if terms.is_empty() {
        return None;
    }
    // Quoted so a term containing FTS5 syntax (a bare `-`, `*`, `^`) is taken as
    // a string rather than as an operator. Embedded quotes are doubled, which is
    // FTS5's own escape.
    Some(
        terms
            .iter()
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

/// Stable content hash, so re-indexing an unchanged chat writes nothing.
///
/// FNV-1a rather than DefaultHasher: the standard hasher's output is explicitly
/// not guaranteed stable across Rust versions, and while a changed hash here
/// would only cost one needless re-index, silently re-indexing everything after
/// a toolchain bump is the kind of thing nobody would ever notice or explain.
fn content_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

// --- the unit of indexing -----------------------------------------------------

/// One question and its answer, indexed together.
///
/// The pair rather than the message: embedding or indexing a bare question loses
/// what was decided, and a bare answer loses what it was answering. "What did we
/// decide about X" lives across both halves, so splitting them makes the most
/// important case the one that works worst.
#[derive(Debug, Clone, PartialEq)]
pub struct Unit {
    pub chat_id: String,
    pub chat_title: String,
    pub first_index: usize,
    pub last_index: usize,
    pub ts_start: u64,
    pub ts_end: u64,
    pub source: String,
    pub text: String,
}

/// Splits a chat into question/answer units.
///
/// Assistant messages before the first question are dropped: a conversation that
/// opens with the assistant is a greeting, and there is no question for it to be
/// the answer to.
pub fn build_units(chat: &Chat) -> Vec<Unit> {
    let mut units = Vec::new();
    let mut current: Option<(usize, Vec<&ChatMessage>)> = None;

    for (index, message) in chat.messages.iter().enumerate() {
        if message.role == "user" {
            if let Some((start, group)) = current.take() {
                units.extend(unit_from_group(chat, start, &group));
            }
            current = Some((index, vec![message]));
        } else if let Some((_, group)) = current.as_mut() {
            group.push(message);
        }
    }
    if let Some((start, group)) = current {
        units.extend(unit_from_group(chat, start, &group));
    }
    units
}

fn unit_from_group(chat: &Chat, start: usize, group: &[&ChatMessage]) -> Vec<Unit> {
    let question = group[0].content.trim();
    // A message can be nothing but a file with no words of its own, and that is
    // still an exchange worth finding — by the file's name, which the line below
    // puts into the indexed text.
    let attached: Vec<&str> = group
        .iter()
        .flat_map(|m| m.attachments.iter())
        .map(|a| a.name.as_str())
        .collect();
    if question.is_empty() && attached.is_empty() {
        return Vec::new();
    }

    // Attached file NAMES are indexed; their CONTENTS deliberately are not.
    //
    // The contents already reach the model in full — inlined into the message
    // when it is sent, and re-inlined into the history on every later turn of
    // that conversation — so retrieving chunks of them would be strictly lossier
    // than what already happens. Indexing them would cost far more than it
    // returns: one 60,000-character file is about fifty units against a whole
    // corpus of nineteen, and the similarity floor is measured from
    // cross-document pairs, so document prose would end up calibrating a
    // threshold meant for conversation turns.
    //
    // The name is the proportionate half: it makes the CONVERSATION findable
    // ("that nutrition plan"), and the file is still sitting in it to open.
    let question = if attached.is_empty() {
        question.to_string()
    } else {
        format!("{question} [attached: {}]", attached.join(", ")).trim().to_string()
    };
    let question = question.as_str();
    let answer = group[1..]
        .iter()
        .map(|m| m.content.trim())
        .filter(|c| !c.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    let ts_start = group[0].ts;
    let ts_end = group.last().map(|m| m.ts).unwrap_or(ts_start);
    // A unit is attributed to voice if any half of it was spoken — the point of
    // the field is "was this said out loud", and a spoken question answered
    // aloud is one exchange however the reply happens to be stored.
    let source = if group.iter().any(|m| m.source == "voice") {
        "voice".to_string()
    } else {
        String::new()
    };

    // Every window repeats the question, so a window cut out of the middle of a
    // long answer still carries what it is an answer to.
    let windows = split_answer(question, &answer);
    windows
        .into_iter()
        .map(|text| Unit {
            chat_id: chat.id.clone(),
            chat_title: chat.title.clone(),
            first_index: start,
            last_index: start + group.len() - 1,
            ts_start,
            ts_end,
            source: source.clone(),
            text,
        })
        .collect()
}

fn split_answer(question: &str, answer: &str) -> Vec<String> {
    if answer.is_empty() {
        // A question nobody answered is still worth finding — it is a thing that
        // was asked, and often the thing being asked about again.
        return vec![format!("Q: {question}")];
    }
    split_with_head(&format!("Q: {question}\nA: "), answer)
}

/// Cuts `body` into units of at most MAX_UNIT_CHARS, repeating `head` on each.
///
/// The head repeats because a window taken out of the middle of a long body
/// loses what it is about otherwise — for a conversation that is the question,
/// for a note its name. Either way the piece has to stand on its own: BM25 and
/// the embedding both only ever see one unit at a time.
fn split_with_head(head: &str, body: &str) -> Vec<String> {
    let room = MAX_UNIT_CHARS.saturating_sub(head.chars().count()).max(200);

    let mut windows = Vec::new();
    let mut rest: Vec<char> = body.chars().collect();
    while !rest.is_empty() {
        let take = rest.len().min(room);
        // Cut at whitespace so a word is never split across two units, unless
        // there is no whitespace to cut at.
        let cut = if take == rest.len() {
            take
        } else {
            rest[..take]
                .iter()
                .rposition(|c| c.is_whitespace())
                .map(|i| i + 1)
                .unwrap_or(take)
        };
        let chunk: String = rest[..cut].iter().collect();
        windows.push(format!("{head}{}", chunk.trim()));
        rest.drain(..cut);
    }
    windows
}

// --- notes --------------------------------------------------------------------
//
// Claude Code writes durable notes of its own to ~/.claude/projects/*/memory/
// — one curated fact per file, with a name and a one-line description. The
// Workspace window already shows them as a graph (content/memory.rs); indexing
// them here makes them findable from a question instead of only by eye.
//
// They are worth indexing precisely because they are curated: a note is
// something judged worth keeping, which is a stronger signal than any single
// conversation turn. What they are NOT is ours to write — see content/memory.rs.

/// A memory file, flattened to the fields indexing needs.
///
/// Separate from content::memory::MemoryNode so the conversion below is a plain
/// function over plain data, testable without a running Tauri app.
#[derive(Debug, Clone)]
pub struct NoteSource {
    pub name: String,
    pub description: String,
    pub body: String,
    /// Which project the note was written under, as a short human name rather
    /// than Claude Code's directory slug. Notes are cross-project — the graph
    /// shows every project's — so this is how a recalled note says where it
    /// came from instead of appearing to be about whatever is open now.
    pub project: String,
    pub updated_at: u64,
}

/// The chat_id a note's units are filed under.
///
/// Prefixed so it can never collide with a real conversation: chat ids are
/// UUIDs, and nothing that came out of `uuid::Uuid::new_v4()` starts with
/// "note:". That prefix is also what lets reconcile drop notes wholesale when
/// the setting is turned off.
pub const NOTE_ID_PREFIX: &str = "note:";

pub fn note_units(notes: &[NoteSource]) -> Vec<Unit> {
    let mut units = Vec::new();
    for note in notes {
        // The description carries the subject in a full sentence where the name
        // is only a slug, so both go in the head and repeat on every window.
        let head = if note.description.is_empty() {
            format!("Note \"{}\": ", note.name)
        } else {
            format!("Note \"{}\" — {}\n", note.name, note.description)
        };
        let body = note.body.trim();
        let texts = if body.is_empty() {
            // A note with only a description is still a fact worth finding.
            vec![head.trim_end().to_string()]
        } else {
            split_with_head(&head, body)
        };
        let title = if note.project.is_empty() {
            note.name.clone()
        } else {
            format!("{} ({})", note.name, note.project)
        };
        for text in texts {
            units.push(Unit {
                chat_id: format!("{NOTE_ID_PREFIX}{}", note.name),
                chat_title: title.clone(),
                first_index: 0,
                last_index: 0,
                ts_start: note.updated_at,
                ts_end: note.updated_at,
                // Read by format_block and by the chat UI's recall note, both of
                // which say "note" rather than naming a conversation that does
                // not exist.
                source: "memory".to_string(),
                text,
            });
        }
    }
    units
}

// --- storage ------------------------------------------------------------------

fn db_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .expect("app data dir must be resolvable");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("recall.sqlite3")
}

/// `porter` wraps unicode61 and stems English, which is where it works: it is an
/// English algorithm and there is no Turkish equivalent here, so Turkish gets
/// diacritic folding and the dotless-i fold above but not stemming. That is the
/// honest split — English is the priority, and what Turkish can get for free it
/// gets.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS turns (
  id          INTEGER PRIMARY KEY,
  chat_id     TEXT NOT NULL,
  chat_title  TEXT NOT NULL,
  first_index INTEGER NOT NULL,
  last_index  INTEGER NOT NULL,
  ts_start    INTEGER NOT NULL,
  ts_end      INTEGER NOT NULL,
  source      TEXT NOT NULL,
  hash        TEXT NOT NULL,
  text        TEXT NOT NULL,
  -- Filled in later and separately from the row itself, by the background
  -- embedder: a turn is searchable by its words the moment it is written, and
  -- becomes searchable by its meaning whenever the embedding server gets to it.
  vec         BLOB,
  vec_model   TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS turns_chat_hash ON turns(chat_id, hash);
CREATE INDEX IF NOT EXISTS turns_ts ON turns(ts_end);
CREATE VIRTUAL TABLE IF NOT EXISTS turns_fts USING fts5(
  norm,
  tokenize = 'porter unicode61 remove_diacritics 2'
);
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

/// Bumped whenever the tables above change shape. Because this whole file is a
/// cache over Chats/*.json, an old version is thrown away and rebuilt rather
/// than migrated — which is the point of the index being derived, and is why
/// changing the embedding model costs nothing to reason about.
const SCHEMA_VERSION: &str = "2";

pub fn open_db(path: &std::path::Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    // WAL so the background indexer and a live query never block each other.
    let _ = conn.pragma_update(None, "journal_mode", "WAL");
    conn.execute_batch(SCHEMA)?;

    let version: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get(0))
        .ok();
    if version.as_deref() != Some(SCHEMA_VERSION) {
        conn.execute_batch(
            "DROP TABLE IF EXISTS turns;
             DROP TABLE IF EXISTS turns_fts;
             DROP TABLE IF EXISTS meta;",
        )?;
        conn.execute_batch(SCHEMA)?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION],
        )?;
    }
    Ok(conn)
}

fn meta_get(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| r.get(0))
        .ok()
}

fn meta_set(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        params![key, value],
    )?;
    Ok(())
}

/// Brings the index in line with one chat's current contents.
///
/// Diffed by hash rather than rewritten: a chat gains one turn at a time, and
/// re-embedding an entire conversation on every message is the difference
/// between this being free and it being something the user notices.
pub fn sync_chat(conn: &Connection, chat: &Chat) -> rusqlite::Result<(usize, usize)> {
    sync_units(conn, &chat.id, build_units(chat))
}

/// The same diff for anything that groups units under one id — a conversation
/// or a memory note. Split out because notes are indexed the same way and
/// having two copies of a hash diff is how the two quietly stop agreeing.
pub fn sync_units(conn: &Connection, chat_id: &str, units: Vec<Unit>) -> rusqlite::Result<(usize, usize)> {
    let wanted: Vec<(String, Unit)> = units
        .into_iter()
        .map(|u| (content_hash(&u.text), u))
        .collect();
    let wanted_hashes: HashSet<&str> = wanted.iter().map(|(h, _)| h.as_str()).collect();

    let mut existing: Vec<(i64, String)> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT id, hash FROM turns WHERE chat_id = ?1")?;
        let rows = stmt.query_map(params![chat_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        for row in rows {
            existing.push(row?);
        }
    }
    let existing_hashes: HashSet<&str> = existing.iter().map(|(_, h)| h.as_str()).collect();

    let mut removed = 0;
    for (id, hash) in &existing {
        if !wanted_hashes.contains(hash.as_str()) {
            conn.execute("DELETE FROM turns WHERE id = ?1", params![id])?;
            conn.execute("DELETE FROM turns_fts WHERE rowid = ?1", params![id])?;
            removed += 1;
        }
    }

    let mut added = 0;
    for (hash, unit) in &wanted {
        if existing_hashes.contains(hash.as_str()) {
            continue;
        }
        conn.execute(
            "INSERT INTO turns (chat_id, chat_title, first_index, last_index, ts_start, ts_end, source, hash, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                unit.chat_id,
                unit.chat_title,
                unit.first_index as i64,
                unit.last_index as i64,
                unit.ts_start as i64,
                unit.ts_end as i64,
                unit.source,
                hash,
                unit.text,
            ],
        )?;
        let id = conn.last_insert_rowid();
        // The chat's title rides along in the searchable text: it is often the
        // only place the subject is named in so many words.
        conn.execute(
            "INSERT INTO turns_fts (rowid, norm) VALUES (?1, ?2)",
            params![id, normalize(&format!("{} {}", unit.chat_title, unit.text))],
        )?;
        added += 1;
    }
    Ok((added, removed))
}

/// Drops rows for chats whose file no longer exists. Without this a deleted
/// conversation stays findable forever, which is worse than not indexing at all.
pub fn reconcile(conn: &Connection, live_chat_ids: &HashSet<String>) -> rusqlite::Result<usize> {
    let mut stale: Vec<i64> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT id, chat_id FROM turns")?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, chat_id) = row?;
            if !live_chat_ids.contains(&chat_id) {
                stale.push(id);
            }
        }
    }
    for id in &stale {
        conn.execute("DELETE FROM turns WHERE id = ?1", params![id])?;
        conn.execute("DELETE FROM turns_fts WHERE rowid = ?1", params![id])?;
    }
    Ok(stale.len())
}

// --- retrieval ----------------------------------------------------------------

#[derive(Debug, Serialize, Clone)]
pub struct Hit {
    pub chat_id: String,
    pub chat_title: String,
    pub ts_end: u64,
    pub source: String,
    pub text: String,
    /// BM25, negated so that larger is better — SQLite returns it the other way
    /// round, and a score that sorts backwards is a bug waiting to be written.
    pub score: f64,
    pub coverage: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct SearchOptions {
    pub limit: usize,
    /// Inclusive millisecond bounds, when the question named a time.
    pub after_ms: Option<u64>,
    pub before_ms: Option<u64>,
    /// Set for a question that is ONLY about a time ("what did we talk about
    /// five days ago") — there are no terms to rank by, so the range itself is
    /// the whole query and the newest turns in it are the answer.
    pub time_only: bool,
    /// At most this many results from any single conversation; 0 for no limit.
    ///
    /// A long answer is indexed as several windows, so without this one
    /// conversation that matches well can take every slot — and the slots are
    /// what gets shown to the model. Three windows of one conversation say less
    /// than one window each of three, because the windows of a single answer
    /// mostly repeat each other.
    pub max_per_chat: usize,
    /// Minimum share of the question's content words a result must contain.
    pub min_coverage: f32,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            limit: 3,
            after_ms: None,
            before_ms: None,
            time_only: false,
            max_per_chat: 0,
            min_coverage: DEFAULT_MIN_COVERAGE,
        }
    }
}

/// Word search, optionally fused with meaning search.
///
/// `query_vec` is the question's own embedding. When it is absent — no server
/// configured, or the call failed — this is exactly the word search, which is
/// why the feature can be turned on and off without the rest of the app
/// noticing.
/// `exclude_chat` is the conversation the question is being asked in, and it is
/// left out of the results.
///
/// Not an optimization. Saving a chat indexes it, and the chat is saved before
/// the question is sent — so without this the search reliably finds the very
/// message being asked, and "remind me what we decided about caching" recalls
/// itself. The current conversation is already in the request as history; recall
/// is for the ones that are not.
pub fn search_hybrid(
    conn: &Connection,
    query: &str,
    opts: SearchOptions,
    query_vec: Option<&[f32]>,
    exclude_chat: &str,
) -> rusqlite::Result<Vec<Hit>> {
    let limit = opts.limit.max(1) as i64;
    let after = opts.after_ms.unwrap_or(0) as i64;
    let before = opts.before_ms.unwrap_or(u64::MAX / 2) as i64;

    if opts.time_only {
        // Over-fetched for the same reason as below: the per-conversation cap is
        // applied here, not in SQL, so a LIMIT alone would return fewer than
        // asked for once duplicates are dropped.
        let mut stmt = conn.prepare(
            "SELECT chat_id, chat_title, ts_end, source, text FROM turns
             WHERE ts_end >= ?1 AND ts_end <= ?2 AND chat_id <> ?4
             ORDER BY ts_end DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![after, before, limit * 5, exclude_chat], |row| {
            Ok(Hit {
                chat_id: row.get(0)?,
                chat_title: row.get(1)?,
                ts_end: row.get::<_, i64>(2)? as u64,
                source: row.get(3)?,
                text: row.get(4)?,
                score: 0.0,
                coverage: 1.0,
            })
        })?;
        let mut hits: Vec<Hit> = Vec::new();
        for row in rows {
            let hit = row?;
            if opts.max_per_chat > 0
                && hits.iter().filter(|h| h.chat_id == hit.chat_id).count() >= opts.max_per_chat
            {
                continue;
            }
            hits.push(hit);
            if hits.len() >= limit as usize {
                break;
            }
        }
        return Ok(hits);
    }

    let terms = content_terms(query);
    // No content words at all — a greeting, or nothing but stopwords. Meaning
    // search is skipped too: embedding "hello" and finding whatever is nearest
    // is precisely the false recall the floors exist to prevent.
    let Some(expression) = match_expression(&terms) else {
        return Ok(Vec::new());
    };

    // Over-fetched, because the coverage floor below rejects some of these and a
    // limit applied before filtering would quietly return fewer than asked for.
    let mut candidates: Vec<(i64, Hit)> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT turns_fts.rowid, t.chat_id, t.chat_title, t.ts_end, t.source, t.text, bm25(turns_fts)
             FROM turns_fts JOIN turns t ON t.id = turns_fts.rowid
             WHERE turns_fts MATCH ?1 AND t.ts_end >= ?2 AND t.ts_end <= ?3 AND t.chat_id <> ?5
             ORDER BY bm25(turns_fts) LIMIT ?4",
        )?;
        let rows = stmt.query_map(params![expression, after, before, limit * 5, exclude_chat], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                Hit {
                    chat_id: row.get(1)?,
                    chat_title: row.get(2)?,
                    ts_end: row.get::<_, i64>(3)? as u64,
                    source: row.get(4)?,
                    text: row.get(5)?,
                    score: -row.get::<_, f64>(6)?,
                    coverage: 0.0,
                },
            ))
        })?;
        for row in rows {
            candidates.push(row?);
        }
    }
    // Turns whose WORDS match, in BM25 order, once the coverage floor has had
    // its say. Kept as a full list rather than truncated here: it is one of two
    // inputs to the fusion below, and truncating an input before fusing throws
    // away exactly the results the other method might have promoted.
    let mut by_words: Vec<(i64, Hit)> = Vec::new();
    if !candidates.is_empty() {
        let ids: Vec<i64> = candidates.iter().map(|(id, _)| *id).collect();
        for (id, mut hit) in candidates {
            hit.coverage = coverage(conn, &terms, &ids, id)?;
            if hit.coverage + f32::EPSILON >= opts.min_coverage {
                by_words.push((id, hit));
            }
        }
    }

    // Turns whose MEANING matches, if there is a question vector to compare
    // against. These need no coverage floor — they were selected by a similarity
    // threshold calibrated against this user's own history, which is the same
    // job done in the units the vectors are actually in.
    // Skipped entirely when there is no calibrated floor, rather than falling
    // back to a constant. A cosine threshold means nothing without knowing what
    // the model scores unrelated text at — measured, bge-m3 puts unrelated pairs
    // near 0.32 and related ones from 0.41, so any figure chosen in advance is
    // either "everything matches" or "nothing does". Until the index is big
    // enough to measure that, word search is the whole answer, which is a
    // complete answer.
    let mut by_meaning: Vec<i64> = Vec::new();
    if let (Some(vector), Some(floor)) = (
        query_vec,
        meta_get(conn, "similarity_floor").and_then(|v| v.parse::<f32>().ok()),
    ) {
        by_meaning = similar_turns(conn, vector, floor, after, before, exclude_chat)?
            .into_iter()
            .map(|(id, _)| id)
            .collect();
    }

    if by_words.is_empty() && by_meaning.is_empty() {
        return Ok(Vec::new());
    }

    let word_ids: Vec<i64> = by_words.iter().map(|(id, _)| *id).collect();
    let ordered = fuse(&[&word_ids, &by_meaning]);

    let mut hits: Vec<Hit> = Vec::new();
    for id in ordered {
        let hit = match by_words.iter().find(|(candidate, _)| *candidate == id) {
            Some((_, hit)) => hit.clone(),
            // Found by meaning alone, so its row was never loaded above.
            None => match load_hit(conn, id)? {
                Some(hit) => hit,
                None => continue,
            },
        };
        if opts.max_per_chat > 0
            && hits.iter().filter(|h| h.chat_id == hit.chat_id).count() >= opts.max_per_chat
        {
            continue;
        }
        hits.push(hit);
        if hits.len() >= limit as usize {
            break;
        }
    }
    Ok(hits)
}

fn load_hit(conn: &Connection, id: i64) -> rusqlite::Result<Option<Hit>> {
    let mut stmt =
        conn.prepare("SELECT chat_id, chat_title, ts_end, source, text FROM turns WHERE id = ?1")?;
    let mut rows = stmt.query_map(params![id], |row| {
        Ok(Hit {
            chat_id: row.get(0)?,
            chat_title: row.get(1)?,
            ts_end: row.get::<_, i64>(2)? as u64,
            source: row.get(3)?,
            text: row.get(4)?,
            score: 0.0,
            // Found by meaning rather than by words, so word coverage is not the
            // measure that admitted it — reported as zero rather than faked.
            coverage: 0.0,
        })
    })?;
    rows.next().transpose()
}

/// Fraction of the question's content words a candidate actually contains.
///
/// Asked of FTS5 per term rather than by substring-searching the stored text,
/// because the two do not agree: the `porter` tokenizer is why "caching" finds a
/// turn that said "cache", and a substring check would then reject that same
/// turn for not containing the letters "caching" — quietly undoing the stemming
/// that made the match possible. Running each term back through the same index
/// makes the floor consistent with the matching by construction, in whatever
/// language, for whatever tokenizer is configured.
///
/// One small query per content word (typically two to five), scoped to the
/// candidates already found.
fn coverage(conn: &Connection, terms: &[String], candidate_ids: &[i64], id: i64) -> rusqlite::Result<f32> {
    if terms.is_empty() {
        return Ok(0.0);
    }
    // The ids are i64 read out of this same table a moment ago, so inlining them
    // is not a string-injection surface — and a dynamic placeholder list would
    // have to be rebuilt per call anyway.
    let id_list = candidate_ids
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");

    let mut present = 0;
    for term in terms {
        let expression = format!("\"{}\"", term.replace('"', "\"\""));
        let sql = format!(
            "SELECT 1 FROM turns_fts WHERE turns_fts MATCH ?1 AND rowid = ?2 AND rowid IN ({id_list}) LIMIT 1"
        );
        let matched: Option<i64> = conn
            .query_row(&sql, params![expression, id], |row| row.get(0))
            .ok();
        if matched.is_some() {
            present += 1;
        }
    }
    Ok(present as f32 / terms.len() as f32)
}

// --- meaning, not just words --------------------------------------------------
//
// Everything above finds a conversation by the words it used. This finds one
// that made the same point in different words — "we'll memoize the results"
// against a question about "the caching approach", which shares no term and is
// invisible to BM25 at any index size.
//
// Off unless configured, and it degrades to exactly the behaviour above: the
// index is usable from the first message either way, and a turn simply gains a
// second way of being found once the embedder reaches it.
//
// No model is bundled. Shipping one would mean ~30MB of weights and a tokenizer
// in the binary for a feature not everyone wants; instead this speaks the
// OpenAI-compatible /v1/embeddings shape, which Ollama and every local runtime
// in this space already serve — the same choice the rest of the app makes for
// chat, speech and transcription.

/// How long a bulk embedding request may take.
///
/// Generous because this one runs on a background thread with nothing waiting on
/// it, and a first pass over a long history is genuinely slow — especially when
/// the embedding model and the chat model share one Ollama instance and it has
/// to swap them in and out of VRAM.
const EMBED_BACKFILL_TIMEOUT_SECS: u64 = 120;

/// How long the ONE embedding of a question may take before recall gives up on
/// meaning search for that question and answers with word search alone.
///
/// Short, and it has to be: this sits between pressing send and the request
/// going out. Measured with an embedding server that accepts and never answers,
/// the 120-second budget above turned a single message into a two-minute wait —
/// and the whole design of this module is that meaning search is the optional
/// half. Giving up quietly after a few seconds is the behaviour that matches
/// that claim.
const EMBED_QUERY_TIMEOUT_SECS: u64 = 6;
const EMBED_BATCH: usize = 32;

fn to_blob(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn from_blob(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Cosine similarity of two vectors, or 0 for a mismatch.
///
/// Length mismatch means the two were produced by different models, which is a
/// real possibility every time the model setting changes — comparing them anyway
/// would produce a confident number that means nothing.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Asks the configured server for one vector per input.
pub fn embed(
    url: &str,
    model: &str,
    api_key: &str,
    inputs: &[String],
    timeout_secs: u64,
) -> Result<Vec<Vec<f32>>, String> {
    #[derive(serde::Deserialize)]
    struct EmbeddingItem {
        embedding: Vec<f32>,
    }
    #[derive(serde::Deserialize)]
    struct EmbeddingResponse {
        data: Vec<EmbeddingItem>,
    }

    let endpoint = format!("{}/embeddings", url.trim().trim_end_matches('/'));
    let mut request = reqwest::blocking::Client::new()
        .post(&endpoint)
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .json(&serde_json::json!({ "model": model, "input": inputs }));
    if !api_key.trim().is_empty() {
        request = request.bearer_auth(api_key.trim());
    }

    let response = request
        .send()
        .map_err(|e| format!("couldn't reach the embedding server: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(format!("embedding server returned {status}: {}", body.trim()));
    }
    let parsed: EmbeddingResponse = response
        .json()
        .map_err(|e| format!("couldn't read the embedding response: {e}"))?;
    if parsed.data.len() != inputs.len() {
        return Err(format!(
            "asked for {} embeddings and got {}",
            inputs.len(),
            parsed.data.len()
        ));
    }
    Ok(parsed.data.into_iter().map(|d| d.embedding).collect())
}

/// The similarity above which two turns count as being about the same thing.
///
/// Calibrated against the user's own history rather than hardcoded, because a
/// cosine threshold is meaningless without knowing the model: some families put
/// unrelated text at 0.1 and others at 0.7, so one constant is either useless or
/// blocks everything. Sampling random pairs measures what "unrelated" actually
/// looks like here, and two standard deviations above that is the point where a
/// pair stops looking like chance.
/// How far above the average unrelated pair a match has to score.
///
/// Measured, not chosen. Against the real corpus on this machine (9 curated
/// notes, bge-m3): cross-document pairs came out at mean 0.537, σ 0.096, and
/// five questions whose correct answer was known scored 0.491 to 0.650 against
/// it, with the best *wrong* note at 0.443 to 0.573 and a deliberately
/// off-topic question topping out at 0.288.
///
/// Sweeping the multiplier against that:
///
/// ```text
/// k     floor   answered  missed  off-topic injected
/// 0.0   0.537      4        1       no
/// 0.5   0.585      4        1       no
/// 1.0   0.632      2        3       no
/// 2.0   0.728      0        5       no
/// ```
///
/// 2σ — which this was — answers nothing at all, so meaning search was running
/// and finding nothing by construction. 0.5σ gets four of five while leaving the
/// off-topic question a wide margin below the line, and unlike 0σ it still means
/// something defensible: "closer than a typical unrelated pair". Five cases is
/// thin evidence for the exact figure; it is conclusive evidence against 2.
const SIGMA_MARGIN: f32 = 0.5;

fn calibrate_similarity_floor(conn: &Connection) -> rusqlite::Result<Option<f32>> {
    let mut stmt = conn.prepare("SELECT chat_id, vec FROM turns WHERE vec IS NOT NULL LIMIT 400")?;
    let vectors: Vec<(String, Vec<f32>)> = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?
        .filter_map(|r| r.ok())
        .map(|(chat_id, blob)| (chat_id, from_blob(&blob)))
        .collect();
    // Below this there is nothing to calibrate against and any figure would be
    // an artefact of two or three documents. Thin, but it is measurement rather
    // than guesswork — and the alternative is not a safer constant, it is a
    // number picked without knowing the model. Measured against bge-m3:
    // unrelated pairs land around 0.32 and genuinely related ones between 0.41
    // and 0.64, so a plausible-sounding 0.6 rejects most true matches while 0.3
    // accepts everything.
    if vectors.len() < 8 {
        return Ok(None);
    }

    let mut samples = Vec::new();
    for i in 0..vectors.len() {
        // A fixed stride rather than random pairs: this has to give the same
        // answer every time it runs, or the floor drifts between startups for no
        // reason the user could ever observe or explain.
        for step in [1, 3, 7, 13] {
            let j = (i + step) % vectors.len();
            if i == j {
                continue;
            }
            // Two units of the SAME document are not an example of "unrelated" —
            // they are consecutive windows of one note or one conversation, and
            // they repeat the same head by design, so they score near 1.0.
            // Sampling them drags the mean up and the floor lands above every
            // true match. Measured: on a real corpus of 4 conversation turns and
            // 14 note windows, including them put the floor at 0.75 while actual
            // question-to-document matches scored 0.53-0.64 — meaning search was
            // rejecting everything it was supposed to find.
            if vectors[i].0 == vectors[j].0 {
                continue;
            }
            samples.push(cosine(&vectors[i].1, &vectors[j].1));
        }
    }
    // Everything indexed so far belongs to one document — there is no
    // cross-document pair to measure, so there is nothing to calibrate yet.
    if samples.len() < 8 {
        return Ok(None);
    }
    let mean = samples.iter().sum::<f32>() / samples.len() as f32;
    let variance = samples.iter().map(|s| (s - mean).powi(2)).sum::<f32>() / samples.len() as f32;
    Ok(Some((mean + SIGMA_MARGIN * variance.sqrt()).clamp(0.2, 0.95)))
}

/// Vector search over the whole table.
///
/// A flat scan, deliberately: at the scale one person's conversations reach, an
/// approximate index costs a dependency and some recall to save time that is
/// already dwarfed by the round trip that produced the query vector.
fn similar_turns(
    conn: &Connection,
    query_vec: &[f32],
    floor: f32,
    after: i64,
    before: i64,
    exclude_chat: &str,
) -> rusqlite::Result<Vec<(i64, f32)>> {
    let mut stmt = conn.prepare(
        "SELECT id, vec FROM turns
         WHERE vec IS NOT NULL AND ts_end >= ?1 AND ts_end <= ?2 AND chat_id <> ?3",
    )?;
    let mut scored: Vec<(i64, f32)> = stmt
        .query_map(params![after, before, exclude_chat], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?
        .filter_map(|r| r.ok())
        .map(|(id, blob)| (id, cosine(query_vec, &from_blob(&blob))))
        .filter(|(_, score)| *score >= floor)
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    Ok(scored)
}

/// Combines the word ranking and the meaning ranking.
///
/// Reciprocal rank fusion rather than adding the scores: BM25 and cosine are not
/// on the same scale and never will be, so any weighted sum is really a guess
/// about two arbitrary units. Fusing by POSITION needs no such guess, and a turn
/// that both methods rank highly beats one that only one of them found — which
/// is exactly the judgement wanted.
/// Takes any number of ranked lists, because the same problem shows up again
/// with more than two: web/search.rs fuses one list per search source, which
/// are no more comparable to each other than BM25 is to cosine.
pub(crate) fn fuse(ranked_lists: &[&[i64]]) -> Vec<i64> {
    const K: f32 = 60.0; // the standard damping constant for RRF
    let mut scores: std::collections::HashMap<i64, f32> = std::collections::HashMap::new();
    for list in ranked_lists {
        for (rank, id) in list.iter().enumerate() {
            *scores.entry(*id).or_insert(0.0) += 1.0 / (K + rank as f32 + 1.0);
        }
    }
    let mut ids: Vec<(i64, f32)> = scores.into_iter().collect();
    // Ties broken by id so the order is stable rather than dependent on hash
    // iteration order, which would make the same question answer differently.
    ids.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    ids.into_iter().map(|(id, _)| id).collect()
}

// --- "five days ago" ----------------------------------------------------------

/// Folds Turkish letters to ASCII. Used only for matching the keywords below,
/// never for anything stored: the index gets `normalize`, and letting two
/// different folds reach the same column is how they end up disagreeing.
fn ascii_fold(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'ç' => 'c',
            'ğ' => 'g',
            'ö' => 'o',
            'ş' => 's',
            'ü' => 'u',
            other => other,
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeQuery {
    pub after_ms: u64,
    pub before_ms: u64,
    /// The question was about a time and nothing else, so there is nothing to
    /// rank against and the range itself is the answer.
    pub time_only: bool,
}

// Words that ask about the conversation rather than about a subject. They are
// what is left over in "what did we talk about five days ago" once the time
// expression and the stopwords are gone — so if nothing but these remains, the
// question is purely temporal.
const CONVERSATION_WORDS: &[&str] = &[
    "talk", "talked", "talking", "discuss", "discussed", "say", "said", "tell", "told", "speak",
    "spoke", "chat", "chatted", "mention", "mentioned", "konus", "konustuk", "konusmustuk",
    "konusuyorduk", "bahset", "bahsettik", "dedik", "demistik", "soyledik", "anlatmistim",
];

/// Reads a time expression out of a question, in either language.
///
/// Days are resolved against the user's local calendar rather than as a rolling
/// 24 hours: "yesterday" means yesterday, not "somewhere between 24 and 48 hours
/// ago", and the difference shows up every time somebody asks in the morning.
pub fn parse_time_range(query: &str, now: chrono::DateTime<chrono::Local>) -> Option<TimeQuery> {
    use chrono::{Duration, TimeZone};

    let folded = ascii_fold(&normalize(query));
    let words: Vec<String> = folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();

    let day_start = |days_ago: i64| -> Option<(u64, u64)> {
        let day = (now - Duration::days(days_ago)).date_naive();
        let start = chrono::Local
            .from_local_datetime(&day.and_hms_opt(0, 0, 0)?)
            .single()?;
        let end = start + Duration::days(1) - Duration::milliseconds(1);
        Some((start.timestamp_millis().max(0) as u64, end.timestamp_millis().max(0) as u64))
    };
    // A window rather than the previous calendar week: "last week" in ordinary
    // use means "the last several days", and a strict Monday-to-Sunday reading
    // would answer "nothing" for anything said on Monday morning.
    let window = |days: i64| -> Option<(u64, u64)> {
        let start = now - Duration::days(days);
        Some((start.timestamp_millis().max(0) as u64, now.timestamp_millis().max(0) as u64))
    };

    let mut consumed: Vec<usize> = Vec::new();
    let mut range = None;

    for (i, word) in words.iter().enumerate() {
        let next = words.get(i + 1).map(String::as_str);
        let after_next = words.get(i + 2).map(String::as_str);

        match word.as_str() {
            "dun" | "yesterday" => {
                range = day_start(1);
                consumed.push(i);
            }
            "bugun" | "today" => {
                range = day_start(0);
                consumed.push(i);
            }
            "gecen" if next == Some("hafta") => {
                range = window(7);
                consumed.extend([i, i + 1]);
            }
            "gecen" if next == Some("ay") => {
                range = window(30);
                consumed.extend([i, i + 1]);
            }
            "last" if next == Some("week") => {
                range = window(7);
                consumed.extend([i, i + 1]);
            }
            "last" if next == Some("month") => {
                range = window(30);
                consumed.extend([i, i + 1]);
            }
            _ => {
                let Ok(n) = word.parse::<i64>() else { continue };
                if !(1..=3650).contains(&n) {
                    continue;
                }
                let unit_and_marker = (next, after_next);
                match unit_and_marker {
                    (Some("gun"), Some("once")) => {
                        range = day_start(n);
                        consumed.extend([i, i + 1, i + 2]);
                    }
                    (Some("hafta"), Some("once")) => {
                        range = day_start(n * 7);
                        consumed.extend([i, i + 1, i + 2]);
                    }
                    (Some("day" | "days"), Some("ago")) => {
                        range = day_start(n);
                        consumed.extend([i, i + 1, i + 2]);
                    }
                    (Some("week" | "weeks"), Some("ago")) => {
                        range = day_start(n * 7);
                        consumed.extend([i, i + 1, i + 2]);
                    }
                    _ => {}
                }
            }
        }
        if range.is_some() {
            break;
        }
    }

    let (after_ms, before_ms) = range?;

    // Whatever is left once the time expression is removed decides whether this
    // is "what did we talk about then" or "what did we decide about X then".
    let leftover: Vec<&String> = words
        .iter()
        .enumerate()
        .filter(|(i, _)| !consumed.contains(i))
        .map(|(_, w)| w)
        .filter(|w| w.chars().count() > 1)
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .filter(|w| !CONVERSATION_WORDS.contains(&w.as_str()))
        .filter(|w| w.parse::<i64>().is_err())
        .collect();

    Some(TimeQuery {
        after_ms,
        before_ms,
        time_only: leftover.is_empty(),
    })
}

// --- the app-facing layer -----------------------------------------------------

/// Whether a model endpoint is somewhere on this machine or this network,
/// rather than somebody else's server.
///
/// This is the whole basis of the privacy guard below, so it errs toward
/// "remote": anything it cannot confidently place is treated as remote, because
/// the cost of being wrong in that direction is a missed recall, and the cost of
/// being wrong the other way is the user's conversation history leaving the
/// building.
pub fn is_local_endpoint(base_url: &str) -> bool {
    let trimmed = base_url.trim();
    let after_scheme = trimmed.split("://").nth(1).unwrap_or(trimmed);
    let authority = after_scheme.split('/').next().unwrap_or("");
    // Strip credentials and the port; IPv6 literals are bracketed.
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    }
    .to_lowercase();

    if host == "localhost" || host == "::1" || host.ends_with(".local") || host.ends_with(".localhost")
    {
        return true;
    }
    let octets: Vec<u8> = host
        .split('.')
        .filter_map(|part| part.parse::<u8>().ok())
        .collect();
    if octets.len() != 4 || host.split('.').count() != 4 {
        return false;
    }
    match (octets[0], octets[1]) {
        (127, _) | (10, _) | (192, 168) => true,
        (172, second) if (16..=31).contains(&second) => true,
        (0, 0) => true,
        _ => false,
    }
}

#[derive(Serialize, Default)]
pub struct RecallContext {
    /// Ready to drop in as a system message, or empty when nothing cleared the
    /// relevance floor — which is the common case and the intended one.
    pub block: String,
    /// The same hits as structured data, so a UI can show what was recalled.
    /// Worth surfacing: a model citing a conversation the user cannot see reads
    /// as the model making things up.
    pub hits: Vec<Hit>,
}

fn format_block(hits: &[Hit], max_chars: usize) -> String {
    if hits.is_empty() {
        return String::new();
    }
    // Says "notes and conversations" rather than only conversations because a
    // hit can be either, and a note presented as something the user said in an
    // earlier chat invites the model to attribute it to a conversation that
    // never happened.
    let mut out = String::from(
        "Things recorded earlier that may be relevant — kept notes and past \
         conversations with this user. Use them only if they actually bear on the \
         question; do not mention them otherwise.\n",
    );
    let mut budget = max_chars;
    for hit in hits {
        let when = chrono::DateTime::from_timestamp_millis(hit.ts_end as i64)
            .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        // Stop when what is left of the budget is too small to say anything
        // useful with. Tested against the budget and not against `room` below:
        // room is capped by the hit's own length, so comparing that instead
        // silently dropped any hit shorter than 40 characters no matter how much
        // budget was free — which is most notes that are only a description.
        if budget < 40 {
            break;
        }
        let flat = hit.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let room = budget.min(flat.chars().count());
        let snippet: String = flat.chars().take(room).collect();
        let kind = if hit.source == "memory" { "note" } else { "chat" };
        out.push_str(&format!("[{when} · {kind}] {}: {snippet}\n", hit.chat_title));
        budget = budget.saturating_sub(room);
    }
    out
}

/// Retrieves whatever earlier conversation bears on `query`, or nothing.
///
/// `base_url` is the endpoint the result is about to be sent to, and it is not
/// optional: this function's whole job is to take text out of the user's private
/// history and hand it to a model, so where that model runs is part of the
/// decision, not an afterthought for the caller to remember.
pub(crate) fn context_for(
    app: &tauri::AppHandle,
    query: &str,
    base_url: &str,
    current_chat_id: &str,
) -> RecallContext {
    use crate::tunables;

    if !tunables::toggle(app, tunables::RECALL_ENABLED) {
        return RecallContext::default();
    }
    if !is_local_endpoint(base_url) && !tunables::toggle(app, tunables::RECALL_SHARE_WITH_CLOUD) {
        // Silent rather than an error: the user asked a question, and the answer
        // to it is still perfectly good without recall. The setting is where
        // this is explained, not the middle of a conversation.
        return RecallContext::default();
    }

    let Ok(conn) = open_db(&db_path(app)) else {
        return RecallContext::default();
    };

    let time = parse_time_range(query, chrono::Local::now());
    let opts = SearchOptions {
        limit: tunables::int(app, tunables::RECALL_MAX_TURNS).max(1) as usize,
        after_ms: time.map(|t| t.after_ms),
        before_ms: time.map(|t| t.before_ms),
        time_only: time.is_some_and(|t| t.time_only),
        // One per conversation: see SearchOptions::max_per_chat.
        max_per_chat: 1,
        min_coverage: tunables::float(app, tunables::RECALL_MIN_COVERAGE) as f32,
    };

    // Words first, and usually last. The word search is a local SQLite query
    // measured in single-digit milliseconds; embedding the question is a network
    // round trip measured at 400ms with the model resident and 2.8s with it cold,
    // and this whole call sits between pressing send and the request going out.
    //
    // So the embedding is only paid for when the word search came back with
    // NOTHING — which is exactly the case it was added for. Meaning search earns
    // its keep on questions that share no words with the answer: a paraphrase, or
    // an English question about a Turkish conversation. When the words already
    // found something, a second ranking to fuse with it is not worth a round trip
    // on every message.
    //
    // What this gives up: when the word search finds a weak-but-passing hit and
    // the meaning search would have found a better one, the weaker hit stands.
    // The coverage floor already keeps the weakest out, and the alternative is
    // paying for meaning search on every message to improve a minority of them.
    let mut hits = search_hybrid(&conn, query, opts, None, current_chat_id).unwrap_or_default();

    if hits.is_empty() {
        // The floor is checked before the request, not after: without a measured
        // one, search_hybrid ignores the meaning path entirely, so embedding the
        // question would buy a vector that is then thrown away.
        let (embed_url, embed_model) = embedding_settings(app);
        let usable = !embed_url.is_empty()
            && !embed_model.is_empty()
            && meta_get(&conn, "similarity_floor").is_some();
        if usable {
            // A failure here is not surfaced: the empty word result above is a
            // complete answer on its own, and an unreachable embedding server
            // should degrade the feature, not interrupt the conversation.
            let query_vec = embed(&embed_url, &embed_model, "", &[query.to_string()], EMBED_QUERY_TIMEOUT_SECS)
                .ok()
                .and_then(|mut v| v.pop());
            if let Some(vector) = query_vec {
                hits = search_hybrid(&conn, query, opts, Some(&vector), current_chat_id).unwrap_or_default();
            }
        }
    }
    let max_chars = tunables::int(app, tunables::RECALL_MAX_CHARS).max(0) as usize;
    RecallContext {
        block: format_block(&hits, max_chars),
        hits,
    }
}

#[tauri::command]
pub async fn recall_context(
    app: tauri::AppHandle,
    query: String,
    base_url: String,
    chat_id: String,
) -> RecallContext {
    crate::offload(move || {
        context_for(&app, &query, &base_url, &chat_id)
})
    .await
}

/// One conversation that matched a sidebar search.
#[derive(Serialize)]
pub struct ChatSearchHit {
    chat_id: String,
    chat_title: String,
    /// The matching text, trimmed to something that fits a sidebar row.
    snippet: String,
    ts_end: u64,
}

/// Finds conversations by what was said in them, for the chat sidebar's search.
///
/// Deliberately NOT recall's search: recall decides whether an old conversation
/// is relevant enough to put in front of a model unasked, so it applies a
/// coverage floor and refuses when unsure. A person typing in a search box has
/// asked, and wants every match ranked — being told "not relevant enough" would
/// be absurd. Same index, different question.
///
/// Notes are excluded: this searches the user's conversations, and a memory note
/// is not one — it has no chat to open.
#[tauri::command]
pub async fn search_chats(app: tauri::AppHandle, query: String, limit: usize) -> Result<Vec<ChatSearchHit>, String> {
    crate::offload(move || {
        let terms = content_terms(&query);
        let Some(expression) = match_expression(&terms) else {
            // A query of nothing but stopwords. Empty, not an error.
            return Ok(Vec::new());
        };
        let limit = limit.clamp(1, 50);
        let conn = open_db(&db_path(&app)).map_err(|e| e.to_string())?;

        // bm25() is an FTS5 auxiliary function and only works in the SELECT list or
        // ORDER BY of a query against the FTS table — NOT inside an aggregate. The
        // obvious "one row per chat" form, GROUP BY chat_id with MIN(bm25(...)),
        // fails to prepare, and this function used to swallow that and return an
        // empty list: the search box looked like it simply found nothing. So the
        // rows come back ranked and flat, and one-per-chat happens below.
        let sql = "SELECT t.chat_id, t.chat_title, t.text, t.ts_end, bm25(turns_fts) AS score
                   FROM turns_fts JOIN turns t ON t.id = turns_fts.rowid
                   WHERE turns_fts MATCH ?1 AND t.source != 'memory'
                   ORDER BY score
                   LIMIT ?2";
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        // Over-fetches, because several of the top rows can belong to one long
        // conversation and would otherwise crowd every other match out.
        let rows = stmt
            .query_map(params![expression, (limit * 4) as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;

        let mut seen = HashSet::new();
        let mut hits = Vec::new();
        for row in rows {
            let (chat_id, chat_title, text, ts_end) = row.map_err(|e| e.to_string())?;
            // Best-scoring row wins per conversation, which is the first one seen
            // because the query is already ordered.
            if !seen.insert(chat_id.clone()) {
                continue;
            }
            hits.push(ChatSearchHit {
                chat_id,
                chat_title,
                snippet: snippet_around(&text, &terms),
                ts_end: ts_end as u64,
            });
            if hits.len() >= limit {
                break;
            }
        }
        Ok(hits)
})
    .await
}

/// A short window of `text` around the first matching term.
///
/// Around the match rather than from the start: a unit begins with the question
/// it belongs to, so the first 120 characters are usually not the part that
/// matched, and a snippet that never shows the search term reads as a wrong hit.
fn snippet_around(text: &str, terms: &[String]) -> String {
    const WINDOW: usize = 140;
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized = normalize(&flat);
    // Char index, not byte: the window is sliced by chars below, and Turkish
    // text makes the two differ.
    let at = terms
        .iter()
        .filter_map(|term| normalized.find(term.as_str()))
        .min()
        .map(|byte_at| normalized[..byte_at].chars().count())
        .unwrap_or(0);

    let chars: Vec<char> = flat.chars().collect();
    let start = at.saturating_sub(30);
    let end = (start + WINDOW).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out
}

#[derive(Serialize)]
pub struct RecallStats {
    added: usize,
    removed: usize,
    total: usize,
}

/// Collects the memory notes to index, or nothing when the setting is off.
///
/// Grouped by note id rather than returned flat, because that is the unit the
/// hash diff works on — one note's windows are synced together, the same way
/// one conversation's are.
fn notes_to_index(app: &tauri::AppHandle) -> Vec<(String, Vec<Unit>)> {
    if !crate::tunables::toggle(app, crate::tunables::RECALL_INDEX_NOTES) {
        return Vec::new();
    }
    let sources: Vec<NoteSource> = crate::content::memory::all_memories(app)
        .into_iter()
        .map(|node| NoteSource {
            project: crate::content::memory::prettify_project_slug(&node.project),
            name: node.name,
            description: node.description,
            body: node.body,
            updated_at: node.updated_at,
        })
        .collect();

    let mut grouped: Vec<(String, Vec<Unit>)> = Vec::new();
    for unit in note_units(&sources) {
        match grouped.last_mut() {
            Some((id, units)) if id == &unit.chat_id => units.push(unit),
            _ => grouped.push((unit.chat_id.clone(), vec![unit])),
        }
    }
    grouped
}

/// Rebuilds the index from the chat files, plus Claude Code's memory notes when
/// that is turned on. Cheap when nothing changed — every unit is diffed by
/// content hash — so it is safe to run at every startup.
pub fn reindex(app: &tauri::AppHandle) -> rusqlite::Result<RecallStats> {
    let conn = open_db(&db_path(app))?;
    let chats = crate::ai::chat::all_chats(app);
    let notes = notes_to_index(app);

    // Everything that should still be in the index, so reconcile can drop the
    // rest. Notes belong in here for exactly the same reason chats do, and
    // leaving them out is how turning the setting off drops them: with no id in
    // the live set, reconcile removes them on the next pass.
    let mut live: HashSet<String> = chats.iter().map(|c| c.id.clone()).collect();
    live.extend(notes.iter().map(|(id, _)| id.clone()));

    let mut added = 0;
    let mut removed = reconcile(&conn, &live)?;
    for chat in &chats {
        let (a, r) = sync_chat(&conn, chat)?;
        added += a;
        removed += r;
    }
    for (id, units) in notes {
        let (a, r) = sync_units(&conn, &id, units)?;
        added += a;
        removed += r;
    }
    // Here as well as after embedding, because a reconcile can shrink the corpus
    // — deleting conversations changes what "unrelated" looks like just as much
    // as adding them does.
    recalibrate(&conn);

    let total: i64 = conn.query_row("SELECT COUNT(*) FROM turns", [], |row| row.get(0))?;
    Ok(RecallStats {
        added,
        removed,
        total: total as usize,
    })
}

#[tauri::command]
pub async fn recall_reindex(app: tauri::AppHandle) -> Result<RecallStats, String> {
    crate::offload(move || {
        let stats = reindex(&app).map_err(|e| e.to_string())?;
        let conn = open_db(&db_path(&app)).map_err(|e| e.to_string())?;
        embed_pending(&app, &conn)?;
        Ok(stats)
})
    .await
}

/// What the index actually contains — for the Memory settings section, which
/// otherwise has nothing to show but two empty text boxes and no way to tell
/// "working" from "silently doing nothing".
#[derive(Serialize)]
pub struct RecallStatus {
    turns: usize,
    /// Every indexed row, conversations and notes together — the denominator
    /// `embedded` is out of. Separate from `turns` because that one deliberately
    /// counts conversation exchanges only, and mixing the two produced a status
    /// line reading "18 of 4 can also be found by meaning".
    units: usize,
    embedded: usize,
    chats: usize,
    /// Units that came from Claude Code's memory notes rather than from a
    /// conversation — shown separately because the two are indexed from
    /// different places and only one of them is something this app wrote.
    notes: usize,
    /// The model the stored vectors were produced with, which is not necessarily
    /// the one currently configured — that difference is exactly what a user
    /// needs to see after changing it.
    vector_model: String,
    /// The measured similarity above which two turns count as related, or 0
    /// while there is still too little history to measure it — in which case
    /// meaning search stays off however well the server is configured.
    similarity_floor: f32,
    /// Empty when meaning search is off; otherwise why, if it can't be used.
    problem: String,
}

#[tauri::command]
pub async fn recall_status(app: tauri::AppHandle) -> RecallStatus {
    crate::offload(move || {
        let (url, model) = embedding_settings(&app);
        let configured_url = crate::tunables::text(&app, crate::tunables::RECALL_EMBEDDING_URL);
        let configured_model = crate::tunables::text(&app, crate::tunables::RECALL_EMBEDDING_MODEL);

        let problem = if configured_url.is_empty() && configured_model.is_empty() {
            String::new()
        } else if configured_url.is_empty() || configured_model.is_empty() {
            "Both the server and the model have to be filled in.".to_string()
        } else if url.is_empty() {
            format!("{configured_url} is not a local address, so it will not be used.")
        } else {
            match embed(&url, &model, "", &["ping".to_string()], EMBED_QUERY_TIMEOUT_SECS) {
                Ok(vectors) if vectors.first().is_some_and(|v| !v.is_empty()) => String::new(),
                Ok(_) => "The server answered without an embedding.".to_string(),
                Err(err) => err,
            }
        };

        let Ok(conn) = open_db(&db_path(&app)) else {
            return RecallStatus {
                turns: 0,
                units: 0,
                embedded: 0,
                chats: 0,
                notes: 0,
                vector_model: String::new(),
                similarity_floor: 0.0,
                problem,
            };
        };
        let count = |sql: &str| -> usize {
            conn.query_row(sql, [], |row| row.get::<_, i64>(0))
                .unwrap_or(0)
                .max(0) as usize
        };
        RecallStatus {
            turns: count("SELECT COUNT(*) FROM turns WHERE source != 'memory'"),
            units: count("SELECT COUNT(*) FROM turns"),
            embedded: count("SELECT COUNT(*) FROM turns WHERE vec IS NOT NULL"),
            chats: count("SELECT COUNT(DISTINCT chat_id) FROM turns WHERE source != 'memory'"),
            notes: count("SELECT COUNT(DISTINCT chat_id) FROM turns WHERE source = 'memory'"),
            vector_model: meta_get(&conn, "vec_model").unwrap_or_default(),
            similarity_floor: meta_get(&conn, "similarity_floor")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0),
            problem,
        }
})
    .await
}

/// Kicks the index into shape on its own thread at startup, and after that
/// whenever a conversation is saved.
///
/// On a thread because it reads and parses every chat file; on startup because
/// the index is a cache and the files may have changed while the app was not
/// running (edited, deleted, synced from another machine).
/// The embedding endpoint and model, or empty strings when meaning search is off.
///
/// The URL is refused unless it is local, and not as a courtesy: this is the one
/// place in the app that would take the user's entire conversation history and
/// post it, in bulk and unprompted, to whatever address is in a settings box.
/// The chat-level guard above governs a few recalled lines going to the model
/// answering the question; this governs everything ever said.
fn embedding_settings(app: &tauri::AppHandle) -> (String, String) {
    use crate::tunables;
    let url = tunables::text(app, tunables::RECALL_EMBEDDING_URL);
    let model = tunables::text(app, tunables::RECALL_EMBEDDING_MODEL);
    if url.is_empty() || model.is_empty() {
        return (String::new(), String::new());
    }
    if !is_local_endpoint(&url) {
        eprintln!("recall: refusing to send conversations to a non-local embedding server ({url})");
        return (String::new(), String::new());
    }
    (url, model)
}

/// Gives every un-embedded turn a vector, a batch at a time.
///
/// Returns the number embedded, so the caller can tell "nothing to do" from
/// "not configured" — both of which are quiet successes.
fn embed_pending(app: &tauri::AppHandle, conn: &Connection) -> Result<usize, String> {
    let (url, model) = embedding_settings(app);
    if url.is_empty() {
        return Ok(0);
    }

    // Changing the model invalidates every existing vector: they are points in a
    // different space, and comparing across them produces confident nonsense.
    // Cleared rather than migrated, for the same reason the whole index is a
    // cache — see SCHEMA_VERSION.
    if meta_get(conn, "vec_model").as_deref() != Some(model.as_str()) {
        conn.execute("UPDATE turns SET vec = NULL, vec_model = NULL", [])
            .map_err(|e| e.to_string())?;
        meta_set(conn, "vec_model", &model).map_err(|e| e.to_string())?;
        let _ = conn.execute("DELETE FROM meta WHERE key = 'similarity_floor'", []);
    }

    let mut embedded = 0;
    loop {
        let pending: Vec<(i64, String)> = conn
            .prepare("SELECT id, text FROM turns WHERE vec IS NULL LIMIT ?1")
            .and_then(|mut stmt| {
                stmt.query_map(params![EMBED_BATCH as i64], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect()
            })
            .map_err(|e| e.to_string())?;
        if pending.is_empty() {
            break;
        }

        let texts: Vec<String> = pending.iter().map(|(_, text)| text.clone()).collect();
        let vectors = embed(&url, &model, "", &texts, EMBED_BACKFILL_TIMEOUT_SECS)?;
        for ((id, _), vector) in pending.iter().zip(vectors) {
            conn.execute(
                "UPDATE turns SET vec = ?1, vec_model = ?2 WHERE id = ?3",
                params![to_blob(&vector), model, id],
            )
            .map_err(|e| e.to_string())?;
            embedded += 1;
        }
    }

    if embedded > 0 {
        recalibrate(conn);
    }
    Ok(embedded)
}

/// Re-measures the similarity floor, and REMOVES it when there is no longer
/// enough to measure it from.
///
/// The removal is the part that matters. A floor measured over a hundred
/// conversations is not a floor for the four that are left after the user
/// deletes most of them — and a stale threshold is exactly the unmeasured
/// constant this design refuses to use, just with a more convincing history.
fn recalibrate(conn: &Connection) {
    match calibrate_similarity_floor(conn) {
        Ok(Some(floor)) => {
            let _ = meta_set(conn, "similarity_floor", &floor.to_string());
        }
        Ok(None) => {
            let _ = conn.execute("DELETE FROM meta WHERE key = 'similarity_floor'", []);
        }
        Err(_) => {}
    }
}

pub fn start_indexer(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        if let Err(err) = reindex(&app) {
            eprintln!("recall: initial index failed: {err}");
            return;
        }
        // After the words, because word search has to work from the first moment
        // and this may take a while on a history that has never been embedded.
        match open_db(&db_path(&app)).map_err(|e| e.to_string()).and_then(|conn| embed_pending(&app, &conn)) {
            Ok(0) => {}
            Ok(n) => eprintln!("recall: embedded {n} turns"),
            Err(err) => eprintln!("recall: embedding failed, falling back to word search: {err}"),
        }
    });
}

/// Indexes one conversation in the background, after it has been saved.
///
/// Detached rather than awaited: this runs on the path that saves a chat and
/// records a spoken turn, and neither should get slower because a search index
/// exists. Nothing depends on it having finished — a turn indexed a moment late
/// is only unfindable for that moment, and a failure leaves the index simply
/// behind, which the next startup reconciles.
pub fn index_chat_soon(app: &tauri::AppHandle, chat: Chat) {
    let app = app.clone();
    std::thread::spawn(move || {
        let conn = match open_db(&db_path(&app)) {
            Ok(conn) => conn,
            Err(err) => return eprintln!("recall: opening the index failed: {err}"),
        };
        if let Err(err) = sync_chat(&conn, &chat) {
            return eprintln!("recall: indexing chat {} failed: {err}", chat.id);
        }
        // Only the turns this save added need a vector, so in normal use this is
        // one short request behind a message that has already been answered.
        if let Err(err) = embed_pending(&app, &conn) {
            eprintln!("recall: embedding new turns failed: {err}");
        }
    });
}

/// Drops one conversation from the index, after its file has been deleted.
pub fn forget_chat_soon(app: &tauri::AppHandle, chat_id: String) {
    let app = app.clone();
    std::thread::spawn(move || {
        let result = open_db(&db_path(&app)).and_then(|conn| {
            let ids: Vec<i64> = conn
                .prepare("SELECT id FROM turns WHERE chat_id = ?1")?
                .query_map(params![chat_id], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for id in ids {
                conn.execute("DELETE FROM turns WHERE id = ?1", params![id])?;
                conn.execute("DELETE FROM turns_fts WHERE rowid = ?1", params![id])?;
            }
            Ok(())
        });
        if let Err(err) = result {
            eprintln!("recall: forgetting chat {chat_id} failed: {err}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, content: &str, ts: u64) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
            ts,
            source: String::new(),
            attachments: Vec::new(),
            image_path: String::new(),
            image_meta: String::new(),
        }
    }

    fn chat(id: &str, title: &str, messages: Vec<ChatMessage>) -> Chat {
        Chat {
            id: id.to_string(),
            title: title.to_string(),
            profile_id: String::new(),
            created_at: 0,
            updated_at: 0,
            messages,
        }
    }

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn
    }

    /// Word search with no question vector — the behaviour when no embedding
    /// server is configured, which is most of what these tests are about. A test
    /// helper rather than a second public entry point: production has exactly
    /// one search function, and a wrapper nothing calls is dead code.
    fn search(conn: &Connection, query: &str, opts: SearchOptions) -> rusqlite::Result<Vec<Hit>> {
        search_hybrid(conn, query, opts, None, "")
    }

    #[test]
    fn the_open_conversation_never_recalls_itself() {
        // Found by driving the real UI: the chat is saved (and so indexed)
        // before the request goes out, so the question was retrieving itself and
        // the recall note in the chat window showed the user their own sentence.
        let conn = db();
        let now = 1_700_000_000_000;
        sync_chat(
            &conn,
            &chat(
                "open-chat",
                "remind me what we decided about caching",
                vec![message("user", "remind me what we decided about caching", now)],
            ),
        )
        .unwrap();
        sync_chat(
            &conn,
            &chat(
                "older",
                "Digest caching",
                vec![
                    message("user", "how should we cache the digest?", now - 500_000),
                    message("assistant", "key it by repo and updated_at", now - 499_000),
                ],
            ),
        )
        .unwrap();

        let hits = search_hybrid(
            &conn,
            "remind me what we decided about caching",
            SearchOptions::default(),
            None,
            "open-chat",
        )
        .unwrap();
        assert!(
            hits.iter().all(|h| h.chat_id != "open-chat"),
            "the question recalled itself: {hits:?}"
        );
        // And it still reaches the conversation it was actually about. This half
        // is why "remind"/"me" are stopwords: counted as subject matter they are
        // half the question, and the real answer falls below the floor.
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chat_id, "older");
    }

    #[test]
    fn dotless_i_folds_so_turkish_is_findable_either_way() {
        // The measured failure this exists for: unicode61's remove_diacritics
        // folds ç ğ ö ş ü, but ı is a letter, not a diacritic, so a query typed
        // on a keyboard without it never matched.
        assert_eq!(normalize("ığdır"), "iğdir");
        assert_eq!(normalize("Iğdır"), normalize("ığdır"));
        assert_eq!(normalize("İSTANBUL"), "istanbul");
        assert_eq!(normalize("Istanbul"), "istanbul");
        // ü is left to the tokenizer's remove_diacritics rather than folded
        // twice — doing it here as well would be a second, divergent rule.
        assert_eq!(normalize("Ücret"), "ücret");
    }

    #[test]
    fn turkish_survives_a_keyboard_without_dotless_i() {
        let conn = db();
        sync_chat(
            &conn,
            &chat(
                "c1",
                "Kararlar",
                vec![
                    message("user", "ığdır şubesi için ne karar verdik?", 100),
                    message("assistant", "ücret tarifesini güncelledik", 200),
                ],
            ),
        )
        .unwrap();
        // Typed without any Turkish-specific letters at all.
        let hits = search(&conn, "igdir ucret", SearchOptions { limit: 3, ..Default::default() }).unwrap();
        assert_eq!(hits.len(), 1, "Turkish text was not findable from a plain-ASCII query");
    }

    #[test]
    fn english_text_is_untouched_apart_from_case() {
        assert_eq!(normalize("Caching The Digest"), "caching the digest");
    }

    #[test]
    fn a_question_and_its_answer_are_one_unit() {
        let c = chat(
            "c1",
            "Caching",
            vec![
                message("user", "how should we cache the digest?", 100),
                message("assistant", "key it by repo and updated_at", 200),
                message("user", "and the ttl?", 300),
                message("assistant", "one hour", 400),
            ],
        );
        let units = build_units(&c);
        assert_eq!(units.len(), 2);
        assert!(units[0].text.contains("how should we cache"));
        assert!(units[0].text.contains("key it by repo"));
        assert_eq!(units[0].ts_start, 100);
        assert_eq!(units[0].ts_end, 200);
        assert!(units[1].text.contains("ttl"));
    }

    #[test]
    fn an_assistant_greeting_before_any_question_is_not_indexed() {
        let c = chat(
            "c1",
            "Greeting",
            vec![
                message("assistant", "hello, how can I help?", 10),
                message("user", "nothing yet", 20),
            ],
        );
        let units = build_units(&c);
        assert_eq!(units.len(), 1);
        assert!(units[0].text.contains("nothing yet"));
    }

    #[test]
    fn an_unanswered_question_is_still_indexed() {
        let c = chat("c1", "Open", vec![message("user", "did we ever fix the leak?", 10)]);
        let units = build_units(&c);
        assert_eq!(units.len(), 1);
        assert!(units[0].text.contains("fix the leak"));
    }

    #[test]
    fn a_long_answer_is_windowed_and_every_window_keeps_the_question() {
        let long = "detail ".repeat(600); // ~4200 characters
        let c = chat(
            "c1",
            "Long",
            vec![
                message("user", "explain the whole thing", 10),
                message("assistant", &long, 20),
            ],
        );
        let units = build_units(&c);
        assert!(units.len() > 2, "expected several windows, got {}", units.len());
        for unit in &units {
            assert!(unit.text.contains("explain the whole thing"));
            assert!(unit.text.chars().count() <= MAX_UNIT_CHARS + 8);
        }
    }

    #[test]
    fn a_spoken_half_makes_the_whole_unit_voice() {
        let mut spoken = message("user", "what is the build status?", 10);
        spoken.source = "voice".to_string();
        let c = chat("v1", "Voice", vec![spoken, message("assistant", "green", 20)]);
        assert_eq!(build_units(&c)[0].source, "voice");
    }

    #[test]
    fn reindexing_an_unchanged_chat_writes_nothing() {
        let conn = db();
        let c = chat(
            "c1",
            "Caching",
            vec![
                message("user", "how should we cache the digest?", 100),
                message("assistant", "key it by repo and updated_at", 200),
            ],
        );
        assert_eq!(sync_chat(&conn, &c).unwrap(), (1, 0));
        assert_eq!(sync_chat(&conn, &c).unwrap(), (0, 0));
    }

    #[test]
    fn appending_a_turn_indexes_only_the_new_one() {
        let conn = db();
        let mut c = chat(
            "c1",
            "Caching",
            vec![
                message("user", "how should we cache the digest?", 100),
                message("assistant", "key it by repo and updated_at", 200),
            ],
        );
        sync_chat(&conn, &c).unwrap();
        c.messages.push(message("user", "and the ttl?", 300));
        c.messages.push(message("assistant", "one hour", 400));
        assert_eq!(sync_chat(&conn, &c).unwrap(), (1, 0));
    }

    #[test]
    fn editing_a_turn_replaces_it_rather_than_duplicating() {
        let conn = db();
        let mut c = chat(
            "c1",
            "Caching",
            vec![
                message("user", "how should we cache the digest?", 100),
                message("assistant", "key it by repo", 200),
            ],
        );
        sync_chat(&conn, &c).unwrap();
        c.messages[1].content = "key it by repo and updated_at".to_string();
        assert_eq!(sync_chat(&conn, &c).unwrap(), (1, 1));
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM turns", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn a_deleted_chat_stops_being_findable() {
        let conn = db();
        let c = chat(
            "c1",
            "Caching",
            vec![
                message("user", "how should we cache the digest?", 100),
                message("assistant", "key it by repo", 200),
            ],
        );
        sync_chat(&conn, &c).unwrap();
        assert_eq!(reconcile(&conn, &HashSet::new()).unwrap(), 1);
        let hits = search(&conn, "cache the digest", SearchOptions { limit: 3, ..Default::default() }).unwrap();
        assert!(hits.is_empty());
        // The FTS shadow table has to be emptied too — leaving it behind makes
        // the JOIN drop the row silently, which looks like it worked.
        let fts: i64 = conn
            .query_row("SELECT COUNT(*) FROM turns_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fts, 0);
    }

    #[test]
    fn finds_an_earlier_conversation_by_its_subject() {
        let conn = db();
        sync_chat(
            &conn,
            &chat(
                "c1",
                "GitHub digest",
                vec![
                    message("user", "how should we cache the digest?", 100),
                    message("assistant", "key it by repo and updated_at", 200),
                ],
            ),
        )
        .unwrap();
        sync_chat(
            &conn,
            &chat(
                "c2",
                "Lunch",
                vec![
                    message("user", "where should we eat?", 100),
                    message("assistant", "the place across the road", 200),
                ],
            ),
        )
        .unwrap();

        let hits = search(&conn, "what did we decide about caching?", SearchOptions { limit: 3, ..Default::default() }).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chat_id, "c1");
    }

    #[test]
    fn english_stemming_matches_a_different_word_form() {
        let conn = db();
        sync_chat(
            &conn,
            &chat(
                "c1",
                "Caching",
                vec![
                    message("user", "we should cache the digest", 100),
                    message("assistant", "agreed", 200),
                ],
            ),
        )
        .unwrap();
        // "caching" only finds "cache" because of the porter tokenizer — this is
        // the concrete thing English gets that Turkish does not.
        let hits = search(&conn, "caching digest", SearchOptions { limit: 3, ..Default::default() }).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn an_unrelated_question_recalls_nothing_rather_than_the_best_of_a_bad_set() {
        let conn = db();
        sync_chat(
            &conn,
            &chat(
                "c1",
                "GitHub digest",
                vec![
                    message("user", "how should we cache the digest?", 100),
                    message("assistant", "key it by repo and updated_at", 200),
                ],
            ),
        )
        .unwrap();
        // Shares exactly one incidental word ("should"), which is a stopword, and
        // one content word at most — below the coverage floor either way.
        let hits = search(
            &conn,
            "what temperature should the oven be for bread",
            SearchOptions { limit: 3, ..Default::default() },
        )
        .unwrap();
        assert!(hits.is_empty(), "injected an irrelevant turn: {hits:?}");
    }

    #[test]
    fn a_greeting_recalls_nothing() {
        // Measured against the real index: "hello" pulled in an unrelated
        // conversation because its reply opened with "Hello!". A greeting is
        // never what a question is about.
        let conn = db();
        sync_chat(
            &conn,
            &chat(
                "c1",
                "hi",
                vec![
                    message("user", "hi", 100),
                    message("assistant", "Hello! What would you like to do?", 200),
                ],
            ),
        )
        .unwrap();
        for greeting in ["hello", "hi", "hey there", "merhaba", "selam", "thanks!"] {
            let hits = search(&conn, greeting, SearchOptions { limit: 3, ..Default::default() }).unwrap();
            assert!(hits.is_empty(), "{greeting} recalled {hits:?}");
        }
    }

    #[test]
    fn a_one_word_question_about_a_real_subject_still_works() {
        // The greeting fix must not become "short questions do not recall" —
        // "what about the caching?" is one content word after stopwords.
        let conn = db();
        sync_chat(
            &conn,
            &chat(
                "c1",
                "Digest",
                vec![
                    message("user", "how should we cache the digest?", 100),
                    message("assistant", "key it by repo", 200),
                ],
            ),
        )
        .unwrap();
        let hits = search(&conn, "what about the caching?", SearchOptions { limit: 3, ..Default::default() }).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn a_question_of_nothing_but_stopwords_recalls_nothing() {
        let conn = db();
        sync_chat(
            &conn,
            &chat("c1", "X", vec![message("user", "cache the digest", 100)]),
        )
        .unwrap();
        assert!(search(&conn, "and what about it?", SearchOptions { limit: 3, ..Default::default() })
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_time_range_narrows_an_otherwise_matching_search() {
        let conn = db();
        sync_chat(
            &conn,
            &chat(
                "old",
                "Caching",
                vec![message("user", "cache the digest by repo", 1_000), message("assistant", "ok", 1_100)],
            ),
        )
        .unwrap();
        sync_chat(
            &conn,
            &chat(
                "new",
                "Caching",
                vec![message("user", "cache the digest by branch", 9_000), message("assistant", "ok", 9_100)],
            ),
        )
        .unwrap();

        let all = search(&conn, "cache digest", SearchOptions { limit: 5, ..Default::default() }).unwrap();
        assert_eq!(all.len(), 2);

        let recent = search(
            &conn,
            "cache digest",
            SearchOptions { limit: 5, after_ms: Some(5_000), ..Default::default() },
        )
        .unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].chat_id, "new");
    }

    #[test]
    fn a_purely_temporal_question_returns_the_range_itself() {
        let conn = db();
        sync_chat(
            &conn,
            &chat("c1", "Anything", vec![message("user", "some subject nobody asked about", 5_000)]),
        )
        .unwrap();
        // No content terms in common — ranking would return nothing, but "what
        // did we talk about then" is asking for the range, not for a match.
        let hits = search(
            &conn,
            "what did we talk about",
            SearchOptions { limit: 5, after_ms: Some(1_000), before_ms: Some(9_000), time_only: true, ..Default::default() },
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
    }

    fn at(y: i32, m: u32, d: u32, h: u32) -> chrono::DateTime<chrono::Local> {
        use chrono::TimeZone;
        chrono::Local
            .with_ymd_and_hms(y, m, d, h, 0, 0)
            .single()
            .unwrap()
    }

    fn day_of(range: &TimeQuery) -> String {
        chrono::DateTime::from_timestamp_millis(range.after_ms as i64)
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d")
            .to_string()
    }

    #[test]
    fn reads_a_time_expression_in_either_language() {
        let now = at(2026, 7, 29, 14);
        for (query, expected_day) in [
            ("5 gün önce ne konuştuk", "2026-07-24"),
            ("5 days ago what did we talk about", "2026-07-24"),
            ("dün ne dedik", "2026-07-28"),
            ("yesterday", "2026-07-28"),
            ("2 hafta önce", "2026-07-15"),
            ("2 weeks ago", "2026-07-15"),
        ] {
            let range = parse_time_range(query, now).unwrap_or_else(|| panic!("no range for {query}"));
            assert_eq!(day_of(&range), expected_day, "{query}");
        }
    }

    #[test]
    fn a_question_with_no_time_in_it_has_no_range() {
        let now = at(2026, 7, 29, 14);
        assert!(parse_time_range("what did we decide about caching", now).is_none());
        // A bare number is not a date — "port 8080" must not become a range.
        assert!(parse_time_range("which port was it, 8080?", now).is_none());
    }

    #[test]
    fn asking_only_about_a_time_is_told_apart_from_asking_about_a_subject() {
        let now = at(2026, 7, 29, 14);
        // Nothing left but conversation words: the range IS the question.
        assert!(parse_time_range("5 gün önce ne konuştuk", now).unwrap().time_only);
        assert!(parse_time_range("what did we talk about 5 days ago", now).unwrap().time_only);
        // A subject survives, so the range is a filter on a real search.
        assert!(!parse_time_range("5 gün önce caching hakkında ne dedik", now).unwrap().time_only);
        assert!(!parse_time_range("what did we say about caching 5 days ago", now).unwrap().time_only);
    }

    #[test]
    fn yesterday_means_the_calendar_day_not_the_last_24_hours() {
        // Asked at 09:00, "yesterday" must still cover all of the previous day,
        // including its evening — a rolling 24 hours would start at 09:00 and
        // miss it.
        let range = parse_time_range("dün", at(2026, 7, 29, 9)).unwrap();
        let start = chrono::DateTime::from_timestamp_millis(range.after_ms as i64)
            .unwrap()
            .with_timezone(&chrono::Local);
        let end = chrono::DateTime::from_timestamp_millis(range.before_ms as i64)
            .unwrap()
            .with_timezone(&chrono::Local);
        assert_eq!(start.format("%Y-%m-%d %H:%M").to_string(), "2026-07-28 00:00");
        assert_eq!(end.format("%Y-%m-%d %H").to_string(), "2026-07-28 23");
    }

    #[test]
    fn nothing_relevant_produces_an_empty_block_rather_than_a_stub() {
        assert_eq!(format_block(&[], 600), "");
    }

    #[test]
    fn one_conversation_cannot_take_every_slot() {
        let conn = db();
        // A long answer becomes several windows, all of which match the same
        // question — without a per-conversation cap they crowd out everything
        // else, and windows of one answer mostly repeat each other.
        let long = "the digest cache is keyed by repo and updated_at ".repeat(60);
        sync_chat(
            &conn,
            &chat(
                "wordy",
                "Digest caching",
                vec![message("user", "how does the digest cache work?", 100), message("assistant", &long, 200)],
            ),
        )
        .unwrap();
        sync_chat(
            &conn,
            &chat(
                "brief",
                "Cache notes",
                vec![message("user", "remind me how the digest cache is keyed", 300), message("assistant", "by repo", 400)],
            ),
        )
        .unwrap();

        let uncapped = search(&conn, "digest cache keyed", SearchOptions { limit: 3, ..Default::default() }).unwrap();
        assert!(
            uncapped.iter().filter(|h| h.chat_id == "wordy").count() > 1,
            "expected the long conversation to occupy several slots without a cap"
        );

        let capped = search(
            &conn,
            "digest cache keyed",
            SearchOptions { limit: 3, max_per_chat: 1, ..Default::default() },
        )
        .unwrap();
        let wordy = capped.iter().filter(|h| h.chat_id == "wordy").count();
        assert_eq!(wordy, 1);
        assert!(capped.iter().any(|h| h.chat_id == "brief"), "the other conversation was crowded out");
    }

    #[test]
    fn a_stricter_floor_recalls_less() {
        let conn = db();
        sync_chat(
            &conn,
            &chat(
                "c1",
                "Digest",
                vec![message("user", "cache the digest by repo", 100), message("assistant", "agreed", 200)],
            ),
        )
        .unwrap();
        // Two of three content words match ("cache", "digest"; "branch" does not).
        let lenient = search(&conn, "cache digest branch", SearchOptions { min_coverage: 0.5, ..Default::default() }).unwrap();
        assert_eq!(lenient.len(), 1);
        let strict = search(&conn, "cache digest branch", SearchOptions { min_coverage: 1.0, ..Default::default() }).unwrap();
        assert!(strict.is_empty());
    }

    /// Arms meaning search with an explicit floor. In production this figure is
    /// measured from the user's own history; a test corpus of two documents has
    /// nothing to measure, so it is stated instead of faked.
    fn arm_meaning(conn: &Connection, floor: f32) {
        meta_set(conn, "similarity_floor", &floor.to_string()).unwrap();
    }

    fn set_vec(conn: &Connection, id: i64, vector: &[f32]) {
        conn.execute(
            "UPDATE turns SET vec = ?1, vec_model = 'test' WHERE id = ?2",
            params![to_blob(vector), id],
        )
        .unwrap();
    }

    #[test]
    fn a_vector_survives_the_round_trip_through_the_blob() {
        let original = vec![0.5_f32, -1.25, 0.0, 3.75];
        assert_eq!(from_blob(&to_blob(&original)), original);
    }

    #[test]
    fn cosine_refuses_to_compare_vectors_of_different_lengths() {
        // The case that matters: the embedding model changed and old rows are
        // points in a different space. A number here would be confident nonsense.
        assert_eq!(cosine(&[1.0, 0.0], &[1.0, 0.0, 0.0]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn meaning_finds_what_words_cannot() {
        let conn = db();
        sync_chat(
            &conn,
            &chat(
                "c1",
                "Results",
                vec![
                    message("user", "should we memoize the expensive lookup?", 100),
                    message("assistant", "yes, keep the computed value in a static", 200),
                ],
            ),
        )
        .unwrap();
        let id: i64 = conn.query_row("SELECT id FROM turns", [], |r| r.get(0)).unwrap();
        set_vec(&conn, id, &[1.0, 0.0, 0.0]);
        arm_meaning(&conn, 0.5);

        // Shares no word with the stored turn: BM25 cannot reach it at any index
        // size, which is the entire reason the vector column exists.
        let words_only = search(&conn, "what was our caching approach?", SearchOptions::default()).unwrap();
        assert!(words_only.is_empty());

        let with_meaning = search_hybrid(
            &conn,
            "what was our caching approach?",
            SearchOptions::default(),
            Some(&[0.99, 0.14, 0.0]),
            "",
        )
        .unwrap();
        assert_eq!(with_meaning.len(), 1);
        assert_eq!(with_meaning[0].chat_id, "c1");
    }

    #[test]
    fn an_unrelated_vector_still_recalls_nothing() {
        let conn = db();
        sync_chat(&conn, &chat("c1", "Results", vec![message("user", "memoize the lookup", 100)])).unwrap();
        let id: i64 = conn.query_row("SELECT id FROM turns", [], |r| r.get(0)).unwrap();
        set_vec(&conn, id, &[1.0, 0.0, 0.0]);
        arm_meaning(&conn, 0.5);
        // Orthogonal: similarity 0, far below any floor.
        let hits = search_hybrid(
            &conn,
            "what temperature for bread",
            SearchOptions::default(),
            Some(&[0.0, 1.0, 0.0]),
            "",
        )
        .unwrap();
        assert!(hits.is_empty(), "recalled {hits:?}");
    }

    // Measured against bge-m3 on real turns: unrelated pairs scored 0.24-0.36
    // and genuinely related ones 0.41-0.58, with an overlap in between. A
    // threshold chosen without that data is not conservative, it is arbitrary —
    // 0.6 sounds careful and rejects every true match. So until there is enough
    // history to measure it, meaning search stays off rather than guessing.
    #[test]
    fn meaning_search_is_skipped_until_the_floor_has_been_measured() {
        let conn = db();
        sync_chat(
            &conn,
            &chat(
                "c1",
                "Results",
                vec![message("user", "should we memoize the expensive lookup?", 100)],
            ),
        )
        .unwrap();
        let id: i64 = conn.query_row("SELECT id FROM turns", [], |r| r.get(0)).unwrap();
        set_vec(&conn, id, &[1.0, 0.0, 0.0]);
        // No similarity_floor in meta — an identical vector must still not match.
        let hits = search_hybrid(
            &conn,
            "what was our caching approach?",
            SearchOptions::default(),
            Some(&[1.0, 0.0, 0.0]),
            "",
        )
        .unwrap();
        assert!(hits.is_empty(), "matched on an unmeasured threshold: {hits:?}");

        arm_meaning(&conn, 0.5);
        let hits = search_hybrid(
            &conn,
            "what was our caching approach?",
            SearchOptions::default(),
            Some(&[1.0, 0.0, 0.0]),
            "",
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn fusion_prefers_what_both_methods_found() {
        // 7 is second-best on words and second-best on meaning; 1 and 2 each top
        // exactly one list. Agreement beats being first in one ranking.
        assert_eq!(fuse(&[&[1, 7, 3], &[2, 7, 4]])[0], 7);
        // Either list alone still orders sensibly.
        assert_eq!(fuse(&[&[5, 6], &[]]), vec![5, 6]);
        assert_eq!(fuse(&[&[], &[5, 6]]), vec![5, 6]);
    }

    #[test]
    fn a_greeting_is_not_embedded_and_matched_either() {
        let conn = db();
        sync_chat(&conn, &chat("c1", "X", vec![message("user", "memoize the lookup", 100)])).unwrap();
        let id: i64 = conn.query_row("SELECT id FROM turns", [], |r| r.get(0)).unwrap();
        set_vec(&conn, id, &[1.0, 0.0, 0.0]);
        arm_meaning(&conn, 0.5);
        // A vector alone must not be able to reach past the "no content words"
        // gate — otherwise "hello" recalls whatever is nearest in the space.
        let hits = search_hybrid(&conn, "hello", SearchOptions::default(), Some(&[1.0, 0.0, 0.0]), "").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn a_changed_schema_version_rebuilds_rather_than_migrates() {
        let dir = std::env::temp_dir().join(format!("recall-schema-{}", content_hash("v")));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("recall.sqlite3");
        let _ = std::fs::remove_file(&path);

        let conn = open_db(&path).unwrap();
        sync_chat(&conn, &chat("c1", "X", vec![message("user", "cache the digest", 100)])).unwrap();
        meta_set(&conn, "schema_version", "0").unwrap();
        drop(conn);

        let conn = open_db(&path).unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM turns", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0, "an out-of-date index must be rebuilt, not carried forward");
        assert_eq!(meta_get(&conn, "schema_version").as_deref(), Some(SCHEMA_VERSION));
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn local_endpoints_are_told_apart_from_hosted_ones() {
        for local in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:8080",
            "http://[::1]:11434/v1",
            "http://192.168.1.50:11434",
            "http://10.0.0.7:1234/v1",
            "http://172.20.0.3:11434",
            "http://desktop.local:11434",
        ] {
            assert!(is_local_endpoint(local), "{local} should be local");
        }
        for remote in [
            "https://api.openai.com/v1",
            "https://generativelanguage.googleapis.com/v1beta",
            "https://api.anthropic.com",
            "http://172.32.0.1:11434", // just outside the private range
            "http://203.0.113.5:11434",
            "",
        ] {
            assert!(!is_local_endpoint(remote), "{remote} should be treated as remote");
        }
    }

    #[test]
    fn the_injected_block_stays_inside_its_budget() {
        let hits: Vec<Hit> = (0..5)
            .map(|i| Hit {
                chat_id: format!("c{i}"),
                chat_title: "Long conversation".to_string(),
                ts_end: 1_700_000_000_000,
                source: String::new(),
                text: "word ".repeat(500),
                score: 1.0,
                coverage: 1.0,
            })
            .collect();
        let budget = 600;
        let block = format_block(&hits, budget);
        // The preamble is fixed and is one line; what must be bounded is
        // everything after it. skip(2) here would also skip the first recalled
        // line, and since a 600-character budget only ever fits one of these,
        // that left the assertion summing nothing at all.
        let recalled: usize = block.lines().skip(1).map(|l| l.chars().count()).sum();
        assert!(recalled > budget / 2, "nothing was recalled to measure");
        assert!(recalled <= budget + 80, "block was {recalled} chars");
    }

    #[test]
    fn fts_syntax_in_a_question_is_treated_as_text() {
        let conn = db();
        sync_chat(&conn, &chat("c1", "X", vec![message("user", "the cache key", 10)])).unwrap();
        // Bare FTS5 operators would otherwise make this a syntax error rather
        // than a search.
        for query in ["cache OR NOT key", "cache*", "\"cache\" AND (key", "cache -key"] {
            assert!(search(&conn, query, SearchOptions { limit: 3, ..Default::default() }).is_ok(), "{query}");
        }
    }

    // --- attachments ----------------------------------------------------------

    fn with_attachment(mut m: ChatMessage, name: &str) -> ChatMessage {
        m.attachments.push(crate::ai::chat::MessageAttachment {
            name: name.to_string(),
            lang: String::new(),
            text: "the file's own contents, which are NOT indexed".to_string(),
            kind: "text".to_string(),
            full_chars: 0,
        });
        m
    }

    #[test]
    fn a_conversation_is_findable_by_the_name_of_a_file_attached_to_it() {
        let conn = db();
        sync_chat(
            &conn,
            &chat(
                "c1",
                "hi",
                vec![
                    with_attachment(message("user", "bu dosyaya gore kac kalori almaliyim", 100), "spor-beslenme-plani-v4.md"),
                    message("assistant", "2200 civari", 101),
                ],
            ),
        )
        .unwrap();

        let hits = search(&conn, "beslenme plani", SearchOptions::default()).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
    }

    #[test]
    fn a_files_contents_are_not_indexed() {
        // Deliberate: the contents already go to the model in full, and indexing
        // them would let one attachment outweigh a whole conversation history.
        let conn = db();
        sync_chat(
            &conn,
            &chat(
                "c1",
                "hi",
                vec![with_attachment(message("user", "buna bak", 100), "plan.md")],
            ),
        )
        .unwrap();

        assert!(search(&conn, "contents indexed", SearchOptions::default()).unwrap().is_empty());
    }

    #[test]
    fn a_message_that_is_only_a_file_is_still_indexed() {
        // Dragging a file in and pressing send with no words typed. Without the
        // attachment names there is nothing to index and the turn vanishes.
        let conn = db();
        sync_chat(
            &conn,
            &chat("c1", "hi", vec![with_attachment(message("user", "", 100), "quarterly-budget.xlsx")]),
        )
        .unwrap();

        let hits = search(&conn, "quarterly budget", SearchOptions::default()).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
    }

    // --- sidebar search -------------------------------------------------------
    //
    // The command itself needs an AppHandle, so these test the query it runs.
    // Worth testing at all because the first version of that SQL did not even
    // prepare — bm25() inside MIN() is invalid — and the caller turned the
    // failure into an empty result set, which is indistinguishable from "no
    // matches" from the outside.

    /// Runs search_chats's query against a test database.
    fn search_chats_sql(conn: &Connection, query: &str, limit: usize) -> rusqlite::Result<Vec<(String, String)>> {
        let terms = content_terms(query);
        let Some(expression) = match_expression(&terms) else {
            return Ok(Vec::new());
        };
        let sql = "SELECT t.chat_id, t.text, bm25(turns_fts) AS score
                   FROM turns_fts JOIN turns t ON t.id = turns_fts.rowid
                   WHERE turns_fts MATCH ?1 AND t.source != 'memory'
                   ORDER BY score
                   LIMIT ?2";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params![expression, (limit * 4) as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for row in rows {
            let (chat_id, text) = row?;
            if !seen.insert(chat_id.clone()) {
                continue;
            }
            out.push((chat_id, snippet_around(&text, &terms)));
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    #[test]
    fn searching_finds_a_conversation_by_what_was_said_in_it() {
        let conn = db();
        sync_chat(
            &conn,
            &chat(
                "c1",
                "hi",
                vec![
                    message("user", "how should we cache the digest?", 100),
                    message("assistant", "key it by repo and updated_at", 101),
                ],
            ),
        )
        .unwrap();
        sync_chat(&conn, &chat("c2", "Other", vec![message("user", "what about the weather", 200)])).unwrap();

        // The title is "hi" and says nothing — finding this conversation at all
        // is the entire point of searching content rather than titles.
        let hits = search_chats_sql(&conn, "digest caching", 10).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].0, "c1");
    }

    #[test]
    fn a_search_result_shows_the_line_that_matched() {
        let conn = db();
        // A long answer whose match is nowhere near the beginning: a snippet cut
        // from the start would never contain the search term.
        let filler = "unrelated preamble ".repeat(20);
        sync_chat(
            &conn,
            &chat(
                "c1",
                "Notes",
                vec![
                    message("user", "give me the summary", 100),
                    message("assistant", &format!("{filler} the migration deadline is March"), 101),
                ],
            ),
        )
        .unwrap();

        let hits = search_chats_sql(&conn, "migration deadline", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].1.contains("migration deadline"), "snippet missed the match: {:?}", hits[0].1);
    }

    #[test]
    fn one_conversation_cannot_fill_the_search_results() {
        let conn = db();
        // Ten exchanges in one chat, all matching, plus one in another. Without
        // the dedupe the first chat takes every slot and the second never shows.
        //
        // Each exchange has to be textually distinct: the index is keyed on
        // (chat_id, content hash), so ten identical turns collapse into one row
        // and the test would not be testing crowding at all.
        let mut messages = Vec::new();
        for i in 0..10 {
            messages.push(message("user", &format!("tell me about caching, part {i}"), 100 + i));
            messages.push(message("assistant", &format!("caching is keyed by repo, note {i}"), 101 + i));
        }
        sync_chat(&conn, &chat("busy", "Busy", messages)).unwrap();
        sync_chat(&conn, &chat("quiet", "Quiet", vec![message("user", "caching once", 500)])).unwrap();

        let hits = search_chats_sql(&conn, "caching", 10).unwrap();
        assert_eq!(hits.len(), 2, "{hits:?}");
        let ids: HashSet<&str> = hits.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains("quiet"), "the busy chat crowded the other one out");
    }

    #[test]
    fn searching_does_not_return_memory_notes() {
        // A note has no conversation to open, so a row for one would be a dead
        // result in the sidebar.
        let conn = db();
        let notes = note_units(&[note("digest-caching", "How the digest is cached", "by repo")]);
        sync_units(&conn, &notes[0].chat_id.clone(), notes).unwrap();

        assert!(search_chats_sql(&conn, "digest caching", 10).unwrap().is_empty());
    }

    #[test]
    fn a_search_for_nothing_but_stopwords_is_empty_not_an_error() {
        let conn = db();
        sync_chat(&conn, &chat("c1", "X", vec![message("user", "the cache key", 10)])).unwrap();
        assert!(search_chats_sql(&conn, "the a of", 10).unwrap().is_empty());
    }

    // --- memory notes ---------------------------------------------------------

    fn note(name: &str, description: &str, body: &str) -> NoteSource {
        NoteSource {
            name: name.to_string(),
            description: description.to_string(),
            body: body.to_string(),
            project: "Widget".to_string(),
            updated_at: 1_700_000_000_000,
        }
    }

    #[test]
    fn a_note_is_findable_by_what_it_says() {
        let conn = db();
        let notes = note_units(&[note(
            "digest-caching",
            "How the daily digest is cached",
            "Key the digest by repo and updated_at, never by wall-clock time.",
        )]);
        sync_units(&conn, &notes[0].chat_id.clone(), notes).unwrap();

        let hits = search(&conn, "how is the digest cached", SearchOptions::default()).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].source, "memory");
        // The project rides along in the title so a recalled note says where it
        // came from rather than looking like it is about whatever is open now.
        assert_eq!(hits[0].chat_title, "digest-caching (Widget)");
    }

    #[test]
    fn a_note_id_can_never_collide_with_a_conversation() {
        // Both are grouped by chat_id in the same table, and a note that landed
        // on a real conversation's id would delete that conversation's rows on
        // the next sync.
        let units = note_units(&[note("some-note", "d", "b")]);
        assert!(units[0].chat_id.starts_with(NOTE_ID_PREFIX));
        assert!(uuid::Uuid::parse_str(&units[0].chat_id).is_err());
    }

    #[test]
    fn a_long_note_repeats_its_name_in_every_piece() {
        let units = note_units(&[note("pricing-rules", "Ücret tarifesi", &"detail ".repeat(600))]);
        assert!(units.len() > 1, "expected a split, got {}", units.len());
        for unit in &units {
            assert!(unit.text.contains("pricing-rules"), "a window lost its subject: {}", unit.text);
            assert!(unit.text.chars().count() <= MAX_UNIT_CHARS);
        }
    }

    #[test]
    fn a_note_with_only_a_description_is_still_indexed() {
        // The description is the fact in the short ones — dropping these would
        // silently skip exactly the notes that are most concentrated.
        let units = note_units(&[note("no-body", "The token is stored in plaintext on purpose", "")]);
        assert_eq!(units.len(), 1);
        assert!(units[0].text.contains("plaintext"));
    }

    #[test]
    fn turning_notes_off_drops_them_but_keeps_the_conversations() {
        // Reconcile is what enforces the setting: notes_to_index returns nothing
        // when it is off, so their ids are absent from the live set. Verified
        // here rather than trusted, because the failure mode is silent — notes
        // staying searchable after the user asked them not to be.
        let conn = db();
        sync_chat(&conn, &chat("c1", "Real chat", vec![message("user", "the cache key", 10)])).unwrap();
        let notes = note_units(&[note("digest-caching", "How the digest is cached", "by repo")]);
        let note_id = notes[0].chat_id.clone();
        sync_units(&conn, &note_id, notes).unwrap();

        let live: HashSet<String> = ["c1".to_string()].into_iter().collect();
        let removed = reconcile(&conn, &live).unwrap();

        assert_eq!(removed, 1);
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM turns WHERE source = 'memory'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
        let chats: i64 = conn
            .query_row("SELECT COUNT(*) FROM turns WHERE chat_id = 'c1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(chats, 1, "the conversation was dropped along with the notes");
    }

    /// Writes a vector row directly, so a calibration test can control exactly
    /// which document each vector belongs to.
    fn seed_vec(conn: &Connection, chat_id: &str, hash: &str, vector: &[f32]) {
        conn.execute(
            "INSERT INTO turns (chat_id, chat_title, first_index, last_index, ts_start, ts_end,
                                source, hash, text, vec, vec_model)
             VALUES (?1, 't', 0, 0, 0, 0, '', ?2, 'x', ?3, 'test')",
            params![chat_id, hash, to_blob(vector)],
        )
        .unwrap();
    }

    /// A unit vector for document `slot`, `drift` apart from its siblings.
    ///
    /// Shaped to behave like a real embedding rather than a convenient one:
    /// every document shares a component (weight 0.6) and has one of its own
    /// (0.8), so any two different documents sit at cosine 0.36 — the sort of
    /// figure bge-m3 actually produces for unrelated text. Orthogonal test
    /// vectors would give cosine 0, which no embedding model ever returns, and
    /// the resulting variance makes mean+2σ meaningless.
    fn doc_vec(slot: usize, drift: f32) -> Vec<f32> {
        let mut v = [0.0_f32; 16];
        v[0] = 0.6;
        v[1 + slot] = 0.8 + drift;
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / norm).collect()
    }

    #[test]
    fn windows_of_one_document_do_not_inflate_the_floor() {
        // Found by measuring the real index after notes were added: a long note
        // is split into windows that all repeat the same head, so they score
        // ~1.0 against each other. Counting those as "unrelated" pairs put the
        // floor at 0.75 while real question-to-note matches scored 0.53-0.64,
        // which silently turned meaning search off.
        let conn = db();
        // One document, eight near-identical windows.
        for i in 0..8 {
            seed_vec(&conn, "note:long", &format!("h{i}"), &doc_vec(0, 0.001 * i as f32));
        }
        // Four genuinely different documents, all mutually at cosine 0.36.
        for slot in 1..5 {
            seed_vec(&conn, &format!("c{slot}"), &format!("d{slot}"), &doc_vec(slot, 0.0));
        }

        let floor = calibrate_similarity_floor(&conn).unwrap().expect("should calibrate");
        // Without the same-document skip this lands at the 0.95 clamp, because
        // the eight windows score ~1.0 against each other.
        assert!(
            floor < 0.5,
            "the same document's own windows dragged the floor to {floor}"
        );
    }

    #[test]
    fn a_single_document_gives_nothing_to_calibrate_against() {
        // Every vector from one note: there is no cross-document pair, so the
        // honest answer is "not yet" rather than a floor measured against a
        // document's similarity to itself.
        let conn = db();
        for i in 0..12 {
            seed_vec(&conn, "note:only", &format!("h{i}"), &doc_vec(0, 0.001 * i as f32));
        }
        assert_eq!(calibrate_similarity_floor(&conn).unwrap(), None);
    }

    #[test]
    fn the_injected_block_says_which_hits_are_notes() {
        // A note presented as an earlier conversation invites the model to
        // attribute it to a chat that never happened.
        let hits = vec![
            Hit {
                chat_id: "note:x".to_string(),
                chat_title: "digest-caching (Widget)".to_string(),
                ts_end: 1_700_000_000_000,
                source: "memory".to_string(),
                text: "Note \"digest-caching\" — key it by repo".to_string(),
                score: 1.0,
                coverage: 1.0,
            },
            Hit {
                chat_id: "c1".to_string(),
                chat_title: "Lookup performance".to_string(),
                ts_end: 1_700_000_000_000,
                source: String::new(),
                text: "Q: how do we avoid recomputing A: memoize it".to_string(),
                score: 1.0,
                coverage: 1.0,
            },
        ];
        let block = format_block(&hits, 4000);
        assert!(block.contains("· note] digest-caching (Widget)"), "{block}");
        assert!(block.contains("· chat] Lookup performance"), "{block}");
    }
}
