// Supervision of the local model servers this widget can start for the user
// (LLM, speech-to-text, text-to-speech, pictures). Nothing here is
// model-specific — it is only "is something answering on this port, and if not,
// spawn this command", plus the reverse for the tray's stop-and-quit.
//
// Lives on its own rather than inside llm.rs because all of llm/stt/tts/images
// use it equally; speech.rs previously had to reach into llm.rs for it, which
// implied a dependency that does not exist.
use std::process::Command;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    cmd.spawn().ok().map(|child| child.id())
}

// Only starts anything if nothing is already answering on base_url — never
// spawns a duplicate server, and never touches a server the user started
// themselves outside the widget.
pub(crate) fn autostart_if_needed(base_url: &str, start_command: &str) {
    if start_command.trim().is_empty() {
        return;
    }
    if is_reachable(base_url) {
        return;
    }
    spawn_detached(start_command);
}

/// The port `base_url` points at.
///
/// Only an explicit port counts. Falling back to the scheme's default would mean
/// a base_url written without one resolved to 80, and stop_local_server below
/// would then go looking for whatever unrelated process was serving that.
pub(crate) fn port_of(base_url: &str) -> Option<u16> {
    reqwest::Url::parse(base_url.trim()).ok()?.port()
}

/// Stops the local server answering at `base_url`, along with any child
/// processes it spawned (a loaded model usually runs in one).
///
/// The server is identified by which process is listening on its port, rather
/// than by remembering the PID we spawned — which is what this used to do, and
/// two separate things broke it:
///
/// 1. A server already running when the widget starts is deliberately not
///    spawned by us (see autostart_if_needed), so there was no PID to remember
///    and the tray's stop-and-quit silently did nothing. Measured: ollama.exe
///    survived every time. Worse, it was self-perpetuating — one abnormal widget
///    exit orphans the server, and every later run then finds the port already
///    answering and declines to adopt it, so the menu item never works again.
/// 2. Windows recycles PIDs. A remembered PID whose process has since exited can
///    name something else entirely by the time the user quits, and killing it
///    would take an unrelated process down with it.
///
/// The port has neither problem: whoever is listening on it *is* the server this
/// widget is configured to talk to, whoever started it. That does mean this stops
/// a server the user started by hand — which is what the one menu item calling it
/// says it does. Plain Quit still leaves everything running.
pub(crate) fn stop_local_server(base_url: &str) {
    let Some(port) = port_of(base_url) else {
        return;
    };
    let Some(pid) = listener_pid(port) else {
        return;
    };
    kill_tree(pid);
}

/// The PID listening on `port`, via netstat.
///
/// netstat rather than an IP Helper FFI binding because it needs no new crate and
/// no unsafe, and this runs once, on the way out.
#[cfg(windows)]
fn listener_pid(port: u16) -> Option<u32> {
    use std::os::windows::process::CommandExt;
    let output = Command::new("netstat")
        .args(["-a", "-n", "-o"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    // Lossy rather than strict: netstat prints in the console codepage, and a
    // stray non-UTF-8 byte in some unrelated line must not lose the whole table.
    parse_listener_pid(&String::from_utf8_lossy(&output.stdout), port)
}

// The rest of this widget is Windows-only anyway (GSMTC media control, WebView2,
// taskkill), so there is no second implementation to keep honest here. Returning
// None makes stop_local_server a no-op rather than a wrong guess.
#[cfg(not(windows))]
fn listener_pid(_port: u16) -> Option<u32> {
    None
}

/// Picks the listening TCP socket's owning PID out of `netstat -ano` output.
///
/// Split out from listener_pid so the field-picking is testable without a live
/// socket. Every guard below is load-bearing — see the tests.
fn parse_listener_pid(netstat_output: &str, port: u16) -> Option<u32> {
    for line in netstat_output.lines() {
        let mut fields = line.split_whitespace();
        let (Some(proto), Some(local), Some(foreign), Some(_state), Some(pid)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            // UDP rows carry no state and so no fifth field, and the header and
            // blank lines carry nothing at all.
            continue;
        };
        if !proto.eq_ignore_ascii_case("tcp") {
            continue;
        }
        // Listening sockets only. A client's connection to the same port shows up
        // as ESTABLISHED against the CLIENT's PID — this widget's own, most of the
        // time — and killing that would quit the widget and leave the server up.
        //
        // Told apart by the wildcard foreign address rather than by the state
        // column, because netstat translates the state word and this machine
        // happening to run an English Windows is not something to depend on: the
        // failure mode of a missed match here is the silent no-op that this whole
        // function exists to fix. The addresses are numeric in every locale, and
        // only a listener has no peer.
        if !is_wildcard_endpoint(foreign) {
            continue;
        }
        // rsplit so `[::1]:8090` yields the port and not the address, and so
        // `:18090` cannot match 8090 the way a suffix comparison would.
        let listening_port = local.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok());
        if listening_port != Some(port) {
            continue;
        }
        return pid.parse().ok();
    }
    None
}

/// Whether a netstat address column is the "no peer" placeholder a listening
/// socket shows: `0.0.0.0:0` for IPv4, `[::]:0` for IPv6, `*:*` for UDP.
fn is_wildcard_endpoint(address: &str) -> bool {
    matches!(address, "0.0.0.0:0" | "[::]:0" | "*:*")
}

fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // /T for the tree: ollama keeps the loaded model in a child process, and
        // killing only the parent leaves that child holding the GPU.
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

#[cfg(test)]
mod tests {
    use super::*;

    // Shaped like real `netstat -ano` output, including the two-space indent and
    // the header block, so the parser is exercised against what it actually gets.
    const NETSTAT: &str = "\r
Active Connections\r
\r
  Proto  Local Address          Foreign Address        State           PID\r
  TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       1204\r
  TCP    127.0.0.1:18090        0.0.0.0:0              LISTENING       7777\r
  TCP    127.0.0.1:11434        0.0.0.0:0              LISTENING       24600\r
  TCP    127.0.0.1:11434        127.0.0.1:52133        ESTABLISHED     33040\r
  TCP    [::1]:8090             [::]:0                 LISTENING       9110\r
  UDP    127.0.0.1:53           *:*                                    1500\r
";

    #[test]
    fn finds_the_listening_owner() {
        assert_eq!(parse_listener_pid(NETSTAT, 11434), Some(24600));
    }

    #[test]
    fn ignores_established_connections_to_the_same_port() {
        // 33040 is a client talking TO the server — most often this widget. If a
        // suffix match or a missing state check let it through, stop-and-quit
        // would kill the widget and leave the server up.
        assert_ne!(parse_listener_pid(NETSTAT, 11434), Some(33040));
    }

    #[test]
    fn does_not_confuse_a_port_that_ends_with_the_same_digits() {
        assert_eq!(parse_listener_pid(NETSTAT, 8090), Some(9110));
        assert_eq!(parse_listener_pid(NETSTAT, 18090), Some(7777));
    }

    #[test]
    fn finds_the_listener_on_a_localised_windows() {
        // Same table with the state column in Turkish, which is what netstat
        // prints under a Turkish display language. The PID must still be found:
        // matching the English word would make stop-and-quit a silent no-op on
        // any machine that is not set to English.
        let localised = NETSTAT
            .replace("LISTENING", "DİNLENİYOR")
            .replace("ESTABLISHED", "KURULDU");
        assert_eq!(parse_listener_pid(&localised, 11434), Some(24600));
    }

    #[test]
    fn no_listener_is_not_an_error() {
        assert_eq!(parse_listener_pid(NETSTAT, 4242), None);
        assert_eq!(parse_listener_pid("", 11434), None);
    }

    #[test]
    fn reads_the_port_out_of_a_base_url() {
        assert_eq!(port_of("http://localhost:11434/v1"), Some(11434));
        assert_eq!(port_of("  http://127.0.0.1:8090  "), Some(8090));
    }

    #[test]
    fn a_base_url_without_a_port_names_no_server() {
        // Not Some(80) — see port_of. A default-port guess would send kill_tree
        // after whatever happened to be serving HTTP on this machine.
        assert_eq!(port_of("http://localhost/v1"), None);
        assert_eq!(port_of("not a url"), None);
    }
}
