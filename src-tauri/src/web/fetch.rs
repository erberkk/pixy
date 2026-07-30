// Fetching a page off the internet and turning it into text a model can read.
//
// The hard part here is not the HTTP. It is that the address being fetched was
// chosen by a language model, which in turn was reading a web page that anyone
// could have written — so "fetch this URL" has to be treated as an instruction
// from a stranger. That is what `is_public_url` below is for, and why it runs
// again on every redirect hop.

use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

/// Hard ceiling on how much of one page is read.
///
/// Two jobs: a hostile server can stream forever, and everything read here ends
/// up in a prompt, so the size of a page is also the size of the injection
/// surface. 512 KB of HTML is far more than any article's worth of text.
const MAX_FETCH_BYTES: usize = 512 * 1024;

/// Redirect hops followed. Each one is re-validated, which is the reason the
/// chain is walked by hand instead of letting reqwest follow it.
const MAX_REDIRECTS: usize = 4;

const FETCH_TIMEOUT_SECS: u64 = 10;

/// A browser-shaped User-Agent.
///
/// Deliberately not identifying this app: a page fetched because the user asked
/// about it is a single request that a browser would also have made, and naming
/// the widget would put a record of who runs it in the logs of every site
/// visited. Volume-wise this is nothing like a crawler, which is where the
/// convention of identifying yourself actually applies.
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

pub struct FetchedPage {
    /// Where the content actually came from, after redirects — not the URL
    /// asked for. Provenance shown to the user has to be the real one.
    pub url: String,
    pub title: String,
    pub text: String,
    pub truncated: bool,
}

/// Whether an address is somewhere on the public internet.
///
/// This is a security boundary, not a convenience check, and it errs toward
/// refusing: anything that cannot be confidently placed on the public internet
/// is rejected. The cost of a false refusal is one page the model cannot read.
/// The cost of a false accept is the model reaching a service on this machine —
/// and on this machine that means Ollama on 11434, the speech servers on
/// 8090/8091, and the widget's own event server, none of which expect a
/// request from a web page.
///
/// Note that `ai::recall::is_local_endpoint` answers a similar-looking question
/// and is deliberately NOT reused: it classifies a URL the *user* typed into
/// settings, so it never resolves DNS. Here the hostname is attacker-chosen,
/// and a name that resolves to 127.0.0.1 is the obvious way past a check that
/// only looks at the text of the host.
pub fn is_public_url(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| format!("Not a usable URL: {url}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("Refusing to fetch a {other}: URL.")),
    }
    let Some(host) = parsed.host_str() else {
        return Err("That URL has no host.".to_string());
    };

    // A literal address is checked as written. Resolving it would be pointless
    // and would let a DNS server decide the answer for an address that has one
    // already.
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        return if is_public_ip(&ip) {
            Ok(())
        } else {
            Err(format!("Refusing to fetch a non-public address ({ip})."))
        };
    }

    // A name: every address it answers with must be public. Checking only the
    // first would let a hostile resolver return a public address alongside
    // 127.0.0.1 and rely on which one gets tried.
    let port = parsed.port_or_known_default().unwrap_or(80);
    let resolved = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("Could not resolve {host}: {e}"))?;
    let mut any = false;
    for address in resolved {
        any = true;
        if !is_public_ip(&address.ip()) {
            return Err(format!(
                "Refusing to fetch {host}: it resolves to the non-public address {}.",
                address.ip()
            ));
        }
    }
    if !any {
        return Err(format!("{host} resolved to no addresses."));
    }
    Ok(())
}

fn is_public_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        // An IPv4 address written as IPv6 is still that IPv4 address, and
        // ::ffff:127.0.0.1 is the tidiest way past a v4-only check.
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public_v4(&v4),
            None => is_public_v6(v6),
        },
    }
}

fn is_public_v4(ip: &Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_private()            // 10/8, 172.16/12, 192.168/16
        || ip.is_loopback()      // 127/8
        || ip.is_link_local()    // 169.254/16 — cloud metadata lives here
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || a == 0                // "this network"
        || (a == 100 && (64..=127).contains(&b))  // carrier-grade NAT
        || a >= 240)             // reserved, includes 255/8
}

fn is_public_v6(ip: &Ipv6Addr) -> bool {
    // std's is_unique_local / is_unicast_link_local are still unstable, so the
    // two ranges that matter are matched on their prefixes directly.
    let first = ip.segments()[0];
    !(ip.is_loopback()
        || ip.is_unspecified()
        || (first & 0xfe00) == 0xfc00   // fc00::/7  unique local
        || (first & 0xffc0) == 0xfe80)  // fe80::/10 link local
}

/// Fetches one page and returns its readable text.
///
/// `max_chars` is applied to the extracted text, not the download: a page is
/// read up to the byte cap either way, and cutting the text afterwards keeps
/// the extraction working on a complete document.
pub fn fetch_text(url: &str, max_chars: usize) -> Result<FetchedPage, String> {
    is_public_url(url)?;

    let client = reqwest::blocking::Client::builder()
        // Followed by hand below so each hop can be re-validated. reqwest's own
        // policy would happily follow a 302 into 127.0.0.1 having only ever
        // checked the address that was asked for.
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| format!("Could not start the request: {e}"))?;

    let mut current = url.to_string();
    let mut response = None;
    for _ in 0..=MAX_REDIRECTS {
        let attempt = client
            .get(&current)
            .header("Accept", "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.8")
            .send()
            .map_err(|e| format!("Could not reach {current}: {e}"))?;

        if attempt.status().is_redirection() {
            let location = attempt
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            if location.is_empty() {
                return Err(format!("{current} redirected without saying where."));
            }
            // Relative redirects are normal, so the hop is resolved against the
            // URL it came from before being checked.
            let next = reqwest::Url::parse(&current)
                .and_then(|base| base.join(&location))
                .map_err(|_| format!("{current} redirected somewhere unusable: {location}"))?;
            current = next.to_string();
            is_public_url(&current)?;
            continue;
        }

        if !attempt.status().is_success() {
            return Err(format!("{current} answered {}.", attempt.status()));
        }
        response = Some(attempt);
        break;
    }

    let Some(response) = response else {
        return Err(format!("{url} redirected more than {MAX_REDIRECTS} times."));
    };

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    let final_url = response.url().to_string();

    // `take` rather than a check on Content-Length: a server is free to lie
    // about the length, or omit it entirely and stream.
    let mut body = Vec::new();
    response
        .take(MAX_FETCH_BYTES as u64)
        .read_to_end(&mut body)
        .map_err(|e| format!("Could not read {final_url}: {e}"))?;
    let downloaded_all = body.len() < MAX_FETCH_BYTES;
    // Lossy on purpose. A page in an encoding we guessed wrong about should
    // come back mangled but readable, not as an error the model can do nothing
    // with.
    let raw = String::from_utf8_lossy(&body);

    let (title, text) = if content_type.contains("html") || raw.trim_start().starts_with('<') {
        super::extract::readable_text(&raw)
    } else {
        (String::new(), super::extract::tidy(&raw))
    };

    if text.trim().is_empty() {
        return Err(format!(
            "{final_url} was reached but had no readable text — it is most likely a page \
             that builds itself with JavaScript, which this cannot run."
        ));
    }

    let mut text = text;
    let mut truncated = !downloaded_all;
    if text.chars().count() > max_chars {
        text = text.chars().take(max_chars).collect();
        truncated = true;
    }

    Ok(FetchedPage {
        url: final_url,
        title,
        text,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every one of these is a way in to something listening on this machine.
    #[test]
    fn local_and_reserved_addresses_are_refused() {
        for url in [
            "http://127.0.0.1:11434/api/tags",   // Ollama
            "http://localhost:8090/",            // speech-to-text
            "http://[::1]:8091/",                // text-to-speech over IPv6
            "http://169.254.169.254/latest/meta-data/", // cloud metadata
            "http://10.0.0.5/",
            "http://192.168.1.1/",
            "http://172.16.0.1/",
            "http://0.0.0.0/",
            "http://[::ffff:127.0.0.1]/",        // IPv4 loopback wearing IPv6
            "http://[fc00::1]/",                 // unique local
            "http://[fe80::1]/",                 // link local
            "http://100.64.0.1/",                // carrier-grade NAT
        ] {
            assert!(
                is_public_url(url).is_err(),
                "should have refused {url}"
            );
        }
    }

    #[test]
    fn non_http_schemes_are_refused() {
        for url in [
            "file:///C:/Users/erber/.ssh/id_rsa",
            "ftp://example.com/",
            "data:text/html,<script>alert(1)</script>",
        ] {
            assert!(is_public_url(url).is_err(), "should have refused {url}");
        }
    }

    #[test]
    fn a_public_literal_address_is_allowed() {
        assert!(is_public_url("https://1.1.1.1/").is_ok());
        assert!(is_public_url("https://[2606:4700:4700::1111]/").is_ok());
    }

    // The check must survive a hostname, not only a literal — this is the
    // resolution path, and the one a text-only check would get wrong.
    #[test]
    fn a_hostname_that_resolves_to_loopback_is_refused() {
        // localhost is the case that exists on every machine without needing a
        // hostile DNS server to demonstrate it.
        assert!(is_public_url("http://localhost/").is_err());
    }

    #[test]
    fn a_url_with_no_host_is_refused() {
        assert!(is_public_url("http:///nowhere").is_err());
        assert!(is_public_url("not a url at all").is_err());
    }

    /// Against the real internet, so excluded from the normal run — a suite
    /// that fails because the network is down is a suite people stop trusting.
    /// Run deliberately with:
    ///   cargo test -- --ignored --nocapture reads_real_pages
    ///
    /// Worth keeping despite that: every synthetic HTML test above can pass
    /// while real pages come back as navigation soup, because real pages look
    /// nothing like hand-written fixtures.
    #[test]
    #[ignore]
    fn reads_real_pages() {
        for url in [
            "https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html",
            "https://en.wikipedia.org/wiki/Rust_(programming_language)",
            "https://v2.tauri.app/develop/calling-rust/",
            "https://news.ycombinator.com/item?id=1",
        ] {
            match fetch_text(url, 6000) {
                Ok(page) => {
                    println!("\n=== {url}\n  title: {}\n  chars: {} (truncated: {})",
                        page.title, page.text.chars().count(), page.truncated);
                    println!("  first 160: {}",
                        page.text.chars().take(160).collect::<String>());
                    assert!(page.text.len() > 200, "suspiciously little text from {url}");
                }
                Err(why) => panic!("{url} failed: {why}"),
            }
        }
    }

    #[test]
    fn classification_of_individual_addresses() {
        assert!(is_public_ip(&"8.8.8.8".parse().unwrap()));
        assert!(is_public_ip(&"2606:4700::1".parse().unwrap()));
        assert!(!is_public_ip(&"127.0.0.1".parse().unwrap()));
        assert!(!is_public_ip(&"::1".parse().unwrap()));
        assert!(!is_public_ip(&"240.0.0.1".parse().unwrap()));
        assert!(!is_public_ip(&"255.255.255.255".parse().unwrap()));
    }
}
