// Notices pushed from the backend's GitHub watchers: a merged-PR toast
// (github/api.rs), issue-thread updates (github/issue_watcher.rs) and the
// pinned daily digest card. The digest is the one notice with no auto-revert —
// it holds the notice lock until the user closes it.
import { invoke } from "../../shared/tauri.js";
import { buildIconSvg, buildStrokeIcon } from "../lib/icons.js";
import { beep } from "../lib/sound.js";
import { reportHotRectSoon } from "../lib/hotrect.js";
import { isNoticeLocked, showPinnedCard, showTransientNotice } from "../notice/notice.js";

const MERGE_ICON_PATH =
  "M5.45 5.154A4.25 4.25 0 0 0 9.25 7.5h1.378a2.251 2.251 0 1 1 0 1.5H9.25A5.734 5.734 0 0 1 5 7.123v3.505a2.25 2.25 0 1 1-1.5 0V5.372a2.25 2.25 0 1 1 1.95-.218ZM4.25 13.5a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5ZM3.5 3.75a.75.75 0 1 1 1.5 0 .75.75 0 0 1-1.5 0Zm8.5.75a.75.75 0 1 0 0-1.5.75.75 0 0 0 0 1.5Z";

function buildMergeIcon() {
  const icon = buildIconSvg(MERGE_ICON_PATH);
  icon.setAttribute("viewBox", "0 0 16 16");
  return icon;
}

export function showGithubMergeNotice({ title, url }) {
  if (isNoticeLocked()) return; // the pinned digest card takes priority

  const row = document.createElement("div");
  row.className = "merge-notice";
  row.appendChild(buildMergeIcon());
  const text = document.createElement("span");
  text.textContent = `"${title}" merged`;
  row.appendChild(text);

  showTransientNotice("state-github_merge", row, 8000, () => {
    if (url) invoke("open_in_browser", { url });
  });
  beep({ freq: 720, duration: 0.14, gain: 0.18 });
  beep({ freq: 960, duration: 0.18, gain: 0.18, delay: 0.15 });
}

// Same one-line transient banner as the merge notice (see github/issue_watcher.rs
// for what actually triggers each kind) — just a different icon/text per

const ISSUE_ICONS = {
  assigned: {
    paths: ["M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2", "M12 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8Z"],
    color: "#58a6ff",
  },
  comment: {
    paths: [
      "M21 11.5a8.38 8.38 0 0 1-.9 3.8 8.5 8.5 0 0 1-7.6 4.7 8.38 8.38 0 0 1-3.8-.9L3 21l1.9-5.7a8.38 8.38 0 0 1-.9-3.8 8.5 8.5 0 0 1 4.7-7.6 8.38 8.38 0 0 1 3.8-.9h.5a8.48 8.48 0 0 1 8 8v.5Z",
    ],
    color: "#8b949e",
  },
  closed: { paths: ["M22 11.08V12a10 10 0 1 1-5.93-9.14", "M22 4 12 14.01l-3-3"], color: "#a371f7" },
  reopened: { paths: ["M1 4v6h6", "M3.51 15a9 9 0 1 0 2.13-9.36L1 10"], color: "#3fb950" },
};

const ISSUE_KIND_TEXT = {
  assigned: (title, number) => `Assigned: "${title}"${number ? ` #${number}` : ""}`,
  comment: (title, number) => `New comment: "${title}"${number ? ` #${number}` : ""}`,
  closed: (title, number) => `Closed: "${title}"${number ? ` #${number}` : ""}`,
  reopened: (title, number) => `Reopened: "${title}"${number ? ` #${number}` : ""}`,
};

export function showGithubIssueNotice({ kind, title, number, url, detail, repo }) {
  if (isNoticeLocked()) return; // the pinned digest card takes priority

  const spec = ISSUE_ICONS[kind] || ISSUE_ICONS.comment;
  const row = document.createElement("div");
  row.className = "merge-notice";
  if (detail) row.title = detail; // hover tooltip — e.g. who commented and a snippet
  const icon = buildStrokeIcon(spec.paths);
  icon.style.color = spec.color;
  row.appendChild(icon);

  // Two lines — a small repo label above the action text — since with
  // several repos in play "Assigned: \"title\"" alone doesn't say WHERE.
  const textWrap = document.createElement("div");
  textWrap.className = "issue-notice-text";
  if (repo) {
    const repoLine = document.createElement("span");
    repoLine.className = "issue-notice-repo";
    repoLine.textContent = repo;
    textWrap.appendChild(repoLine);
  }
  const actionLine = document.createElement("span");
  actionLine.className = "issue-notice-action";
  actionLine.textContent = (ISSUE_KIND_TEXT[kind] || ISSUE_KIND_TEXT.comment)(title, number);
  textWrap.appendChild(actionLine);
  row.appendChild(textWrap);

  showTransientNotice("state-github_merge", row, 8000, () => {
    if (url) invoke("open_in_browser", { url });
  });
  beep({ freq: 720, duration: 0.14, gain: 0.18 });
  beep({ freq: 960, duration: 0.18, gain: 0.18, delay: 0.15 });
}

const GITHUB_ICON_PATH =
  "M12 .5C5.65.5.5 5.65.5 12c0 5.08 3.29 9.39 7.86 10.91.57.1.79-.25.79-.55 0-.27-.01-1.16-.02-2.11-3.2.7-3.88-1.36-3.88-1.36-.52-1.33-1.28-1.68-1.28-1.68-1.04-.71.08-.7.08-.7 1.15.08 1.76 1.18 1.76 1.18 1.03 1.76 2.7 1.25 3.36.96.1-.75.4-1.25.73-1.54-2.55-.29-5.23-1.28-5.23-5.68 0-1.25.45-2.28 1.18-3.08-.12-.29-.51-1.46.11-3.04 0 0 .96-.31 3.15 1.18a10.9 10.9 0 015.74 0c2.19-1.49 3.15-1.18 3.15-1.18.62 1.58.23 2.75.11 3.04.74.8 1.18 1.83 1.18 3.08 0 4.41-2.69 5.38-5.25 5.67.42.36.78 1.07.78 2.16 0 1.56-.01 2.82-.01 3.2 0 .31.21.66.8.55A10.99 10.99 0 0023.5 12C23.5 5.65 18.35.5 12 .5z";

// Categorizes an "Attention" line by its content so the dot color carries
// meaning at a glance (red = someone's blocking you, green = ready to go,
// gray = just old) instead of every row looking identical.
function digestRowCategory(line) {
  if (/changes requested/i.test(line)) return "danger";
  if (/ready to merge|approved/i.test(line)) return "ok";
  if (/review\/approval/i.test(line)) return "info";
  if (/stale/i.test(line)) return "stale";
  return "action";
}

// Renders the Workload line as a small stat header, then each "Attention"
// item as its own row with a category-colored dot — a plain text blob was
// hard to scan at a glance.
function renderDigestList(container, summary) {
  container.innerHTML = "";

  const header = document.createElement("div");
  header.className = "digest-header";
  const icon = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  icon.setAttribute("viewBox", "0 0 24 24");
  icon.setAttribute("fill", "currentColor");
  const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
  path.setAttribute("d", GITHUB_ICON_PATH);
  icon.appendChild(path);
  const title = document.createElement("span");
  title.textContent = "GitHub Digest";
  header.appendChild(icon);
  header.appendChild(title);
  container.appendChild(header);

  const lines = summary
    .split("\n")
    .map((l) => l.replace(/^[-•]\s*/, "").trim())
    .filter(Boolean);

  const workloadLine = lines.find((l) => /^workload:/i.test(l));
  const attentionLines = lines
    .filter((l) => /^attention:/i.test(l))
    .map((l) => l.replace(/^attention:\s*/i, ""));

  if (workloadLine) {
    const stat = document.createElement("div");
    stat.className = "digest-workload";
    stat.textContent = workloadLine.replace(/^workload:\s*/i, "");
    container.appendChild(stat);
  }

  const list = document.createElement("div");
  list.className = "digest-list";

  for (const line of attentionLines) {
    const row = document.createElement("div");
    row.className = `digest-row digest-row--${digestRowCategory(line)}`;

    const dot = document.createElement("span");
    dot.className = "digest-dot";
    const text = document.createElement("span");
    text.textContent = line;

    row.appendChild(dot);
    row.appendChild(text);
    list.appendChild(row);
  }
  container.appendChild(list);
}

// Unlike every other notice, this one has NO auto-revert timer — it stays
// up until the user clicks the dedicated Close button, and noticeLocked
// blocks every other state change (setState/merge notice) from touching it
// in the meantime.
//
// Goes through showPinnedCard rather than taking the lock itself, so that if the
// morning mail brief is already up this waits behind it instead of being
// silently dropped by the isNoticeLocked() guard. The beep lives inside the
// render callback for the same reason: a card that was queued must not announce
// itself until it is actually on screen.
export function showGithubDigestNotice(summary) {
  showPinnedCard("state-github_digest", (notice) => {
    renderDigestList(notice, summary);
    beep({ freq: 600, duration: 0.15, gain: 0.15 });
  });
}
