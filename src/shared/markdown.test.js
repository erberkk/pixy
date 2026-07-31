// Tests for the hand-rolled markdown renderer.
//
// This file exists because of a real vulnerability, not for coverage: the
// external-link branch of inlineMd used to interpolate a URL straight into
// href="…" without escaping quotes or checking the scheme, which turned any web
// page the model read into script execution inside a webview holding the whole
// Tauri API. The three payloads below are the ones that were confirmed to fire.
//
// It is the only frontend test file, and deliberately so — this is the one
// module where a mistake is a security bug rather than a visual glitch, and the
// regex it hinges on is the kind of line that gets "tidied up" a year later.
//
// Runs on Node's built-in test runner, so it adds no dependency and no build
// step to a project that has neither: `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";

import { inlineMd, markdownToHtml, escapeHtml } from "./markdown.js";

/// Tags in the output that carry an executable attribute.
///
/// The security property is specifically "inside a tag": an interpolated value
/// that stayed put is text, and `onfocus="…"` sitting in text is inert — the
/// refused payloads are echoed as literal source on purpose, so a test that
/// searched the whole string would flag its own fix. Only what lands between
/// `<` and `>` can execute. Reading real tags out with a regex is safe here
/// because escapeHtml has already turned every `<` in the input into `&lt;`,
/// so the only angle brackets left are ones this renderer emitted itself.
///
/// Matched loosely on purpose: the claim is that NO executable attribute can
/// appear, not that a particular one cannot.
function dangerousTags(html) {
  const tags = html.match(/<[^>]*>/g) || [];
  return tags.filter((tag) => /\son[a-z]+\s*=/i.test(tag) || /\sautofocus/i.test(tag));
}

test("a quote in a link URL cannot open an attribute", () => {
  const html = inlineMd('[Full report](" onmouseover="alert(1)');
  assert.deepEqual(dangerousTags(html), []);
});

test("the no-interaction autofocus variant cannot fire", () => {
  const html = inlineMd('[x](" autofocus onfocus="alert(1)');
  // autofocus needs no handler of its own — it just has to land as an attribute.
  assert.deepEqual(dangerousTags(html), []);
});

test("javascript: is not emitted as a link", () => {
  const html = inlineMd("[click me](javascript:alert`1`)");
  assert.ok(!/href\s*=\s*"javascript:/i.test(html), html);
  // Left as literal text rather than silently dropped: a reader who sees the
  // source can judge it, where a vanished link tells them nothing.
  assert.ok(html.includes("click me"), html);
});

test("other executable schemes are refused too", () => {
  for (const url of ["data:text/html,<script>alert(1)</script>", "vbscript:msgbox", "JaVaScRiPt:alert(1)", "file:///C:/Windows"]) {
    const html = inlineMd(`[label](${url})`);
    assert.ok(!/class="ext-link"/.test(html), `${url} was rendered as a link: ${html}`);
  }
});

test("ordinary links still render, query strings intact", () => {
  const html = inlineMd("[docs](https://example.com/a?x=1&y=2)");
  // escapeHtml already turned & into &amp;, which is correct inside an
  // attribute. A second escaping pass here would produce &amp;amp; and a
  // broken URL — this asserts it does not happen.
  assert.ok(html.includes('href="https://example.com/a?x=1&amp;y=2"'), html);
  assert.ok(!html.includes("&amp;amp;"), html);
  assert.ok(html.includes('rel="noopener noreferrer"'), html);
});

test("mailto, anchors and relative paths are allowed", () => {
  for (const url of ["mailto:someone@example.com", "#section", "/local/page"]) {
    assert.ok(inlineMd(`[a](${url})`).includes('class="ext-link"'), url);
  }
});

test("wiki links keep working and quote-escape their target", () => {
  const html = inlineMd('[[a "note"]]');
  assert.ok(html.includes('data-note="a &quot;note&quot;"'), html);
  assert.deepEqual(dangerousTags(html), []);
});

test("markup in the source text is escaped, not executed", () => {
  const html = markdownToHtml("<img src=x onerror=alert(1)>");
  assert.deepEqual(dangerousTags(html), []);
  assert.ok(html.includes("&lt;img"), html);
});

test("a poisoned link inside a table cell is escaped too", () => {
  // Table cells run through inlineMd separately, so they are their own sink.
  const html = markdownToHtml('| h |\n| --- |\n| [x](" autofocus onfocus="alert(1) |');
  assert.deepEqual(dangerousTags(html), []);
});

test("escapeHtml escapes exactly the three characters that need it", () => {
  assert.equal(escapeHtml('&<>"\''), "&amp;&lt;&gt;\"'");
});
