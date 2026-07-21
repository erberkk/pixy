const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// Each pooled terminal window (see tauri.conf.json's terminal/terminal2/3/4
// + windows.rs's TERMINAL_POOL) runs this same file — labeling the titlebar
// with its own slot number is the only thing that tells them apart visually
// when several are open at once.
const currentLabel = window.__TAURI__.window.getCurrentWindow().label;
const slotMatch = currentLabel.match(/(\d+)$/);
document.getElementById("titlebar-title").textContent = slotMatch
  ? `Agent Terminal ${slotMatch[1]}`
  : "Agent Terminal";

const term = new Terminal({
  fontFamily: "Consolas, 'Courier New', monospace",
  fontSize: 14,
  theme: {
    background: "#0d0d0d",
    foreground: "#e8e8e8",
  },
  cursorBlink: true,
});

const fitAddon = new FitAddon.FitAddon();
term.loadAddon(fitAddon);
term.open(document.getElementById("term"));
fitAddon.fit();

function reportSize() {
  invoke("resize_pty", { cols: term.cols, rows: term.rows }).catch(() => {});
}

window.addEventListener("resize", () => {
  fitAddon.fit();
  reportSize();
});

term.onData((data) => {
  invoke("write_to_pty", { input: data }).catch(() => {});
});

// Prompt detection reads xterm.js's own rendered screen buffer instead of
// the raw PTY byte stream — xterm already correctly interprets cursor
// movement/carriage-return redraws/erasure, which the raw bytes alone don't
// give us without reimplementing a terminal emulator on the Rust side.
let snapshotTimer = null;
function scheduleSnapshot() {
  if (snapshotTimer) clearTimeout(snapshotTimer);
  snapshotTimer = setTimeout(reportSnapshot, 150);
}

function reportSnapshot() {
  const buf = term.buffer.active;
  const lines = [];
  for (let i = 0; i < term.rows; i++) {
    const line = buf.getLine(buf.viewportY + i);
    if (line) lines.push(line.translateToString(true));
  }
  invoke("report_terminal_text", { text: lines.join("\n") }).catch(() => {});
}

// Event NAME is suffixed with this window's own label (see terminal.rs) —
// plain listen() with no target option matches ANY emit target by default,
// so a shared event name across all pooled terminal windows meant every
// window's xterm received every OTHER window's output too (keystrokes
// typed in one terminal appeared live in all the others). Scoping via the
// name itself sidesteps that instead of relying on target-based filtering.
listen(`terminal-output:${currentLabel}`, (event) => {
  term.write(new Uint8Array(event.payload), scheduleSnapshot);
});

listen(`terminal-closed:${currentLabel}`, () => {
  term.write("\r\n\x1b[90m[session ended]\x1b[0m\r\n");
});

invoke("start_terminal_session")
  .then(() => reportSize())
  .catch((err) => {
    term.write(`\r\n\x1b[31mFailed to start terminal session: ${err}\x1b[0m\r\n`);
  });

document.getElementById("titlebar-close").addEventListener("click", () => {
  invoke("hide_terminal");
});
document.getElementById("titlebar-minimize").addEventListener("click", () => {
  window.__TAURI__.window.getCurrentWindow().minimize();
});
document.getElementById("titlebar-maximize").addEventListener("click", () => {
  window.__TAURI__.window.getCurrentWindow().toggleMaximize();
});
