const { invoke } = window.__TAURI__.core;

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

function updateStartCommandEnabled() {
  el("startCommandField").classList.toggle("disabled", !el("autostart").checked);
}

async function loadLlmConfig() {
  const cfg = await invoke("get_llm_config");
  el("baseUrl").value = cfg.base_url || "";
  el("model").value = cfg.model || "";
  el("apiKey").value = cfg.api_key || "";
  el("autostart").checked = !!cfg.autostart;
  el("startCommand").value = cfg.start_command || "";
  el("think").checked = !!cfg.think;
  el("maxTokens").value = cfg.max_tokens || 600;
  updateStartCommandEnabled();
}

function currentLlmFields() {
  return {
    base_url: el("baseUrl").value.trim(),
    model: el("model").value.trim(),
    api_key: el("apiKey").value.trim(),
    autostart: el("autostart").checked,
    start_command: el("startCommand").value.trim(),
    think: el("think").checked,
    max_tokens: parseInt(el("maxTokens").value, 10) || 600,
  };
}

async function testLlmConnection() {
  const { base_url, model, api_key, think, max_tokens } = currentLlmFields();
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

async function saveLlmConfig() {
  const fields = currentLlmFields();
  const btn = el("llmSaveBtn");
  btn.disabled = true;
  try {
    await invoke("save_llm_config", {
      baseUrl: fields.base_url,
      model: fields.model,
      apiKey: fields.api_key,
      autostart: fields.autostart,
      startCommand: fields.start_command,
      think: fields.think,
      maxTokens: fields.max_tokens,
    });
    setStatus(el("llmStatus"), "ok", "Saved.");
  } catch (err) {
    setStatus(el("llmStatus"), "error", String(err));
  } finally {
    btn.disabled = false;
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

// ---------- shared ----------

window.addEventListener("DOMContentLoaded", async () => {
  setupNav();

  loadLlmConfig();
  const token = await loadGithubConfig();
  if (token) refreshReport();

  el("llmTestBtn").addEventListener("click", testLlmConnection);
  el("llmSaveBtn").addEventListener("click", saveLlmConfig);
  ["baseUrl", "model", "apiKey", "startCommand", "maxTokens"].forEach((id) => {
    el(id).addEventListener("input", () => clearStatus(el("llmStatus")));
  });
  el("autostart").addEventListener("change", () => {
    updateStartCommandEnabled();
    clearStatus(el("llmStatus"));
  });
  el("think").addEventListener("change", () => clearStatus(el("llmStatus")));

  el("githubTestBtn").addEventListener("click", testGithubConnection);
  el("githubSaveBtn").addEventListener("click", saveGithubConfig);
  el("token").addEventListener("input", () => clearStatus(el("githubStatus")));
  el("refreshBtn").addEventListener("click", refreshReport);

  el("titlebar-close").addEventListener("click", () => {
    invoke("hide_settings");
  });

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") invoke("hide_settings");
  });
});
