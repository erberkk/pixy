// Chat mode: a direct conversation with a locally-configured LLM, backed by
// ai/chat.rs (one JSON file per conversation) and ai/llm.rs's streaming
// send_chat_message. Unrelated to the Claude Code hook plumbing in
// agent/server.rs — this is a plain user <-> local-model chat.
import { initPip, setPipState } from "../../mascot/pip/pip.js";
import { invoke, listen } from "../../shared/tauri.js";
import { bindCopyButton, copyText, escapeAttr, escapeHtml, markdownToHtml } from "../../shared/markdown.js";
import { timeAgo } from "../../shared/format.js";
import { showToast } from "../lib/toast.js";
import {
  el,
  chatListEl,
  chatMainEl,
  chatMessagesEl,
  chatTitleInput,
  chatModelSelect,
  chatInput,
  chatSendBtn,
  chatComposerEl,
  chatInstructionsPanel,
  chatInstructionsInput,
  notesCount,
} from "../lib/dom.js";

let chats = []; // ChatSummary[] for the sidebar list
let activeChatId = null;
let activeChatMessages = []; // full ChatMessage[] for the open conversation
let llmProfiles = [];
let activeLlmProfileId = "";
let streamingChatId = null; // which chat a reply is currently streaming into
let streamingText = "";
let sendingMessage = false;
let chatModeInited = false;
// Attachments staged for the next message, in the order they were picked.
// A list rather than one, because a question is often about several files at
// once ("compare these two") and attaching them one message at a time loses
// exactly the comparison. Each entry is a ChatAttachment from
// pick_chat_attachment — see ai/chat.rs for the shape.
let pendingAttachments = [];

async function loadLlmProfiles() {
  const settings = await invoke("get_llm_settings");
  llmProfiles = settings.profiles || [];
  activeLlmProfileId =
    settings.active_profile_id && llmProfiles.some((p) => p.id === settings.active_profile_id)
      ? settings.active_profile_id
      : llmProfiles[0]?.id || "";
  renderModelSelect();
}

function renderModelSelect() {
  chatModelSelect.innerHTML = "";
  if (llmProfiles.length === 0) {
    const opt = document.createElement("option");
    opt.value = "";
    opt.textContent = "No model configured";
    chatModelSelect.appendChild(opt);
    chatModelSelect.disabled = true;
    return;
  }
  chatModelSelect.disabled = false;
  for (const p of llmProfiles) {
    const opt = document.createElement("option");
    opt.value = p.id;
    opt.textContent = p.label || p.model || "(untitled)";
    chatModelSelect.appendChild(opt);
  }
  chatModelSelect.value = activeLlmProfileId;
}

chatModelSelect.addEventListener("change", () => {
  activeLlmProfileId = chatModelSelect.value;
  invoke("set_active_llm_profile", { profileId: activeLlmProfileId });
});

export async function loadChatMode() {
  if (!chatModeInited) {
    chatModeInited = true;
    initPip({
      canvas: el("chatMascotCanvasSmall"),
      root: el("chatMascotRootSmall"),
    });
    invoke("get_chat_instructions").then((instructions) => {
      chatInstructionsInput.value = instructions || "";
    });
  }
  await loadLlmProfiles();
  await loadChatsList();
  if (!activeChatId) {
    chatMainEl.classList.remove("has-active");
  }
}

async function loadChatsList() {
  chats = await invoke("list_chats");
  renderChatList();
}

// --- sidebar search -----------------------------------------------------------
//
// Two kinds of match, shown separately because they answer different questions:
// a title match is what the user named the conversation, and a message match is
// something said inside one. Titles are filtered here from the list already in
// memory (instant, no round trip); messages come from the search index, which
// is the only thing that knows what is inside a conversation without reading
// every file (see recall.rs's search_chats).

let chatQuery = "";
// Chat ids that matched on content but not on title, with the line that matched.
let contentMatches = [];
let contentSearchToken = 0;

function titleMatches(chat, query) {
  return (chat.title || "New chat").toLowerCase().includes(query);
}

async function runChatSearch(raw) {
  chatQuery = raw.trim().toLowerCase();
  // Each run gets a token, and a late reply from an earlier keystroke is
  // discarded — otherwise a slow query for "ca" can land after "caching" and
  // replace the newer results with older ones.
  const token = ++contentSearchToken;
  if (chatQuery.length < 2) {
    contentMatches = [];
    renderChatList();
    return;
  }
  renderChatList(); // show the title matches immediately
  // Logged rather than swallowed: an empty result and a broken query look
  // identical in the sidebar, and that is exactly how a query that could not
  // even be prepared went unnoticed once already.
  const hits = await invoke("search_chats", { query: raw.trim(), limit: 20 }).catch((err) => {
    console.error("chat search failed", err);
    return [];
  });
  if (token !== contentSearchToken) return;
  const titleMatched = new Set(chats.filter((c) => titleMatches(c, chatQuery)).map((c) => c.id));
  contentMatches = hits.filter((h) => !titleMatched.has(h.chat_id));
  renderChatList();
}

function chatItemHtml(chat, snippet) {
  return (
    '<div class="chat-item ' +
    (chat.id === activeChatId ? "active" : "") +
    '" data-id="' +
    chat.id +
    '">' +
    '<div class="chat-item-title">' +
    escapeHtml(chat.title || "New chat") +
    "</div>" +
    (snippet ? '<div class="chat-item-snippet">' + escapeHtml(snippet) + "</div>" : "") +
    '<span class="chat-item-meta">' +
    timeAgo(chat.updated_at) +
    // No count when it isn't known (a search hit for a conversation the sidebar
    // list has not caught up with) rather than "null msgs".
    (chat.message_count === null
      ? ""
      : " · " + chat.message_count + (chat.message_count === 1 ? " msg" : " msgs")) +
    "</span>" +
    '<button class="chat-item-delete" data-id="' +
    chat.id +
    '" title="Delete chat">' +
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 6h18M8 6V4a2 2 0 012-2h4a2 2 0 012 2v2m3 0l-1 14a2 2 0 01-2 2H8a2 2 0 01-2-2L5 6h14z"/><path d="M10 11v6M14 11v6"/></svg>' +
    "</button>" +
    "</div>"
  );
}

function renderChatList() {
  const count = chats.length;
  notesCount.textContent = count + (count === 1 ? " chat" : " chats");

  if (count === 0) {
    chatListEl.innerHTML = '<div class="chat-empty-list">No chats yet.<br>Start a new conversation.</div>';
    return;
  }

  if (chatQuery) {
    const byTitle = chats.filter((c) => titleMatches(c, chatQuery));
    let html = byTitle.map((c) => chatItemHtml(c, "")).join("");
    if (contentMatches.length) {
      html +=
        '<div class="chat-list-group">In messages</div>' +
        contentMatches
          .map((hit) => {
            // Prefers the sidebar's own summary for the message count, but falls
            // back to the hit itself. Dropping the row when the summary is
            // missing looked like "no matches" for a conversation the index had
            // definitely found — the list is loaded once and the index is
            // updated in the background, so the two can disagree.
            const chat = chats.find((c) => c.id === hit.chat_id) || {
              id: hit.chat_id,
              title: hit.chat_title,
              updated_at: hit.ts_end,
              message_count: null,
            };
            return chatItemHtml(chat, hit.snippet);
          })
          .join("");
    }
    chatListEl.innerHTML =
      html || '<div class="chat-empty-list">Nothing matches.<br>Try fewer words.</div>';
    return;
  }

  chatListEl.innerHTML = chats.map((c) => chatItemHtml(c, "")).join("");

  chatListEl.querySelectorAll(".chat-item").forEach((row) => {
    row.addEventListener("click", (e) => {
      if (e.target.closest(".chat-item-delete")) return;
      openChat(row.dataset.id);
    });
  });
  chatListEl.querySelectorAll(".chat-item-delete").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      deleteChat(btn.dataset.id);
    });
  });
}

async function openChat(id) {
  const chat = await invoke("load_chat", { id });
  if (!chat) return;
  activeChatId = chat.id;
  activeChatMessages = chat.messages || [];
  // Indexes belong to the conversation that was open — carrying them across
  // would pin a recall note to whatever message happens to sit at that position
  // in the next one.
  recalledByIndex = new Map();
  webSourcesByIndex = new Map();
  chatTitleInput.value = chat.title || "";
  pendingAttachments = [];
  renderPendingAttachments();
  if (chat.profile_id && llmProfiles.some((p) => p.id === chat.profile_id)) {
    activeLlmProfileId = chat.profile_id;
    chatModelSelect.value = chat.profile_id;
  }
  renderMessages();
  renderChatList();
}

function newChat() {
  activeChatId = null;
  activeChatMessages = [];
  recalledByIndex = new Map();
  webSourcesByIndex = new Map();
  chatTitleInput.value = "";
  pendingAttachments = [];
  renderPendingAttachments();
  chatMessagesEl.innerHTML = "";
  chatMainEl.classList.remove("has-active");
  renderChatList();
  chatInput.focus();
}

async function deleteChat(id) {
  await invoke("delete_chat", { id });
  chats = chats.filter((c) => c.id !== id);
  if (activeChatId === id) newChat();
  renderChatList();
}

// Which earlier conversations were recalled for a given message index, this
// session only. Deliberately not saved into the chat file: it is an explanation
// of one answer, not part of the conversation, and persisting it would grow
// every chat with bookkeeping nobody reads back.
let recalledByIndex = new Map();

function recallNoteHtml(index) {
  const hits = recalledByIndex.get(index);
  if (!hits?.length) return "";
  // Shown because a model referring to something the user cannot see reads as
  // the model making it up. Naming the conversation and the date makes it
  // checkable.
  const items = hits
    .map((h) => {
      const when = new Date(h.ts_end).toISOString().slice(0, 10);
      // A hit can be one of Claude Code's own notes rather than an earlier
      // conversation — labelled, because "checkable" means the user can go and
      // find the thing, and those two live in completely different places.
      const kind = h.source === "memory" ? "note " : "";
      return escapeHtml(`${kind}${h.chat_title} · ${when}`);
    })
    .join(", ");
  return `<div class="chat-recall-note" title="Added to this question from your earlier conversations">↩ ${items}</div>`;
}

// What the model looked at on the web to answer a given message index. Session
// only, for the same reason as recalledByIndex above.
let webSourcesByIndex = new Map();

function webSourcesHtml(index) {
  const sources = webSourcesByIndex.get(index);
  if (!sources?.length) return "";
  // Real links, not just names: the whole value of showing these is that the
  // user can open one and see whether the answer is actually in it. Opened
  // through the opener plugin rather than a bare href, which inside a webview
  // would navigate the app itself.
  const items = sources
    .map(
      (s) =>
        // escapeAttr, not escapeHtml, for the two attributes — see its comment:
        // these values came from a web page, and escapeHtml leaves quotes alone.
        '<button type="button" class="chat-source" data-url="' +
        escapeAttr(s.url) +
        '" title="' +
        escapeAttr(s.url) +
        '">' +
        escapeHtml(s.title || s.url) +
        "</button>",
    )
    .join("");
  return '<div class="chat-sources">Read from the web: ' + items + "</div>";
}

// Cards for the files attached to a saved message. Same look as the pending
// cards above the composer, so a file looks the same before and after sending.
function messageAttachmentsHtml(message, index) {
  const attachments = message.attachments || [];
  if (!attachments.length) return "";
  const cards = attachments
    .map((a, at) => {
      const lines = a.text ? a.text.split("\n").length : 0;
      const detail =
        a.kind === "image"
          ? "image"
          : `${lines.toLocaleString()} line${lines === 1 ? "" : "s"}` +
            (a.full_chars > a.text.length ? " · truncated" : "");
      // An image's bytes are not persisted, so there is nothing to open — only
      // text attachments are clickable, and the card says so by not offering it.
      const clickable = a.kind !== "image" && a.text;
      return (
        '<button type="button" class="chat-msg-attachment' +
        (clickable ? " clickable" : "") +
        '" data-message="' +
        index +
        '" data-attachment="' +
        at +
        '"' +
        (clickable ? ' title="Open"' : "") +
        (clickable ? "" : " disabled") +
        ">" +
        '<span class="chat-msg-attachment-name">' +
        escapeHtml(a.name) +
        "</span>" +
        '<span class="chat-msg-attachment-detail">' +
        escapeHtml(detail) +
        "</span>" +
        "</button>"
      );
    })
    .join("");
  return '<div class="chat-msg-attachments">' + cards + "</div>";
}

// The attachment viewer. A dialog rather than a panel, because the contents can
// be long and the point is to read them without the conversation moving.
function openAttachmentViewer(messageIndex, attachmentIndex) {
  const attachment = activeChatMessages[messageIndex]?.attachments?.[attachmentIndex];
  if (!attachment?.text) return;
  const dialog = el("attachmentViewer");
  el("attachmentViewerName").textContent = attachment.name;
  const lines = attachment.text.split("\n").length;
  el("attachmentViewerMeta").textContent =
    `${lines.toLocaleString()} line${lines === 1 ? "" : "s"}` +
    (attachment.full_chars > attachment.text.length
      ? ` · showing the first ${attachment.text.length.toLocaleString()} of ${attachment.full_chars.toLocaleString()} characters`
      : "");
  // Rendered through the same markdown path as a message, so the file arrives
  // highlighted, gutter and all, and its own Copy button comes for free.
  el("attachmentViewerBody").innerHTML = markdownToHtml(
    "```" + (attachment.lang || "") + "\n" + attachment.text + "\n```"
  );
  dialog.showModal();
}

function renderMessages() {
  // Markdown-rendered (tables/bold/code fences/etc, see markdownToHtml —
  // shared with the notes preview) — only reached once per full render, not
  // per streamed token (see the chat-stream-chunk listener below), so
  // re-parsing markdown here never competes with a token actually arriving.
  chatMessagesEl.innerHTML = activeChatMessages
    .map(
      (m, index) =>
        '<div class="chat-msg ' +
        m.role +
        (m.source === "voice" ? " voice" : "") +
        '">' +
        recallNoteHtml(index) +
        '<div class="chat-msg-bubble">' +
        markdownToHtml(m.content) +
        // Spoken turns are marked because they are read differently: a user
        // message is a speech-recognition guess rather than something typed
        // deliberately, so an odd-looking exchange is usually the transcript's
        // fault, not the model's.
        (m.source === "voice" ? '<span class="chat-msg-tag">voice</span>' : "") +
        "</div>" +
        // Attached files as cards above the actions, not as text inside the
        // bubble. Clicking one opens its contents; the transcript stays about
        // what was asked.
        messageAttachmentsHtml(m, index) +
        // Pages the model read to write this. Below the bubble, like the recall
        // note above it: neither is something the model said, both are why it
        // knew.
        webSourcesHtml(index) +
        // Copies the message's own markdown source, not the rendered HTML —
        // read from activeChatMessages by index rather than scraped back out of
        // the DOM, so what lands on the clipboard is exactly what the model
        // wrote, fences and all.
        '<div class="chat-msg-actions">' +
        '<button class="chat-msg-copy" type="button" data-index="' +
        index +
        '" title="Copy this message">Copy</button>' +
        "</div>" +
        "</div>"
    )
    .join("");
  chatMainEl.classList.toggle("has-active", activeChatMessages.length > 0);
  chatMessagesEl.scrollTop = chatMessagesEl.scrollHeight;
}

function lastBubbleEl() {
  const bubbles = chatMessagesEl.querySelectorAll(".chat-msg-bubble");
  return bubbles[bubbles.length - 1] || null;
}

// Whether the message list is already scrolled at (or very near) its
// bottom edge — streaming shouldn't yank the view back down every token if
// the user has deliberately scrolled up to read something earlier.
function chatNearBottom(thresholdPx = 80) {
  return chatMessagesEl.scrollHeight - chatMessagesEl.scrollTop - chatMessagesEl.clientHeight < thresholdPx;
}

// Attach button — opens a native file picker (Rust side reads the file
// directly, see ai/chat.rs's pick_chat_attachment) and stashes the result
// until the next send. Images go as images; text, code, spreadsheets, PDFs,
// Word and PowerPoint files are turned into text there and inlined.
async function attachFile() {
  const attachment = await invoke("pick_chat_attachment");
  if (!attachment) return;
  if (attachment.kind === "unsupported") {
    showToast(
      `.${attachment.mime} dosyaları okunamıyor — resim, metin, kod, Excel, PDF, Word, PowerPoint ve zip eklenebilir`
    );
    return;
  }
  // A format we can read that this file defeated — a scanned PDF, an empty
  // workbook. The Rust side words the reason; repeating it here is the whole
  // point of keeping the two cases apart.
  if (attachment.kind === "failed") {
    showToast(`${attachment.name}: ${attachment.problem}`);
    return;
  }
  pendingAttachments.push(attachment);
  renderPendingAttachments();
  // Truncation has to be visible at attach time, not discovered later in the
  // transcript: the user may want to raise the limit or attach less.
  if (attachment.full_chars > attachment.data.length) {
    showToast(
      `${attachment.name} çok uzun — ilk ${attachment.data.length.toLocaleString()} karakteri ` +
        `gönderilecek (toplam ${attachment.full_chars.toLocaleString()}). Sınır: Ayarlar → Advanced → Chat`
    );
  }
}

// The badge on a card: the extension, which is what people recognise a file by.
function attachmentBadge(attachment) {
  const dot = attachment.name.lastIndexOf(".");
  const ext = dot > 0 ? attachment.name.slice(dot + 1) : attachment.mime;
  return (ext || "file").toUpperCase().slice(0, 5);
}

// The line under the name. Lines for text, because that is the unit a person
// thinks in for a file of code or a document, and it is also the honest measure
// of how much of the model's context this will take.
function attachmentDetail(attachment) {
  if (attachment.kind === "image") return "image";
  const lines = attachment.data ? attachment.data.split("\n").length : 0;
  const shown = `${lines.toLocaleString()} line${lines === 1 ? "" : "s"}`;
  return attachment.full_chars > attachment.data.length ? `${shown} · truncated` : shown;
}

function renderPendingAttachments() {
  const box = el("chatAttachments");
  if (!pendingAttachments.length) {
    box.style.display = "none";
    box.innerHTML = "";
    return;
  }
  box.style.display = "flex";
  box.innerHTML = pendingAttachments
    .map((attachment, index) => {
      const remove =
        '<button type="button" class="chat-attachment-remove" data-index="' +
        index +
        '" title="Remove">' +
        '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4"><path d="M6 6l12 12M18 6L6 18"/></svg>' +
        "</button>";
      // An image shows itself. The data is already base64 in memory, so this
      // costs no extra read.
      if (attachment.kind === "image") {
        return (
          '<div class="chat-attachment-card image">' +
          '<img alt="" src="data:' +
          escapeHtml(attachment.mime) +
          ";base64," +
          attachment.data +
          '" />' +
          remove +
          "</div>"
        );
      }
      return (
        '<div class="chat-attachment-card">' +
        '<div class="chat-attachment-title" title="' +
        escapeHtml(attachment.name) +
        '">' +
        escapeHtml(attachment.name) +
        "</div>" +
        '<div class="chat-attachment-detail">' +
        escapeHtml(attachmentDetail(attachment)) +
        "</div>" +
        '<span class="chat-attachment-badge">' +
        escapeHtml(attachmentBadge(attachment)) +
        "</span>" +
        remove +
        "</div>"
      );
    })
    .join("");
}

function autoTitleFromMessages() {
  const firstUser = activeChatMessages.find((m) => m.role === "user");
  if (!firstUser) return "";
  const trimmed = firstUser.content.trim();
  return trimmed.length > 48 ? trimmed.slice(0, 48) + "…" : trimmed;
}

async function persistActiveChat() {
  const title = chatTitleInput.value.trim() || autoTitleFromMessages() || "New chat";
  const saved = await invoke("save_chat", {
    chat: {
      id: activeChatId || "",
      title,
      profile_id: activeLlmProfileId,
      created_at: 0,
      updated_at: 0,
      messages: activeChatMessages,
    },
  });
  activeChatId = saved.id;
  chatTitleInput.value = saved.title;
  const summary = { id: saved.id, title: saved.title, updated_at: saved.updated_at, message_count: saved.messages.length };
  const idx = chats.findIndex((c) => c.id === saved.id);
  if (idx >= 0) chats[idx] = summary;
  else chats.unshift(summary);
  chats.sort((a, b) => b.updated_at - a.updated_at);
  renderChatList();
}

function autoResizeChatInput() {
  chatInput.style.height = "auto";
  chatInput.style.height = Math.min(chatInput.scrollHeight, 160) + "px";
}

// One attached file as markdown for the prompt: the filename on its own line,
// then a fence carrying the LANGUAGE so the block highlights. The filename is
// deliberately not the fence token — that is what used to produce ```script.py,
// a language no highlighter knows.
// How long the memory lookup may hold up a message before it is sent without
// one. Above the 400ms a warm lookup measures, below the 2.8s a cold one does:
// the common case always gets its memory, and the cold case sends promptly and
// gets it on the next message (the embedding model is resident by then).
const RECALL_DEADLINE_MS = 1500;

// Resolves to null if `promise` has not settled within `ms`. The work is not
// cancelled — it cannot be, and letting it finish is what warms the model for
// the next message — its result is simply no longer waited for.
function withDeadline(promise, ms, label) {
  let timer;
  const deadline = new Promise((resolve) => {
    timer = setTimeout(() => {
      console.warn(`${label} took longer than ${ms}ms; continuing without it`);
      resolve(null);
    }, ms);
  });
  return Promise.race([promise.catch(() => null), deadline]).finally(() => clearTimeout(timer));
}

function inlineAttachment(attachment) {
  // A pending attachment carries its text in `data` (the ChatAttachment shape
  // the Rust picker returns); a saved one carries it in `text`
  // (MessageAttachment). Accepting both keeps one function for the send path and
  // for rebuilding history, which is the only way the two can't disagree about
  // what the model was shown.
  const body = attachment.text ?? attachment.data ?? "";
  const truncated = attachment.full_chars > body.length;
  const heading = truncated
    ? `${attachment.name} — first ${body.length.toLocaleString()} of ` +
      `${attachment.full_chars.toLocaleString()} characters`
    : attachment.name;
  return `${heading}\n\`\`\`${attachment.lang}\n${body}\n\`\`\``;
}

async function sendChatMessage() {
  const text = chatInput.value.trim();
  const attachments = pendingAttachments;
  if ((!text && !attachments.length) || sendingMessage) return;
  const profile = llmProfiles.find((p) => p.id === activeLlmProfileId) || llmProfiles[0];
  if (!profile) {
    showToast("No LLM configured — add one in Settings");
    return;
  }

  pendingAttachments = [];
  renderPendingAttachments();

  // What actually gets sent to the model for THIS turn (may be an
  // OpenAI-style content-parts array with images inlined as data URIs, plus
  // any attached text inlined as fenced code blocks) vs. what gets
  // stored/shown permanently (plain text only — an image's bytes are never
  // written to the saved chat file, so history doesn't balloon with base64
  // forever; a lightweight "📎 filename" marker stands in for it instead,
  // meaning later turns no longer have the image in context).
  const images = attachments.filter((a) => a.kind === "image");
  const texts = attachments.filter((a) => a.kind === "text");
  // The model gets the whole file. The transcript does NOT: it keeps the
  // attachment beside the message (see ChatMessage.attachments in ai/chat.rs) and
  // shows a card, because a 150-line file pasted into the bubble buries the
  // question the user actually asked.
  const withText = [text, texts.map(inlineAttachment).join("\n\n")].filter(Boolean).join("\n\n");

  let wireContent = withText;
  if (images.length) {
    wireContent = [
      // A prompt is required alongside an image: a bare image part with no text
      // leaves some servers with nothing to answer.
      { type: "text", text: withText || (images.length === 1 ? "What's in this image?" : "What's in these images?") },
      ...images.map((a) => ({
        type: "image_url",
        image_url: { url: `data:${a.mime};base64,${a.data}` },
      })),
    ];
  }
  const displayContent = text;
  // An image's base64 is deliberately not persisted — it would grow the chat
  // file without bound, and a later turn cannot re-send it anyway.
  const storedAttachments = attachments.map((a) => ({
    name: a.name,
    lang: a.lang,
    kind: a.kind,
    text: a.kind === "image" ? "" : a.data,
    full_chars: a.full_chars,
  }));

  // Started here, before the message is rendered and written to disk, and
  // awaited much further down — so the lookup runs alongside that work instead
  // of after it. The query is this message plus the previous turn, because a
  // follow-up ("and how do we do that?") has no subject of its own to search on.
  //
  // The open conversation is excluded: persistActiveChat below indexes it, so
  // without this the search would find the question being asked and recall it to
  // itself.
  const recallQuery = [activeChatMessages.at(-1)?.content, text].filter(Boolean).join(" ");
  const recallPromise = withDeadline(
    invoke("recall_context", {
      query: recallQuery,
      baseUrl: profile.base_url,
      chatId: activeChatId || "",
    }),
    RECALL_DEADLINE_MS,
    "recall"
  );

  activeChatMessages.push({
    role: "user",
    content: displayContent,
    ts: Date.now(),
    attachments: storedAttachments,
  });
  chatInput.value = "";
  autoResizeChatInput();
  renderMessages();
  await persistActiveChat();

  sendingMessage = true;
  chatSendBtn.disabled = true;
  setPipState("chat_typing");

  const chatId = activeChatId;
  streamingChatId = chatId;
  streamingText = "";

  // Earlier turns are rebuilt WITH their attachments inlined again: the files
  // live beside those messages rather than in them, so sending only `content`
  // would quietly drop every file from the conversation after the turn it was
  // attached to — the model would answer "as we discussed in that file" having
  // never seen it twice.
  const history = activeChatMessages.slice(0, -1).map((m) => ({
    role: m.role,
    content: [m.content, (m.attachments || []).filter((a) => a.text).map(inlineAttachment).join("\n\n")]
      .filter(Boolean)
      .join("\n\n"),
  }));
  history.push({ role: "user", content: wireContent });

  // Anything decided in an EARLIER conversation that bears on this message —
  // usually nothing, and nothing is what gets injected then.
  //
  // It has to be awaited, because it goes INTO the array that is about to be
  // sent. That makes it the one thing standing between pressing send and the
  // request leaving, so it is bounded: measured at 400ms with the embedding
  // model resident and 2.8s with it cold, and a message that waits three seconds
  // to start is a worse trade than a message that occasionally goes without its
  // memory. See RECALL_DEADLINE_MS.
  //
  // The endpoint goes with the query: recall takes text out of your own
  // conversations, and whether that may leave this machine depends on where the
  // answer is coming from (see recall.share_with_cloud in Settings > Advanced).
  const recalled = await recallPromise;
  if (recalled?.block) history.unshift({ role: "system", content: recalled.block });
  // Attached to the answer this recall was for — activeChatMessages is about to
  // gain the assistant turn, so its index is the current length.
  if (recalled?.hits?.length) recalledByIndex.set(activeChatMessages.length, recalled.hits);

  // Unshifted last so the user's own instructions stay the first thing the model
  // reads — recalled history is context, not a persona.
  const systemPrompt = chatInstructionsInput.value.trim();
  if (systemPrompt) history.unshift({ role: "system", content: systemPrompt });
  activeChatMessages.push({ role: "assistant", content: "", ts: Date.now() });
  renderMessages();
  const bubble = lastBubbleEl();
  if (bubble) bubble.classList.add("streaming");

  try {
    await invoke("send_chat_message", {
      chatId,
      baseUrl: profile.base_url,
      model: profile.model,
      apiKey: profile.api_key,
      think: profile.think,
      maxTokens: profile.max_tokens,
      messages: history,
    });
  } catch (err) {
    activeChatMessages[activeChatMessages.length - 1].content = "Error: " + String(err);
    finishStreaming();
  }
}

function finishStreaming() {
  streamingChatId = null;
  streamingText = "";
  sendingMessage = false;
  chatSendBtn.disabled = false;
  setPipState("chat_idle");
  renderMessages();
  persistActiveChat();
}

listen("chat-stream-chunk", (event) => {
  const { chat_id, delta } = event.payload;
  if (chat_id !== streamingChatId) return;
  // Plain text, not markdownToHtml — re-parsing markdown on every token
  // was the main source of the streaming feeling choppy (a fresh regex pass
  // + DOM rebuild per chunk). Full markdown formatting is applied once,
  // after the reply finishes (see finishStreaming's renderMessages() call).
  const wasNearBottom = chatNearBottom();
  streamingText += delta;
  const last = activeChatMessages[activeChatMessages.length - 1];
  if (last && last.role === "assistant") last.content = streamingText;
  const bubble = lastBubbleEl();
  if (bubble) {
    bubble.textContent = streamingText;
    if (wasNearBottom) chatMessagesEl.scrollTop = chatMessagesEl.scrollHeight;
  }
});

listen("chat-stream-done", (event) => {
  const { chat_id, full_text } = event.payload;
  if (chat_id !== streamingChatId) return;
  const last = activeChatMessages[activeChatMessages.length - 1];
  if (last && last.role === "assistant") last.content = full_text || streamingText;
  finishStreaming();
});

// A tool round is a second full request to the model with a page fetch in
// between — measured at 20-40s on a local 9B model. Without something on
// screen that whole stretch is a frozen bubble, which reads as a crash.
listen("chat-tool-start", (event) => {
  const { chat_id, tool, arguments: args } = event.payload;
  if (chat_id !== streamingChatId) return;
  // Whatever the model said before asking for the tool ("let me look that
  // up") is preamble, not the answer: chat-stream-done replaces the bubble
  // with the final text anyway, so clearing here stops the two from being
  // visibly concatenated while the tool runs.
  streamingText = "";
  const detail = tool === "web_search" ? args?.query : args?.url;
  const label =
    tool === "web_search"
      ? "Searching the web"
      : tool === "fetch_url"
        ? "Reading the page"
        : tool;
  const last = activeChatMessages[activeChatMessages.length - 1];
  if (last && last.role === "assistant") last.content = "";
  const bubble = lastBubbleEl();
  if (bubble) {
    bubble.innerHTML =
      '<span class="chat-tool-running">' +
      escapeHtml(label) +
      (detail ? " — " + escapeHtml(String(detail)) : "") +
      "</span>";
    if (chatNearBottom()) chatMessagesEl.scrollTop = chatMessagesEl.scrollHeight;
  }
});

listen("chat-tool-done", (event) => {
  const { chat_id, sources } = event.payload;
  if (chat_id !== streamingChatId) return;
  if (!sources?.length) return;
  // Keyed to the assistant message being written, which is the last one — the
  // same indexing recalledByIndex uses. Appended rather than replaced: two
  // rounds (search, then read one of the results) both belong to this answer.
  const index = activeChatMessages.length - 1;
  const existing = webSourcesByIndex.get(index) || [];
  const seen = new Set(existing.map((s) => s.url));
  webSourcesByIndex.set(index, existing.concat(sources.filter((s) => !seen.has(s.url))));
});

// Opened through the opener plugin, never as a link: a bare href inside a
// webview navigates the app's own window away from itself.
chatMessagesEl.addEventListener("click", (event) => {
  const button = event.target.closest(".chat-source");
  if (!button) return;
  const url = button.dataset.url;
  if (url) invoke("open_in_browser", { url }).catch((err) => showToast(String(err)));
});

listen("chat-stream-error", (event) => {
  const { chat_id, error } = event.payload;
  if (chat_id !== streamingChatId) return;
  const last = activeChatMessages[activeChatMessages.length - 1];
  if (last && last.role === "assistant") {
    last.content = (streamingText ? streamingText + "\n\n" : "") + "⚠ " + error;
  }
  finishStreaming();
});

// The voice assistant writes its turns into a per-day "Voice" conversation from
// the mascot window (ai/chat.rs's record_voice_turn), so a Chat window that is
// already open has no other way to learn its history list went stale — or that
// the very conversation on screen just grew.
listen("voice-turn-recorded", async (event) => {
  await loadChatsList();
  if (event.payload?.chat_id && event.payload.chat_id === activeChatId) {
    await openChat(activeChatId);
  }
});

el("chatAttachBtn").addEventListener("click", attachFile);
// Delegated, because the cards are rebuilt on every change.
el("chatAttachments").addEventListener("click", (event) => {
  const button = event.target.closest(".chat-attachment-remove");
  if (!button) return;
  pendingAttachments.splice(Number(button.dataset.index), 1);
  renderPendingAttachments();
});
el("newChatBtn").addEventListener("click", newChat);
el("chatDeleteBtn").addEventListener("click", () => {
  if (activeChatId) deleteChat(activeChatId);
});

// Debounced, because every keystroke would otherwise run an FTS query. 180ms is
// below the point a search feels laggy and above a fast typist's gap between
// letters, so a word costs one query rather than one per letter.
let chatSearchTimer = null;
el("chatSearchInput").addEventListener("input", (event) => {
  const raw = event.target.value;
  clearTimeout(chatSearchTimer);
  chatSearchTimer = setTimeout(() => runChatSearch(raw), 180);
});
el("chatSearchInput").addEventListener("keydown", (event) => {
  if (event.key !== "Escape") return;
  event.target.value = "";
  clearTimeout(chatSearchTimer);
  runChatSearch("");
});

// Delegated on the message list because the rows are rebuilt wholesale on every
// render, so per-row listeners would be lost and leak.
chatMessagesEl.addEventListener("click", (event) => {
  const copy = event.target.closest(".chat-msg-copy");
  if (copy) {
    const message = activeChatMessages[Number(copy.dataset.index)];
    // The attachments go along, so a pasted message is the whole turn — the
    // model saw the files, and someone reading the paste should too.
    if (message) {
      bindCopyButton(copy, () =>
        [message.content, (message.attachments || []).filter((a) => a.text).map(inlineAttachment).join("\n\n")]
          .filter(Boolean)
          .join("\n\n")
      );
    }
    return;
  }
  const card = event.target.closest(".chat-msg-attachment.clickable");
  if (card) openAttachmentViewer(Number(card.dataset.message), Number(card.dataset.attachment));
});

el("attachmentViewerClose").addEventListener("click", () => el("attachmentViewer").close());
// Clicking the backdrop closes it — <dialog> reports those clicks as landing on
// the dialog element itself rather than on any of its children.
el("attachmentViewer").addEventListener("click", (event) => {
  if (event.target === el("attachmentViewer")) el("attachmentViewer").close();
});

el("chatCopyAllBtn").addEventListener("click", async (event) => {
  if (!activeChatMessages.length) return showToast("Kopyalanacak bir şey yok");
  // Labelled by role and separated by a rule, so a pasted conversation is
  // still readable as a conversation rather than as one run-on block.
  const transcript = activeChatMessages
    .map((m) => `## ${m.role === "user" ? "You" : "Assistant"}\n\n${m.content}`)
    .join("\n\n---\n\n");
  const title = chatTitleInput.value.trim();
  try {
    await copyText((title ? `# ${title}\n\n` : "") + transcript);
    showToast(`Konuşma kopyalandı (${activeChatMessages.length} mesaj)`);
  } catch (err) {
    console.error("copy failed", err);
    showToast("Kopyalanamadı — konsola bakın");
  }
  event.currentTarget.blur();
});
chatTitleInput.addEventListener("blur", () => {
  if (activeChatId) persistActiveChat();
});
el("chatInstructionsBtn").addEventListener("click", () => {
  chatInstructionsPanel.classList.toggle("visible");
});
el("closeChatInstructionsBtn").addEventListener("click", () => {
  chatInstructionsPanel.classList.remove("visible");
});
chatInstructionsInput.addEventListener("blur", () => {
  invoke("save_chat_instructions", { instructions: chatInstructionsInput.value.trim() });
});
chatComposerEl.addEventListener("submit", (e) => {
  e.preventDefault();
  sendChatMessage();
});
chatInput.addEventListener("input", autoResizeChatInput);
chatInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    sendChatMessage();
  }
});
