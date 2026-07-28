// Minimal markdown renderer — deliberately hand-rolled rather than pulling in
// a library, because this app ships no bundler and the subset it needs is
// small: headings, emphasis, links, lists, task lists, quotes, GFM tables and
// fenced code. Used by the notes preview, memory bodies and chat messages.

export function escapeHtml(str) {
  const d = document.createElement("div");
  d.textContent = str;
  return d.innerHTML;
}

export function inlineMd(text) {
  text = escapeHtml(text);
  text = text.replace(/\[\[([^\]]+)\]\]/g, (m, p1) => '<a class="wiki-link" data-note="' + p1.replace(/"/g, "&quot;") + '">' + p1 + "</a>");
  text = text.replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a class="ext-link" href="$2" target="_blank" rel="noopener">$1</a>');
  text = text.replace(/`([^`]+)`/g, "<code>$1</code>");
  text = text.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
  text = text.replace(/\*([^*]+)\*/g, "<em>$1</em>");
  return text;
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
      html +=
        "<pre><code" +
        (lang ? ' class="lang-' + escapeHtml(lang) + '"' : "") +
        ">" +
        escapeHtml(codeLines.join("\n")) +
        "</code></pre>";
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
