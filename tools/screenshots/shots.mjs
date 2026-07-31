// Screenshots of the real UI without running the app.
//
// The frontend only touches Tauri through four bindings (shared/tauri.js), so a
// stub installed before any page script runs is indistinguishable from the real
// thing. CDP's addScriptToEvaluateOnNewDocument does exactly that, which means
// nothing in src/ has to change to be photographed.
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { readFile, mkdir, writeFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";

const ROOT = process.argv[2] || "src";
const OUT = process.argv[3] || "shots";
const PORT = 8731;
const CDP_PORT = 9333;
const SCALE = 2; // retina-crisp output

const MIME = {
  ".html": "text/html", ".js": "text/javascript", ".css": "text/css",
  ".png": "image/png", ".svg": "image/svg+xml", ".json": "application/json",
  ".wasm": "application/wasm", ".onnx": "application/octet-stream",
  ".woff2": "font/woff2", ".ttf": "font/ttf",
};

// --- static server ------------------------------------------------------------
const server = createServer(async (req, res) => {
  const path = decodeURIComponent(req.url.split("?")[0]);
  const file = join(ROOT, normalize(path).replace(/^(\.\.[/\\])+/, ""));
  try {
    const body = await readFile(file);
    res.writeHead(200, { "content-type": MIME[extname(file)] || "application/octet-stream" });
    res.end(body);
  } catch {
    res.writeHead(404).end("not found");
  }
});
await new Promise((r) => server.listen(PORT, r));

// --- chrome + CDP -------------------------------------------------------------
const CHROME = "C:/Program Files/Google/Chrome/Application/chrome.exe";
const profile = join(process.env.TEMP || "/tmp", "pixy-shots-profile");
const chrome = spawn(CHROME, [
  "--headless=new",
  `--remote-debugging-port=${CDP_PORT}`,
  `--user-data-dir=${profile}`,
  "--no-first-run", "--no-default-browser-check", "--disable-gpu",
  "--hide-scrollbars", "--allow-insecure-localhost",
  `--force-device-scale-factor=${SCALE}`,
  "--window-size=1400,900",
  "about:blank",
], { stdio: "ignore" });

async function cdpTarget() {
  for (let i = 0; i < 60; i++) {
    try {
      const r = await fetch(`http://127.0.0.1:${CDP_PORT}/json/list`);
      const list = await r.json();
      const page = list.find((t) => t.type === "page");
      if (page) return page.webSocketDebuggerUrl;
    } catch {}
    await new Promise((r) => setTimeout(r, 250));
  }
  throw new Error("chrome never came up");
}

const ws = new WebSocket(await cdpTarget());
await new Promise((r) => (ws.onopen = r));
let msgId = 0;
const pending = new Map();
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pending.has(m.id)) {
    const { resolve, reject } = pending.get(m.id);
    pending.delete(m.id);
    m.error ? reject(new Error(JSON.stringify(m.error))) : resolve(m.result);
  }
};
const send = (method, params = {}) =>
  new Promise((resolve, reject) => {
    const id = ++msgId;
    pending.set(id, { resolve, reject });
    ws.send(JSON.stringify({ id, method, params }));
  });

await send("Page.enable");
await send("Runtime.enable");
await send("DOM.enable");


// --- the Tauri stub -----------------------------------------------------------
// Permissive on purpose: anything the UI calls that isn't listed returns null
// rather than throwing, so one unhandled command can't blank a whole window.
const { tunableDefaults } = await import("./tunables.mjs");
const { RESPONSES } = await import("./fixtures.mjs");
RESPONSES.get_tunables = {
  groups: [], settings: [], overridden: [],
  values: await tunableDefaults(),
};

const STUB = `
window.__PIXY = { handlers: {}, calls: [] };
const RESPONSES = ${JSON.stringify(RESPONSES)};
const noop = () => {};
window.__TAURI__ = {
  core: {
    invoke: async (cmd) => {
      window.__PIXY.calls.push(cmd);
      return cmd in RESPONSES ? RESPONSES[cmd] : null;
    },
  },
  event: {
    listen: async (name, cb) => {
      (window.__PIXY.handlers[name] ||= []).push(cb);
      return noop;
    },
    emit: async () => {},
  },
  window: {
    getCurrentWindow: () => ({
      startDragging: noop, minimize: noop, toggleMaximize: noop,
      close: noop, show: noop, hide: noop, setSize: noop,
      outerSize: async () => ({}),
    }),
  },
  webview: { getCurrentWebview: () => ({ onDragDropEvent: async () => noop }) },
};
window.__TAURI_INTERNALS__ = new Proxy({}, { get: () => async () => null });
// Fire a backend event the way the real listener would receive it.
window.__fire = (name, payload) =>
  (window.__PIXY.handlers[name] || []).forEach((cb) => cb({ payload, event: name }));
`;
await send("Page.addScriptToEvaluateOnNewDocument", { source: STUB });

// --- helpers ------------------------------------------------------------------
const evaluate = async (expression) => {
  const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || "eval failed");
  return r.result?.value;
};

async function open(page) {
  await send("Page.navigate", { url: `http://127.0.0.1:${PORT}/${page}` });
  await new Promise((r) => setTimeout(r, 900)); // modules + first paint
}

async function shot(name, selector, { pad = 0, bg = null } = {}) {
  await send("Emulation.setDefaultBackgroundColorOverride", {
    color: bg || { r: 0, g: 0, b: 0, a: 0 },
  });
  // An array unions the boxes — the quick menu and the pill are siblings, so
  // photographing "the widget with its menu open" needs both, not either.
  const selectors = Array.isArray(selector) ? selector : [selector];
  const box = await evaluate(`(() => {
    const rects = ${JSON.stringify(selectors)}
      .flatMap((s) => [...document.querySelectorAll(s)])
      .filter((el) => el && el.getClientRects().length)
      .map((el) => el.getBoundingClientRect())
      .filter((r) => r.width > 0 && r.height > 0);
    if (!rects.length) return null;
    const x = Math.min(...rects.map((r) => r.left));
    const y = Math.min(...rects.map((r) => r.top));
    const right = Math.max(...rects.map((r) => r.right));
    const bottom = Math.max(...rects.map((r) => r.bottom));
    return { x, y, width: right - x, height: bottom - y };
  })()`);
  if (!box || box.width < 2) throw new Error(`${name}: "${selector}" not visible`);
  const { data } = await send("Page.captureScreenshot", {
    format: "png",
    captureBeyondViewport: true,
    clip: {
      x: Math.max(0, box.x - pad), y: Math.max(0, box.y - pad),
      width: box.width + pad * 2, height: box.height + pad * 2, scale: SCALE,
    },
  });
  await mkdir(OUT, { recursive: true });
  await writeFile(join(OUT, `${name}.png`), Buffer.from(data, "base64"));
  console.log(`  ${name.padEnd(26)} ${Math.round(box.width)}x${Math.round(box.height)} @${SCALE}x`);
}

// --- run ----------------------------------------------------------------------
const script = process.argv[4];
if (script) {
  const { pathToFileURL } = await import("node:url");
  const { resolve } = await import("node:path");
  const mod = await import(pathToFileURL(resolve(script)).href);
  await mod.default({ open, evaluate, shot, send });
}

ws.close();
chrome.kill();
server.close();
