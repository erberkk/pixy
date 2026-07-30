// Turning a fetched HTML page into the text that was actually on it.
//
// The naive version of this — strip the tags, keep what is left — produces a
// wall that opens with the cookie banner, the nav menu and the newsletter
// prompt, and buries the article in the middle. A small model reading that
// tends to describe the page instead of answering from it, so the two things
// this module does beyond stripping tags are: pick the element the content is
// in, and drop the furniture.

use scraper::{Html, Node, Selector};

/// Elements whose text is never the page's content. Not descended into at all,
/// so nothing inside them survives either.
const FURNITURE: &[&str] = &[
    "script", "style", "noscript", "nav", "footer", "header", "aside", "form", "svg", "iframe",
    "template", "button", "select", "option", "label", "figcaption", "dialog",
];

/// Elements that end a line. Without these the whole page collapses into one
/// paragraph and sentences from unrelated sections run together.
const BLOCKS: &[&str] = &[
    "p", "div", "br", "li", "tr", "section", "article", "main", "blockquote", "pre", "hr", "h1",
    "h2", "h3", "h4", "h5", "h6", "td", "th", "dt", "dd", "figure", "details", "summary",
];

/// Where the content usually is, most specific first.
const CONTENT_ROOTS: &[&str] = &["main", "article", "[role=main]", "#content", "#main", ".post"];

/// A content root has to hold this share of the page's total text to be
/// believed.
///
/// Some pages have a `<main>` holding only a heading, with the article rendered
/// beside it. Taking that root on the grounds that it exists would throw the
/// page away, so a root that thin is rejected in favour of the whole body.
const MIN_ROOT_SHARE: f32 = 0.25;

/// Extracts `(title, text)` from an HTML document.
pub fn readable_text(html: &str) -> (String, String) {
    let document = Html::parse_document(html);
    let title = page_title(&document);

    let body_text = Selector::parse("body")
        .ok()
        .and_then(|selector| document.select(&selector).next())
        .map(|body| text_of(*body))
        .unwrap_or_default();

    let mut best = String::new();
    for candidate in CONTENT_ROOTS {
        let Ok(selector) = Selector::parse(candidate) else {
            continue;
        };
        for element in document.select(&selector) {
            let text = text_of(*element);
            if text.len() > best.len() {
                best = text;
            }
        }
        if !best.is_empty() {
            break;
        }
    }

    let text = if !best.is_empty() && best.len() as f32 >= body_text.len() as f32 * MIN_ROOT_SHARE {
        best
    } else {
        body_text
    };
    // A document with no <body> at all (a fragment, or markup mangled enough
    // that the parser gave up) still has text worth reading.
    let text = if text.trim().is_empty() {
        text_of(*document.root_element())
    } else {
        text
    };

    (title, text)
}

fn page_title(document: &Html) -> String {
    for candidate in ["title", "h1"] {
        let Ok(selector) = Selector::parse(candidate) else {
            continue;
        };
        if let Some(element) = document.select(&selector).next() {
            let title = tidy(&element.text().collect::<String>());
            if !title.is_empty() {
                return title.lines().next().unwrap_or_default().to_string();
            }
        }
    }
    String::new()
}

fn text_of(root: ego_tree::NodeRef<'_, Node>) -> String {
    let mut out = String::new();
    walk(root, &mut out);
    tidy(&out)
}

fn walk(node: ego_tree::NodeRef<'_, Node>, out: &mut String) {
    match node.value() {
        Node::Text(text) => out.push_str(&text.text),
        Node::Element(element) => {
            let name = element.name();
            if FURNITURE.contains(&name) {
                return;
            }
            let is_block = BLOCKS.contains(&name);
            if is_block {
                out.push('\n');
            }
            for child in node.children() {
                walk(child, out);
            }
            if is_block {
                out.push('\n');
            }
        }
        _ => {
            for child in node.children() {
                walk(child, out);
            }
        }
    }
}

/// Collapses the whitespace an HTML document is full of into readable lines.
///
/// Also drops repeated adjacent lines: menus and link lists routinely produce
/// the same word several times in a row, and a model reading that has to spend
/// attention deciding it means nothing.
pub fn tidy(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    let mut collapsed: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            continue;
        }
        collapsed.push(line);
    }
    let mut previous: Option<&str> = None;
    for line in &collapsed {
        if previous == Some(line.as_str()) {
            continue;
        }
        lines.push(line);
        previous = Some(line);
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_article_wins_over_the_furniture_around_it() {
        let html = r#"
            <html><head><title>Real Title</title></head><body>
              <nav><a href="/">Home</a><a href="/about">About</a></nav>
              <header>Subscribe to our newsletter!</header>
              <main><p>The first real sentence.</p><p>The second one.</p></main>
              <footer>Copyright 2026</footer>
              <script>var tracking = 1;</script>
            </body></html>"#;
        let (title, text) = readable_text(html);
        assert_eq!(title, "Real Title");
        assert!(text.contains("The first real sentence."));
        assert!(text.contains("The second one."));
        assert!(!text.contains("Home"), "nav leaked: {text}");
        assert!(!text.contains("newsletter"), "header leaked: {text}");
        assert!(!text.contains("Copyright"), "footer leaked: {text}");
        assert!(!text.contains("tracking"), "script leaked: {text}");
    }

    // The reason MIN_ROOT_SHARE exists: taking <main> here would return a
    // heading and throw the page away.
    #[test]
    fn a_nearly_empty_content_root_falls_back_to_the_whole_body() {
        let html = format!(
            "<html><body><main><h1>Hi</h1></main><div>{}</div></body></html>",
            "The actual article text. ".repeat(40)
        );
        let (_, text) = readable_text(&html);
        assert!(text.contains("The actual article text."));
        assert!(text.len() > 200, "fell back to the stub: {text}");
    }

    #[test]
    fn block_elements_become_line_breaks() {
        let html = "<body><div><p>One</p><p>Two</p></div></body>";
        let (_, text) = readable_text(html);
        assert_eq!(text, "One\nTwo");
    }

    #[test]
    fn repeated_adjacent_lines_collapse() {
        assert_eq!(tidy("Menu\nMenu\nMenu\nReal"), "Menu\nReal");
    }

    #[test]
    fn whitespace_inside_a_line_collapses() {
        assert_eq!(tidy("  a   b \t c  "), "a b c");
        assert_eq!(tidy("\n\n\n"), "");
    }

    // A page that renders itself with JavaScript leaves nothing behind. The
    // caller turns this into an explanation for the model rather than a
    // mystery, so the empty result has to actually be empty.
    #[test]
    fn a_javascript_only_page_yields_nothing() {
        let html = r#"<html><body><div id="root"></div>
            <script>ReactDOM.render(<App/>, root);</script></body></html>"#;
        let (_, text) = readable_text(html);
        assert!(text.trim().is_empty(), "expected nothing, got: {text}");
    }

    #[test]
    fn the_title_falls_back_to_the_first_heading() {
        let (title, _) = readable_text("<body><h1>Heading Only</h1><p>x</p></body>");
        assert_eq!(title, "Heading Only");
    }

    #[test]
    fn a_fragment_without_a_body_still_produces_text() {
        let (_, text) = readable_text("<p>Just a fragment.</p>");
        assert!(text.contains("Just a fragment."));
    }
}
