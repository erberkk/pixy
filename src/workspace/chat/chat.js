// Chat mode: a direct conversation with a locally-configured LLM, backed by
// ai/chat.rs (one JSON file per conversation) and ai/llm.rs's streaming
// send_chat_message. Unrelated to the Claude Code hook plumbing in
// agent/server.rs — this is a plain user <-> local-model chat.
import { initPip, setPipState } from "../../mascot/pip/pip.js";
import { invoke, listen } from "../../shared/tauri.js";
import { escapeHtml, markdownToHtml } from "../../shared/markdown.js";
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
let pendingAttachment = null; // {name, kind: "image"|"text"|"unsupported", mime, data} from pick_chat_attachment

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

function renderChatList() {
  const count = chats.length;
  notesCount.textContent = count + (count === 1 ? " chat" : " chats");

  if (count === 0) {
    chatListEl.innerHTML = '<div class="chat-empty-list">No chats yet.<br>Start a new conversation.</div>';
    return;
  }

  chatListEl.innerHTML = chats
    .map(
      (c) =>
        '<div class="chat-item ' +
        (c.id === activeChatId ? "active" : "") +
        '" data-id="' +
        c.id +
        '">' +
        '<div class="chat-item-title">' +
        escapeHtml(c.title || "New chat") +
        "</div>" +
        '<span class="chat-item-meta">' +
        timeAgo(c.updated_at) +
        " · " +
        c.message_count +
        (c.message_count === 1 ? " msg" : " msgs") +
        "</span>" +
        '<button class="chat-item-delete" data-id="' +
        c.id +
        '" title="Delete chat">' +
        '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 6h18M8 6V4a2 2 0 012-2h4a2 2 0 012 2v2m3 0l-1 14a2 2 0 01-2 2H8a2 2 0 01-2-2L5 6h14z"/><path d="M10 11v6M14 11v6"/></svg>' +
        "</button>" +
        "</div>"
    )
    .join("");

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
  chatTitleInput.value = chat.title || "";
  pendingAttachment = null;
  renderPendingAttachment();
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
  chatTitleInput.value = "";
  pendingAttachment = null;
  renderPendingAttachment();
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

function renderMessages() {
  // Markdown-rendered (tables/bold/code fences/etc, see markdownToHtml —
  // shared with the notes preview) — only reached once per full render, not
  // per streamed token (see the chat-stream-chunk listener below), so
  // re-parsing markdown here never competes with a token actually arriving.
  chatMessagesEl.innerHTML = activeChatMessages
    .map(
      (m) =>
        '<div class="chat-msg ' +
        m.role +
        (m.source === "voice" ? " voice" : "") +
        '"><div class="chat-msg-bubble">' +
        markdownToHtml(m.content) +
        // Spoken turns are marked because they are read differently: a user
        // message is a speech-recognition guess rather than something typed
        // deliberately, so an odd-looking exchange is usually the transcript's
        // fault, not the model's.
        (m.source === "voice" ? '<span class="chat-msg-tag">voice</span>' : "") +
        "</div></div>"
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
// until the next send. Only images and plain-text-ish files are usable
// right now; PDF/Excel/other binary formats come back "unsupported" since
// parsing those would need a real parser this app doesn't have yet.
async function attachFile() {
  const attachment = await invoke("pick_chat_attachment");
  if (!attachment) return;
  if (attachment.kind === "unsupported") {
    showToast("Bu dosya türü henüz desteklenmiyor (örn. PDF/Excel) — sadece resim ve düz metin dosyaları eklenebilir");
    return;
  }
  pendingAttachment = attachment;
  renderPendingAttachment();
}

function renderPendingAttachment() {
  const chip = el("chatAttachmentChip");
  if (!pendingAttachment) {
    chip.style.display = "none";
    return;
  }
  chip.style.display = "flex";
  el("chatAttachmentName").textContent = pendingAttachment.name;
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

async function sendChatMessage() {
  const text = chatInput.value.trim();
  const attachment = pendingAttachment;
  if ((!text && !attachment) || sendingMessage) return;
  const profile = llmProfiles.find((p) => p.id === activeLlmProfileId) || llmProfiles[0];
  if (!profile) {
    showToast("No LLM configured — add one in Settings");
    return;
  }

  pendingAttachment = null;
  renderPendingAttachment();

  // What actually gets sent to the model for THIS turn (may be an
  // OpenAI-style content-parts array with the image inlined as a data URI,
  // or the attached text file's contents inlined as a fenced code block) vs.
  // what gets stored/shown permanently (plain text only — an image's bytes
  // are never written to the saved chat file, so history doesn't balloon
  // with base64 forever; a lightweight "📎 filename" marker stands in for
  // it instead, meaning later turns no longer have the image in context).
  let wireContent = text;
  let displayContent = text;
  if (attachment?.kind === "image") {
    wireContent = [
      { type: "text", text: text || "What's in this image?" },
      { type: "image_url", image_url: { url: `data:${attachment.mime};base64,${attachment.data}` } },
    ];
    displayContent = (text ? text + "\n\n" : "") + `📎 ${attachment.name}`;
  } else if (attachment?.kind === "text") {
    const inlined = "```" + attachment.name + "\n" + attachment.data + "\n```";
    wireContent = text ? text + "\n\n" + inlined : inlined;
    displayContent = wireContent;
  }

  activeChatMessages.push({ role: "user", content: displayContent, ts: Date.now() });
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

  const history = activeChatMessages.slice(0, -1).map((m) => ({ role: m.role, content: m.content }));
  history.push({ role: "user", content: wireContent });
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
el("chatAttachRemoveBtn").addEventListener("click", () => {
  pendingAttachment = null;
  renderPendingAttachment();
});
el("newChatBtn").addEventListener("click", newChat);
el("chatDeleteBtn").addEventListener("click", () => {
  if (activeChatId) deleteChat(activeChatId);
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
