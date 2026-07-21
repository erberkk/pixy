use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;

use crate::config::{read_config, write_config};

// Notes are plain .md/.txt files in a real, user-visible folder (not a
// hidden JSON blob) — filename (sans extension) is the title, file
// contents are the body, so any note can be opened/edited by any other
// text editor too. Per-note extras that don't belong in the file itself
// (pinned/trashed) live in a small sidecar keyed by absolute path.
#[derive(Serialize, Deserialize, Clone)]
pub struct Note {
    file_path: String,
    title: String,
    content: String,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    trashed: bool,
    #[serde(default)]
    created_at: u64,
    updated_at: u64,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct NoteMeta {
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    trashed: bool,
    #[serde(default)]
    created_at: u64,
}

fn current_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn meta_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .expect("app data dir must be resolvable");
    dir.join("notes-meta.json")
}

fn read_meta(app: &tauri::AppHandle) -> HashMap<String, NoteMeta> {
    match fs::read_to_string(meta_path(app)) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

fn write_meta(app: &tauri::AppHandle, meta: &HashMap<String, NoteMeta>) {
    if let Ok(json) = serde_json::to_string_pretty(meta) {
        let _ = fs::write(meta_path(app), json);
    }
}

fn notes_dir(app: &tauri::AppHandle) -> PathBuf {
    let cfg = read_config(app);
    let dir = match cfg.notes_dir {
        Some(dir) => PathBuf::from(dir),
        None => app
            .path()
            .app_data_dir()
            .expect("app data dir must be resolvable")
            .join("Notes"),
    };
    let _ = fs::create_dir_all(&dir);
    dir
}

fn set_notes_dir(app: &tauri::AppHandle, dir: &Path) {
    let mut cfg = read_config(app);
    cfg.notes_dir = Some(dir.to_string_lossy().to_string());
    write_config(app, &cfg);
}

// Records a path opened from outside notes_dir so it keeps appearing in the
// sidebar on future launches (list_notes only scans notes_dir on its own).
fn track_external_file(app: &tauri::AppHandle, path: &Path) {
    let dir = notes_dir(app);
    if path.parent() == Some(dir.as_path()) {
        return; // already inside the managed folder, nothing to track
    }
    let mut cfg = read_config(app);
    let key = path.to_string_lossy().to_string();
    if !cfg.external_files.contains(&key) {
        cfg.external_files.push(key);
        write_config(app, &cfg);
    }
}

fn untrack_external_file(app: &tauri::AppHandle, path: &str) {
    let mut cfg = read_config(app);
    let before = cfg.external_files.len();
    cfg.external_files.retain(|p| p != path);
    if cfg.external_files.len() != before {
        write_config(app, &cfg);
    }
}

// Windows forbids \ / : * ? " < > | in filenames, and trailing dots/spaces
// silently get stripped by the OS in ways that break round-tripping.
fn sanitize_filename(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| if r#"\/:*?"<>|"#.contains(c) { '-' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']).to_string();
    if trimmed.is_empty() {
        "Untitled".to_string()
    } else {
        trimmed
    }
}

fn unique_path(dir: &Path, stem: &str, ext: &str, keep: Option<&Path>) -> PathBuf {
    let mut candidate = dir.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while candidate.exists() && keep != Some(candidate.as_path()) {
        candidate = dir.join(format!("{stem} {n}.{ext}"));
        n += 1;
    }
    candidate
}

fn file_millis(path: &Path, pick: impl Fn(std::fs::Metadata) -> std::io::Result<std::time::SystemTime>) -> u64 {
    fs::metadata(path)
        .and_then(pick)
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn note_from_file(path: &Path, meta_store: &mut HashMap<String, NoteMeta>) -> Option<Note> {
    let content = fs::read_to_string(path).ok()?;
    let title = path.file_stem()?.to_string_lossy().to_string();
    let key = path.to_string_lossy().to_string();

    let meta = meta_store.entry(key.clone()).or_insert_with(|| NoteMeta {
        created_at: file_millis(path, |m| m.created().or_else(|_| m.modified())),
        ..Default::default()
    });

    Some(Note {
        file_path: key,
        title,
        content,
        pinned: meta.pinned,
        trashed: meta.trashed,
        created_at: meta.created_at,
        updated_at: file_millis(path, |m| m.modified()),
    })
}

#[tauri::command]
pub fn get_notes_dir(app: tauri::AppHandle) -> String {
    notes_dir(&app).to_string_lossy().to_string()
}

#[tauri::command]
pub fn choose_notes_dir(app: tauri::AppHandle) -> Option<String> {
    let picked = app.dialog().file().blocking_pick_folder()?;
    let path = picked.into_path().ok()?;
    set_notes_dir(&app, &path);
    Some(path.to_string_lossy().to_string())
}

#[tauri::command]
pub fn open_external_file(app: tauri::AppHandle) -> Option<Note> {
    let picked = app
        .dialog()
        .file()
        .add_filter("Notes", &["md", "txt"])
        .blocking_pick_file()?;
    let path = picked.into_path().ok()?;
    let mut meta_store = read_meta(&app);
    let note = note_from_file(&path, &mut meta_store)?;
    write_meta(&app, &meta_store);
    track_external_file(&app, &path);
    Some(note)
}

// Used for drag-and-drop: the frontend gets the dropped file's absolute
// path from Tauri's native drag-drop event (not the browser File API, which
// never exposes real paths), then asks us to open it directly by path.
#[tauri::command]
pub fn open_path(app: tauri::AppHandle, path: String) -> Option<Note> {
    let path = PathBuf::from(path);
    let is_text_like = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "txt" | "markdown"))
        .unwrap_or(false);
    if !is_text_like {
        return None;
    }
    let mut meta_store = read_meta(&app);
    let note = note_from_file(&path, &mut meta_store)?;
    write_meta(&app, &meta_store);
    track_external_file(&app, &path);
    Some(note)
}

#[tauri::command]
pub fn list_notes(app: tauri::AppHandle) -> Vec<Note> {
    let dir = notes_dir(&app);
    let mut meta_store = read_meta(&app);

    let mut notes: Vec<Note> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("txt"))
                .unwrap_or(false)
        })
        .filter_map(|path| note_from_file(&path, &mut meta_store))
        .collect();

    // Files opened from outside notes_dir aren't found by the scan above —
    // pull them in from the tracked list, dropping any that were since
    // moved/deleted externally.
    let cfg = read_config(&app);
    let mut still_valid = Vec::new();
    for external in &cfg.external_files {
        let path = PathBuf::from(external);
        if let Some(note) = note_from_file(&path, &mut meta_store) {
            notes.push(note);
            still_valid.push(external.clone());
        }
    }
    if still_valid.len() != cfg.external_files.len() {
        let mut cfg = cfg;
        cfg.external_files = still_valid;
        write_config(&app, &cfg);
    }

    write_meta(&app, &meta_store);
    notes.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    notes
}

#[tauri::command]
pub fn save_note(app: tauri::AppHandle, note: Note, ext: Option<String>) -> Note {
    let now = current_millis();
    let mut meta_store = read_meta(&app);

    let (old_path, is_new) = if note.file_path.is_empty() {
        (None, true)
    } else {
        (Some(PathBuf::from(&note.file_path)), false)
    };

    // `ext` lets the frontend force an extension: the default-format choice
    // for brand-new notes, or an explicit "convert this note to .md/.txt"
    // action on an existing one. Otherwise an existing note just keeps
    // whatever extension it already has.
    let ext = ext.unwrap_or_else(|| {
        old_path
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .unwrap_or("txt")
            .to_string()
    });
    let parent = old_path
        .as_ref()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| notes_dir(&app));

    let stem = sanitize_filename(&note.title);
    let final_path = unique_path(&parent, &stem, &ext, old_path.as_deref());

    if let Some(old) = &old_path {
        if old != &final_path {
            let _ = fs::rename(old, &final_path);
            if let Some(m) = meta_store.remove(&old.to_string_lossy().to_string()) {
                meta_store.insert(final_path.to_string_lossy().to_string(), m);
            }
        }
    }

    let _ = fs::write(&final_path, &note.content);

    let key = final_path.to_string_lossy().to_string();
    let created_at = if is_new {
        now
    } else {
        meta_store.get(&key).map(|m| m.created_at).unwrap_or(now)
    };
    meta_store.insert(
        key.clone(),
        NoteMeta {
            pinned: note.pinned,
            trashed: note.trashed,
            created_at,
        },
    );
    write_meta(&app, &meta_store);

    Note {
        file_path: key,
        title: stem,
        content: note.content,
        pinned: note.pinned,
        trashed: note.trashed,
        created_at,
        updated_at: file_millis(&final_path, |m| m.modified()),
    }
}

#[tauri::command]
pub fn delete_note(app: tauri::AppHandle, file_path: String) {
    let _ = fs::remove_file(&file_path);
    let mut meta_store = read_meta(&app);
    meta_store.remove(&file_path);
    write_meta(&app, &meta_store);
    untrack_external_file(&app, &file_path);
}
