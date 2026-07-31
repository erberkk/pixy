// The mascot window is transparent and click-through everywhere except the
// pill itself, so the backend needs to know that rectangle. Kept in its own
// module because everything that changes the window's visible shape has to
// re-report it — and having it here rather than in main.js is what keeps the
// import graph acyclic (spotify.js and pixystate.js both need it).
import { invoke } from "../../shared/tauri.js";

export function reportHotRect() {
  const scale = window.devicePixelRatio || 1;
  const pill = document.getElementById("mascot").getBoundingClientRect();

  // The quick menu is anchored to the pill, whose box changes with state — 46px
  // bar, 66px resting circle, 90px waiting panel, and up to 620x560 for a
  // digest card. Since we are already measuring it here — on every state
  // change, on right-click, and again once the CSS transition settles — publish
  // the whole box, so the menu's position AND the width of its fan can be
  // derived from the pill it belongs to instead of being tuned for one state.
  const root = document.documentElement;
  root.style.setProperty("--pill-top", pill.top + "px");
  root.style.setProperty("--pill-bottom", pill.bottom + "px");
  root.style.setProperty("--pill-center", pill.left + pill.width / 2 + "px");
  root.style.setProperty("--pill-width", pill.width + "px");

  // Room the fan needs under the pill. The window is a fixed 740x620 and does
  // not grow, so under a tall card there is no room left below — the menu would
  // be silently clipped off the bottom edge rather than moved, so it flips and
  // fans upward instead.
  root.classList.toggle("quick-menu-above", pill.bottom + 52 > window.innerHeight);

  const rects = [pill];
  const quickMenu = document.getElementById("quick-menu");
  if (quickMenu.classList.contains("visible")) {
    // quick-menu itself is a 0x0 positioning anchor — its buttons are placed
    // via transform outside that box, so union each button instead.
    document.querySelectorAll(".quick-menu-item").forEach((btn) => rects.push(btn.getBoundingClientRect()));
  }
  const left = Math.min(...rects.map((r) => r.left));
  const top = Math.min(...rects.map((r) => r.top));
  const right = Math.max(...rects.map((r) => r.right));
  const bottom = Math.max(...rects.map((r) => r.bottom));
  invoke("set_hot_rect", {
    x: left * scale,
    y: top * scale,
    width: (right - left) * scale,
    height: (bottom - top) * scale,
  });
}

// Box sizes animate over ~0.2-0.3s (CSS transitions) — report both the
// starting size and, after the transition settles, the final size, rather
// than tracking every intermediate frame.
// Exported for spotify.js — that panel lives inside #mascot too and needs
// the same hot-rect bookkeeping.
export function reportHotRectSoon() {
  reportHotRect();
  setTimeout(reportHotRect, 320);
}

// Exported (as a function, not a live binding) for spotify.js to check
