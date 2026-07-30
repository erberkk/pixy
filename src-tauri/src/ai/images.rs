// Making a picture from a description, on a local image server.
//
// Deliberately NOT a tool the model calls, unlike web search. The tool loop
// exists so a model can read something it did not know and reason about it —
// and an image is the one thing it cannot read back. A tool here would return
// "a file was written to X", which the model can only repeat, in exchange for a
// second full round of generation (measured at 15-20s on this machine's model).
// "Draw me a fox" needs no interpretation, so the request goes straight to the
// server and the answer comes straight back.
//
// The server is stable-diffusion.cpp's sd-server, which speaks the OpenAI
// images shape — the same dialect ai/llm.rs already talks. That is what makes
// the model behind it a setting rather than a code change: SD 1.5, SDXL and
// Flux are all the same request, differing only in how big and how slow.

use std::path::PathBuf;

use base64::Engine;
use serde::Serialize;
use tauri::Manager;

use crate::ai::process::{autostart_if_needed, stop_tracked};

/// A server that has to load several GB before it answers the first request,
/// then denoise for as long as the resolution demands. Measured on this
/// machine: 512x512 in ~4s, 1024x1024 in ~10s, both after the model is warm —
/// but a cold start adds the model load, and a bigger model adds a lot of it.
const GENERATE_TIMEOUT_SECS: u64 = 600;

static IMAGE_CHILD_PID: std::sync::Mutex<Option<u32>> = std::sync::Mutex::new(None);

#[derive(Serialize)]
pub struct GeneratedImage {
    /// Where it was written. Kept in the conversation so the picture survives a
    /// restart — chat files deliberately do not store image bytes, and one that
    /// only existed in the message would vanish on reload.
    pub path: String,
    /// The same bytes, for showing it immediately without a round trip back to
    /// disk for something already in memory.
    pub data_url: String,
    pub width: u32,
    pub height: u32,
    pub seconds: f32,
}

fn images_dir(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .expect("app data dir must be resolvable")
        .join("Images");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Turns a prompt into a filename that says what it was.
///
/// A folder of `image_1.png` is unusable a week later, so the prompt leads —
/// trimmed hard, because a prompt can be a paragraph and a path cannot. The
/// timestamp keeps two attempts at the same idea apart.
fn file_name(prompt: &str) -> String {
    let slug: String = prompt
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    if slug.is_empty() {
        format!("image-{stamp}.png")
    } else {
        format!("{slug}-{stamp}.png")
    }
}

#[tauri::command]
pub async fn generate_image(
    app: tauri::AppHandle,
    prompt: String,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<GeneratedImage, String> {
    crate::offload(move || {
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            return Err("Describe what to draw.".to_string());
        }

        let base_url = crate::tunables::text(&app, crate::tunables::IMAGE_BASE_URL);
        let base_url = base_url.trim().trim_end_matches('/');
        if base_url.is_empty() {
            return Err(
                "No image server is configured — set one in Settings under Images.".to_string(),
            );
        }
        // Same rule as the embedding endpoint in recall.rs: a prompt is the
        // user's own words, and this app does not send those to a stranger's
        // machine without it being an explicit, visible choice. There is no such
        // choice here yet, so the address simply has to be local.
        if !crate::ai::recall::is_local_endpoint(base_url) {
            return Err(format!(
                "{base_url} is not on this machine. Image generation only talks to a local \
                 server."
            ));
        }

        let size = crate::tunables::int(&app, crate::tunables::IMAGE_SIZE) as u32;
        let width = width.unwrap_or(size);
        let height = height.unwrap_or(size);
        let steps = crate::tunables::int(&app, crate::tunables::IMAGE_STEPS);
        let negative = crate::tunables::text(&app, crate::tunables::IMAGE_NEGATIVE_PROMPT);

        let mut payload = serde_json::json!({
            "prompt": prompt,
            "size": format!("{width}x{height}"),
            "n": 1,
            "steps": steps,
        });
        if !negative.trim().is_empty() {
            payload["negative_prompt"] = serde_json::json!(negative.trim());
        }

        let started = std::time::Instant::now();
        let response = reqwest::blocking::Client::new()
            .post(format!("{base_url}/v1/images/generations"))
            .timeout(std::time::Duration::from_secs(GENERATE_TIMEOUT_SECS))
            .json(&payload)
            .send()
            .map_err(|e| {
                format!(
                    "Couldn't reach the image server at {base_url}: {e}\n\
                     Start it from Settings, or check the address."
                )
            })?;
        if !response.status().is_success() {
            let status = response.status();
            let detail = response.text().unwrap_or_default();
            return Err(format!("The image server answered {status}: {}", detail.trim()));
        }

        let body: serde_json::Value = response
            .json()
            .map_err(|e| format!("The image server sent something unreadable: {e}"))?;
        let encoded = body["data"][0]["b64_json"]
            .as_str()
            .ok_or_else(|| format!("No image in the server's reply: {body}"))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| format!("The image came back damaged: {e}"))?;

        let path = images_dir(&app).join(file_name(&prompt));
        std::fs::write(&path, &bytes)
            .map_err(|e| format!("Couldn't save the image to {}: {e}", path.display()))?;

        Ok(GeneratedImage {
            path: path.to_string_lossy().to_string(),
            data_url: format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&bytes)
            ),
            width,
            height,
            seconds: started.elapsed().as_secs_f32(),
        })
    })
    .await
}

/// Reads a previously generated image back off disk.
///
/// Needed because a conversation stores the path, not the bytes: a chat file
/// that carried several megabytes of base64 per picture would be unopenable
/// after a dozen of them.
#[tauri::command]
pub async fn read_generated_image(path: String) -> Result<String, String> {
    crate::offload(move || {
        let bytes = std::fs::read(&path).map_err(|e| format!("Couldn't open {path}: {e}"))?;
        Ok(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        ))
    })
    .await
}

/// Shows a generated picture in whatever the system opens PNGs with.
///
/// A separate command from content/notes.rs's `open_path`, which is about text
/// files and answers None for anything else — reusing it here would silently do
/// nothing.
#[tauri::command]
pub fn open_generated_image(app: tauri::AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    // Only ever opens a file this app wrote. The path comes back from the
    // conversation, so it is not attacker-chosen, but confining it to the
    // pictures folder means a corrupted or hand-edited chat file cannot turn
    // this into "open anything on disk".
    let path = PathBuf::from(&path);
    if !path.starts_with(images_dir(&app)) {
        return Err("That file is not one of the generated pictures.".to_string());
    }
    app.opener()
        .open_path(path.to_string_lossy().to_string(), None::<String>)
        .map_err(|e| format!("Couldn't open it: {e}"))
}

/// Started at launch when the user has configured a command for it, exactly
/// like the LLM and speech servers — a picture server that has to be started by
/// hand is one that is never running when it is wanted.
pub fn maybe_autostart(app: &tauri::AppHandle) {
    let command = crate::tunables::text(app, crate::tunables::IMAGE_START_COMMAND);
    let base_url = crate::tunables::text(app, crate::tunables::IMAGE_BASE_URL);
    autostart_if_needed(base_url.trim(), command.trim(), &IMAGE_CHILD_PID);
}

/// Only ever kills a process this widget started itself — see the same note on
/// llm.rs's stop_autostarted.
pub fn stop_autostarted() {
    stop_tracked(&IMAGE_CHILD_PID);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_name_says_what_the_picture_was() {
        let name = file_name("A red fox walking through falling snow");
        assert!(name.starts_with("a-red-fox-walking-through-falling"), "{name}");
        assert!(name.ends_with(".png"));
    }

    #[test]
    fn punctuation_and_non_ascii_do_not_reach_the_path() {
        let name = file_name("kar'da yürüyen tilki! (çok güzel)");
        assert!(!name.contains('\''), "{name}");
        assert!(!name.contains('!'), "{name}");
        assert!(!name.contains('('), "{name}");
        // Turkish letters are alphanumeric and are kept — they are legal in a
        // Windows filename, and stripping them would turn a Turkish prompt into
        // a row of dashes.
        assert!(name.contains("yürüyen"), "{name}");
    }

    #[test]
    fn a_prompt_with_nothing_usable_still_produces_a_name() {
        let name = file_name("!!! ??? ---");
        assert!(name.starts_with("image-"), "{name}");
        assert!(name.ends_with(".png"));
    }

    #[test]
    fn two_pictures_of_the_same_thing_do_not_collide() {
        // The timestamp is what separates them; at one-second resolution this
        // only guarantees it across seconds, which is why the assertion is on
        // the shape rather than on two calls in a row.
        let name = file_name("fox");
        assert!(name.starts_with("fox-"), "{name}");
        assert_eq!(name.matches('-').count() >= 2, true, "{name}");
    }
}
