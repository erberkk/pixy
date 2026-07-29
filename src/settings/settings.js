import { invoke } from "../shared/tauri.js";

function el(id) {
  return document.getElementById(id);
}

function setStatus(box, kind, text) {
  box.className = `status visible ${kind}`;
  box.textContent = text;
}

function clearStatus(box) {
  box.className = "status";
  box.textContent = "";
}

// Shared by the LLM/STT/TTS sections' "Start now" buttons — spawns
// start_command if base_url isn't already reachable (see ai/llm.rs's
// start_server_now; never touches a server already running, whether the
// widget started it or the user did outside the widget).
async function startServerNow(statusEl, baseUrl, startCommand) {
  if (!startCommand.trim()) {
    setStatus(statusEl, "error", "No start command entered.");
    return;
  }
  setStatus(statusEl, "pending", "Starting…");
  try {
    const message = await invoke("start_server_now", { baseUrl, startCommand });
    setStatus(statusEl, "ok", message);
  } catch (err) {
    setStatus(statusEl, "error", String(err));
  }
}

function setupNav() {
  const items = document.querySelectorAll(".hub-nav-item");
  items.forEach((item) => {
    item.addEventListener("click", () => {
      items.forEach((i) => i.classList.remove("active"));
      item.classList.add("active");
      document.querySelectorAll(".hub-section").forEach((s) => s.classList.remove("active"));
      el(`section-${item.dataset.section}`).classList.add("active");
    });
  });
}

// ---------- LLM section ----------
// Multiple named profiles (base_url/model/api_key/think/max_tokens each) so
// the Chat window can offer a model picker instead of the widget only ever
// knowing one endpoint. autostart/start_command stay global (they start a
// single local runtime process, independent of which profile talks to it).

let llmProfiles = [];
let activeProfileId = ""; // last profile the chat UI used — only touched here if it gets deleted
let editingProfileId = null; // profile currently loaded into the form; null = unsaved new profile

function updateStartCommandEnabled() {
  el("startCommandField").classList.toggle("disabled", !el("autostart").checked);
}

function renderProfileList() {
  const container = el("llmProfileList");
  container.innerHTML = "";

  if (llmProfiles.length === 0) {
    const empty = document.createElement("div");
    empty.className = "llm-profile-empty";
    empty.textContent = "No profiles yet — fill in the form below and save one.";
    container.appendChild(empty);
    return;
  }

  for (const p of llmProfiles) {
    const row = document.createElement("div");
    row.className = `llm-profile-row${p.id === editingProfileId ? " editing" : ""}`;

    const label = document.createElement("span");
    label.className = "llm-profile-row-label";
    label.textContent = p.label || "(untitled)";

    const model = document.createElement("span");
    model.className = "llm-profile-row-model";
    model.textContent = p.model || "";

    const del = document.createElement("button");
    del.className = "llm-profile-row-del";
    del.textContent = "×";
    del.title = "Delete profile";
    del.addEventListener("click", (e) => {
      e.stopPropagation();
      deleteProfile(p.id);
    });

    row.appendChild(label);
    row.appendChild(model);
    row.appendChild(del);
    row.addEventListener("click", () => loadProfileIntoForm(p.id));
    container.appendChild(row);
  }
}

function loadProfileIntoForm(id) {
  const p = llmProfiles.find((x) => x.id === id);
  if (!p) return;
  editingProfileId = p.id;
  el("profileLabel").value = p.label || "";
  el("baseUrl").value = p.base_url || "";
  el("model").value = p.model || "";
  el("apiKey").value = p.api_key || "";
  el("think").checked = !!p.think;
  el("maxTokens").value = p.max_tokens || 600;
  clearStatus(el("llmStatus"));
  renderProfileList();
}

function newProfileForm() {
  editingProfileId = null;
  el("profileLabel").value = "";
  el("baseUrl").value = "http://localhost:11434/v1";
  el("model").value = "";
  el("apiKey").value = "";
  el("think").checked = false;
  el("maxTokens").value = 600;
  clearStatus(el("llmStatus"));
  renderProfileList();
}

async function loadLlmConfig() {
  const settings = await invoke("get_llm_settings");
  llmProfiles = settings.profiles || [];
  activeProfileId = settings.active_profile_id || "";
  el("autostart").checked = !!settings.autostart;
  el("startCommand").value = settings.start_command || "";
  updateStartCommandEnabled();

  if (llmProfiles.length > 0) {
    loadProfileIntoForm(activeProfileId && llmProfiles.some((p) => p.id === activeProfileId) ? activeProfileId : llmProfiles[0].id);
  } else {
    newProfileForm();
  }
}

function currentFormFields() {
  return {
    label: el("profileLabel").value.trim(),
    base_url: el("baseUrl").value.trim(),
    model: el("model").value.trim(),
    api_key: el("apiKey").value.trim(),
    think: el("think").checked,
    max_tokens: parseInt(el("maxTokens").value, 10) || 600,
  };
}

async function persistLlmSettings() {
  await invoke("save_llm_settings", {
    settings: {
      profiles: llmProfiles,
      active_profile_id: activeProfileId,
      autostart: el("autostart").checked,
      start_command: el("startCommand").value.trim(),
    },
  });
}

async function testLlmConnection() {
  const { base_url, model, api_key, think, max_tokens } = currentFormFields();
  const status = el("llmStatus");
  if (!base_url || !model) {
    setStatus(status, "error", "Base URL and model name are required.");
    return;
  }
  const btn = el("llmTestBtn");
  btn.disabled = true;
  setStatus(status, "pending", "Connecting…");
  try {
    const reply = await invoke("test_llm_connection", {
      baseUrl: base_url,
      model,
      apiKey: api_key,
      think,
      maxTokens: max_tokens,
    });
    setStatus(status, "ok", `Working. Model replied: "${reply.trim()}"`);
  } catch (err) {
    setStatus(status, "error", String(err));
  } finally {
    btn.disabled = false;
  }
}

async function saveProfile() {
  const fields = currentFormFields();
  if (!fields.base_url || !fields.model) {
    setStatus(el("llmStatus"), "error", "Base URL and model name are required.");
    return;
  }
  const btn = el("llmSaveBtn");
  btn.disabled = true;
  try {
    const id = editingProfileId || crypto.randomUUID();
    const profile = { id, ...fields };
    const idx = llmProfiles.findIndex((p) => p.id === id);
    if (idx >= 0) llmProfiles[idx] = profile;
    else llmProfiles.push(profile);
    if (!activeProfileId) activeProfileId = id;
    editingProfileId = id;

    await persistLlmSettings();
    renderProfileList();
    setStatus(el("llmStatus"), "ok", "Saved.");
  } catch (err) {
    setStatus(el("llmStatus"), "error", String(err));
  } finally {
    btn.disabled = false;
  }
}

async function deleteProfile(id) {
  llmProfiles = llmProfiles.filter((p) => p.id !== id);
  if (activeProfileId === id) activeProfileId = llmProfiles[0]?.id || "";
  try {
    await persistLlmSettings();
  } catch (err) {
    setStatus(el("llmStatus"), "error", String(err));
  }
  if (editingProfileId === id) {
    if (llmProfiles.length > 0) loadProfileIntoForm(llmProfiles[0].id);
    else newProfileForm();
  } else {
    renderProfileList();
  }
}

// ---------- GitHub section ----------

async function loadGithubConfig() {
  const cfg = await invoke("get_github_config");
  el("token").value = cfg.token || "";
  return cfg.token || "";
}

async function testGithubConnection() {
  const token = el("token").value.trim();
  const status = el("githubStatus");
  if (!token) {
    setStatus(status, "error", "Token is required.");
    return;
  }
  const btn = el("githubTestBtn");
  btn.disabled = true;
  setStatus(status, "pending", "Connecting…");
  try {
    const reply = await invoke("test_github_connection", { token });
    setStatus(status, "ok", reply);
  } catch (err) {
    setStatus(status, "error", String(err));
  } finally {
    btn.disabled = false;
  }
}

async function saveGithubConfig() {
  const token = el("token").value.trim();
  const btn = el("githubSaveBtn");
  btn.disabled = true;
  try {
    await invoke("save_github_config", { token });
    setStatus(el("githubStatus"), "ok", "Saved.");
    refreshReport();
  } catch (err) {
    setStatus(el("githubStatus"), "error", String(err));
  } finally {
    btn.disabled = false;
  }
}

function relativeTime(isoString) {
  const then = new Date(isoString).getTime();
  if (Number.isNaN(then)) return "";
  const diffMs = Date.now() - then;
  const minutes = Math.round(diffMs / 60000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  if (days < 30) return `${days}d ago`;
  const months = Math.round(days / 30);
  return `${months}mo ago`;
}

function renderList(containerId, items, emptyText) {
  const container = el(containerId);
  container.innerHTML = "";

  if (!items || items.length === 0) {
    const empty = document.createElement("div");
    empty.className = "report-empty";
    empty.textContent = emptyText;
    container.appendChild(empty);
    return;
  }

  for (const item of items) {
    const row = document.createElement("div");
    row.className = "report-item";
    row.title = item.title;

    const dot = document.createElement("span");
    dot.className = `report-dot ${item.state}`;

    const title = document.createElement("span");
    title.className = "report-item-title";
    title.textContent = item.title;

    const meta = document.createElement("span");
    meta.className = "report-item-meta";
    meta.textContent = `${item.repo} #${item.number} · ${relativeTime(item.updated_at)}`;

    row.appendChild(dot);
    row.appendChild(title);
    row.appendChild(meta);

    row.addEventListener("click", () => {
      invoke("open_in_browser", { url: item.url });
    });

    container.appendChild(row);
  }
}

async function refreshReport() {
  const errorBox = el("reportError");
  errorBox.className = "report-error";
  errorBox.textContent = "";

  const refreshBtn = el("refreshBtn");
  refreshBtn.disabled = true;
  try {
    const report = await invoke("get_github_report", { token: el("token").value.trim() });
    renderList("issuesList", report.issues, "No open issues assigned to you.");
    renderList("prsList", report.pull_requests, "No pull requests found.");
    el("reportUpdated").textContent = `Updated ${relativeTime(new Date().toISOString())}`;
  } catch (err) {
    errorBox.className = "report-error visible";
    errorBox.textContent = String(err);
  } finally {
    refreshBtn.disabled = false;
  }
}

// ---------- STT / TTS sections ----------
// Same multi-profile list-editor UX as the LLM section above, but neither
// has a "test connection" or autostart concept (no actual capture/playback
// pipeline exists yet — this only persists connection details). Factored
// into one function since it's now the same shape three times over.
function createProfileSection({ prefix, getCmd, saveCmd, fields, hasAutostart }) {
  let profiles = [];
  let activeId = "";
  let editingId = null;

  const listEl = el(`${prefix}ProfileList`);
  const statusEl = el(`${prefix}Status`);
  const fieldEl = (f) => el(`${prefix}${f.id}`);

  function render() {
    listEl.innerHTML = "";
    if (profiles.length === 0) {
      const empty = document.createElement("div");
      empty.className = "llm-profile-empty";
      empty.textContent = "No profiles yet — fill in the form below and save one.";
      listEl.appendChild(empty);
      return;
    }
    for (const p of profiles) {
      const row = document.createElement("div");
      row.className = `llm-profile-row${p.id === editingId ? " editing" : ""}`;

      const label = document.createElement("span");
      label.className = "llm-profile-row-label";
      label.textContent = p.label || "(untitled)";

      const model = document.createElement("span");
      model.className = "llm-profile-row-model";
      model.textContent = p.model || "";

      const del = document.createElement("button");
      del.className = "llm-profile-row-del";
      del.textContent = "×";
      del.title = "Delete profile";
      del.addEventListener("click", (e) => {
        e.stopPropagation();
        deleteProfile(p.id);
      });

      row.append(label, model, del);
      row.addEventListener("click", () => loadIntoForm(p.id));
      listEl.appendChild(row);
    }
  }

  function loadIntoForm(id) {
    const p = profiles.find((x) => x.id === id);
    if (!p) return;
    editingId = p.id;
    for (const f of fields) fieldEl(f).value = p[f.key] || "";
    clearStatus(statusEl);
    render();
  }

  function newForm() {
    editingId = null;
    for (const f of fields) fieldEl(f).value = "";
    clearStatus(statusEl);
    render();
  }

  function currentFields() {
    const out = {};
    for (const f of fields) out[f.key] = fieldEl(f).value.trim();
    return out;
  }

  async function persist() {
    const settings = { profiles, active_profile_id: activeId };
    if (hasAutostart) {
      settings.autostart = el(`${prefix}Autostart`).checked;
      settings.start_command = el(`${prefix}StartCommand`).value.trim();
    }
    await invoke(saveCmd, { settings });
  }

  async function load() {
    const settings = await invoke(getCmd);
    profiles = settings.profiles || [];
    activeId = settings.active_profile_id || "";
    if (hasAutostart) {
      el(`${prefix}Autostart`).checked = !!settings.autostart;
      el(`${prefix}StartCommand`).value = settings.start_command || "";
    }
    if (profiles.length > 0) {
      loadIntoForm(activeId && profiles.some((p) => p.id === activeId) ? activeId : profiles[0].id);
    } else {
      newForm();
    }
  }

  async function save() {
    const values = currentFields();
    if (!values.base_url || !values.model) {
      setStatus(statusEl, "error", "Base URL and model name are required.");
      return;
    }
    const id = editingId || crypto.randomUUID();
    const profile = { id, ...values };
    const idx = profiles.findIndex((p) => p.id === id);
    if (idx >= 0) profiles[idx] = profile;
    else profiles.push(profile);
    if (!activeId) activeId = id;
    editingId = id;
    await persist();
    render();
    setStatus(statusEl, "ok", "Saved.");
  }

  async function deleteProfile(id) {
    profiles = profiles.filter((p) => p.id !== id);
    if (activeId === id) activeId = profiles[0]?.id || "";
    await persist();
    if (editingId === id) {
      if (profiles.length > 0) loadIntoForm(profiles[0].id);
      else newForm();
    } else {
      render();
    }
  }

  el(`${prefix}SaveBtn`).addEventListener("click", save);
  el(`${prefix}NewProfileBtn`).addEventListener("click", newForm);
  el(`${prefix}DeleteProfileBtn`).addEventListener("click", () => {
    if (editingId) deleteProfile(editingId);
    else newForm();
  });
  fields.forEach((f) => fieldEl(f).addEventListener("input", () => clearStatus(statusEl)));

  if (hasAutostart) {
    el(`${prefix}Autostart`).addEventListener("change", () => {
      clearStatus(statusEl);
      persist();
    });
    el(`${prefix}StartCommand`).addEventListener("change", () => persist());
    el(`${prefix}StartNowBtn`).addEventListener("click", () => {
      const activeProfile = profiles.find((p) => p.id === activeId) || profiles[0];
      startServerNow(statusEl, activeProfile?.base_url || "", el(`${prefix}StartCommand`).value.trim());
    });
  }

  return { load };
}

const sttSection = createProfileSection({
  prefix: "stt",
  getCmd: "get_stt_settings",
  saveCmd: "save_stt_settings",
  hasAutostart: true,
  fields: [
    { id: "ProfileLabel", key: "label" },
    { id: "BaseUrl", key: "base_url" },
    { id: "Model", key: "model" },
    { id: "Language", key: "language" },
    { id: "ApiKey", key: "api_key" },
  ],
});

const ttsSection = createProfileSection({
  prefix: "tts",
  getCmd: "get_tts_settings",
  saveCmd: "save_tts_settings",
  hasAutostart: true,
  fields: [
    { id: "ProfileLabel", key: "label" },
    { id: "BaseUrl", key: "base_url" },
    { id: "Model", key: "model" },
    { id: "Voice", key: "voice" },
    { id: "ApiKey", key: "api_key" },
  ],
});

// ---------- Voice assistant section ----------
// Just three settings (on/off, sensitivity, and a readout of whether the three
// servers it depends on are configured) — no profiles of its own, because it
// drives the STT/TTS/LLM profiles the sections above already own.

const READINESS_ROWS = [
  ["stt_configured", "Speech-to-text profile", "STT"],
  ["llm_configured", "Language model profile", "LLM"],
  ["tts_configured", "Text-to-speech profile", "TTS"],
];

async function loadVoiceSettings() {
  const readiness = await invoke("get_voice_readiness");

  el("voiceEnabled").checked = readiness.enabled;
  el("voiceThreshold").value = readiness.threshold;
  el("voiceThresholdOut").textContent = readiness.threshold.toFixed(2);

  // Spelled out per dependency rather than as one "not ready" message: when it
  // isn't working, which of the three is missing is the only thing the user
  // actually needs to know.
  const list = el("voiceReadiness");
  list.innerHTML = "";
  for (const [key, label, section] of READINESS_ROWS) {
    const ok = readiness[key];
    const row = document.createElement("div");
    row.className = `voice-check ${ok ? "ok" : "missing"}`;
    row.textContent = `${ok ? "✓" : "•"} ${label}${ok ? "" : " — not set up yet"}`;
    if (!ok) {
      // Clicking the missing dependency jumps to the section that fixes it —
      // otherwise the user has to work out that "STT" is the tab they want.
      row.tabIndex = 0;
      const go = () => document.querySelector(`.hub-nav-item[data-section="${section.toLowerCase()}"]`)?.click();
      row.addEventListener("click", go);
      row.addEventListener("keydown", (e) => {
        if (e.key === "Enter" || e.key === " ") go();
      });
    }
    list.appendChild(row);
  }
}

// ---------- Advanced section ----------
// Generated from the schema src-tauri/src/tunables.rs sends, rather than written
// out in HTML. Two reasons: adding a setting should be one Rust edit and not
// four, and a hand-written form drifts from the code that reads it — a field
// labelled in seconds bound to a value read as milliseconds looks fine and is
// silently wrong.

let tunableSchema = [];
let tunableDefaults = {};

// Matches the backend's normalization (tunables.rs's split_names) so "equal to
// the default" means the same thing on both sides — otherwise typing the default
// back in with different spacing would save a redundant override.
function normalizeNames(text) {
  return String(text)
    .split(",")
    .map((part) => part.trim().toLowerCase())
    .filter(Boolean)
    .join(",");
}

function readTunableInput(setting, input) {
  if (setting.kind === "toggle") return input.checked;
  if (setting.kind === "text") return input.value.trim();
  if (setting.kind === "names") return normalizeNames(input.value);
  // An empty or unparseable number field reads as the default rather than 0:
  // clearing a field means "I don't want to set this", not "zero".
  const number = Number(input.value);
  return input.value.trim() === "" || Number.isNaN(number) ? tunableDefaults[setting.id] : number;
}

function isAtDefault(setting, value) {
  return value === tunableDefaults[setting.id];
}

function refreshTunableRow(setting, row, input) {
  const atDefault = isAtDefault(setting, readTunableInput(setting, input));
  row.classList.toggle("overridden", !atDefault);
  row.querySelector(".tunable-reset").disabled = atDefault;
}

// The event port is the one setting with a consequence outside the widget: the
// Claude Code hooks post to it by number, so changing it here without changing
// them there silently breaks the integration. Rather than trying to rewrite the
// user's Claude Code settings, show them exactly what the hooks now have to say.
function renderEventPortNote(row, input) {
  const note = document.createElement("p");
  note.className = "hint subtle tunable-port-note";
  const update = () => {
    const port = input.value.trim() || String(tunableDefaults["network.event_port"]);
    note.textContent =
      `Your Claude Code hooks have to post to http://127.0.0.1:${port}/event and ` +
      `/decide — update the commands in ~/.claude/settings.json to match (SETUP.md ` +
      `has the full block). Takes effect after the widget restarts.`;
  };
  update();
  input.addEventListener("input", update);
  row.appendChild(note);

  // Whether the port was actually claimed at launch. Shown here because a
  // release build has no console for the backend to complain to, so a port
  // already in use would otherwise present as "Claude Code hooks stopped
  // working" with nothing anywhere to explain why.
  const live = document.createElement("p");
  live.className = "hint subtle tunable-port-note";
  row.appendChild(live);
  invoke("get_event_server_status")
    .then((status) => {
      if (!status.known) {
        live.remove();
      } else if (status.listening) {
        live.textContent = `Currently listening on port ${status.port}.`;
        live.classList.add("ok");
      } else {
        live.textContent =
          `Not listening on port ${status.port} — ${status.error || "the port could not be claimed"}. ` +
          `Something else on this machine is probably using it; pick another and restart.`;
      }
    })
    .catch(() => live.remove());
}

function buildTunableRow(setting, value) {
  const row = document.createElement("div");
  row.className = "tunable";
  row.dataset.id = setting.id;

  const head = document.createElement("div");
  head.className = "tunable-head";
  const label = document.createElement("span");
  label.className = "tunable-label";
  label.textContent = setting.label;
  head.appendChild(label);
  if (setting.restart) {
    const badge = document.createElement("span");
    badge.className = "tunable-badge";
    badge.textContent = "needs restart";
    head.appendChild(badge);
  }
  row.appendChild(head);

  const inputRow = document.createElement("div");
  inputRow.className = "tunable-input";
  const input = document.createElement("input");
  if (setting.kind === "toggle") {
    // Same switch markup the rest of the settings window uses, so an on/off
    // setting looks like every other on/off setting rather than like a number
    // field that happens to accept two values.
    input.type = "checkbox";
    input.checked = !!value;
    const track = document.createElement("span");
    track.className = "switch";
    track.appendChild(input);
    track.insertAdjacentHTML("beforeend", '<span class="switch-track"><span class="switch-thumb"></span></span>');
    inputRow.appendChild(track);
  } else {
    if (setting.kind === "names" || setting.kind === "text") {
      input.type = "text";
      // A `text` setting's default is often empty (that is how an optional
      // connection setting says "not configured"), so the schema carries a
      // separate example to show in the box.
      input.placeholder = setting.placeholder || setting.default;
    } else {
      input.type = "number";
      input.min = setting.min;
      input.max = setting.max;
      input.step = setting.step;
    }
    input.value = value;
    inputRow.appendChild(input);
  }

  if (setting.unit) {
    const unit = document.createElement("span");
    unit.className = "tunable-unit";
    unit.textContent = setting.unit;
    inputRow.appendChild(unit);
  }

  const reset = document.createElement("button");
  reset.className = "tunable-reset";
  reset.type = "button";
  reset.textContent = "↺";
  const defaultLabel = setting.kind === "toggle" ? (setting.default ? "on" : "off") : setting.default;
  reset.title = `Back to the default (${defaultLabel})`;
  reset.addEventListener("click", () => {
    if (setting.kind === "toggle") input.checked = !!setting.default;
    else input.value = setting.default;
    refreshTunableRow(setting, row, input);
    clearStatus(el("advancedStatus"));
  });
  inputRow.appendChild(reset);
  row.appendChild(inputRow);

  const help = document.createElement("p");
  help.className = "hint subtle tunable-help";
  help.textContent = setting.help;
  row.appendChild(help);

  if (setting.id === "network.event_port") renderEventPortNote(row, input);

  // Both events: a text or number field reports "input" as it is typed, while a
  // checkbox is only reliably reported by "change".
  for (const event of ["input", "change"]) {
    input.addEventListener(event, () => {
      refreshTunableRow(setting, row, input);
      clearStatus(el("advancedStatus"));
    });
  }
  refreshTunableRow(setting, row, input);
  return row;
}

// The Memory group has its own section in the sidebar, because it is a feature
// rather than a pile of numbers — so Advanced leaves it out instead of showing
// every field twice.
const MEMORY_GROUP = "Memory";

async function loadTunableSettings() {
  const payload = await invoke("get_tunables");
  tunableSchema = payload.settings;
  tunableDefaults = Object.fromEntries(payload.settings.map((s) => [s.id, s.default]));

  const advanced = el("tunableGroups");
  const memory = el("memoryTunables");
  advanced.innerHTML = "";
  memory.innerHTML = "";

  // Grouped in the order the backend declares, so related settings stay together
  // and the form's shape is decided next to the values rather than here.
  for (const group of payload.groups) {
    const block = document.createElement("div");
    block.className = "tunable-group";
    if (group !== MEMORY_GROUP) {
      const heading = document.createElement("h4");
      heading.textContent = group;
      block.appendChild(heading);
    }
    for (const setting of payload.settings.filter((s) => s.group === group)) {
      block.appendChild(buildTunableRow(setting, payload.values[setting.id]));
    }
    (group === MEMORY_GROUP ? memory : advanced).appendChild(block);
  }
}

// What the index holds, and whether meaning search is actually working. Without
// it the section is two text boxes with no way to tell "set up correctly" from
// "quietly doing nothing".
async function loadMemoryStatus() {
  const box = el("memoryStatus");
  const status = await invoke("recall_status").catch(() => null);
  if (!status) {
    box.innerHTML = '<div class="memory-line bad">Couldn\'t read the index.</div>';
    return;
  }

  const lines = [];
  lines.push(
    `<div class="memory-line">${status.turns} exchange${status.turns === 1 ? "" : "s"} indexed` +
      ` across ${status.chats} conversation${status.chats === 1 ? "" : "s"}.</div>`
  );
  // Counted separately from conversations because they come from somewhere else
  // entirely — Claude Code's own notes, which this app reads and never writes.
  if (status.notes > 0) {
    lines.push(
      `<div class="memory-line">Plus ${status.notes} of Claude Code's own note${status.notes === 1 ? "" : "s"}` +
        " — the ones the Memory graph in the workspace shows.</div>"
    );
  }

  if (status.problem) {
    lines.push(`<div class="memory-line bad">${escapeHtml(status.problem)}</div>`);
  } else if (status.embedded > 0 && status.similarity_floor > 0) {
    // Out of `units`, not `turns` — turns counts conversation exchanges only,
    // while everything indexed gets a vector, notes included.
    lines.push(
      `<div class="memory-line ok">${status.embedded} of ${status.units} can also be found by meaning` +
        ` (${escapeHtml(status.vector_model)}, related above ${status.similarity_floor.toFixed(2)}).</div>`
    );
  } else if (status.embedded > 0) {
    // Configured and embedded, but there is not enough history yet to measure
    // what "unrelated" looks like for this model — and a threshold guessed
    // without that either matches everything or nothing.
    lines.push(
      `<div class="memory-line">${status.embedded} entr${status.embedded === 1 ? "y" : "ies"} embedded with` +
        ` ${escapeHtml(status.vector_model)}. Meaning search switches on once there is enough history to` +
        " measure how close counts as related — a few more conversations.</div>"
    );
  } else {
    // The honest description of the default: it works, it just works on words.
    lines.push(
      '<div class="memory-line">Found by the words they used. Fill in a server and model below to also' +
        " find conversations that made the same point in different words.</div>"
    );
  }
  box.innerHTML = lines.join("");
}

function escapeHtml(text) {
  return String(text).replace(
    /[&<>"']/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]
  );
}

// Sends every setting, with null for the ones sitting on their default — that is
// what removes an override, so config.json only ever holds what the user actually
// changed and a future change to a default reaches anyone who never touched it.
// Both sections edit one set of values, so saving from either sends all of them
// — a form split across two places must not let one half overwrite the other
// with whatever it happened to be showing.
async function saveTunableSettings(status) {
  const values = {};
  for (const setting of tunableSchema) {
    const input = document.querySelector(`.tunable[data-id="${setting.id}"] input`);
    if (!input) continue;
    const value = readTunableInput(setting, input);
    values[setting.id] = isAtDefault(setting, value) ? null : value;
  }

  setStatus(status, "pending", "Saving…");
  try {
    await invoke("save_tunables", { values });
    // Re-read rather than trusting what was sent: the backend normalizes the
    // app-name lists, so what is stored isn't always character-for-character
    // what was typed.
    await loadTunableSettings();
    loadMemoryStatus();
    const restarts = tunableSchema.filter((s) => s.restart && values[s.id] !== null);
    setStatus(
      status,
      "ok",
      restarts.length > 0 ? "Saved — restart the widget to apply the event port." : "Saved."
    );
  } catch (err) {
    setStatus(status, "error", String(err));
  }
}

async function loadChatsDir() {
  const info = await invoke("get_chats_dir").catch(() => null);
  if (!info) return;
  const path = el("chatsDirPath");
  path.textContent = info.dir;
  path.title = info.dir;
  // Reset is only meaningful when there is something to reset to — offering it
  // on the built-in location would be a button that does nothing.
  el("chatsDirResetBtn").hidden = info.is_default;
}

// Reports what actually happened to the files rather than just the new path: a
// skipped or failed file means the history is now split across two folders, and
// only the user can decide what to do about that.
function describeChatsDirChange(change) {
  const parts = [];
  if (change.moved > 0) parts.push(`moved ${change.moved} conversation${change.moved === 1 ? "" : "s"}`);
  if (change.skipped > 0) parts.push(`left ${change.skipped} behind (already there)`);
  if (change.failed.length > 0) parts.push(`couldn't move ${change.failed.length}: ${change.failed.join(", ")}`);
  if (parts.length === 0) return "Nothing to move.";
  return parts.join("; ") + ".";
}

async function changeChatsDir(command) {
  const status = el("chatsDirStatus");
  setStatus(status, "pending", "Moving…");
  try {
    const change = await invoke(command);
    // Cancelling the folder picker returns nothing — not an error, and not
    // something to report as one.
    if (!change) return clearStatus(status);
    await loadChatsDir();
    // The index reconciles on its own thread after the move, so the counts are
    // read back once it has had a chance to notice.
    setTimeout(loadMemoryStatus, 800);
    setStatus(status, change.failed.length > 0 ? "error" : "ok", describeChatsDirChange(change));
  } catch (err) {
    setStatus(status, "error", String(err));
  }
}

async function rebuildRecallIndex() {
  const status = el("memorySettingsStatus");
  setStatus(status, "pending", "Rebuilding…");
  try {
    const stats = await invoke("recall_reindex");
    await loadMemoryStatus();
    setStatus(status, "ok", `Indexed ${stats.total} exchange${stats.total === 1 ? "" : "s"}.`);
  } catch (err) {
    setStatus(status, "error", String(err));
  }
}

async function resetAllTunables() {
  const status = el("advancedStatus");
  setStatus(status, "pending", "Resetting…");
  try {
    await invoke("save_tunables", {
      values: Object.fromEntries(tunableSchema.map((s) => [s.id, null])),
    });
    await loadTunableSettings();
    loadMemoryStatus();
    setStatus(status, "ok", "Everything is back to its default.");
  } catch (err) {
    setStatus(status, "error", String(err));
  }
}

// ---------- shared ----------

window.addEventListener("DOMContentLoaded", async () => {
  setupNav();

  loadLlmConfig();
  sttSection.load();
  ttsSection.load();
  loadVoiceSettings();
  loadChatsDir();
  loadTunableSettings()
    .then(loadMemoryStatus)
    .catch((err) => setStatus(el("advancedStatus"), "error", String(err)));
  const token = await loadGithubConfig();
  if (token) refreshReport();

  el("llmTestBtn").addEventListener("click", testLlmConnection);
  el("llmSaveBtn").addEventListener("click", saveProfile);
  el("newProfileBtn").addEventListener("click", newProfileForm);
  el("deleteProfileBtn").addEventListener("click", () => {
    if (editingProfileId) deleteProfile(editingProfileId);
    else newProfileForm();
  });
  ["profileLabel", "baseUrl", "model", "apiKey", "startCommand", "maxTokens"].forEach((id) => {
    el(id).addEventListener("input", () => clearStatus(el("llmStatus")));
  });
  el("autostart").addEventListener("change", () => {
    updateStartCommandEnabled();
    clearStatus(el("llmStatus"));
    persistLlmSettings();
  });
  el("startCommand").addEventListener("change", () => persistLlmSettings());
  el("think").addEventListener("change", () => clearStatus(el("llmStatus")));
  el("llmStartNowBtn").addEventListener("click", () => {
    startServerNow(el("llmStatus"), el("baseUrl").value.trim(), el("startCommand").value.trim());
  });

  el("githubTestBtn").addEventListener("click", testGithubConnection);
  el("githubSaveBtn").addEventListener("click", saveGithubConfig);
  el("token").addEventListener("input", () => clearStatus(el("githubStatus")));
  el("refreshBtn").addEventListener("click", refreshReport);

  el("voiceEnabled").addEventListener("change", async (e) => {
    await invoke("set_voice_enabled", { enabled: e.target.checked });
    // Re-read rather than trusting the checkbox: this is also where a
    // half-configured setup gets its warning refreshed.
    loadVoiceSettings();
  });
  el("voiceThreshold").addEventListener("input", (e) => {
    el("voiceThresholdOut").textContent = Number(e.target.value).toFixed(2);
  });
  // Persisted on "change", not "input": dragging the slider fires input for
  // every step, and each save re-broadcasts to the mascot.
  el("voiceThreshold").addEventListener("change", (e) => {
    invoke("set_voice_threshold", { threshold: Number(e.target.value) });
  });

  el("advancedSaveBtn").addEventListener("click", () => saveTunableSettings(el("advancedStatus")));
  el("advancedResetBtn").addEventListener("click", resetAllTunables);
  el("memorySaveBtn").addEventListener("click", () => saveTunableSettings(el("memorySettingsStatus")));
  el("memoryReindexBtn").addEventListener("click", rebuildRecallIndex);
  el("chatsDirChangeBtn").addEventListener("click", () => changeChatsDir("choose_chats_dir"));
  el("chatsDirResetBtn").addEventListener("click", () => changeChatsDir("reset_chats_dir"));

  el("titlebar-close").addEventListener("click", () => {
    invoke("hide_settings");
  });

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") invoke("hide_settings");
  });
});
