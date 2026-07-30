// Asking every source at once, merging what comes back, and reading the best
// of it.
//
// Three decisions carry most of the quality here, and each is there because the
// obvious alternative measurably fails:
//
// 1. The sources run in PARALLEL and are merged by RANK. Their scores are not
//    comparable — a Stack Overflow vote count, a Wikipedia relevance number and
//    Marginalia's quality float share no scale — so anything that adds or
//    weights them is inventing a conversion. Reciprocal rank fusion needs no
//    such invention, and it is already implemented and tested for recall.
// 2. The top results are FETCHED, not just listed. A snippet is ~200 characters
//    of description; the page is thousands of characters of the actual answer.
//    Handed only snippets, a small model describes the search results ("there
//    are some links about this") instead of answering from them.
// 3. An extract that shares no words with the question is DROPPED. A fetch that
//    returns bytes but none of the user's words is a cookie banner or a
//    consent wall, and is worse than nothing: it looks like content, so the
//    model tries to answer from it.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::{sources, SearchHit};

/// How long the whole search may take before it answers with what it has.
///
/// Everything after this is a second full model round, so the user is already
/// waiting on the slowest part. Sources have their own shorter timeout; this is
/// the backstop for several of them being slow at once.
const SEARCH_BUDGET: Duration = Duration::from_secs(9);

/// How long fetching the promising results may take, in total and in parallel.
const READ_BUDGET: Duration = Duration::from_secs(12);

/// Results whose pages are opened and read.
const PAGES_TO_READ: usize = 3;

/// Characters kept from any one page, and from all of them together. Bounded
/// because every tool result is re-sent on each following round of the loop.
const CHARS_PER_PAGE: usize = 2500;
const CHARS_TOTAL: usize = 7000;

/// Links listed for provenance, whether or not they were read.
const LINKS_LISTED: usize = 6;

/// Shortest token that counts as a content word.
///
/// Three characters removes most cross-language stopwords (the, and, for, ve,
/// bir, ile) without needing a per-language list — which would only work for
/// the languages someone remembered to add.
const MIN_TOKEN_LEN: usize = 3;

const CACHE_TTL: Duration = Duration::from_secs(15 * 60);

/// Repeated searches inside one run of the app answer from here.
///
/// In memory rather than on disk deliberately: the value is in a model asking
/// the same thing twice in one conversation (common — it rewords and retries),
/// not in results surviving a restart, where staleness would start to matter.
/// It also protects Stack Overflow's 300-a-day keyless budget from a loop.
type Cached = (Instant, String, Vec<SearchHit>);
static CACHE: OnceLock<Mutex<HashMap<String, Cached>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, Cached>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Splits text into lowercase content tokens.
fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= MIN_TOKEN_LEN)
        .map(|t| t.to_string())
        .collect()
}

/// How many distinct question words appear in `text`.
fn overlap(text: &str, question: &[String]) -> usize {
    if question.is_empty() {
        return 1; // nothing to check against — do not reject on no evidence
    }
    let present: std::collections::HashSet<String> = tokens(text).into_iter().collect();
    question.iter().filter(|word| present.contains(*word)).count()
}

/// Guesses which Wikipedia edition to also ask, alongside English.
///
/// A character test, not language detection: the letters below appear in
/// Turkish and essentially not in English, so a Turkish question reaches the
/// Turkish edition. It knows only this one language beyond English, which is
/// the honest limit — adding more means adding their letters here, and no
/// heuristic this small will ever place a language whose script it shares with
/// another. English is always asked regardless, so a wrong guess costs nothing
/// but one extra request.
fn second_wikipedia(query: &str) -> Option<&'static str> {
    query
        .chars()
        .any(|c| matches!(c, 'ğ' | 'Ğ' | 'ş' | 'Ş' | 'ı' | 'İ'))
        .then_some("tr")
}

/// Runs the search.
///
/// Returns what the model should read, and separately the results that were
/// listed — the UI shows those under the reply so the user can check where an
/// answer came from without taking the model's word for it.
pub fn search(app: &tauri::AppHandle, query: &str) -> (String, Vec<SearchHit>) {
    let query = query.trim();
    if query.is_empty() {
        return (
            "That search arrived with no query. Ask the user what to look for.".to_string(),
            Vec::new(),
        );
    }

    let key = query.to_lowercase();
    if let Some((stored, answer, hits)) = cache().lock().ok().and_then(|c| c.get(&key).cloned()) {
        if stored.elapsed() < CACHE_TTL {
            return (answer, hits);
        }
    }

    let hits = gather(app, query);
    if hits.is_empty() {
        return (
            super::failure_notice(
                &format!("A web search for \"{query}\""),
                "no free source returned anything for it. These sources cover \
                 documentation, encyclopedia articles, programming questions and \
                 discussion — they do not cover the commercial web, current news or \
                 prices.",
            ),
            Vec::new(),
        );
    }

    let listed: Vec<SearchHit> = hits.iter().take(LINKS_LISTED).cloned().collect();
    let answer = read_and_format(query, hits);
    if let Ok(mut cache) = cache().lock() {
        cache.insert(key, (Instant::now(), answer.clone(), listed.clone()));
    }
    (answer, listed)
}

/// Asks every source at once and merges the answers by rank.
fn gather(app: &tauri::AppHandle, query: &str) -> Vec<SearchHit> {
    let token = crate::config::read_config(app).github_token.unwrap_or_default();

    // Boxed closures so the list of sources is data rather than repeated
    // spawn/collect code — adding one is a line here.
    type Source = (&'static str, Box<dyn FnOnce() -> Result<Vec<SearchHit>, String> + Send>);
    let query_for = |q: &str| q.to_string();
    let mut jobs: Vec<Source> = vec![
        ("Marginalia", {
            let q = query_for(query);
            Box::new(move || sources::marginalia(&q))
        }),
        ("Wikipedia", {
            let q = query_for(query);
            Box::new(move || sources::wikipedia(&q, "en"))
        }),
        ("Stack Overflow", {
            let q = query_for(query);
            Box::new(move || sources::stackoverflow(&q))
        }),
        ("Hacker News", {
            let q = query_for(query);
            Box::new(move || sources::hacker_news(&q))
        }),
        ("crates.io", {
            let q = query_for(query);
            Box::new(move || sources::crates_io(&q))
        }),
        ("GitHub", {
            let q = query_for(query);
            Box::new(move || sources::github(&q, &token))
        }),
    ];
    if let Some(lang) = second_wikipedia(query) {
        let q = query_for(query);
        jobs.push((
            "Wikipedia",
            Box::new(move || sources::wikipedia(&q, lang)),
        ));
    }

    let (sender, receiver) = mpsc::channel();
    let count = jobs.len();
    for (index, (name, job)) in jobs.into_iter().enumerate() {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let result = job();
            // A source that has already been given up on has a closed channel;
            // failing to send is the normal end of that race, not an error.
            let _ = sender.send((index, name, result));
        });
    }
    drop(sender);

    // Threads are detached rather than joined: one hung source must not be able
    // to hold the answer past the budget, and abandoning it costs only the
    // request it was already making.
    let deadline = Instant::now() + SEARCH_BUDGET;
    let mut per_source: Vec<Vec<SearchHit>> = vec![Vec::new(); count];
    for _ in 0..count {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok((index, _name, Ok(hits))) => per_source[index] = hits,
            Ok((_, name, Err(why))) => {
                // Not surfaced to the user: one source being down while five
                // answer is not something to interrupt an answer for.
                eprintln!("web search: {name} failed: {why}");
            }
            Err(_) => break,
        }
    }

    merge(per_source)
}

/// Deduplicates by URL and orders by reciprocal rank fusion.
fn merge(per_source: Vec<Vec<SearchHit>>) -> Vec<SearchHit> {
    let mut all: Vec<SearchHit> = Vec::new();
    let mut position: HashMap<String, i64> = HashMap::new();
    let mut ranked_lists: Vec<Vec<i64>> = Vec::new();

    for hits in per_source {
        let mut ranked = Vec::new();
        for hit in hits {
            // Same page found by two sources: it keeps its first identity but
            // appears in both ranking lists, which is exactly the signal RRF
            // rewards.
            let id = match position.get(&hit.url) {
                Some(existing) => *existing,
                None => {
                    let id = all.len() as i64;
                    position.insert(hit.url.clone(), id);
                    all.push(hit);
                    id
                }
            };
            ranked.push(id);
        }
        ranked_lists.push(ranked);
    }

    let borrowed: Vec<&[i64]> = ranked_lists.iter().map(|l| l.as_slice()).collect();
    crate::ai::recall::fuse(&borrowed)
        .into_iter()
        .filter_map(|id| all.get(id as usize).cloned())
        .collect()
}

/// Reads the most promising results and builds what the model sees.
fn read_and_format(query: &str, hits: Vec<SearchHit>) -> String {
    let question = tokens(query);

    let (sender, receiver) = mpsc::channel();
    for (rank, hit) in hits.iter().take(PAGES_TO_READ).enumerate() {
        let sender = sender.clone();
        let url = hit.url.clone();
        std::thread::spawn(move || {
            let _ = sender.send((rank, super::fetch::fetch_text(&url, CHARS_PER_PAGE)));
        });
    }
    drop(sender);

    let deadline = Instant::now() + READ_BUDGET;
    let mut pages: Vec<Option<super::fetch::FetchedPage>> = (0..PAGES_TO_READ.min(hits.len()))
        .map(|_| None)
        .collect();
    for _ in 0..pages.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok((rank, Ok(page))) => pages[rank] = Some(page),
            Ok((rank, Err(why))) => eprintln!("web search: could not read result {rank}: {why}"),
            Err(_) => break,
        }
    }

    let mut body = String::new();
    let mut used = 0usize;
    let mut read_any = false;
    for (rank, page) in pages.into_iter().enumerate() {
        let Some(page) = page else { continue };
        if overlap(&page.text, &question) == 0 {
            eprintln!(
                "web search: dropping result {} — {} chars with no question words in them",
                rank + 1,
                page.text.len()
            );
            continue;
        }
        // Counted in characters, not bytes, to match CHARS_PER_PAGE and the cap
        // fetch_text applied — mixing the two would make the real budget depend
        // on the language, tightening it for anything non-ASCII.
        let length = page.text.chars().count();
        if used + length > CHARS_TOTAL {
            break;
        }
        used += length;
        read_any = true;
        let heading = if page.title.is_empty() {
            page.url.clone()
        } else {
            format!("{} ({})", page.title, page.url)
        };
        body.push_str(&format!("\n--- from {heading} ---\n{}\n", page.text));
    }

    let links: String = hits
        .iter()
        .take(LINKS_LISTED)
        .enumerate()
        .map(|(index, hit)| {
            format!(
                "{}. {} — {} [{}]\n   {}\n",
                index + 1,
                hit.title,
                hit.snippet,
                hit.source,
                hit.url
            )
        })
        .collect();

    if !read_any {
        // Links but nothing readable. Saying so explicitly is what stops a
        // small model from treating the list of titles as an answer.
        return format!(
            "A web search for \"{query}\" found these results, but none of their pages \
             could be read (they may need JavaScript, or refuse automated requests).\n\n\
             {links}\n\
             Tell the user you found these but could not read them, and offer to open one \
             if they pick it. Do NOT state facts about the topic that are not in the \
             titles above — nothing else was retrieved."
        );
    }

    format!(
        "{}\n\nOther results found for \"{query}\", for reference only:\n{links}\n\
         Answer the user's question from the page content above, and mention which \
         source it came from.",
        super::fence(&format!("a web search for \"{query}\""), body.trim())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(url: &str, title: &str) -> SearchHit {
        SearchHit {
            title: title.to_string(),
            url: url.to_string(),
            snippet: String::new(),
            source: "test",
        }
    }

    // The point of fusing: a result two sources both found should beat one that
    // only a single source ranked first.
    #[test]
    fn a_result_found_by_two_sources_outranks_one_found_by_one() {
        let merged = merge(vec![
            vec![hit("https://a", "A"), hit("https://shared", "Shared")],
            vec![hit("https://b", "B"), hit("https://shared", "Shared")],
        ]);
        assert_eq!(merged[0].url, "https://shared");
    }

    #[test]
    fn the_same_url_from_two_sources_appears_once() {
        let merged = merge(vec![
            vec![hit("https://same", "One")],
            vec![hit("https://same", "One")],
        ]);
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn a_source_that_returned_nothing_does_not_disturb_the_order() {
        let merged = merge(vec![
            vec![hit("https://a", "A"), hit("https://b", "B")],
            vec![],
        ]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].url, "https://a");
    }

    #[test]
    fn short_words_are_not_content_tokens() {
        assert_eq!(tokens("a of the rust"), vec!["the", "rust"]);
    }

    #[test]
    fn tokens_are_lowercased_and_split_on_punctuation() {
        assert_eq!(tokens("Rust's async/await!"), vec!["rust", "async", "await"]);
    }

    // The gate that drops consent walls: text with none of the question's words
    // in it scores zero.
    #[test]
    fn boilerplate_scores_no_overlap_with_the_question() {
        let question = tokens("rust ownership borrow checker");
        assert_eq!(
            overlap(
                "We value your privacy. This site uses cookies. Accept all. Manage choices.",
                &question
            ),
            0
        );
        assert!(overlap("Ownership is how Rust manages memory.", &question) >= 2);
    }

    // An empty question must not cause everything to be rejected.
    #[test]
    fn nothing_to_compare_against_passes_rather_than_rejects() {
        assert_eq!(overlap("any text at all", &[]), 1);
    }

    #[test]
    fn a_turkish_question_also_asks_the_turkish_wikipedia() {
        assert_eq!(second_wikipedia("Türkiye'nin nüfusu kaçtır"), Some("tr"));
        assert_eq!(second_wikipedia("ışık hızı"), Some("tr"));
        assert_eq!(second_wikipedia("what is the speed of light"), None);
    }
}
