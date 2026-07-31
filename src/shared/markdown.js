// Minimal markdown renderer — deliberately hand-rolled rather than pulling in
// a library, because this app ships no bundler and the subset it needs is
// small: headings, emphasis, links, lists, task lists, quotes, GFM tables and
// fenced code. Used by the notes preview, memory bodies and chat messages.

export function escapeHtml(str) {
  // No DOM: the test runner imports this module directly under Node, and this
  // renderer is the one piece of frontend that has to be tested rather than
  // eyeballed (see markdown.test.js). The two paths agree on everything that
  // matters — setting textContent and reading innerHTML back escapes exactly
  // & < > — with one cosmetic difference: the DOM path also serialises U+00A0
  // as &nbsp;. Nothing downstream distinguishes them.
  if (typeof document === "undefined") {
    return String(str).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  }
  const d = document.createElement("div");
  d.textContent = str;
  return d.innerHTML;
}

// For a value going into a quoted HTML attribute rather than into text.
//
// escapeHtml alone is NOT enough there, and the reason is easy to miss: it works
// by setting textContent and reading innerHTML back, and that serialisation
// escapes & < > but leaves quotes alone — because a quote in text position needs
// no escaping. Inside `attr="..."` it very much does: one double quote ends the
// attribute and everything after it is parsed as markup. That matters most for
// values that came off the internet (a URL from a search result is written by
// whoever owns the page), which is exactly where this gets used.
export function escapeAttr(str) {
  return escapeHtml(str).replace(/"/g, "&quot;").replace(/'/g, "&#39;");
}

// Schemes this renderer is willing to emit as a link. Everything else — most
// importantly `javascript:` — is left as literal text.
//
// A link in a model's reply is attacker-influenced input, not authored content:
// the model reads pages that anyone can write (see web/fetch.rs), so a URL
// arriving in an answer carries exactly the trust level of a URL in a search
// result, which is none. Allow-list rather than deny-list, because the set of
// schemes a webview will execute is not something this file can enumerate.
const SAFE_HREF = /^(?:https?:\/\/|mailto:|#|\/)/i;

export function inlineMd(text) {
  text = escapeHtml(text);
  text = text.replace(/\[\[([^\]]+)\]\]/g, (m, p1) => '<a class="wiki-link" data-note="' + p1.replace(/"/g, "&quot;") + '">' + p1 + "</a>");
  // The href goes into a quoted attribute, so quotes must die here even though
  // escapeHtml already ran: escapeHtml leaves them alone by design (a quote in
  // text position needs no escaping) and one of them ends the attribute, after
  // which the rest of the URL is parsed as markup — `" autofocus onfocus="…`
  // needs no click to fire. escapeAttr is deliberately NOT reused: escapeHtml
  // has already turned the & of a query string into &amp;, and running it twice
  // would corrupt the link.
  text = text.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (whole, label, href) => {
    const url = href.trim();
    // Not a scheme we emit: show the markdown source rather than a link whose
    // destination the reader has no way to judge.
    if (!SAFE_HREF.test(url)) return whole;
    const safe = url.replace(/"/g, "&quot;").replace(/'/g, "&#39;");
    return '<a class="ext-link" href="' + safe + '" target="_blank" rel="noopener noreferrer">' + label + "</a>";
  });
  text = text.replace(/`([^`]+)`/g, "<code>$1</code>");
  text = text.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
  text = text.replace(/\*([^*]+)\*/g, "<em>$1</em>");
  return text;
}

// --- fenced code blocks -------------------------------------------------------
//
// Rendered as a framed block with a language label, a line-number gutter and a
// copy button, and syntax-coloured by the vendored highlight.js when it is
// loaded (see vendor/highlight/). Everything degrades: with no highlighter the
// block still gets its frame and numbers, just in one colour.
//
// Highlighting here rather than in a pass over the DOM afterwards, because this
// is the one place that knows the language. It costs nothing per streamed token:
// the chat appends plain text while a reply streams and only runs markdown once
// the reply is finished (see chat.js's chat-stream-chunk listener).

// Some fences carry a filename or a label rather than a language. Taking only
// the leading word means ```python title=x still highlights, and a fence of
// ```script.py falls through to "unknown" instead of being handed to the
// highlighter as a language name.
function fenceLanguage(token) {
  const first = (token || "").trim().split(/[\s,:]/)[0].toLowerCase();
  return /^[a-z0-9#+._-]+$/.test(first) ? first : "";
}

function highlightCode(code, lang) {
  const hljs = typeof window !== "undefined" ? window.hljs : null;
  // getLanguage resolves aliases too ("py", "sh", "toml"), so this accepts more
  // than listLanguages() would suggest. Unknown languages are NOT passed to
  // hljs.highlight: it throws on those rather than falling back.
  if (!hljs || !lang || !hljs.getLanguage(lang)) return escapeHtml(code);
  try {
    // ignoreIllegals, because a model's answer often contains a fragment rather
    // than a complete valid file, and a grammar hitting something illegal must
    // not lose the whole block.
    return hljs.highlight(code, { language: lang, ignoreIllegals: true }).value;
  } catch {
    return escapeHtml(code);
  }
}

function codeBlockHtml(code, langToken) {
  const lang = fenceLanguage(langToken);
  const lineCount = code.split("\n").length;
  const gutter = Array.from({ length: lineCount }, (_, i) => i + 1).join("\n");
  const label = escapeHtml(lang || (langToken || "").trim() || "text");
  return (
    '<div class="code-block' +
    (lineCount === 1 ? " one-line" : "") +
    '">' +
    '<div class="code-block-bar">' +
    '<span class="code-block-lang">' +
    label +
    "</span>" +
    // type=button so a block inside a form never submits it.
    '<button class="code-copy" type="button" title="Copy this block">Copy</button>' +
    "</div>" +
    '<div class="code-block-body">' +
    '<pre class="code-block-gutter" aria-hidden="true">' +
    gutter +
    "</pre>" +
    '<pre><code class="hljs">' +
    highlightCode(code, lang) +
    "</code></pre>" +
    "</div></div>"
  );
}

/// Puts text on the system clipboard, and resolves only if it got there.
///
/// Goes through Rust rather than navigator.clipboard. The web API rejects with
/// NotAllowedError ("Document is not focused") whenever this window does not
/// hold OS focus, and in that state it writes nothing at all — measured against
/// the real Windows clipboard, which kept its previous contents while the button
/// reported success. The Rust side has no such condition.
export async function copyText(text) {
  // Imported here rather than at the top of the file, and this is the one place
  // in the frontend where that matters: shared/tauri.js reads Tauri's injected
  // globals at module scope, so importing it eagerly would make this module
  // unloadable anywhere `window.__TAURI__` does not exist — including the test
  // runner, which is the only reason the renderer below has any test coverage
  // at all. A click is not a hot path, so the deferred import costs nothing.
  const { invoke } = await import("./tauri.js");
  await invoke("plugin:clipboard-manager|write_text", { label: null, text });
}

/// Wires a button to copy `getText()`, with the button reporting what happened.
///
/// Shared so the code-block button and the whole-message button cannot drift
/// into reporting success differently — the failure that started this was a
/// button that said "Copied" when nothing had been copied.
export function bindCopyButton(button, getText, restingLabel = "Copy") {
  copyText(getText()).then(
    () => {
      button.textContent = "Copied";
      button.classList.add("copied");
      setTimeout(() => {
        button.textContent = restingLabel;
        button.classList.remove("copied");
      }, 1200);
    },
    (err) => {
      // Says so rather than lying. The message is short because it sits in a
      // small button; the detail goes to the console for a bug report.
      console.error("copy failed", err);
      button.textContent = "Failed";
      button.classList.add("failed");
      setTimeout(() => {
        button.textContent = restingLabel;
        button.classList.remove("failed");
      }, 1600);
    }
  );
}

// One delegated listener for every code block in the document, registered once
// on import — the blocks themselves are built as HTML strings and replaced
// wholesale on each render, so per-block listeners would leak with every
// re-render and be lost on the next one.
if (typeof document !== "undefined") {
  document.addEventListener("click", (event) => {
    const button = event.target.closest?.(".code-copy");
    if (!button) return;
    const code = button.closest(".code-block")?.querySelector(".code-block-body > pre > code");
    if (!code) return;
    // textContent, not innerHTML: it gives back exactly the original source,
    // because the highlighter only wraps text in spans and adds none of its own.
    bindCopyButton(button, () => code.textContent);
  });
}

const TABLE_SEPARATOR_RE = /^\s*\|?(\s*:?-{1,}:?\s*\|)+\s*:?-{1,}:?\s*\|?\s*$/;

function splitTableRow(line) {
  let l = line.trim();
  if (l.startsWith("|")) l = l.slice(1);
  if (l.endsWith("|")) l = l.slice(0, -1);
  return l.split("|").map((c) => c.trim());
}

export function markdownToHtml(src) {
  const lines = src.split("\n");
  let html = "";
  let listType = null;
  let inQuote = false;

  function closeList() {
    if (listType) {
      html += "</" + listType + ">";
      listType = null;
    }
  }
  function closeQuote() {
    if (inQuote) {
      html += "</blockquote>";
      inQuote = false;
    }
  }

  let i = 0;
  while (i < lines.length) {
    const line = lines[i];

    // Fenced code block: ```lang ... ``` — content is escaped verbatim, no
    // inline markdown parsing inside (matches standard fenced-code
    // semantics), checked first so a pipe inside a code block never gets
    // mistaken for a table row below.
    const fence = line.match(/^\s*```(\S*)\s*$/);
    if (fence) {
      closeList();
      closeQuote();
      const lang = fence[1];
      i++;
      const codeLines = [];
      while (i < lines.length && !/^\s*```\s*$/.test(lines[i])) {
        codeLines.push(lines[i]);
        i++;
      }
      i++; // skip the closing fence (or the end of input if unterminated)
      html += codeBlockHtml(codeLines.join("\n"), lang);
      continue;
    }

    // GFM table: a row containing "|" immediately followed by a
    // "---|---"-style separator row.
    if (line.includes("|") && i + 1 < lines.length && TABLE_SEPARATOR_RE.test(lines[i + 1])) {
      closeList();
      closeQuote();
      const headerCells = splitTableRow(line);
      html +=
        "<table><thead><tr>" +
        headerCells.map((c) => "<th>" + inlineMd(c) + "</th>").join("") +
        "</tr></thead><tbody>";
      i += 2;
      while (i < lines.length && lines[i].includes("|") && lines[i].trim() !== "") {
        const rowCells = splitTableRow(lines[i]);
        html += "<tr>" + rowCells.map((c) => "<td>" + inlineMd(c) + "</td>").join("") + "</tr>";
        i++;
      }
      html += "</tbody></table>";
      continue;
    }

    const h = line.match(/^(#{1,6})\s+(.*)$/);
    const check = line.match(/^(\s*)-\s+\[( |x|X)\]\s+(.*)$/);
    const ul = line.match(/^(\s*)[-*]\s+(.*)$/);
    const ol = line.match(/^(\s*)\d+\.\s+(.*)$/);
    const quote = line.match(/^>\s?(.*)$/);
    const hr = line.match(/^(-{3,}|\*{3,})$/);

    if (hr) {
      closeList();
      closeQuote();
      html += "<hr>";
    } else if (h) {
      closeList();
      closeQuote();
      const level = h[1].length;
      html += "<h" + level + ">" + inlineMd(h[2]) + "</h" + level + ">";
    } else if (check) {
      closeQuote();
      if (listType !== "ul") {
        closeList();
        html += "<ul>";
        listType = "ul";
      }
      const done = check[2].toLowerCase() === "x";
      html +=
        '<li class="md-check' +
        (done ? " done" : "") +
        '"><label><input type="checkbox" data-line="' +
        i +
        '" ' +
        (done ? "checked" : "") +
        "> " +
        inlineMd(check[3]) +
        "</label></li>";
    } else if (ul) {
      closeQuote();
      if (listType !== "ul") {
        closeList();
        html += "<ul>";
        listType = "ul";
      }
      html += "<li>" + inlineMd(ul[2]) + "</li>";
    } else if (ol) {
      closeQuote();
      if (listType !== "ol") {
        closeList();
        html += "<ol>";
        listType = "ol";
      }
      html += "<li>" + inlineMd(ol[2]) + "</li>";
    } else if (quote) {
      closeList();
      if (!inQuote) {
        html += "<blockquote>";
        inQuote = true;
      }
      html += inlineMd(quote[1]) + "<br>";
    } else {
      closeList();
      closeQuote();
      if (line.trim() !== "") html += "<p>" + inlineMd(line) + "</p>";
    }
    i++;
  }
  closeList();
  closeQuote();
  return html;
}
