// The places a search question is asked.
//
// Every one of these is free, needs no account and no API key, and was checked
// against the live service before being included. That constraint is the whole
// design: a source that needs the user to sign up somewhere is a source most
// users will never turn on, so the tool would do nothing out of the box.
//
// What it costs is generality. None of these is a general web index — the two
// that exist (Google, Bing) sell access, and the one free-by-scraping option
// (DuckDuckGo) serves a CAPTCHA after a single request, measured. So instead of
// one broad source there are several narrow ones, fused by rank in search.rs.
// A question about Rust lifetimes is answered well by three of them; a question
// about yesterday's news by none, and the tool says so rather than inventing.

use serde_json::Value;

use super::SearchHit;

/// Per-source timeout. Short on purpose: the sources run in parallel and the
/// slowest one decides when the user sees an answer, so a source having a bad
/// day should drop out rather than hold up the five that answered.
const SOURCE_TIMEOUT_SECS: u64 = 6;

/// Results taken from any one source before fusion.
const PER_SOURCE: usize = 5;

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(SOURCE_TIMEOUT_SECS))
        // Several of these APIs reject or throttle a request with no
        // User-Agent (crates.io answers 403). A plain, honest identifier is
        // what their docs ask for, and unlike the page fetcher there is no
        // per-site privacy cost here: these are APIs, not someone's blog.
        .user_agent("Widget (personal desktop assistant)")
        .build()
        .map_err(|e| e.to_string())
}

fn get_json(url: &str) -> Result<Value, String> {
    let response = client()?
        .get(url)
        .header("Accept", "application/json")
        .send()
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    response.json().map_err(|e| e.to_string())
}

fn encode(query: &str) -> String {
    // Percent-encoding by hand rather than pulling in a crate for it: the set
    // that must be escaped in a query string is small and fixed.
    let mut out = String::with_capacity(query.len() * 3);
    for byte in query.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Strips the HTML some APIs put inside their snippets (Wikipedia marks the
/// matched words with `<span class="searchmatch">`).
fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut inside = false;
    for ch in text.chars() {
        match ch {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Marginalia — an independent index, and the only general-web source here.
///
/// Measured: eight rapid queries in a row all answered, where DuckDuckGo
/// served a CAPTCHA after one. It is small and deliberately favours the
/// non-commercial web, so it finds essays and documentation well and product
/// pages poorly. The query goes in the PATH, where `+` is a literal plus and
/// only percent-encoding means space.
pub fn marginalia(query: &str) -> Result<Vec<SearchHit>, String> {
    let body = get_json(&format!(
        "https://api.marginalia.nu/public/search/{}",
        encode(query)
    ))?;
    Ok(body["results"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .take(PER_SOURCE)
        .filter_map(|hit| {
            Some(SearchHit {
                title: hit["title"].as_str()?.to_string(),
                url: hit["url"].as_str()?.to_string(),
                snippet: strip_tags(hit["description"].as_str().unwrap_or_default()),
                source: "Marginalia",
            })
        })
        .collect())
}

/// Wikipedia's own search, in one language edition.
pub fn wikipedia(query: &str, lang: &str) -> Result<Vec<SearchHit>, String> {
    let body = get_json(&format!(
        "https://{lang}.wikipedia.org/w/api.php?action=query&list=search&srsearch={}\
         &srlimit={PER_SOURCE}&srnamespace=0&format=json",
        encode(query)
    ))?;
    Ok(body["query"]["search"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|hit| {
            let title = hit["title"].as_str()?;
            Some(SearchHit {
                title: title.to_string(),
                url: format!(
                    "https://{lang}.wikipedia.org/wiki/{}",
                    encode(&title.replace(' ', "_"))
                ),
                snippet: strip_tags(hit["snippet"].as_str().unwrap_or_default()),
                source: "Wikipedia",
            })
        })
        .collect())
}

/// Stack Overflow. Keyless access is capped at 300 requests a day per IP,
/// which is plenty for one person but is the reason this is not called on
/// every keystroke anywhere.
pub fn stackoverflow(query: &str) -> Result<Vec<SearchHit>, String> {
    let body = get_json(&format!(
        "https://api.stackexchange.com/2.3/search/advanced?order=desc&sort=relevance\
         &q={}&site=stackoverflow&pagesize={PER_SOURCE}",
        encode(query)
    ))?;
    Ok(body["items"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|item| {
            let answered = item["is_answered"].as_bool().unwrap_or(false);
            let score = item["score"].as_i64().unwrap_or(0);
            Some(SearchHit {
                title: strip_tags(item["title"].as_str()?),
                url: item["link"].as_str()?.to_string(),
                snippet: format!(
                    "Stack Overflow question, score {score}, {}.",
                    if answered { "has an accepted answer" } else { "unanswered" }
                ),
                source: "Stack Overflow",
            })
        })
        .collect())
}

/// Hacker News via its Algolia index — discussion and first-hand experience,
/// which is a different thing from documentation and often the only place a
/// tradeoff is written down honestly.
pub fn hacker_news(query: &str) -> Result<Vec<SearchHit>, String> {
    let body = get_json(&format!(
        "https://hn.algolia.com/api/v1/search?query={}&hitsPerPage={PER_SOURCE}",
        encode(query)
    ))?;
    Ok(body["hits"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|hit| {
            let title = hit["title"]
                .as_str()
                .or_else(|| hit["story_title"].as_str())?;
            let id = hit["objectID"].as_str()?;
            let points = hit["points"].as_i64().unwrap_or(0);
            let comments = hit["num_comments"].as_i64().unwrap_or(0);
            Some(SearchHit {
                title: title.to_string(),
                // The discussion, not the linked article: the comments are
                // what this source is good for, and the article can be fetched
                // separately if it turns out to be the interesting part.
                url: format!("https://news.ycombinator.com/item?id={id}"),
                snippet: format!("Hacker News discussion, {points} points, {comments} comments."),
                source: "Hacker News",
            })
        })
        .collect())
}

/// crates.io, for questions about Rust packages.
pub fn crates_io(query: &str) -> Result<Vec<SearchHit>, String> {
    let body = get_json(&format!(
        "https://crates.io/api/v1/crates?q={}&per_page={PER_SOURCE}",
        encode(query)
    ))?;
    Ok(body["crates"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|krate| {
            let name = krate["name"].as_str()?;
            Some(SearchHit {
                title: format!(
                    "{name} {}",
                    krate["max_version"].as_str().unwrap_or_default()
                ),
                url: format!("https://crates.io/crates/{name}"),
                snippet: format!(
                    "{} ({} downloads)",
                    krate["description"].as_str().unwrap_or("Rust crate"),
                    krate["downloads"].as_i64().unwrap_or(0)
                ),
                source: "crates.io",
            })
        })
        .collect())
}

/// GitHub repositories. Runs unauthenticated when no token is configured,
/// which GitHub allows at a much lower rate — so a missing token quietly
/// costs recall rather than breaking the source.
pub fn github(query: &str, token: &str) -> Result<Vec<SearchHit>, String> {
    let mut request = client()?
        .get(format!(
            "https://api.github.com/search/repositories?q={}&per_page={PER_SOURCE}",
            encode(query)
        ))
        .header("Accept", "application/vnd.github+json");
    if !token.trim().is_empty() {
        request = request.bearer_auth(token.trim());
    }
    let response = request.send().map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let body: Value = response.json().map_err(|e| e.to_string())?;
    Ok(body["items"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|repo| {
            Some(SearchHit {
                title: repo["full_name"].as_str()?.to_string(),
                url: repo["html_url"].as_str()?.to_string(),
                snippet: format!(
                    "{} ({} stars)",
                    repo["description"].as_str().unwrap_or("GitHub repository"),
                    repo["stargazers_count"].as_i64().unwrap_or(0)
                ),
                source: "GitHub",
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The bug this prevents was measured: with `+` for the space, every
    // multi-word query against Marginalia came back with zero results, because
    // `+` only means space in a query string and this one goes in the path.
    #[test]
    fn spaces_are_percent_encoded_not_plus() {
        assert_eq!(encode("rust async"), "rust%20async");
        assert!(!encode("rust async").contains('+'));
    }

    #[test]
    fn encoding_survives_non_ascii() {
        assert_eq!(encode("gö"), "g%C3%B6");
        assert_eq!(encode("a&b=c"), "a%26b%3Dc");
    }

    #[test]
    fn tags_are_stripped_from_snippets() {
        assert_eq!(
            strip_tags("The <span class=\"searchmatch\">Model</span> Context Protocol"),
            "The Model Context Protocol"
        );
        assert_eq!(strip_tags("plain text"), "plain text");
    }

    #[test]
    fn stripping_also_collapses_whitespace() {
        assert_eq!(strip_tags("a  <b>b</b>\n  c"), "a b c");
    }

    /// Against the live services, so kept out of the normal run. Every source
    /// here is somebody else's API that can change its response shape without
    /// telling anyone, and the parsing above is the part that silently returns
    /// an empty list when it does.
    ///
    ///   cargo test -- --ignored --nocapture every_source_answers
    #[test]
    #[ignore]
    fn every_source_answers() {
        let checks: Vec<(&str, Box<dyn Fn() -> Result<Vec<SearchHit>, String>>)> = vec![
            ("marginalia", Box::new(|| marginalia("rust async"))),
            ("wikipedia/en", Box::new(|| wikipedia("model context protocol", "en"))),
            ("wikipedia/tr", Box::new(|| wikipedia("İstanbul", "tr"))),
            ("stackoverflow", Box::new(|| stackoverflow("rust lifetime"))),
            ("hacker news", Box::new(|| hacker_news("tauri"))),
            ("crates.io", Box::new(|| crates_io("serde"))),
            ("github", Box::new(|| github("tauri", ""))),
        ];
        let mut empty = Vec::new();
        for (name, run) in checks {
            // One retry, because Marginalia intermittently does not answer at
            // all: measured, 2 of ~15 identical requests hung past 8s while the
            // rest returned in under half a second. The real search path
            // handles this by letting the source drop out of that one query —
            // but a test that fails 1 run in 10 for a reason outside this
            // codebase is a test people learn to ignore.
            let result = run().or_else(|first| {
                println!("{name}: retrying after {first}");
                run()
            });
            match result {
                Ok(hits) if hits.is_empty() => {
                    println!("{name}: EMPTY");
                    empty.push(name);
                }
                Ok(hits) => println!(
                    "{name}: {} hits — {} | {}",
                    hits.len(),
                    hits[0].title.chars().take(48).collect::<String>(),
                    hits[0].url.chars().take(58).collect::<String>()
                ),
                Err(why) => {
                    println!("{name}: FAILED {why}");
                    empty.push(name);
                }
            }
        }
        assert!(empty.is_empty(), "sources returning nothing: {empty:?}");
    }
}
