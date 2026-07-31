// Pip pixel-art sprite renderer — ported from pip-pixel.html (24x24 canvas,
// drawn at 10fps on purpose so it reads as pixel art, not smooth animation).
// Sprite data, palette math and the draw() switch are kept as close to the
// source as possible; only the plumbing around it (module exports, which
// element supplies --a, start/stop of the tick loop) is adapted to this app.

const ART = {
  bulb: [".oo.", "obbo", "obbo", ".oo."],
  body: [
    "..oooooooooo..",
    ".obbbbbbbbbbo.",
    "obaaaaaaaaaabo",
    "oaooooooooooao",
    "oaoggggggggoao",
    "oaoggggggggoao",
    "oaoggggggggoao",
    "oaooooooooooao",
    "oaaaaaaaaaaaao",
    ".oddaaaaaaddo.",
    "..oooooooooo..",
  ],
  arm: ["oo", "aa", "aa", "oo"],
  laptop: [".oooooo.", ".osssso.", ".osssso.", "oooooooo", "okkkkkko"],
  mug: ["ooooo.", "owwwoo", "owww.o", "owwwoo", ".ooo.."],
  wrenchA: ["m.m.", "mmm.", ".m..", ".m..", ".m.."],
  wrenchB: ["..mm", ".mm.", "mm..", "m...", "...."],
  cap: [".......ww.", "....oorro.", "..oorrro..", "oorrrroo.."],
  flagA: ["ooooo", "orrro", "orrro", ".oro.", "..o..", "..o..", "..o.."],
  flagB: ["ooooo", "orrro", ".orro", "..oro", "..o..", "..o..", "..o.."],
  sign: ["...o...", "..oao..", "..oyo..", ".oayao.", ".oaaao.", ".oayao.", "ooooooo"],
  note: [".bb", ".b.", "bb.", "bb."],
  zed: ["bbb", ".b.", "bbb"],
  lamp: [".bbb.", "bbbbb", "bbbbb", ".bbb.", ".ooo.", ".ooo."],
  lens: [".mm..", "m..m.", "m..m.", ".mm..", "..mm.", "...mm"],
  bug: ["o.o", "ooo"],
  page: ["ooooooo", "owwwwwo", "okkkkwo", "owkkkwo", "ooooooo"],
  rocket: ["..o..", ".oao.", ".oao.", "oaaao", "ooooo"],
  flame: [".b.b.", "..b.."],
  cloud: ["..bbb..", ".bbbbb.", "bbbbbbb"],
  up: [".a.", "aaa", ".a.", ".a."],
  down: [".a.", ".a.", "aaa", ".a."],
  hourA: ["ooooo", "obbbo", ".obo.", "..o..", ".o.o.", "o...o", "ooooo"],
  hourB: ["ooooo", "o...o", ".o.o.", "..o..", ".obo.", "obbbo", "ooooo"],
  pad: ["oooooo", "owwwwo", "okkkwo", "owkkko", "oooooo"],
  phone: ["ooo..", ".o...", "..o..", "...o.", "..ooo"],
  gamepad: [".ooooooo.", "o.k...b.o", "okkk.bbbo", "o.k...b.o", ".ooooooo."],
  batt: ["ooooooo", "o.....o", "o.....o", "o.....o", "ooooooo"],
  bubble: [".oooo.", "o....o", "o....o", "o....o", ".oo.o.", "..o..."],
  // The mail states' own props (see the postman poses in draw()). Held items are
  // five columns wide and no more: the canvas ends at x=23 and the body already
  // reaches x+13, so six would be clipped. Checked on screen at 11x, not guessed.
  //
  // Dark band across the top for the flap, white below for the paper. A V-shaped
  // flap was tried first and read as an X — five pixels is not enough width to
  // draw a diagonal that says "envelope" instead of "cross".
  envelope: ["oooooo", "okkkko", "owwwwo", "owwwwo", "oooooo"],
  // Two of the same stacked, for the morning card: a whole delivery rather than
  // one message. Tried as a satchel first, which at this size was just a striped
  // box — repeating a shape that already reads is better than inventing one that
  // doesn't.
  mailStack: ["oooooo", "okkkko", "owwwwo", "oooooo", "okkkko", "owwwwo", "oooooo"],
  // A peaked cap. The last row is the peak, deliberately asymmetric — it sticks
  // out only on the side the face looks towards. A symmetric band read as a
  // beanie, which is a different character entirely.
  postCap: ["...aaaaaa...", ".aaaaaaaaaa.", "oooooooooooo", "ooooo......."],
};

// 8x3 faces drawn inside the visor
const FACES = {
  open: ["........", ".ee..ee.", "........"],
  blink: ["........", "........", ".ee..ee."],
  up: [".ee..ee.", "........", "........"],
  squint: ["........", "..e..e..", "........"],
  wide: [".ee..ee.", ".ee..ee.", "........"],
  happy: [".e....e.", "e.e..e.e", "........"],
  dim: ["........", ".hh..hh.", "........"],
};

const BX = 5; // body left edge
const BY = 7; // body top edge (before bob)

// title/sub/indicator per state — ported from pip-pixel.html's STATES table,
// trimmed of the fields (line/progress/metaL/metaR) that belonged to its
// click-to-expand detail panel, which this app doesn't have (the permission
// card, digest card and Spotify panel already cover that role — see pip.js's
// header comment / the plan discussion for why that panel wasn't ported).
export const PIP_META = {
  idle: { ind: "none", title: "Idle", sub: "standing by" },
  thinking: { ind: "dots", title: "Thinking", sub: "working through it" },
  working: { ind: "ring", title: "Working", sub: "running the build" },
  waiting: { ind: "dots", title: "Waiting", sub: "blocked on something" },
  happy: { ind: "check", title: "Done", sub: "all tests passed" },
  alert: { ind: "bang", title: "Needs you", sub: "the build broke" },
  sleeping: { ind: "moon", title: "Sleeping", sub: "back when you are" },
  coding: { ind: "caret", title: "Coding", sub: "writing the patch" },
  debugging: { ind: "dots", title: "Debugging", sub: "chasing it down" },
  reviewing: { ind: "dots", title: "Reviewing", sub: "reading the diff" },
  deploying: { ind: "ring", title: "Deploying", sub: "pushing to prod" },
  syncing: { ind: "ring", title: "Syncing", sub: "pulling changes" },
  writing: { ind: "caret", title: "Writing", sub: "drafting notes" },
  call: { ind: "eq", title: "On a call", sub: "mic hot" },
  break: { ind: "none", title: "On a break", sub: "back soon" },
  lowpower: { ind: "bat", title: "Low power", sub: "plug something in" },
  listening: { ind: "eq", title: "Listening", sub: "now playing" },
  gaming: { ind: "caret", title: "Gaming", sub: "do not disturb" },
  streaming: { ind: "rec", title: "Streaming", sub: "you are live" },
  // --- app-specific states beyond the original pip-pixel set ---
  // The voice assistant's three turns (voice/voice.js). Distinct from
  // `listening`, which is Spotify playback — these mean the microphone is
  // actually open and pointed at you. `thinking` is reused for the gap between
  // them, so there is no fourth entry here.
  hearing: { ind: "eq", title: "Listening", sub: "go ahead" },
  speaking: { ind: "eq", title: "Answering", sub: "" },
  juggling: { ind: "dots", title: "Juggling", sub: "several sessions running" },
  forgotten: { ind: "bang", title: "Still waiting", sub: "approval sitting a while" },
  review_requested: { ind: "caret", title: "Review requested", sub: "someone needs your eyes" },
  mentioned: { ind: "check", title: "Mentioned", sub: "new comment for you" },
  assigned: { ind: "check", title: "Assigned", sub: "new issue for you" },
  digest_ready: { ind: "check", title: "Digest ready", sub: "daily summary waiting" },
  welcome_back: { ind: "check", title: "Welcome back", sub: "here's what happened" },
  // Mail and calendar (mail/watcher.rs, calendar/watcher.rs). These get their
  // own postman poses rather than borrowing happy/digest_ready, so mail is
  // recognisable at a glance without reading the text beside it.
  mail_new: { ind: "check", title: "New mail", sub: "just arrived" },
  mail_reply: { ind: "check", title: "You got a reply", sub: "someone answered you" },
  brief_ready: { ind: "check", title: "Morning brief", sub: "mail and meetings" },
  meeting_soon: { ind: "bang", title: "Meeting soon", sub: "starting shortly" },
  // Chat window's own assistant persona (chat.js) — always rendered in a
  // fixed muted grey (see workspace's chat.css .chat-mascot --a override) rather
  // than picking up a colored accent like every other state here, so it
  // reads as a distinct, quieter character instead of one more ambient mood.
  chat_idle: { ind: "none", title: "Assistant", sub: "ready to chat" },
  chat_typing: { ind: "dots", title: "Assistant", sub: "typing…" },
};

let pal = {};
let ox = null;
let targetCtx = null;
let tick = 0;
let current = "idle";
let tickTimer = null;

function px(x, y, c) {
  if (!c || x < 0 || y < 0 || x > 23 || y > 23) return;
  ox.fillStyle = c;
  ox.fillRect(x, y, 1, 1);
}

function blit(rows, x, y, swap) {
  for (let j = 0; j < rows.length; j++) {
    const row = rows[j];
    for (let i = 0; i < row.length; i++) {
      const key = row[i];
      if (key === ".") continue;
      px(x + i, y + j, pal[(swap && swap[key]) || key]);
    }
  }
}

function box(x, y, w, h, c) {
  ox.fillStyle = c;
  ox.fillRect(x, y, w, h);
}

// deterministic noise so sparks and confetti do not jitter randomly
const rnd = (n) => {
  const v = Math.sin(n * 127.1) * 43758.545;
  return v - Math.floor(v);
};

function shade(hex, amt) {
  const n = parseInt(hex.trim().slice(1), 16);
  const mix = (c) => Math.max(0, Math.min(255, amt > 0 ? c + (255 - c) * amt : c * (1 + amt)));
  return (
    "#" +
    [16, 8, 0]
      .map((s) => Math.round(mix((n >> s) & 255)).toString(16).padStart(2, "0"))
      .join("")
  );
}

// Reads --a off the element carrying data-pip-state (set per-state in
// pip.css) rather than off <body> — the layout state (body.className) and
// the ambient/mood state (data-pip-state) are deliberately two separate
// attributes so this sprite's accent never has to fight the pinned-card /
// notice layout system in main.js.
function readPalette(accentEl) {
  const a = getComputedStyle(accentEl).getPropertyValue("--a").trim() || "#5ad1e6";
  const b = shade(a, 0.55);
  accentEl.style.setProperty("--a2", b);
  pal = {
    o: "#070910",
    a: a,
    b: b,
    d: shade(a, -0.38),
    g: "#0f1320",
    e: b,
    h: shade(a, -0.15),
    w: "#f2f5ff",
    k: "#39415c",
    s: shade(b, 0.35),
    m: "#9aa3bd",
    r: shade(a, -0.25),
    y: "#070910",
  };
}

function draw() {
  const t = tick,
    s = current;
  ox.clearRect(0, 0, 24, 24);

  let by = BY,
    dx = 0,
    face = "open",
    blinks = true,
    faceDx = 0;
  let armLy = 0,
    armRy = 0,
    armY = 5,
    hideArms = false;

  switch (s) {
    case "idle":
      by += [0, 0, 0, -1, -1, -1, 0, 0][t % 8];
      faceDx = t % 61 < 8 ? 1 : t % 89 < 6 ? -1 : 0;
      break;
    case "thinking":
      by += [0, 0, -1, -1][t % 4];
      dx = -1;
      face = "up";
      faceDx = 1;
      blinks = false;
      break;
    case "debugging":
      by += [0, 0, -1, 0][t % 4];
      face = "squint";
      blinks = false;
      faceDx = t % 9 < 5 ? -1 : 0;
      break;
    case "reviewing":
      by += [0, 0, 0, -1][t % 4];
      hideArms = true;
      blinks = false;
      faceDx = [0, 1, 0, -1][Math.floor(t / 3) % 4];
      break;
    case "deploying":
      by += [0, -1, -1, 0][t % 4];
      face = "up";
      blinks = false;
      armY = 3;
      break;
    case "syncing":
      by += [0, 0, -1, -1][t % 4];
      faceDx = 1;
      face = "up";
      break;
    case "waiting":
      by += [0, 0, 0, 0, -1, -1, -1, -1][t % 8];
      blinks = true;
      faceDx = [0, 0, 1, 1, 0, 0, -1, -1][t % 8];
      break;
    case "writing":
      by += t % 4 < 2 ? 0 : -1;
      face = "squint";
      faceDx = 1;
      blinks = false;
      break;
    case "call":
      by += [0, -1][Math.floor(t / 3) % 2];
      faceDx = [0, 1][Math.floor(t / 2) % 2];
      break;
    case "break":
      by += [0, 0, 0, -1, -1, -1][t % 6];
      face = "happy";
      blinks = false;
      break;
    case "gaming":
      by += t % 2 ? 0 : -1;
      face = "squint";
      blinks = false;
      hideArms = true;
      armLy = t % 2;
      armRy = (t + 1) % 2;
      break;
    case "streaming":
      by += [0, -1, 0, 0][t % 4];
      armY = 7;
      break;
    case "lowpower":
      by += [0, 0, 0, 0, 1, 1, 1, 1][t % 8];
      face = "dim";
      blinks = false;
      armY = 6;
      break;
    case "coding":
      by += t % 2 ? 0 : -1;
      face = "squint";
      blinks = false;
      hideArms = true;
      armLy = t % 2 ? 0 : 1;
      armRy = t % 2 ? 1 : 0;
      break;
    case "working":
      by += [0, -1, 0, 0][t % 4];
      dx = [0, 1, 0, -1][t % 4];
      break;
    case "listening":
      by += [0, -1, -2, -1][t % 4];
      face = "happy";
      blinks = false;
      armY = 7;
      break;
    // Leaning in, eyes wide and unblinking: attentive rather than the bobbing
    // enjoyment of `listening` (Spotify) directly above.
    case "hearing":
      by += [0, 0, -1, 0][t % 4];
      face = "wide";
      blinks = false;
      faceDx = 1;
      break;
    case "speaking":
      by += [0, -1, 0, 0][t % 4];
      face = "open";
      blinks = false;
      break;
    case "sleeping":
      by += [0, 0, 0, 0, 0, 1, 1, 1, 1, 1][t % 10];
      face = "dim";
      blinks = false;
      break;
    case "happy":
      by += [0, -2, -2, -1, 0, 0][t % 6];
      face = "happy";
      blinks = false;
      break;
    case "alert":
      dx = [-1, 1][t % 2];
      by += [0, -1][Math.floor(t / 2) % 2];
      face = "wide";
      blinks = false;
      break;
    // --- app-specific states beyond the original pip-pixel set ---
    case "juggling":
      by += [0, -1, 0, -1, 0, 0][t % 6];
      faceDx = [-1, 0, 1, 0][Math.floor(t / 2) % 4]; // eyes darting between sessions
      blinks = false;
      break;
    case "forgotten":
      by += [0, 0, -1, -1, 0, 0, 1, 1][t % 8];
      face = t % 16 < 8 ? "wide" : "open";
      faceDx = t % 20 < 10 ? -1 : 1;
      blinks = false;
      break;
    case "review_requested":
      by += [0, 0, -1, 0][t % 4];
      face = "up";
      faceDx = 1;
      blinks = false;
      break;
    case "mentioned":
    case "assigned":
    case "digest_ready":
    case "welcome_back":
      by += [0, -1, -1, 0][t % 4];
      face = "happy";
      blinks = false;
      break;
    // A brisk two-step walk rather than the idle float — a postman arriving.
    case "mail_new":
    case "brief_ready":
      by += [0, 0, -1, -1][t % 4];
      dx = [0, 0, 1, 1][t % 4];
      armY = 3; // arm up, holding the envelope out
      faceDx = 1; // looking at you as they hand it over
      break;
    // The one kind of mail you were already waiting on, so it hops like happy.
    case "mail_reply":
      by += [0, -2, -2, -1, 0, 0][t % 6];
      face = "happy";
      blinks = false;
      armY = 3;
      break;
    case "meeting_soon":
      dx = [0, 1][t % 2];
      by += [0, -1][Math.floor(t / 2) % 2];
      face = "wide";
      blinks = false;
      break;
    case "chat_idle":
      by += [0, 0, 0, -1, -1, -1, 0, 0][t % 8];
      faceDx = t % 61 < 8 ? 1 : t % 89 < 6 ? -1 : 0;
      break;
    case "chat_typing":
      by += [0, 0, -1, -1][t % 4];
      face = "squint";
      blinks = false;
      faceDx = 1;
      break;
  }
  if (blinks && (t + 9) % 41 < 2) face = "blink";

  const x = BX + dx;

  // ground shadow, tightens as the bot rises
  const lift = by - BY;
  ox.fillStyle = "rgba(7,9,16,.28)";
  ox.fillRect(x + 2 - Math.min(0, lift), 19, 10 + Math.min(0, lift) * 2, 1);

  // props behind the body
  if (s === "sleeping") {
    for (let i = 0; i < 3; i++) {
      const p = (t + i * 4) % 12;
      if (p < 9) blit(ART.zed, 19 + Math.floor(p / 3), 9 - p, null);
    }
  }
  if (s === "listening") {
    for (let i = 0; i < 2; i++) {
      const p = (t + i * 5) % 10;
      blit(ART.note, 21, 13 - p - i, null);
    }
  }
  if (s === "happy" || s === "mentioned" || s === "assigned" || s === "digest_ready" || s === "welcome_back" || s === "mail_reply") {
    for (let i = 0; i < 7; i++) {
      const p = (t * 2 + i * 3) % 22;
      px(1 + Math.floor(rnd(i) * 22), p, i % 2 ? pal.b : pal.a);
    }
  }
  if (s === "thinking" && t % 8 < 5) {
    px(20, 5 - (t % 8), pal.b);
    px(21, 3 - Math.floor((t % 8) / 2), pal.b);
  }

  // arms
  if (!hideArms) {
    blit(ART.arm, x - 2, by + armY + armLy, null);
    blit(ART.arm, x + 14, by + armY + armRy, null);
  }

  // antenna
  const pulse =
    s === "alert" || s === "forgotten" || s === "meeting_soon"
      ? t % 2 === 0
      : s === "sleeping"
        ? t % 10 < 5
        : t % 8 < 4;
  box(x + 6, by - 1, 2, 1, pal.o);
  blit(ART.bulb, x + 5, by - 5, pulse ? null : { b: "d" });

  // body + face
  blit(ART.body, x, by, null);
  blit(FACES[face], x + 3 + faceDx, by + 4, null);

  // props in front / worn
  switch (s) {
    case "coding": {
      blit(ART.laptop, x + 3, by + 8, null);
      for (let i = 0; i < 3; i++) {
        if (rnd(t * 3 + i) > 0.5) px(x + 4 + Math.floor(rnd(t + i) * 6), by + 12, pal.b);
      }
      box(x + 5, by + 9 + (t % 2), 4, 1, pal.b);
      box(x + 3, by + 11 + armLy, 2, 1, pal.a);
      box(x + 9, by + 11 + armRy, 2, 1, pal.a);
      box(x + 3, by + 12 + armLy, 2, 1, pal.o);
      box(x + 9, by + 12 + armRy, 2, 1, pal.o);
      break;
    }
    case "thinking": {
      const on = t % 10 > 4;
      blit(ART.lamp, x + 11, by - 6, on ? null : { b: "d" });
      if (on) {
        px(x + 10, by - 5, pal.b);
        px(x + 17, by - 5, pal.b);
        px(x + 10, by - 2, pal.b);
        px(x + 17, by - 2, pal.b);
      }
      break;
    }
    case "debugging": {
      blit(ART.lens, x + 13, by + 2, null);
      blit(ART.bug, x + 2 + (t % 9), by + 8, null);
      break;
    }
    case "reviewing":
      blit(ART.page, x + 3, by + 6, null);
      break;
    case "deploying": {
      const ry = 12 - (t % 20);
      blit(ART.rocket, x + 13, ry, null);
      if (t % 2) blit(ART.flame, x + 13, ry + 5, null);
      break;
    }
    case "syncing": {
      blit(ART.cloud, x + 12, by - 4, null);
      blit(t % 4 < 2 ? ART.up : ART.down, x + 14, by - 1, null);
      break;
    }
    case "waiting": {
      const flip = Math.floor(t / 10) % 2;
      blit(flip ? ART.hourB : ART.hourA, x + 14, by + 3, null);
      if (!flip && t % 2) px(x + 16, by + 7, pal.b);
      break;
    }
    case "writing": {
      const wx = x + 13 + [0, 1, 2, 1][t % 4];
      blit(ART.pad, x + 11, by + 6, null);
      box(wx, by + 3, 1, 3, pal.b);
      px(wx, by + 6, pal.o);
      break;
    }
    case "call": {
      blit(ART.phone, x + 12, by + 1, null);
      if (t % 4 < 2) {
        px(x + 17, by + 1, pal.b);
        px(x + 18, by + 3, pal.b);
      }
      break;
    }
    case "break": {
      blit(ART.mug, x + 13, by + 6, null);
      px(x + 15, by + 4 - (t % 3), pal.w);
      break;
    }
    case "gaming": {
      blit(ART.gamepad, x + 2, by + 7, null);
      box(x + 2, by + 8 + armLy, 2, 1, pal.a);
      box(x + 9, by + 8 + armRy, 2, 1, pal.a);
      if (t % 7 < 2) box(x + 3, by + 4, 8, 1, pal.b);
      break;
    }
    case "streaming": {
      box(x + 1, by - 1, 12, 1, pal.o);
      px(x + 1, by, pal.o);
      px(x + 12, by, pal.o);
      box(x - 2, by + 2, 2, 5, pal.o);
      box(x + 14, by + 2, 2, 5, pal.o);
      box(x - 2, by + 3, 2, 3, pal.a);
      box(x + 14, by + 3, 2, 3, pal.a);
      px(x - 1, by + 7, pal.o);
      px(x, by + 8, pal.o);
      px(x + 1, by + 8, pal.b);
      if (t % 4 < 2) box(x + 16, by - 4, 2, 2, pal.b);
      break;
    }
    case "lowpower": {
      const lv = 3 - Math.floor((t % 24) / 8);
      blit(ART.batt, x + 10, by - 5, null);
      box(x + 11, by - 4, lv, 3, lv > 1 ? pal.a : "#ff6f7d");
      px(x + 17, by - 4, pal.o);
      px(x + 17, by - 3, pal.o);
      break;
    }
    case "working": {
      blit(t % 2 ? ART.wrenchA : ART.wrenchB, x + 14, by + 1, null);
      if (t % 4 === 0) {
        px(x + 17, by, pal.b);
        px(x + 13, by - 1, pal.b);
      }
      break;
    }
    case "listening": {
      box(x + 1, by - 1, 12, 1, pal.o);
      px(x + 1, by, pal.o);
      px(x + 12, by, pal.o);
      box(x - 2, by + 2, 2, 5, pal.o);
      box(x + 14, by + 2, 2, 5, pal.o);
      box(x - 2, by + 3, 2, 3, pal.a);
      box(x + 14, by + 3, 2, 3, pal.a);
      break;
    }
    // The voice assistant's two audible turns. Both draw the same three pairs
    // of sound bars either side of the body at ear height; the only difference
    // is which way the lit one travels — inward while it listens to you,
    // outward while it answers. At 24x24 that direction is the whole cue, so
    // they are drawn together rather than as two near-identical blocks.
    case "hearing":
    case "speaking": {
      const step = s === "hearing" ? 2 - (t % 3) : t % 3;
      for (let k = 0; k < 3; k++) {
        const lit = k === step;
        // Placed clear of both the body (x..x+13) and the arms (x-2 / x+14)
        // so nothing overdraws the sprite itself.
        box(x - 3 - k, by + (lit ? 4 : 5), 1, lit ? 5 : 3, lit ? pal.a : pal.d);
        box(x + 16 + k, by + (lit ? 4 : 5), 1, lit ? 5 : 3, lit ? pal.a : pal.d);
      }
      break;
    }
    case "sleeping":
      blit(ART.cap, x + 1, by - 3, null);
      break;
    case "happy":
      blit(t % 2 ? ART.flagA : ART.flagB, x + 13, by + 1, null);
      break;
    case "alert":
      blit(ART.sign, x + 12, by + 1, null);
      break;
    // The postman: cap on the head for all three, and either an envelope (one
    // message) or the satchel (a morning's worth) held up beside it.
    case "mail_new":
    case "mail_reply":
    case "brief_ready": {
      // Four rows now (crown, crown, band, peak), so it starts one row higher
      // than a three-row cap would and still lands its band on the head.
      blit(ART.postCap, x + 1, by - 4, null);
      const held = s === "brief_ready" ? ART.mailStack : ART.envelope;
      // Six columns wide, so it starts one pixel inside the body's right edge
      // (x+13) rather than clear of it — which is also what makes it look held
      // rather than floating. Rides the body's bob so arm and cargo stay together.
      blit(held, x + 13, by + 1 + armRy, null);
      break;
    }
    case "meeting_soon":
      // The hourglass already in this sprite set, reused: a meeting about to
      // start is the same "time is running out" idea it was drawn for.
      blit(t % 4 < 2 ? ART.hourA : ART.hourB, x + 14, by + 3, null);
      break;
    case "chat_idle":
      blit(ART.bubble, x + 12, by + 1, null);
      break;
    case "chat_typing": {
      blit(ART.bubble, x + 12, by + 1, null);
      const dots = Math.floor(t / 3) % 4;
      for (let i = 0; i < dots; i++) px(x + 14 + i, by + 3, pal.b);
      break;
    }
  }

  targetCtx.clearRect(0, 0, 24, 24);
  targetCtx.drawImage(ox.canvas, 0, 0);
}

const still = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
// idle/sleeping don't need the full 10fps cadence (idle's bob cycle is 8
// ticks, sleeping's is 10) — slowing the tick rate while ambient and nothing
// is actively happening keeps this from burning CPU on an always-on-top
// overlay that's visible 100% of the time.
const FAST_MS = 95;
const SLOW_MS = 380;
const SLOW_STATES = new Set(["idle", "sleeping", "lowpower", "chat_idle"]);

function scheduleTick() {
  if (tickTimer) clearInterval(tickTimer);
  if (still) return;
  const ms = SLOW_STATES.has(current) ? SLOW_MS : FAST_MS;
  tickTimer = setInterval(() => {
    tick++;
    draw();
  }, ms);
}

let els = null;

// els: { canvas, root, ind, title, sub }.
// root is the element carrying data-pip-state + --a (see pip.css) — kept
// separate from <body> so the sprite's mood/accent never touches the
// existing layout-state class system in main.js (body.className stays
// exactly the current 7-value scheme; see main.js/pipstate.js).
export function initPip(elements) {
  els = elements;
  const off = document.createElement("canvas");
  off.width = off.height = 24;
  ox = off.getContext("2d");
  targetCtx = els.canvas.getContext("2d");
  targetCtx.imageSmoothingEnabled = false;
  els.root.dataset.pipState = "idle";
  readPalette(els.root);
  applyMeta("idle");
  tick = 0;
  draw();
  scheduleTick();
}

function applyMeta(name, overrides = {}) {
  const meta = { ...(PIP_META[name] || PIP_META.idle), ...overrides };
  if (els.ind) els.ind.dataset.ind = meta.ind || "none";
  if (els.title) els.title.textContent = meta.title || "";
  if (els.sub) els.sub.textContent = meta.sub || "";
}

export function setPipState(name, overrides = {}) {
  if (!els) return;
  current = PIP_META[name] ? name : "idle";
  els.root.dataset.pipState = current;
  readPalette(els.root);
  applyMeta(current, overrides);
  tick = 0;
  draw();
  scheduleTick();
}

export function pausePipAnimation() {
  if (tickTimer) {
    clearInterval(tickTimer);
    tickTimer = null;
  }
}

export function resumePipAnimation() {
  if (els) scheduleTick();
}
