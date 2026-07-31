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
// The server is stable-diffusion.cpp's sd-server. It has two APIs and this tries
// its own one first, falling back to the OpenAI images shape that ai/llm.rs also
// talks — the same probe-then-fall-back arrangement, for the same reason.
//
// The fallback is genuinely a fallback and not an equal: measured against
// sd-server, the OpenAI-compatible endpoint silently ignores `seed`, `steps` and
// `width`/`height`, reading only `prompt`, `size` and `negative_prompt`. Drawing
// through it meant every request for the same prompt returned the same bytes
// forever, and two settings the user could change did nothing at all.
//
// Either way the model behind it stays a setting rather than a code change: SD
// 1.5, SDXL and Flux are all the same request, differing only in how big and how
// slow.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use serde::Serialize;
use tauri::Manager;

use crate::ai::process::{is_reachable, spawn_detached, stop_local_server};

/// How long to wait for the server to finish loading before giving up. Ten
/// gigabytes of weights off an SSD took about forty-five seconds when measured
/// here, so this is generous rather than tight — the alternative to waiting is
/// telling the user it failed while it is still starting.
const START_TIMEOUT_SECS: u64 = 180;

/// How often the idle watcher asks whether drawing has stopped.
const IDLE_CHECK_SECS: u64 = 15;

/// A server that has to load several GB before it answers the first request,
/// then denoise for as long as the resolution demands. Measured on this
/// machine: 512x512 in ~4s, 1024x1024 in ~10s, both after the model is warm —
/// but a cold start adds the model load, and a bigger model adds a lot of it.
const GENERATE_TIMEOUT_SECS: u64 = 600;

/// How often to ask whether an accepted job has finished. Short enough that a
/// two-second picture is not reported a second late, long enough that a
/// ten-minute one does not cost hundreds of requests.
const POLL_INTERVAL_MS: u64 = 250;

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
    /// What the picture was drawn from, when the server was one that takes a
    /// seed. Shown under the image because it is the only way back to a result
    /// you liked: the same seed and the same prompt reproduce it exactly, and
    /// without it a good picture is gone the moment you draw another.
    pub seed: Option<i64>,
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
        let pinned = crate::tunables::int(&app, crate::tunables::IMAGE_SEED);
        let seed = if pinned < 0 { random_seed() } else { pinned };

        let started = std::time::Instant::now();

        // Counted around everything below, including the load, so the idle
        // watcher cannot shut the server down while a picture is being drawn —
        // or while the one before it is still loading the weights.
        DRAWING.fetch_add(1, Ordering::SeqCst);
        let outcome = (|| -> Result<(Vec<u8>, Option<i64>), String> {
            ensure_running(&app, base_url)?;
            match sdcpp_generate(base_url, &prompt, negative.trim(), width, height, steps, seed) {
                Some(result) => Ok((result?, Some(seed))),
                // Not a stable-diffusion.cpp server. The generic endpoint still
                // draws, it just cannot be told a seed or a step count.
                None => Ok((
                    openai_generate(base_url, &prompt, negative.trim(), width, height)?,
                    None,
                )),
            }
        })();
        // Refreshed on the way out whether or not it worked: a failed attempt
        // still left a loaded server that should be given its idle period rather
        // than shut down a second later.
        *last_drawn().lock().unwrap() = Some(Instant::now());
        DRAWING.fetch_sub(1, Ordering::SeqCst);

        let (bytes, seed): (Vec<u8>, Option<i64>) = outcome?;

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
            seed,
            seconds: started.elapsed().as_secs_f32(),
        })
    })
    .await
}

/// A fresh seed for each picture.
///
/// Rolled here rather than by asking the server for a random one, because the
/// obvious way to do that does not work: sd-server documents -1 as "random" and
/// its own web UI defaults to it, but measured against this build, two requests
/// with `seed: -1` came back byte-identical. Left to the server, every drawing of
/// the same prompt is the same picture forever — which is exactly the symptom
/// this replaced.
///
/// Uuid rather than a new rand dependency: v4 is already used elsewhere in this
/// crate and is CSPRNG-backed, and four of its bytes are as good a seed as any.
fn random_seed() -> i64 {
    let bytes = uuid::Uuid::new_v4();
    let bytes = bytes.as_bytes();
    i64::from(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// stable-diffusion.cpp's own endpoint, which is the only one that takes a seed.
///
/// `None` means "this server does not have this endpoint" and the caller should
/// fall back — the same probe-then-fall-back shape as llm.rs's Ollama-native
/// path, and for the same reason: the richer API is worth using when it is there,
/// and its absence must not be an error.
///
/// Measured against sd-server, which is what made this necessary. On the
/// OpenAI-compatible endpoint below: `seed` is ignored, `width`/`height` are
/// ignored (only `size` is read), and `steps` is ignored — 6 steps and 30 steps
/// returned the same bytes in the same time. Here, all three take effect, and the
/// same seed twice reproduces the picture exactly.
fn sdcpp_generate(
    base_url: &str,
    prompt: &str,
    negative: &str,
    width: u32,
    height: u32,
    steps: i64,
    seed: i64,
) -> Option<Result<Vec<u8>, String>> {
    let mut payload = serde_json::json!({
        "prompt": prompt,
        "width": width,
        "height": height,
        "seed": seed,
        "sample_params": { "sample_steps": steps },
    });
    if !negative.is_empty() {
        payload["negative_prompt"] = serde_json::json!(negative);
    }

    let client = reqwest::blocking::Client::new();
    let response = client
        .post(format!("{base_url}/sdcpp/v1/img_gen"))
        .timeout(std::time::Duration::from_secs(30))
        .json(&payload)
        .send()
        .ok()?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return None;
    }
    Some(sdcpp_await_job(&client, base_url, response))
}

/// Waits out an accepted job.
///
/// This endpoint is asynchronous where the OpenAI-compatible one is not: it
/// answers 202 with a job id and a URL to poll. Polling rather than a long-held
/// request is the server's design, not a choice available here.
fn sdcpp_await_job(
    client: &reqwest::blocking::Client,
    base_url: &str,
    response: reqwest::blocking::Response,
) -> Result<Vec<u8>, String> {
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().unwrap_or_default();
        return Err(format!("The image server answered {status}: {}", detail.trim()));
    }
    let job: serde_json::Value = response
        .json()
        .map_err(|e| format!("The image server sent something unreadable: {e}"))?;
    let poll_url = job["poll_url"]
        .as_str()
        .ok_or("The image server accepted the job but did not say where to collect it")?;
    let poll_url = format!("{base_url}{poll_url}");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(GENERATE_TIMEOUT_SECS);
    loop {
        if std::time::Instant::now() > deadline {
            return Err("The image server is still working after ten minutes — giving up.".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS));
        let status: serde_json::Value = client
            .get(&poll_url)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .map_err(|e| format!("Lost contact with the image server: {e}"))?
            .json()
            .map_err(|e| format!("The image server sent something unreadable: {e}"))?;

        match status["status"].as_str().unwrap_or_default() {
            "completed" | "succeeded" => {
                let encoded = status["result"]["images"][0]["b64_json"]
                    .as_str()
                    .ok_or("The image server finished but sent no picture")?;
                return base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|e| format!("The image came back damaged: {e}"));
            }
            "failed" => {
                let why = status["error"].as_str().unwrap_or("no reason given");
                return Err(format!("The image server could not draw it: {why}"));
            }
            // queued / running: keep waiting.
            _ => {}
        }
    }
}

/// The generic endpoint, for any local server that is not stable-diffusion.cpp.
///
/// Only `prompt`, `size` and `negative_prompt` are sent, because those are the
/// only fields this endpoint was measured to read.
fn openai_generate(
    base_url: &str,
    prompt: &str,
    negative: &str,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let mut payload = serde_json::json!({
        "prompt": prompt,
        "size": format!("{width}x{height}"),
        "n": 1,
    });
    if !negative.is_empty() {
        payload["negative_prompt"] = serde_json::json!(negative);
    }

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
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| format!("The image came back damaged: {e}"))
}

/// Writes an image the user attached to a message into the pictures folder, and
/// answers where it went.
///
/// Attachments used to be the one kind of image with nowhere to live: a chat
/// stores no bytes, so a pasted screenshot existed only until the message was
/// sent and its card was then a label with nothing behind it. Kept beside the
/// generated pictures rather than somewhere of its own — both are "an image this
/// conversation refers to", and read_generated_image and open_generated_image
/// already confine themselves to that folder.
#[tauri::command]
pub async fn save_attached_image(
    app: tauri::AppHandle,
    name: String,
    data: String,
) -> Result<String, String> {
    crate::offload(move || {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .map_err(|e| format!("That image could not be read: {e}"))?;
        // Named from the attachment but through the same slug-and-timestamp rule
        // as a generated one, so two screenshots pasted a minute apart cannot
        // land on the same file, whatever the browser called them.
        let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(&name);
        let path = images_dir(&app).join(file_name(stem));
        std::fs::write(&path, &bytes)
            .map_err(|e| format!("Couldn't save the image to {}: {e}", path.display()))?;
        Ok(path.to_string_lossy().to_string())
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

// --- on-demand lifecycle ------------------------------------------------------
//
// The picture server is the one local server that is NOT started at launch, and
// the reason is arithmetic. A current image model holds about ten gigabytes of
// VRAM; the chat model holds another ten. On a sixteen-gigabyte card the two do
// not fit, and Windows does not refuse — it pages the excess to system memory and
// both keep reporting themselves as resident on the GPU while everything crawls
// across PCIe. Measured consequence: mail summaries began timing out the day a
// bigger image model was installed, with nothing to connect the two.
//
// So it is started by the first drawing and stopped once drawing stops. Not
// stopped after each picture: loading ten gigabytes takes about as long as five
// pictures, and asking for three in a row would pay it three times.

/// When the last picture finished, or None when this widget has not started the
/// server during this run.
///
/// Doubles as the "ours to stop" flag. A server the user started by hand — from
/// Settings, or in a terminal — leaves this None and is never shut down
/// underneath them, which is the same distinction process::stop_local_server
/// deliberately gave up for the tray's explicit stop-everything button.
static LAST_DRAWN: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();

/// Drawings currently in progress. The idle timer must not fire between two
/// pictures in a batch, and "reachable" says nothing about "busy".
static DRAWING: AtomicUsize = AtomicUsize::new(0);

fn last_drawn() -> &'static Mutex<Option<Instant>> {
    LAST_DRAWN.get_or_init(|| Mutex::new(None))
}

/// Makes sure something is answering at `base_url`, starting it if not.
///
/// Waits for the load rather than returning as soon as the process exists: a
/// server that has not finished reading its weights refuses connections, and the
/// caller is about to send it a request.
fn ensure_running(app: &tauri::AppHandle, base_url: &str) -> Result<(), String> {
    if is_reachable(base_url) {
        return Ok(());
    }
    let command = crate::tunables::text(app, crate::tunables::IMAGE_START_COMMAND);
    if command.trim().is_empty() {
        return Err(format!(
            "Nothing is answering at {base_url} and no start command is set — \
             fill one in under Settings → Images."
        ));
    }
    if spawn_detached(command.trim()).is_none() {
        return Err("Couldn't launch the image server — check the start command.".to_string());
    }
    // Marked as ours before the wait, so a load that times out still leaves a
    // process the idle timer will clean up rather than one that lingers forever.
    *last_drawn().lock().unwrap() = Some(Instant::now());

    let deadline = Instant::now() + Duration::from_secs(START_TIMEOUT_SECS);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(700));
        if is_reachable(base_url) {
            return Ok(());
        }
    }
    Err(format!(
        "The image server did not come up within {START_TIMEOUT_SECS}s. A large \
         model can take a while to load — try again, or start it from Settings → Images."
    ))
}

/// Stops the picture server once nothing has been drawn for a while.
///
/// One long-lived thread rather than a timer armed per drawing: the condition is
/// "quiet for long enough", and that is cheaper to ask periodically than to
/// cancel and re-arm on every request.
pub fn start_idle_watcher(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(IDLE_CHECK_SECS));

        let idle_secs = crate::tunables::int(&app, crate::tunables::IMAGE_IDLE_SHUTDOWN);
        // Zero means "leave it running", for a machine with the memory to spare.
        if idle_secs <= 0 || DRAWING.load(Ordering::SeqCst) > 0 {
            continue;
        }
        let Some(last) = *last_drawn().lock().unwrap() else {
            continue; // not ours — see LAST_DRAWN
        };
        if last.elapsed() < Duration::from_secs(idle_secs as u64) {
            continue;
        }

        let base_url = crate::tunables::text(&app, crate::tunables::IMAGE_BASE_URL);
        stop_local_server(base_url.trim());
        *last_drawn().lock().unwrap() = None;
    });
}

/// Stops the local picture server for the tray's stop-and-quit, whoever started
/// it — see process::stop_local_server.
pub fn stop_server(app: &tauri::AppHandle) {
    stop_local_server(crate::tunables::text(app, crate::tunables::IMAGE_BASE_URL).trim());
}

/// The Images section's "Start now" button.
///
/// Takes no arguments, unlike llm.rs's start_server_now which is handed a URL and
/// a command by whichever of the LLM/STT/TTS forms called it. Those three keep
/// their settings in AppConfig, where the form owns them; this server's live in
/// the tunables registry, and having the settings window read them out of its own
/// inputs to hand straight back would put a second copy of them in the frontend —
/// which is the thing tunables.rs exists to prevent.
#[tauri::command]
pub async fn start_image_server_now(app: tauri::AppHandle) -> Result<String, String> {
    crate::offload(move || {
        let base_url = crate::tunables::text(&app, crate::tunables::IMAGE_BASE_URL);
        let command = crate::tunables::text(&app, crate::tunables::IMAGE_START_COMMAND);
        if command.trim().is_empty() {
            return Err("No start command configured.".to_string());
        }
        if crate::ai::process::is_reachable(base_url.trim()) {
            return Ok("Already running.".to_string());
        }
        match crate::ai::process::spawn_detached(command.trim()) {
            Some(_) => Ok("Starting… a big model takes a few seconds to load.".to_string()),
            None => Err("Couldn't launch that command — check it's a valid path.".to_string()),
        }
    })
    .await
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
