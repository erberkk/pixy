// Turning one message into the line or two that a notice or the morning card
// shows.
//
// The rule this file exists to enforce: there is ALWAYS something to show. A
// model can be absent, hosted, slow, or wrong, and in every one of those cases
// the message still gets described — with its own opening lines, which is what
// every mail client shows anyway. Summarizing is an improvement on that, never a
// precondition for it.
use crate::ai::llm::MailSummaryEndpoint;
use crate::google::gmail::MailMessage;
use crate::tunables;

/// How much of the message to show when there is no summary. Roughly a notice's
/// worth of text — long enough to tell what a mail is about, short enough not to
/// turn the pill into a reading window.
const PREVIEW_CHARS: usize = 240;

/// What can be done with a given message, decided before anything slow happens.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// Show the message's own opening lines.
    Preview,
    /// Worth asking a model to compress.
    Summarize,
}

/// Why a message is or isn't worth a model call.
///
/// Pure and separate from the work so the one rule that matters can be tested
/// without a model, a token or an AppHandle: every branch that isn't Summarize
/// still produces a usable description.
pub fn plan(body_chars: usize, min_chars: usize, has_model: bool, local_only: bool, model_is_local: bool) -> Plan {
    if body_chars < min_chars {
        // Short mail explains itself; a summary of three lines is three lines.
        return Plan::Preview;
    }
    if !has_model {
        return Plan::Preview;
    }
    // The user's switch: summarizing sends the message body to whichever profile
    // is active, and that profile can be pointed at a hosted API at any time.
    if local_only && !model_is_local {
        return Plan::Preview;
    }
    Plan::Summarize
}

/// Cuts a reply's quoted history off the end.
///
/// Without this, a two-line answer on a long thread previews as the *previous*
/// message — every mail client strips this for the same reason. Kept to the
/// markers that actually appear rather than trying to be exhaustive: a missed
/// one costs a slightly worse preview, and an over-eager one would silently eat
/// the real message.
fn strip_quoted(body: &str) -> &str {
    let mut end = body.len();
    for (offset, line) in line_offsets(body) {
        let trimmed = line.trim();
        let is_quote = trimmed.starts_with('>')
            || trimmed.starts_with("-----Original Message-----")
            || trimmed.starts_with("________________________________")
            // Gmail's attribution line, in whatever language the sender's client
            // is set to: "On Thu, ... <ali@x.com> wrote:" / "... şunu yazdı:".
            || ((trimmed.ends_with("wrote:") || trimmed.ends_with("yazdı:")) && trimmed.contains('@'));
        if is_quote {
            end = offset;
            break;
        }
    }
    let kept = body[..end].trim();
    // A message that is nothing but quoted text still has to show something.
    if kept.is_empty() {
        body.trim()
    } else {
        kept
    }
}

fn line_offsets(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    text.split_inclusive('\n').map(move |line| {
        let start = offset;
        offset += line.len();
        (start, line)
    })
}

/// Strips the furniture a plain-text alternative carries instead of pictures.
///
/// Almost no bulk mail is written as plain text; it is generated from the HTML
/// version, and that generator leaves markers behind — `[image: Logo]` where a
/// picture was, `<https://…>` around every link. A short announcement whose
/// preview then reads `[image: .] <https://hr-link.net/Go/6TXLD912MSMBF…>` has
/// told the reader nothing, which is worse than telling them nothing in fewer
/// characters.
///
/// Only the wrappers go. The link text a human wrote stays, and so does any URL
/// typed inline in an ordinary message, because that one is usually the point of
/// the message.
fn strip_generated_markers(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;

    while let Some(start) = rest.find(['[', '<']) {
        let (opener, closer) = match &rest[start..start + 1] {
            "[" => ('[', ']'),
            _ => ('<', '>'),
        };
        // `[image: …]` and `[cid:…]` are the two the generators emit; any other
        // bracketed text is the sender's own and is left alone.
        let is_marker = |inner: &str| {
            let lower = inner.trim().to_ascii_lowercase();
            match opener {
                '[' => lower.starts_with("image:") || lower.starts_with("cid:"),
                _ => lower.starts_with("http://") || lower.starts_with("https://"),
            }
        };

        let Some(end) = rest[start..].find(closer).map(|offset| start + offset) else {
            break;
        };
        if is_marker(&rest[start + 1..end]) {
            out.push_str(&rest[..start]);
            rest = &rest[end + 1..];
        } else {
            out.push_str(&rest[..=start]);
            rest = &rest[start + 1..];
        }
    }
    out.push_str(rest);

    // A line that was nothing but markers collapses to punctuation and spaces —
    // "—", "·", a stray dash. Nothing survives that a reader would miss.
    let cleaned = out.trim();
    if cleaned.chars().all(|c| !c.is_alphanumeric()) {
        return String::new();
    }
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The message's own opening lines, quoted history and generated markers removed.
pub fn preview(body: &str) -> String {
    let body = strip_quoted(body);
    let mut collapsed = String::new();
    for line in body.lines() {
        let line = strip_generated_markers(line.trim());
        let line = line.as_str();
        if line.is_empty() {
            continue;
        }
        if !collapsed.is_empty() {
            collapsed.push(' ');
        }
        collapsed.push_str(line);
        if collapsed.chars().count() > PREVIEW_CHARS {
            break;
        }
    }
    if collapsed.chars().count() > PREVIEW_CHARS {
        let cut: String = collapsed.chars().take(PREVIEW_CHARS).collect();
        // Back off to the last space so the preview doesn't end mid-word.
        let cut = match cut.rfind(' ') {
            Some(space) if space > PREVIEW_CHARS / 2 => cut[..space].to_string(),
            _ => cut,
        };
        format!("{cut}…")
    } else {
        collapsed
    }
}

/// One line or two describing this message, whatever is available.
///
/// Blocks for as long as the summary timeout allows — callers run it on a
/// watcher thread, and the notice deliberately waits for the result rather than
/// appearing and rewriting itself under the user.
pub fn describe(app: &tauri::AppHandle, message: &MailMessage) -> String {
    let body = message.readable_body();
    let fallback = preview(body);

    let profile = crate::ai::llm::get_active_llm_profile(app.clone());
    let has_model = !profile.model.trim().is_empty();
    let model_is_local = crate::ai::recall::is_local_endpoint(&profile.base_url);
    let decision = plan(
        body.chars().count(),
        tunables::int(app, tunables::MAIL_SUMMARIZE_MIN_CHARS).max(0) as usize,
        has_model,
        tunables::toggle(app, tunables::MAIL_LOCAL_MODELS_ONLY),
        model_is_local,
    );
    if decision == Plan::Preview {
        return fallback;
    }

    let endpoint = MailSummaryEndpoint {
        base_url: &profile.base_url,
        model: &profile.model,
        api_key: &profile.api_key,
        think: profile.think,
        max_tokens: profile.max_tokens,
        timeout_secs: tunables::int(app, tunables::MAIL_SUMMARY_TIMEOUT).max(1) as u64,
    };

    match crate::ai::llm::summarize_mail(
        endpoint,
        &message.from_name,
        &message.subject,
        body,
        message.is_reply_to_me,
    ) {
        Ok(Some(summary)) => summary,
        // Both remaining cases — the model answered with nothing usable, or the
        // call failed outright — land on exactly the same fallback. That is the
        // point: there is no path here that produces an empty description.
        _ => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line that prompted this, copied off the morning card. Everything a
    /// reader could use is in the first half; the rest is what the HTML-to-text
    /// generator left behind.
    #[test]
    fn a_generated_plain_text_preview_keeps_only_the_words() {
        let body = "Araslar için \"Kariyer Fırsatları\" — [image: .] \
                    <https://hr-link.net/Go/6TXLD912MSMBFJSX08CXJVZDZ6ZP8TXGTR>";
        assert_eq!(preview(body), "Araslar için \"Kariyer Fırsatları\" —");
    }

    #[test]
    fn a_line_that_was_only_markers_disappears_entirely() {
        let body = "[image: header.png]\n<https://track.example.com/x>\nActual message here.";
        assert_eq!(preview(body), "Actual message here.");
    }

    /// The narrow part: only the two shapes a generator emits are stripped. A
    /// bracketed aside is the sender's own writing, and a URL someone typed into
    /// a sentence is usually why they wrote at all.
    #[test]
    fn brackets_and_links_a_human_wrote_survive() {
        assert_eq!(
            preview("The build [see attached] failed"),
            "The build [see attached] failed"
        );
        assert_eq!(
            preview("Deploy notes are at https://wiki.internal/deploy"),
            "Deploy notes are at https://wiki.internal/deploy"
        );
    }

    // The requirement this whole file is built around: every combination that
    // isn't "long mail, local model available" still ends up describable.
    #[test]
    fn only_a_long_message_with_a_usable_model_is_summarized() {
        assert_eq!(plan(2000, 800, true, true, true), Plan::Summarize);

        // Too short to be worth a model.
        assert_eq!(plan(100, 800, true, true, true), Plan::Preview);
        // No model configured at all.
        assert_eq!(plan(2000, 800, false, true, true), Plan::Preview);
        // A hosted model with the local-only switch on.
        assert_eq!(plan(2000, 800, true, true, false), Plan::Preview);
        // The same hosted model once the user has turned that switch off.
        assert_eq!(plan(2000, 800, true, false, false), Plan::Summarize);
    }

    #[test]
    fn quoted_history_is_cut_off_the_preview() {
        let body = "Tamam, yarın gönderiyorum.\n\n\
                    On Thu, 30 Jul 2026 at 09:00, Ali <ali@example.com> wrote:\n\
                    > Fatura ne zaman gelir?";
        assert_eq!(preview(body), "Tamam, yarın gönderiyorum.");

        let turkish = "Olur.\n\n30 Tem 2026 Per, Ali <ali@example.com> şunu yazdı:\n> soru";
        assert_eq!(preview(turkish), "Olur.");
    }

    #[test]
    fn a_message_that_is_only_quoted_text_still_previews() {
        assert!(!preview("> nothing but a quote").is_empty());
    }

    #[test]
    fn a_long_preview_is_cut_at_a_word_boundary() {
        let body = "kelime ".repeat(200);
        let preview = preview(&body);
        assert!(preview.ends_with('…'));
        assert!(preview.chars().count() <= PREVIEW_CHARS + 1);
        // Cutting mid-word would leave a fragment like "keli…".
        assert!(preview.trim_end_matches('…').ends_with("kelime"));
    }

    #[test]
    fn blank_lines_are_collapsed_rather_than_shown() {
        assert_eq!(preview("bir\n\n\niki\n"), "bir iki");
    }
}
