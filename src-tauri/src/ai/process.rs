// Supervision of the local model servers this widget can start for the user
// (LLM, speech-to-text, text-to-speech). Nothing here is model-specific — it is
// only "is something answering on this port, and if not, spawn this command and
// remember the PID so we can stop exactly what we started".
//
// Lives on its own rather than inside llm.rs because all three of llm/stt/tts
// use it equally; speech.rs previously had to reach into llm.rs for it, which
// implied a dependency that does not exist.
use std::process::Command;
use std::sync::Mutex;

pub(crate) fn is_reachable(base_url: &str) -> bool {
    reqwest::blocking::Client::new()
        .get(base_url.trim_end_matches('/'))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .is_ok()
}

pub(crate) fn spawn_detached(command_line: &str) -> Option<u32> {
    let mut parts = command_line.split_whitespace();
    let program = parts.next()?;
    let mut cmd = Command::new(program);
    cmd.args(parts);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    cmd.spawn().ok().map(|child| child.id())
}

// Only starts anything if nothing is already answering on base_url — never
// spawns a duplicate server, and never touches a server the user started
// themselves outside the widget. pid_store is the caller's per-kind static
// (LLM/STT/TTS) so stop_tracked can later kill only what this widget spawned.
pub(crate) fn autostart_if_needed(base_url: &str, start_command: &str, pid_store: &'static Mutex<Option<u32>>) {
    if start_command.trim().is_empty() {
        return;
    }
    if is_reachable(base_url) {
        return;
    }
    if let Some(pid) = spawn_detached(start_command) {
        *pid_store.lock().unwrap() = Some(pid);
    }
}

pub(crate) fn stop_tracked(pid_store: &'static Mutex<Option<u32>>) {
    let Some(pid) = pid_store.lock().unwrap().take() else {
        return;
    };

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
}
