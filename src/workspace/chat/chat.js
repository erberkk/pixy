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
  chatModelPicker,
  chatModelBtn,
  chatModelLabel,
  chatModelMenu,
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

function profileName(profile) {
  return profile.label || profile.model || "(untitled)";
}

function renderModelSelect() {
  chatModelBtn.disabled = llmProfiles.length === 0;
  if (llmProfiles.length === 0) {
    chatModelLabel.textContent = "No model configured";
    chatModelMenu.innerHTML = "";
    return;
  }
  const active = llmProfiles.find((p) => p.id === activeLlmProfileId) || llmProfiles[0];
  chatModelLabel.textContent = profileName(active);
  chatModelMenu.innerHTML = llmProfiles
    .map(
      (p) =>
        '<button type="button" role="option" class="chat-model-option' +
        (p.id === active.id ? " selected" : "") +
        '" data-id="' +
        escapeAttr(p.id) +
        '" aria-selected="' +
        (p.id === active.id) +
        '">' +
        // The model name under the label, because two profiles are often the
        // same server with different models and the label alone cannot say so.
        "<span>" +
        escapeHtml(profileName(p)) +
        "</span>" +
        (p.label && p.model ? '<span class="chat-model-option-sub">' + escapeHtml(p.model) + "</span>" : "") +
        "</button>"
    )
    .join("");
}

function setModelMenuOpen(open) {
  chatModelPicker.classList.toggle("open", open);
  chatModelBtn.setAttribute("aria-expanded", String(open));
}

chatModelBtn.addEventListener("click", () => {
  setModelMenuOpen(!chatModelPicker.classList.contains("open"));
});

chatModelMenu.addEventListener("click", (event) => {
  const option = event.target.closest(".chat-model-option");
  if (!option) return;
  activeLlmProfileId = option.dataset.id;
  setModelMenuOpen(false);
  renderModelSelect();
  invoke("set_active_llm_profile", { profileId: activeLlmProfileId });
  // The picture that was fine a moment ago may be unreadable to the model just
  // picked, or the other way round.
  refreshVisionWarning();
});

// A native <select> closed itself on Escape and on a click elsewhere; a div has
// to be told. Captured on document rather than the picker so a click that lands
// on any other control closes it before that control reacts.
document.addEventListener("click", (event) => {
  if (!chatModelPicker.contains(event.target)) setModelMenuOpen(false);
});
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && chatModelPicker.classList.contains("open")) {
    setModelMenuOpen(false);
    chatModelBtn.focus();
  }
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
  generatedByIndex = new Map();
  chatTitleInput.value = chat.title || "";
  pendingAttachments = [];
  renderPendingAttachments();
  if (chat.profile_id && llmProfiles.some((p) => p.id === chat.profile_id)) {
    activeLlmProfileId = chat.profile_id;
    renderModelSelect();
  }
  // Opening a conversation shows its end, whatever the last one was scrolled to.
  renderMessages({ scroll: "bottom" });
  renderChatList();
  // Pictures are read back afterwards rather than before the first paint: the
  // conversation should appear at once, with each picture filling in as it
  // arrives.
  loadGeneratedImages();
}

function newChat() {
  activeChatId = null;
  activeChatMessages = [];
  recalledByIndex = new Map();
  generatedByIndex = new Map();
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

// Data URLs for generated pictures, by message index. The chat file stores only
// the path (see ChatMessage.image_path), so reopening a conversation refills
// this from disk — held here rather than on the message so the bytes are never
// what gets saved.
let generatedByIndex = new Map();

function generatedImageHtml(message, index) {
  if (!message.image_path) return "";
  const src = generatedByIndex.get(index);
  if (!src) {
    // Still being read off disk; the placeholder keeps the layout from jumping
    // when it arrives.
    return '<div class="chat-generated loading">Loading picture…</div>';
  }
  return (
    '<div class="chat-generated">' +
    '<img src="' +
    escapeAttr(src) +
    '" alt="" data-path="' +
    escapeAttr(message.image_path) +
    '" />' +
    '<div class="chat-generated-meta">' +
    escapeHtml(message.image_meta || "") +
    '<button type="button" class="chat-generated-open" data-path="' +
    escapeAttr(message.image_path) +
    '">Open folder</button>' +
    "</div>" +
    "</div>"
  );
}

// Fills generatedByIndex for a conversation that was just opened. Reads run in
// parallel and the list is re-rendered once at the end rather than per picture,
// so a chat with several does not repaint for each.
async function loadGeneratedImages() {
  const wanted = activeChatMessages
    .map((m, index) => [index, m.image_path])
    .filter(([index, path]) => path && !generatedByIndex.has(index));
  if (!wanted.length) return;
  const results = await Promise.all(
    wanted.map(([, path]) => invoke("read_generated_image", { path }).catch(() => null)),
  );
  let any = false;
  wanted.forEach(([index], i) => {
    if (results[i]) {
      generatedByIndex.set(index, results[i]);
      any = true;
    }
  });
  if (any) renderMessages();
}

// Read off the message rather than a side map, so reopening a conversation still
// shows what it was answered from — see ChatMessage.sources for why these are
// saved where the recall note beside them is not.
function webSourcesHtml(message) {
  const sources = message.sources;
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
      // Openable when there is something behind the card: a text attachment
      // carries its own contents, an image carries a path to the file it was
      // written to. Images attached before that path existed have neither, and
      // the card says so by not offering to open.
      const clickable = a.kind === "image" ? Boolean(a.path) : Boolean(a.text);
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
async function openAttachmentViewer(messageIndex, attachmentIndex) {
  const attachment = activeChatMessages[messageIndex]?.attachments?.[attachmentIndex];
  if (!attachment) return;
  const dialog = el("attachmentViewer");

  // A picture is shown, not fenced. Read from disk at open time rather than held
  // in memory: a conversation with a dozen screenshots in it would otherwise
  // carry all of them the whole time it is open, to show one on demand.
  if (attachment.kind === "image") {
    if (!attachment.path) return;
    el("attachmentViewerName").textContent = attachment.name;
    el("attachmentViewerMeta").textContent = "image";
    const src = await invoke("read_generated_image", { path: attachment.path }).catch(() => null);
    if (!src) {
      showToast("That picture is no longer on disk");
      return;
    }
    const img = document.createElement("img");
    img.className = "attachment-viewer-image";
    img.src = src;
    img.alt = attachment.name;
    el("attachmentViewerBody").replaceChildren(img);
    dialog.showModal();
    return;
  }

  if (!attachment.text) return;
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

// One message, as HTML. Split out of renderMessages so a single turn can be
// repainted without rebuilding the list — see renderLastMessage.
function messageHtml(m, index) {
  // An assistant turn with nothing in it yet is the gap between pressing send
  // and the first token, which on a local model is routinely ten seconds or
  // more. A blinking caret was the only sign anything was happening, and it
  // looks identical to a reply that has stalled.
  const isWaiting = m.role === "assistant" && !m.content && streamingChatId;
  // A /image turn carries a picture and no text, and the empty bubble it used to
  // get was a bordered box of nothing sitting on top of the image.
  const hasBubble = Boolean(m.content) || isWaiting || m.source === "voice";
  return (
    '<div class="chat-msg ' +
    m.role +
    (m.source === "voice" ? " voice" : "") +
    '">' +
    recallNoteHtml(index) +
    // Who is talking. Needed since the assistant's reply stopped being a bubble:
    // alignment alone distinguished them before, and a full-width answer has no
    // alignment to read. Only on the assistant — a right-aligned tinted bubble
    // is already unmistakably your own.
    (m.role === "assistant"
      ? '<div class="chat-msg-role">' +
        '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" ' +
        'stroke-linecap="round" stroke-linejoin="round">' +
        '<rect x="4" y="8" width="16" height="12" rx="3"/>' +
        '<path d="M12 4v4M9 14h.01M15 14h.01"/></svg>' +
        "<span>Assistant</span></div>"
      : "") +
    (hasBubble
      ? '<div class="chat-msg-bubble">' +
        (isWaiting
          ? '<span class="chat-thinking"><i></i><i></i><i></i></span>'
          : markdownToHtml(m.content)) +
        // Spoken turns are marked because they are read differently: a user
        // message is a speech-recognition guess rather than something typed
        // deliberately, so an odd-looking exchange is usually the transcript's
        // fault, not the model's.
        (m.source === "voice" ? '<span class="chat-msg-tag">voice</span>' : "") +
        "</div>"
      : "") +
    // Attached files as cards above the actions, not as text inside the bubble.
    // Clicking one opens its contents; the transcript stays about what was asked.
    messageAttachmentsHtml(m, index) +
    // A picture this turn produced, if it was a /image command.
    generatedImageHtml(m, index) +
    // Pages the model read to write this. Below the bubble, like the recall note
    // above it: neither is something the model said, both are why it knew.
    webSourcesHtml(m) +
    // Copies the message's own markdown source, not the rendered HTML — read
    // from activeChatMessages by index rather than scraped back out of the DOM,
    // so what lands on the clipboard is exactly what the model wrote, fences and
    // all.
    //
    // Retry and Edit are always in the markup and hidden by CSS while a reply is
    // streaming, rather than left out and added on the next render. Rendering
    // them conditionally is what forced a full rebuild of the list at the end of
    // every answer, and that rebuild is what threw away the reader's scroll
    // position and text selection.
    '<div class="chat-msg-actions">' +
    '<button class="chat-msg-copy" type="button" data-index="' +
    index +
    '" title="Copy this message">Copy</button>' +
    // Only on the last answer. Retrying an earlier one would have to throw away
    // every turn after it, which is a different and much more destructive action
    // than the word suggests.
    (m.role === "assistant" && index === activeChatMessages.length - 1
      ? '<button class="chat-msg-action" type="button" data-retry="1" ' +
        'title="Ask the model again, same question">Retry</button>'
      : "") +
    // On the question rather than the answer: a bad reply is usually a badly-put
    // question, and retyping it by hand was the only way to change one. Editing
    // drops this turn and everything after it — said out loud in the tooltip,
    // because that is not recoverable.
    (m.role === "user"
      ? '<button class="chat-msg-action" type="button" data-edit="' +
        index +
        '" title="Put this back in the box and drop everything after it">Edit</button>'
      : "") +
    "</div>" +
    "</div>"
  );
}

// Rebuilds the whole transcript. Callers say whether the view should follow the
// bottom, because doing it unconditionally is a bug: the streaming path already
// checks chatNearBottom() before scrolling, and then this threw that away by
// jumping anyway on the render at the end of the turn — so reading something
// further up got yanked to the end the moment the answer finished.
function renderMessages({ scroll = "auto" } = {}) {
  // Markdown-rendered (tables/bold/code fences/etc, see markdownToHtml — shared
  // with the notes preview) — only reached once per full render, not per
  // streamed token (see the chat-stream-chunk listener below), so re-parsing
  // markdown here never competes with a token actually arriving.
  const follow = scroll === "bottom" || (scroll === "auto" && chatNearBottom());
  chatMessagesEl.innerHTML = activeChatMessages.map(messageHtml).join("");
  chatMainEl.classList.toggle("has-active", activeChatMessages.length > 0);
  if (follow) scrollToBottom();
  updateScrollDownButton();
}

// Repaints only the final turn, leaving every earlier node — and so the
// selection inside it, and the scroll position — untouched. This is what the end
// of a reply uses: everything that changes then (the markdown, the sources, the
// Retry button) belongs to that one message.
function renderLastMessage() {
  const index = activeChatMessages.length - 1;
  const node = chatMessagesEl.lastElementChild;
  if (index < 0 || !node) {
    renderMessages();
    return;
  }
  const follow = chatNearBottom();
  node.outerHTML = messageHtml(activeChatMessages[index], index);
  if (follow) scrollToBottom();
  updateScrollDownButton();
}

function scrollToBottom() {
  chatMessagesEl.scrollTop = chatMessagesEl.scrollHeight;
}

// Shown whenever the end is off screen — including while a reply streams into a
// part of the transcript the reader has scrolled away from, which is exactly
// when it is most wanted.
function updateScrollDownButton() {
  el("chatScrollDown").classList.toggle("visible", !chatNearBottom());
}

chatMessagesEl.addEventListener("scroll", updateScrollDownButton);
el("chatScrollDown").addEventListener("click", () => {
  chatMessagesEl.scrollTo({ top: chatMessagesEl.scrollHeight, behavior: "smooth" });
});

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
  refreshVisionWarning();
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

// Whether the selected model can read images, as far as its server will say.
// Only Ollama answers this; anything else leaves `known` false and no warning
// is shown, because a warning on a setup that actually works teaches the user
// to ignore warnings.
let visionWarning = "";

async function refreshVisionWarning() {
  const previous = visionWarning;
  visionWarning = "";
  const profile = llmProfiles.find((p) => p.id === activeLlmProfileId);
  if (profile && pendingAttachments.some((a) => a.kind === "image")) {
    const caps = await invoke("get_model_capabilities", {
      baseUrl: profile.base_url,
      model: profile.model,
      apiKey: profile.api_key || "",
    }).catch(() => null);
    if (caps?.known && !caps.vision) {
      visionWarning =
        (profile.label || profile.model) +
        " cannot read images — it will only see your text. Pick a model with vision to ask about this picture.";
    }
  }
  // Re-render only on a change, so the check (which can involve a request)
  // cannot loop through the render that triggered it.
  if (visionWarning !== previous) renderPendingAttachments();
}

function renderPendingAttachments() {
  const box = el("chatAttachments");
  if (!pendingAttachments.length) {
    box.style.display = "none";
    box.innerHTML = "";
    visionWarning = "";
    return;
  }
  box.style.display = "flex";
  const warning = visionWarning
    ? '<div class="chat-attachment-warning">' + escapeHtml(visionWarning) + "</div>"
    : "";
  box.innerHTML = warning + pendingAttachments
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

// Drawing is a direct action, not something the model decides to do — see the
// comment at the top of ai/images.rs. "/image a red fox" is unambiguous, so it
// goes straight to the image server; routing it through the model would add a
// full round of generation to interpret a request that needs no interpreting,
// and hand back a file path the model cannot look at anyway.
const IMAGE_COMMAND = /^\/(image|görsel|gorsel)\s+/i;

async function generateImageTurn(prompt) {
  activeChatMessages.push({ role: "user", content: "/image " + prompt, ts: Date.now() });
  activeChatMessages.push({ role: "assistant", content: "", ts: Date.now() });
  const index = activeChatMessages.length - 1;
  renderMessages({ scroll: "bottom" });
  const bubble = lastBubbleEl();
  if (bubble) {
    bubble.innerHTML = '<span class="chat-tool-running">Drawing — ' + escapeHtml(prompt) + "</span>";
  }
  chatMessagesEl.scrollTop = chatMessagesEl.scrollHeight;

  try {
    const image = await invoke("generate_image", { prompt });
    // The bytes go in the message only for this session; what is saved is the
    // path (see ai/images.rs). Chat files hold no image data by design.
    generatedByIndex.set(index, image.data_url);
    activeChatMessages[index].content = "";
    activeChatMessages[index].image_path = image.path;
    // The seed is part of the record, not decoration: it is the only way back to
    // a picture you liked. Absent when the server was a generic one that does not
    // take a seed, in which case saying nothing is honest — see GeneratedImage.
    activeChatMessages[index].image_meta =
      image.width +
      "×" +
      image.height +
      " · " +
      image.seconds.toFixed(1) +
      "s" +
      (image.seed === null || image.seed === undefined ? "" : " · seed " + image.seed);
  } catch (err) {
    activeChatMessages[index].content = "⚠ " + String(err);
  }
  renderMessages();
  persistActiveChat();
}

async function sendChatMessage() {
  const text = chatInput.value.trim();
  const attachments = pendingAttachments;
  if ((!text && !attachments.length) || sendingMessage) return;

  if (IMAGE_COMMAND.test(text)) {
    chatInput.value = "";
    chatInput.style.height = "auto";
    await generateImageTurn(text.replace(IMAGE_COMMAND, "").trim());
    return;
  }

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
  // file without bound, and a later turn cannot re-send it anyway. The bytes are
  // written to the pictures folder instead and the message keeps the path, so
  // the card in the transcript still opens the picture a week later. A failed
  // write costs the preview, not the message: the turn goes out either way.
  const storedAttachments = await Promise.all(
    attachments.map(async (a) => ({
      name: a.name,
      lang: a.lang,
      kind: a.kind,
      text: a.kind === "image" ? "" : a.data,
      full_chars: a.full_chars,
      path:
        a.kind === "image"
          ? await invoke("save_attached_image", { name: a.name, data: a.data }).catch(() => "")
          : "",
    }))
  );

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
  // Following the bottom is right here and only here: you just pressed send, so
  // the message you are looking for is the one at the end.
  renderMessages({ scroll: "bottom" });
  await persistActiveChat();

  await runAssistantTurn(profile, { wireContent, recallPromise });
}

// Streams one reply into a fresh assistant message, for whatever state
// activeChatMessages is in — which must already end with the turn to answer.
//
// Split out of sendChatMessage because sending, retrying and editing all do
// exactly this once the transcript says what the user wants answered; the only
// thing that differs is how it got into that state.
async function runAssistantTurn(profile, { wireContent, recallPromise } = {}) {
  sendingMessage = true;
  updateComposerState();
  setPipState("chat_typing");

  const chatId = activeChatId;
  streamingChatId = chatId;
  streamingText = "";

  // Earlier turns are rebuilt WITH their attachments inlined again: the files
  // live beside those messages rather than in them, so sending only `content`
  // would quietly drop every file from the conversation after the turn it was
  // attached to — the model would answer "as we discussed in that file" having
  // never seen it twice.
  const history = activeChatMessages.map((m) => ({
    role: m.role,
    content: [m.content, (m.attachments || []).filter((a) => a.text).map(inlineAttachment).join("\n\n")]
      .filter(Boolean)
      .join("\n\n"),
  }));
  // The turn being sent right now may carry images, which are content parts
  // rather than text and exist only for this request (see storedAttachments).
  // A retry has no override: the bytes were never saved, so it re-asks the
  // question with the picture described only by its filename marker.
  if (wireContent !== undefined) history[history.length - 1].content = wireContent;

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
  const recalled = recallPromise ? await recallPromise : null;
  if (recalled?.block) history.unshift({ role: "system", content: recalled.block });
  // Attached to the answer this recall was for — activeChatMessages is about to
  // gain the assistant turn, so its index is the current length.
  if (recalled?.hits?.length) recalledByIndex.set(activeChatMessages.length, recalled.hits);

  // Unshifted last so the user's own instructions stay the first thing the model
  // reads — recalled history is context, not a persona.
  const systemPrompt = chatInstructionsInput.value.trim();
  if (systemPrompt) history.unshift({ role: "system", content: systemPrompt });
  activeChatMessages.push({ role: "assistant", content: "", ts: Date.now() });
  renderMessages({ scroll: "bottom" });
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

// Asks the model to answer the same question again, replacing the reply that is
// there. The old answer is dropped rather than kept beside the new one: this is
// for when the reply was wrong, and a transcript that accumulates every rejected
// attempt is worse to read than the one good answer.
async function retryLastAnswer() {
  if (sendingMessage) return;
  const profile = llmProfiles.find((p) => p.id === activeLlmProfileId) || llmProfiles[0];
  if (!profile) {
    showToast("No LLM configured — add one in Settings");
    return;
  }
  if (activeChatMessages.at(-1)?.role !== "assistant") return;
  activeChatMessages.pop();
  // The indexes of the side maps still point at the answer just removed; the new
  // one lands at the same position, so its own recall note replaces this.
  recalledByIndex.delete(activeChatMessages.length);
  renderMessages();
  await runAssistantTurn(profile);
}

// Puts a question back in the composer to be rewritten, and drops it along with
// everything said after it.
//
// Truncating is the point rather than a side effect: the model answers the whole
// transcript, so an edited question left sitting above its own old answer would
// be asked in the presence of a reply to the question it no longer is.
function editMessage(index) {
  if (sendingMessage) return;
  const message = activeChatMessages[index];
  if (!message || message.role !== "user") return;
  chatInput.value = message.content;
  activeChatMessages = activeChatMessages.slice(0, index);
  renderMessages();
  persistActiveChat();
  autoResizeChatInput();
  chatInput.focus();
  // To the end, not selected: this is a message to amend, and a selection would
  // make the first keystroke delete it.
  chatInput.setSelectionRange(chatInput.value.length, chatInput.value.length);
  if (message.attachments?.length) {
    showToast("Attachments aren't carried over — attach them again if they matter");
  }
}

// Stops the reply mid-flight, keeping what has arrived (see llm.rs's
// cancel_chat_message). The composer is NOT reset here: the backend still
// answers with chat-stream-done, and letting that one path finish the turn is
// what keeps a stop from leaving the box disabled forever.
function stopGenerating() {
  if (!sendingMessage || !streamingChatId) return;
  invoke("cancel_chat_message", { chatId: streamingChatId });
  chatSendBtn.disabled = true; // no second press while the stop lands
}

function finishStreaming() {
  streamingChatId = null;
  streamingText = "";
  sendingMessage = false;
  chatSendBtn.disabled = false;
  updateComposerState();
  setPipState("chat_idle");
  // Only the turn that just finished — see renderLastMessage. Repainting the
  // whole list here is what used to throw away the reader's place and any text
  // they had selected.
  renderLastMessage();
  persistActiveChat();
}

// The send button doubles as the stop button, rather than a second control that
// is dead most of the time: while a reply is streaming, sending is exactly what
// you cannot do, and stopping is the only thing you might want.
const SEND_ICON = chatSendBtn.innerHTML;
const STOP_ICON =
  '<svg viewBox="0 0 24 24" fill="currentColor" stroke="none"><rect x="7" y="7" width="10" height="10" rx="2"/></svg>';

function updateComposerState() {
  chatSendBtn.classList.toggle("is-stop", sendingMessage);
  chatSendBtn.title = sendingMessage ? "Stop generating" : "Send";
  chatSendBtn.innerHTML = sendingMessage ? STOP_ICON : SEND_ICON;
  // Retry and Edit are in the markup at all times and hidden from here, so that
  // finishing a reply does not have to re-render the list just to add them.
  chatMessagesEl.classList.toggle("is-streaming", sendingMessage);
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
  const { chat_id, full_text, stopped } = event.payload;
  if (chat_id !== streamingChatId) return;
  const last = activeChatMessages[activeChatMessages.length - 1];
  if (last && last.role === "assistant") {
    last.content = full_text || streamingText;
    // Marked in the text itself rather than as a separate field on the message:
    // it has to survive being saved and reopened, and a half-sentence with no
    // explanation reads as the model having failed rather than as you having
    // stopped it. An answer stopped before it said anything gets the note alone,
    // which is the only thing that distinguishes it from an empty reply.
    if (stopped) last.content = (last.content ? last.content + "\n\n" : "") + "_⏹ stopped_";
  }
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
  // Written onto the assistant message being answered — the last one — so the
  // save at the end of the turn carries it. Appended rather than replaced: two
  // rounds (search, then read one of the results) both belong to this answer.
  const message = activeChatMessages[activeChatMessages.length - 1];
  if (!message) return;
  const existing = message.sources || [];
  const seen = new Set(existing.map((s) => s.url));
  message.sources = existing.concat(sources.filter((s) => !seen.has(s.url)));
});

// Opened through the opener plugin, never as a link: a bare href inside a
// webview navigates the app's own window away from itself.
chatMessagesEl.addEventListener("click", (event) => {
  const source = event.target.closest(".chat-source");
  if (source?.dataset.url) {
    invoke("open_in_browser", { url: source.dataset.url }).catch((err) => showToast(String(err)));
    return;
  }
  // A generated picture: the button reveals the file, clicking the picture
  // itself opens it full size in the system viewer.
  const reveal = event.target.closest(".chat-generated-open");
  const picture = event.target.closest(".chat-generated img");
  const path = reveal?.dataset.path || picture?.dataset.path;
  if (path) {
    invoke("open_generated_image", { path }).catch((err) => showToast(String(err)));
    return;
  }
  // Delegated like everything else here: the transcript is rebuilt from scratch
  // on every render, so per-button listeners would have to be re-attached each
  // time and would leak the ones belonging to messages that no longer exist.
  if (event.target.closest("[data-retry]")) {
    retryLastAnswer();
    return;
  }
  const edit = event.target.closest("[data-edit]");
  if (edit) editMessage(Number(edit.dataset.edit));
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
  refreshVisionWarning();
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
  // The one button, two jobs — see updateComposerState.
  if (sendingMessage) stopGenerating();
  else sendChatMessage();
});

// A screenshot lives in the clipboard as a bitmap with no file behind it, so the
// attach button — which opens a file picker — could never reach one. Text paste
// is left completely alone: the handler only takes over when the clipboard holds
// an image and no text at all, which is what Win+Shift+S and the clipboard
// history put there. Copying from a page usually carries text/plain alongside
// the picture, and pasting that should still paste the text.
chatInput.addEventListener("paste", async (event) => {
  const items = [...(event.clipboardData?.items || [])];
  if (items.some((item) => item.type === "text/plain")) return;
  const images = items.filter((item) => item.type.startsWith("image/"));
  if (!images.length) return;
  event.preventDefault();

  for (const item of images) {
    const blob = item.getAsFile();
    if (!blob) continue;
    const base64 = await blobToBase64(blob);
    if (!base64) continue;
    pendingAttachments.push({
      // Named for when it was taken, because there is no filename to inherit and
      // "image.png" three times over tells you nothing about which is which.
      name: `pasted-${new Date().toTimeString().slice(0, 8).replace(/:/g, "")}.${
        blob.type.split("/")[1] || "png"
      }`,
      kind: "image",
      mime: blob.type,
      data: base64,
      problem: "",
      lang: "",
      // Only meaningful for text, where it is how truncation is detected.
      full_chars: 0,
    });
  }
  renderPendingAttachments();
  refreshVisionWarning();
});

// The data: URL prefix is stripped because everything downstream — the card
// thumbnail, the image_url content part — builds its own from `mime`.
function blobToBase64(blob) {
  return new Promise((resolve) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result).split(",")[1] || "");
    reader.onerror = () => resolve("");
    reader.readAsDataURL(blob);
  });
}

chatInput.addEventListener("input", autoResizeChatInput);
chatInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    if (!sendingMessage) sendChatMessage();
  }
});
