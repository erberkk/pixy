// Notices pushed from the backend's Google watchers: an arriving message
// (mail/watcher.rs), a meeting about to start (calendar/watcher.rs), and the
// pinned morning card (mail/brief.rs).
//
// Deliberately its own module and its own stylesheet rather than reusing the
// GitHub notice classes. The two look similar today and have no reason to stay
// that way: a mail notice's second line is a summary that can run to two lines,
// where an issue notice's is a fixed "Assigned: …" string, and sharing a class
// would mean every change to one had to be checked against the other.
import { invoke } from "../../shared/tauri.js";
import { beep } from "../lib/sound.js";
import { isNoticeLocked, showPinnedCard, showTransientNotice } from "../notice/notice.js";

// How long an arriving-mail notice stays up. Longer than the GitHub notices'
// 8s because there is more to read here — a sender, a subject and a summary,
// against "\"title\" merged".
const MAIL_NOTICE_MS = 11000;
const CALENDAR_NOTICE_MS = 12000;

const ICON = {
  mail: [
    "M4 4h16a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2Z",
    "m22 6-10 7L2 6",
  ],
  reply: ["M9 17H5a2 2 0 0 1-2-2V7", "m9 21-6-6 6-6", "M21 15v-2a4 4 0 0 0-4-4H3"],
  calendar: [
    "M5 4h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2Z",
    "M16 2v4M8 2v4M3 10h18",
  ],
};

const ACCENT = { mail: "#58a6ff", reply: "#3fb950", calendar: "#e8b04b" };

function buildIcon(kind) {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("fill", "none");
  svg.setAttribute("stroke", "currentColor");
  svg.setAttribute("stroke-width", "2");
  svg.setAttribute("stroke-linecap", "round");
  svg.setAttribute("stroke-linejoin", "round");
  for (const d of ICON[kind]) {
    const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
    path.setAttribute("d", d);
    svg.appendChild(path);
  }
  svg.style.color = ACCENT[kind];
  return svg;
}

// Every notice in this module is the same shape: an icon, a small top line
// saying who or when, and the substance below it.
function buildRow(kind, topLine, mainLine, account) {
  const row = document.createElement("div");
  row.className = "mail-notice";
  row.appendChild(buildIcon(kind));

  const text = document.createElement("div");
  text.className = "mail-notice-text";

  const top = document.createElement("span");
  top.className = "mail-notice-top";
  top.textContent = topLine;
  // The address is a separate element rather than more text in the same one, so
  // it can opt out of the top line's uppercasing — an upper-cased email address
  // is heavy enough to outweigh the sender's name it sits next to.
  if (account) {
    const sep = document.createElement("span");
    sep.className = "mail-notice-account";
    sep.textContent = account;
    top.appendChild(sep);
  }
  text.appendChild(top);

  const main = document.createElement("span");
  main.className = "mail-notice-main";
  main.textContent = mainLine;
  text.appendChild(main);

  row.appendChild(text);
  return row;
}

export function showMailNotice({ from, subject, description, is_reply_to_me, url, collapsed_count, account }) {
  if (isNoticeLocked()) return; // a pinned card owns the screen

  // The collapsed form, for when more arrived at once than the cap allows.
  // It carries no sender or subject at all — naming one of twelve would be
  // arbitrary, and naming all twelve is what the cap exists to prevent.
  const row =
    collapsed_count > 0
      ? buildRow(
          "mail",
          "Inbox",
          `${collapsed_count} more new message${collapsed_count === 1 ? "" : "s"}`,
          account,
        )
      : buildRow(
          is_reply_to_me ? "reply" : "mail",
          is_reply_to_me ? `${from} replied` : from,
          description ? `${subject} — ${description}` : subject,
          account,
        );

  showTransientNotice("state-mail_notice", row, MAIL_NOTICE_MS, () => {
    if (url) invoke("open_in_browser", { url });
  });

  // A reply gets its own two-tone chirp: it is the one kind of mail the user is
  // actively waiting on, and it should be recognisable without looking.
  if (is_reply_to_me) {
    beep({ freq: 660, duration: 0.12, gain: 0.16 });
    beep({ freq: 880, duration: 0.16, gain: 0.16, delay: 0.13 });
  } else {
    beep({ freq: 520, duration: 0.13, gain: 0.14 });
  }
}

export function showCalendarNotice({ title, clock, starts_in_minutes, location, url, account }) {
  if (isNoticeLocked()) return;

  // starts_in_minutes can be zero or negative: the backend's lookup window
  // reaches slightly into the past so a poll landing just after the hour still
  // says something, rather than skipping the meeting entirely.
  const when =
    starts_in_minutes <= 0
      ? `starting now · ${clock}`
      : `in ${starts_in_minutes} min · ${clock}`;

  const row = buildRow("calendar", when, location ? `${title} — ${location}` : title, account);
  showTransientNotice("state-mail_notice", row, CALENDAR_NOTICE_MS, () => {
    if (url) invoke("open_in_browser", { url });
  });
  beep({ freq: 780, duration: 0.14, gain: 0.17 });
  beep({ freq: 780, duration: 0.14, gain: 0.17, delay: 0.22 });
}

// Which mailbox a row came from, as a chip rather than another line of prose:
// with two accounts connected every row would otherwise repeat the same long
// address in the middle of the sentence.
function buildAccountChip(account) {
  const chip = document.createElement("span");
  chip.className = "brief-account";
  chip.textContent = account;
  return chip;
}

function buildBriefSection(container, label) {
  const heading = document.createElement("div");
  heading.className = "brief-section-label";
  heading.textContent = label;
  container.appendChild(heading);
}

function renderBrief(container, { unread_count, days, items, events }) {
  container.innerHTML = "";

  const header = document.createElement("div");
  header.className = "brief-header";
  header.appendChild(buildIcon("mail"));
  const title = document.createElement("span");
  title.textContent = "Morning Brief";
  header.appendChild(title);
  container.appendChild(header);

  // The window is named, not implied. This number used to be Gmail's all-time
  // unread count — tens of thousands on a real mailbox — sitting above five
  // messages from this morning, so the headline and the list were answering
  // different questions. Now they cover the same days, and it says which.
  const stat = document.createElement("div");
  stat.className = "brief-unread";
  stat.textContent = days
    ? `${unread_count} unread · last ${days} day${days === 1 ? "" : "s"}`
    : `${unread_count} unread`;
  container.appendChild(stat);

  if (events && events.length > 0) {
    buildBriefSection(container, "Today");
    const list = document.createElement("div");
    list.className = "brief-list";
    for (const event of events) {
      const row = document.createElement("div");
      row.className = "brief-row brief-row--event";
      if (event.url) {
        row.dataset.url = event.url;
        row.classList.add("brief-row--clickable");
      }

      const clock = document.createElement("span");
      clock.className = "brief-clock";
      clock.textContent = event.clock;
      row.appendChild(clock);

      const text = document.createElement("span");
      text.className = "brief-row-text";
      const title = document.createElement("span");
      title.textContent = event.location ? `${event.title} · ${event.location}` : event.title;
      text.appendChild(title);
      if (event.account) text.appendChild(buildAccountChip(event.account));
      row.appendChild(text);

      list.appendChild(row);
    }
    container.appendChild(list);
  }

  if (items && items.length > 0) {
    buildBriefSection(container, "Mail");
    const list = document.createElement("div");
    list.className = "brief-list";
    for (const item of items) {
      const row = document.createElement("div");
      row.className = `brief-row brief-row--mail${item.is_reply_to_me ? " brief-row--reply" : ""}`;
      if (item.url) {
        row.dataset.url = item.url;
        row.classList.add("brief-row--clickable");
      }

      const dot = document.createElement("span");
      dot.className = "brief-dot";
      row.appendChild(dot);

      const text = document.createElement("span");
      text.className = "brief-row-text";

      const who = document.createElement("strong");
      who.textContent = item.is_reply_to_me ? `${item.from} replied` : item.from;
      text.appendChild(who);

      const detail = document.createElement("span");
      detail.className = "brief-row-detail";
      detail.textContent = item.description ? `${item.subject} — ${item.description}` : item.subject;
      text.appendChild(detail);

      if (item.account) text.appendChild(buildAccountChip(item.account));

      row.appendChild(text);
      list.appendChild(row);
    }
    container.appendChild(list);
  }
}

export function showDailyBrief(payload) {
  showPinnedCard("state-mail_brief", (notice) => {
    renderBrief(notice, payload);
    // The bottom fade is only honest when there is something below the fold, so
    // it is driven by measurement rather than assumed. Measured after render,
    // which is the earliest scrollHeight means anything.
    notice.classList.toggle("is-scrollable", notice.scrollHeight > notice.clientHeight);
    beep({ freq: 640, duration: 0.15, gain: 0.15 });
  });
}

// Delegated rather than one listener per row: the card is rebuilt from scratch
// every morning, and per-row listeners would have to be re-attached each time.
export function bindBriefClicks() {
  document.getElementById("notice").addEventListener("click", (event) => {
    const row = event.target.closest(".brief-row--clickable");
    if (!row || !document.body.classList.contains("state-mail_brief")) return;
    invoke("open_in_browser", { url: row.dataset.url });
  });
}

// The grant died — most often because the OAuth consent screen is still in
// "Testing", where Google expires refresh tokens after seven days. Shown as a
// normal transient notice: it is not urgent enough to pin, and clicking it goes
// straight to the page that fixes it.
export function showAuthNeededNotice({ reason, account }) {
  if (isNoticeLocked()) return;
  const row = buildRow(
    "mail",
    "Google sign-in expired",
    reason || "Reconnect your account in Settings.",
    account,
  );
  showTransientNotice("state-mail_notice", row, MAIL_NOTICE_MS, () => {
    invoke("open_settings");
  });
  beep({ freq: 420, duration: 0.2, gain: 0.16 });
}
