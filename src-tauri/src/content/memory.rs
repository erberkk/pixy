use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri_plugin_dialog::DialogExt;

use crate::config::{read_config, write_config};

// Read-only viewer over Claude Code's OWN memory system
// (~/.claude/projects/<slug>/memory/*.md) — this module never writes,
// classifies, or decides what's worth remembering; that's entirely Claude's
// own judgment call (see the memory-writing instructions in Claude Code's
// system prompt). All this does is find the files it already wrote, parse
// the small YAML-like frontmatter block + [[links]] it already puts in
// them, and hand back a flat list for the frontend to lay out as a graph.

#[derive(Serialize, Clone)]
pub struct MemoryNode {
    // Frontmatter's `name:` slug — the stable id [[links]] reference, and
    // what ties a node to its incoming edges regardless of filename.
    name: String,
    description: String,
    // Frontmatter's metadata.type (user/feedback/project/reference) when
    // present; real files in the wild also use the older `node_type` key or
    // omit it entirely, so this tolerantly falls back to "note".
    #[serde(rename = "type")]
    node_type: String,
    // Raw markdown body (frontmatter stripped) — rendered client-side with
    // the same markdownToHtml the regular notes editor already uses.
    body: String,
    // Other memories' `name:` slugs this one's body references via
    // [[name]] — may point at a slug with no matching file (a memory not
    // written yet, or renamed); the frontend renders those as ghost nodes
    // rather than silently dropping the edge, matching Obsidian's own
    // unresolved-link convention.
    links: Vec<String>,
    project: String,
    file_path: String,
    updated_at: u64,
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

// Claude Code's own default — respects CLAUDE_CONFIG_DIR the same way the
// CLI itself does, so a user who's set that env var still gets their real
// memory files instead of silently seeing the wrong (or an empty) one.
fn default_projects_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("projects"));
    }
    home_dir().map(|h| h.join(".claude").join("projects"))
}

// "c--Users-erber-Projects-Widget" -> "Widget" — Claude Code slugifies a
// project's absolute path into its ~/.claude/projects/ directory name by
// replacing path separators with hyphens; the last non-empty segment is the
// original folder's own name, which is all a human needs to tell projects
// apart in this UI (the full slug is still kept as `project` for filtering,
// this is only used for anything wanting a short label — currently none of
// the Rust side, but kept here as the one place this logic should live if
// the frontend ever wants it pre-computed instead of re-deriving it itself).
#[allow(dead_code)]
fn prettify_project_slug(slug: &str) -> String {
    slug.rsplit('-').find(|s| !s.is_empty()).unwrap_or(slug).to_string()
}

fn file_millis(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// Hand-parses the `---\n key: value \n---` frontmatter block used by every
// memory file — a real YAML parser is overkill for a format this small and
// this codebase already avoids pulling in crates for parsing jobs of this
// size (see agent/terminal.rs's own hand-rolled line scanners). Returns
// (name, description, type, body_without_frontmatter).
fn parse_frontmatter(raw: &str) -> Option<(String, String, String, String)> {
    let rest = raw.strip_prefix("---")?;
    let (frontmatter, body) = rest.split_once("\n---")?;
    let body = body.strip_prefix('\n').unwrap_or(body).to_string();

    let mut name = String::new();
    let mut description = String::new();
    let mut node_type = String::new();
    let mut in_metadata = false;

    for line in frontmatter.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        // Any indented line belongs to the `metadata:` block (the only
        // nested key these files use) — its own sub-keys (type/node_type/
        // originSessionId/...) are read the same indented way.
        let indented = trimmed.starts_with(' ') || trimmed.starts_with('\t');
        let content = trimmed.trim_start();

        if !indented {
            in_metadata = content.starts_with("metadata:");
            if let Some(v) = content.strip_prefix("name:") {
                name = v.trim().to_string();
            } else if let Some(v) = content.strip_prefix("description:") {
                description = v.trim().to_string();
            }
            continue;
        }

        if in_metadata {
            if let Some(v) = content.strip_prefix("type:") {
                node_type = v.trim().to_string();
            } else if node_type.is_empty() {
                if let Some(v) = content.strip_prefix("node_type:") {
                    node_type = v.trim().to_string();
                }
            }
        }
    }

    if name.is_empty() {
        return None; // no usable id to link against — skip this file
    }
    if node_type.is_empty() {
        node_type = "note".to_string();
    }
    Some((name, description, node_type, body))
}

// Scans body text for [[name]] occurrences without pulling in a regex
// dependency — a plain bracket-matching pass is all this needs since these
// files are written by Claude itself in a single consistent shape.
fn extract_links(body: &str) -> Vec<String> {
    let mut links = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            if let Some(end) = body[i + 2..].find("]]") {
                let name = body[i + 2..i + 2 + end].trim().to_string();
                if !name.is_empty() && !links.contains(&name) {
                    links.push(name);
                }
                i += 2 + end + 2;
                continue;
            }
        }
        i += 1;
    }
    links
}

fn memory_files_in(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let is_md = p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("md"))
                .unwrap_or(false);
            // MEMORY.md is the plain-link index the memory system itself
            // maintains, not a memory node with its own frontmatter/id.
            let is_index = p.file_name().and_then(|n| n.to_str()) == Some("MEMORY.md");
            is_md && !is_index
        })
        .collect()
}

// A "projects root" is a directory shaped like ~/.claude/projects itself:
// one subdirectory per project, each optionally containing its own
// memory/*.md files. Shared between the default root and every extra root
// a user adds (see add_memory_root) so both are scanned identically rather
// than the extra ones needing some different, second-class shape.
fn scan_root(root: &Path, nodes: &mut Vec<MemoryNode>) {
    let Ok(entries) = fs::read_dir(root) else { return };

    for entry in entries.filter_map(|e| e.ok()) {
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }
        let project_slug = entry.file_name().to_string_lossy().to_string();
        let memory_dir = project_dir.join("memory");
        if !memory_dir.is_dir() {
            continue;
        }

        for path in memory_files_in(&memory_dir) {
            let Ok(raw) = fs::read_to_string(&path) else { continue };
            let Some((name, description, node_type, body)) = parse_frontmatter(&raw) else { continue };
            let links = extract_links(&body);
            nodes.push(MemoryNode {
                name,
                description,
                node_type,
                body,
                links,
                project: project_slug.clone(),
                file_path: path.to_string_lossy().to_string(),
                updated_at: file_millis(&path),
            });
        }
    }
}

#[tauri::command]
pub fn list_memories(app: tauri::AppHandle) -> Vec<MemoryNode> {
    let mut nodes = Vec::new();

    if let Some(root) = default_projects_root() {
        scan_root(&root, &mut nodes);
    }
    for extra in read_config(&app).memory_extra_roots {
        scan_root(&PathBuf::from(extra), &mut nodes);
    }

    // A memory with the same `name` present in more than one root (e.g. an
    // extra root that's actually a synced duplicate of the default one) —
    // keep only the most recently modified copy rather than showing the
    // same node/edges twice.
    let mut by_name: std::collections::HashMap<String, MemoryNode> = std::collections::HashMap::new();
    for node in nodes {
        match by_name.get(&node.name) {
            Some(existing) if existing.updated_at >= node.updated_at => {}
            _ => {
                by_name.insert(node.name.clone(), node);
            }
        }
    }
    let mut nodes: Vec<MemoryNode> = by_name.into_values().collect();

    nodes.sort_by(|a, b| a.project.cmp(&b.project).then(a.name.cmp(&b.name)));
    nodes
}

#[derive(Serialize)]
pub struct MemoryRoots {
    // The default root's path, purely informational — shown in the UI so a
    // user who's confused about "where is Claude even looking" has an
    // answer, but it's not itself removable (there's always a default).
    default_root: Option<String>,
    default_root_exists: bool,
    extra_roots: Vec<String>,
}

#[tauri::command]
pub fn get_memory_roots(app: tauri::AppHandle) -> MemoryRoots {
    let default_root = default_projects_root();
    MemoryRoots {
        default_root_exists: default_root.as_ref().map(|p| p.is_dir()).unwrap_or(false),
        default_root: default_root.map(|p| p.to_string_lossy().to_string()),
        extra_roots: read_config(&app).memory_extra_roots,
    }
}

// Lets a user point the Memory graph at another ~/.claude/projects-shaped
// directory — e.g. CLAUDE_CONFIG_DIR on a different machine's synced
// backup, a second OS user profile, or any other copy of Claude Code's own
// project/memory layout the default root wouldn't otherwise reach.
#[tauri::command]
pub fn add_memory_root(app: tauri::AppHandle) -> Option<String> {
    let picked = app.dialog().file().blocking_pick_folder()?;
    let path = picked.into_path().ok()?.to_string_lossy().to_string();

    let mut cfg = read_config(&app);
    if !cfg.memory_extra_roots.contains(&path) {
        cfg.memory_extra_roots.push(path.clone());
        write_config(&app, &cfg);
    }
    Some(path)
}

#[tauri::command]
pub fn remove_memory_root(app: tauri::AppHandle, path: String) {
    let mut cfg = read_config(&app);
    cfg.memory_extra_roots.retain(|p| p != &path);
    write_config(&app, &cfg);
}
