use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use base64::{engine::general_purpose, Engine as _};
use serde::Serialize;
use tauri::{Emitter, Manager};
use windows::core::Interface;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession, GlobalSystemMediaTransportControlsSessionManager,
};
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference};
use windows::Win32::Foundation::{CloseHandle, S_OK};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    eCapture, eMultimedia, eRender, EDataFlow, IAudioSessionControl2, IAudioSessionManager2,
    IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

fn debug_log_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .expect("app data dir must be resolvable");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("media-debug.log")
}

fn append_debug_log(app: &tauri::AppHandle, entry: &str) {
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(debug_log_path(app))
    {
        let _ = writeln!(file, "{entry}\n");
    }
}

#[derive(Serialize, Clone, PartialEq)]
pub struct NowPlaying {
    title: String,
    artist: String,
    is_playing: bool,
    // Data URL ("data:image/jpeg;base64,...") built straight from GSMTC's
    // thumbnail stream — None if the track has no art or it couldn't be
    // read, in which case the frontend just hides the art element.
    art: Option<String>,
}

// GSMTC exposes the album art as a stream reference, not raw bytes — this
// opens it, reads the whole thing via a DataReader, and returns it together
// with its content type (so the data: URL gets the right MIME instead of
// guessing image/jpeg for everything).
fn read_thumbnail(thumb: &IRandomAccessStreamReference) -> Option<(String, Vec<u8>)> {
    let stream = thumb.OpenReadAsync().ok()?.get().ok()?;
    let content_type = stream.ContentType().ok()?.to_string_lossy();
    let size = stream.Size().ok()? as u32;
    if size == 0 {
        return None;
    }
    let reader = DataReader::CreateDataReader(&stream).ok()?;
    reader.LoadAsync(size).ok()?.get().ok()?;
    let mut buf = vec![0u8; size as usize];
    reader.ReadBytes(&mut buf).ok()?;
    Some((content_type, buf))
}

// GSMTC's GetSessions() returns every media session system-wide (browser
// tabs, other apps, etc.) — this filters to specifically Spotify's, so the
// panel always reflects Spotify regardless of what else might be playing
// elsewhere.
fn find_spotify_session(
    manager: &GlobalSystemMediaTransportControlsSessionManager,
) -> Option<GlobalSystemMediaTransportControlsSession> {
    let sessions = manager.GetSessions().ok()?;
    for session in sessions {
        if let Ok(id) = session.SourceAppUserModelId() {
            if id.to_string_lossy().to_lowercase().contains("spotify") {
                return Some(session);
            }
        }
    }
    None
}

fn get_now_playing_inner() -> Result<Option<NowPlaying>, String> {
    ensure_com_initialized();
    let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
        .map_err(|e| format!("RequestAsync failed: {e}"))?
        .get()
        .map_err(|e| format!("RequestAsync.get failed: {e}"))?;

    let Some(session) = find_spotify_session(&manager) else {
        return Ok(None);
    };

    let props = session
        .TryGetMediaPropertiesAsync()
        .map_err(|e| format!("TryGetMediaPropertiesAsync failed: {e}"))?
        .get()
        .map_err(|e| format!("TryGetMediaPropertiesAsync.get failed: {e}"))?;

    let title = props.Title().map(|s| s.to_string_lossy()).unwrap_or_default();
    let artist = props.Artist().map(|s| s.to_string_lossy()).unwrap_or_default();

    let is_playing = session
        .GetPlaybackInfo()
        .and_then(|info| info.PlaybackStatus())
        .map(|status| status.0 == 4) // GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing
        .unwrap_or(false);

    let art = props.Thumbnail().ok().and_then(|thumb| read_thumbnail(&thumb)).map(|(content_type, bytes)| {
        let mime = if content_type.is_empty() { "image/jpeg".to_string() } else { content_type };
        format!("data:{mime};base64,{}", general_purpose::STANDARD.encode(bytes))
    });

    Ok(Some(NowPlaying { title, artist, is_playing, art }))
}

fn ensure_com_initialized() {
    // Tauri commands can run on different thread-pool threads per call, and
    // COM apartment state is per-thread — calling this at the top of every
    // command/probe (ignoring the result) is simpler and safer here than
    // trying to funnel every call through one dedicated thread. A thread
    // already initialized with the same (or no) apartment just gets back a
    // harmless S_FALSE/already-initialized result.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

fn with_spotify_session<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce(&GlobalSystemMediaTransportControlsSession) -> windows::core::Result<T>,
{
    ensure_com_initialized();
    let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
        .map_err(|e| format!("RequestAsync failed: {e}"))?
        .get()
        .map_err(|e| format!("RequestAsync.get failed: {e}"))?;
    let session = find_spotify_session(&manager).ok_or_else(|| "no Spotify session found".to_string())?;
    f(&session).map_err(|e| format!("command failed: {e}"))
}

#[tauri::command]
pub fn spotify_play_pause() -> Result<(), String> {
    with_spotify_session(|s| s.TryTogglePlayPauseAsync()?.get())?;
    Ok(())
}

#[tauri::command]
pub fn spotify_next() -> Result<(), String> {
    with_spotify_session(|s| s.TrySkipNextAsync()?.get())?;
    Ok(())
}

#[tauri::command]
pub fn spotify_previous() -> Result<(), String> {
    with_spotify_session(|s| s.TrySkipPreviousAsync()?.get())?;
    Ok(())
}

// Per-app volume (like the individual sliders in Windows' own Volume
// Mixer) — a completely separate, older Win32 COM API from GSMTC, which
// has no volume control of its own at all. Keyed by pid — list_audio_sessions
// already knows every session's pid directly, and Spotify's own volume is
// just one row in that same list rather than a separate lookup now.
fn with_session_by_pid<F, T>(pid: u32, f: F) -> Result<T, String>
where
    F: FnOnce(&ISimpleAudioVolume) -> windows::core::Result<T>,
{
    ensure_com_initialized();
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| format!("CoCreateInstance failed: {e}"))?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eMultimedia)
            .map_err(|e| format!("GetDefaultAudioEndpoint failed: {e}"))?;
        let session_manager: IAudioSessionManager2 =
            device.Activate(CLSCTX_ALL, None).map_err(|e| format!("Activate failed: {e}"))?;
        let sessions = session_manager
            .GetSessionEnumerator()
            .map_err(|e| format!("GetSessionEnumerator failed: {e}"))?;
        let count = sessions.GetCount().map_err(|e| format!("GetCount failed: {e}"))?;
        for i in 0..count {
            let control = sessions.GetSession(i).map_err(|e| format!("GetSession failed: {e}"))?;
            let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
                continue;
            };
            let session_pid = control2.GetProcessId().unwrap_or(0);
            if session_pid == pid {
                let volume: ISimpleAudioVolume =
                    control2.cast().map_err(|e| format!("cast to ISimpleAudioVolume failed: {e}"))?;
                return f(&volume).map_err(|e| format!("volume operation failed: {e}"));
            }
        }
    }
    Err(format!("no audio session found for pid {pid}"))
}

// One-shot fetch for the frontend to paint immediately on hover, rather
// than waiting for the next ~2s watcher tick.
#[tauri::command]
pub fn spotify_get_state() -> Result<Option<NowPlaying>, String> {
    get_now_playing_inner()
}

#[derive(Serialize, Clone)]
pub struct AudioSessionInfo {
    pid: u32,
    name: String,
    volume: f32,
    muted: bool,
}

// Looks up a running process's exe name by pid — GetDisplayName() on most
// app's audio sessions is left empty (Windows itself falls back to the exe
// name in its own Volume Mixer for exactly this reason), so this is used
// directly rather than dealing with GetDisplayName's caller-frees-the-
// string COM allocation semantics for a value most sessions leave blank
// anyway.
fn find_process_name(pid: u32) -> Option<String> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = None;
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                if entry.th32ProcessID == pid {
                    let end = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                    let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
                    found = Some(name);
                    break;
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
        found
    }
}

fn strip_exe_suffix(name: &str) -> String {
    name.strip_suffix(".exe").or_else(|| name.strip_suffix(".EXE")).unwrap_or(name).to_string()
}

// Lists every active per-app volume session on the default output device —
// the same list Windows' own Volume Mixer shows — rather than just a
// single system-master slider, so the panel can adjust ANY currently
// playing app's volume, not only Spotify's. Fetched fresh on every hover
// reveal (see spotify.js) rather than kept live-synced, since the set of
// running apps changes independently of anything this app watches.
#[tauri::command]
pub fn list_audio_sessions() -> Result<Vec<AudioSessionInfo>, String> {
    ensure_com_initialized();
    let mut out = Vec::new();
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| format!("CoCreateInstance failed: {e}"))?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eMultimedia)
            .map_err(|e| format!("GetDefaultAudioEndpoint failed: {e}"))?;
        let session_manager: IAudioSessionManager2 =
            device.Activate(CLSCTX_ALL, None).map_err(|e| format!("Activate failed: {e}"))?;
        let sessions = session_manager
            .GetSessionEnumerator()
            .map_err(|e| format!("GetSessionEnumerator failed: {e}"))?;
        let count = sessions.GetCount().map_err(|e| format!("GetCount failed: {e}"))?;
        for i in 0..count {
            let Ok(control) = sessions.GetSession(i) else { continue };
            let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };
            // Skip the synthetic "system sounds" session — it's not a real
            // app and Windows' own mixer hides it from the per-app list too.
            // IsSystemSoundsSession returns a raw HRESULT (S_OK = is one,
            // S_FALSE = isn't) rather than a Result — both are "success"
            // HRESULTs (non-negative), so `.is_ok()`-style success checks
            // can't tell them apart; comparing directly against S_OK is the
            // only correct check (using is_ok() here was silently treating
            // EVERY session as the system sounds session, which is why the
            // list only ever showed the synthetic Master row).
            if control2.IsSystemSoundsSession() == S_OK {
                continue;
            }
            let pid = control2.GetProcessId().unwrap_or(0);
            if pid == 0 {
                continue;
            }
            let Some(name) = find_process_name(pid) else { continue };
            let Ok(volume) = control2.cast::<ISimpleAudioVolume>() else { continue };
            let level = volume.GetMasterVolume().unwrap_or(0.0);
            let muted = volume.GetMute().map(|b| b.as_bool()).unwrap_or(false);
            out.push(AudioSessionInfo { pid, name: strip_exe_suffix(&name), volume: level, muted });
        }
    }
    Ok(out)
}

#[tauri::command]
pub fn set_session_volume(pid: u32, level: f32) -> Result<(), String> {
    let clamped = level.clamp(0.0, 1.0);
    with_session_by_pid(pid, |v| unsafe { v.SetMasterVolume(clamped, std::ptr::null()) })
}

#[tauri::command]
pub fn set_session_muted(pid: u32, muted: bool) -> Result<(), String> {
    with_session_by_pid(pid, |v| unsafe { v.SetMute(muted, std::ptr::null()) })
}

// A general system-wide input/output audio panel (speaker + mic, mute +
// volume) rather than a Discord-specific one — the user explicitly wanted
// this NOT scoped to one app's session (unlike Spotify's volume above),
// since it should always work regardless of whether Discord/whatever app
// currently holds an open capture session. This targets the OS's default
// render/capture ENDPOINT directly (IAudioEndpointVolume), the same thing
// Windows' own volume flyout and mic mute button control — same effect as
// clicking those, just from inside the mascot.
fn with_default_endpoint_volume<F, T>(flow: EDataFlow, f: F) -> Result<T, String>
where
    F: FnOnce(&IAudioEndpointVolume) -> windows::core::Result<T>,
{
    ensure_com_initialized();
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| format!("CoCreateInstance failed: {e}"))?;
        let device = enumerator
            .GetDefaultAudioEndpoint(flow, eMultimedia)
            .map_err(|e| format!("GetDefaultAudioEndpoint failed: {e}"))?;
        let endpoint_volume: IAudioEndpointVolume =
            device.Activate(CLSCTX_ALL, None).map_err(|e| format!("Activate failed: {e}"))?;
        f(&endpoint_volume).map_err(|e| format!("endpoint volume operation failed: {e}"))
    }
}

#[tauri::command]
pub fn system_speaker_get_volume() -> Result<f32, String> {
    with_default_endpoint_volume(eRender, |v| unsafe { v.GetMasterVolumeLevelScalar() })
}

#[tauri::command]
pub fn system_speaker_set_volume(level: f32) -> Result<(), String> {
    let clamped = level.clamp(0.0, 1.0);
    with_default_endpoint_volume(eRender, |v| unsafe { v.SetMasterVolumeLevelScalar(clamped, std::ptr::null()) })
}

#[tauri::command]
pub fn system_speaker_get_muted() -> Result<bool, String> {
    with_default_endpoint_volume(eRender, |v| unsafe { Ok(v.GetMute()?.as_bool()) })
}

#[tauri::command]
pub fn system_speaker_set_muted(muted: bool) -> Result<(), String> {
    with_default_endpoint_volume(eRender, |v| unsafe { v.SetMute(muted, std::ptr::null()) })
}


#[tauri::command]
pub fn system_mic_get_muted() -> Result<bool, String> {
    with_default_endpoint_volume(eCapture, |v| unsafe { Ok(v.GetMute()?.as_bool()) })
}

#[tauri::command]
pub fn system_mic_set_muted(muted: bool) -> Result<(), String> {
    with_default_endpoint_volume(eCapture, |v| unsafe { v.SetMute(muted, std::ptr::null()) })
}

// Temporary risk-spike probe (see plan doc) — wired to a tray menu item so
// it can be triggered manually with a real Spotify session playing, before
// building the rest of the feature on top of an unverified API surface.
pub fn probe_spotify(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        match get_now_playing_inner() {
            Ok(Some(np)) => append_debug_log(
                &app,
                &format!("probe: OK — title={:?} artist={:?} is_playing={}", np.title, np.artist, np.is_playing),
            ),
            Ok(None) => append_debug_log(&app, "probe: OK — no Spotify session found (is Spotify open and playing?)"),
            Err(e) => append_debug_log(&app, &format!("probe: FAILED — {e}")),
        }
    });
}

pub fn start_spotify_watcher(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let last: Mutex<Option<Option<NowPlaying>>> = Mutex::new(None);
        loop {
            match get_now_playing_inner() {
                Ok(current) => {
                    let mut last_guard = last.lock().unwrap();
                    let changed = last_guard.as_ref() != Some(&current);
                    if changed {
                        let _ = app.emit("spotify-now-playing", current.clone());
                        *last_guard = Some(current);
                    }
                }
                Err(e) => {
                    append_debug_log(&app, &format!("watcher: poll failed — {e}"));
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}
