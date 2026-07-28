// The mascot window is transparent and click-through everywhere except the
// pill itself, so the backend needs to know that rectangle. Kept in its own
// module because everything that changes the window's visible shape has to
// re-report it — and having it here rather than in main.js is what keeps the
// import graph acyclic (spotify.js and pipstate.js both need it).
import { invoke } from "../../shared/tauri.js";

export function reportHotRect() {
  const scale = window.devicePixelRatio || 1;
  const pill = document.getElementById("mascot").getBoundingClientRect();

  // The quick menu is anchored below the pill, but the pill's height changes
  // with state (46px bar / 66px resting circle / 90px waiting panel). Since we
  // are already measuring it here — on every state change, and again once the
  // CSS transition settles — publish its bottom edge so the menu can sit the
  // same distance under it in every state instead of being tuned for one.
  document.documentElement.style.setProperty("--pill-bottom", pill.bottom + "px");

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
