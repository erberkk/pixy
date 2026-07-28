// Memory mode: a read-only, force-directed graph over Claude Code's own
// memory files (see content/memory.rs). It never writes, classifies or decides
// what is worth remembering — Claude already does that. It only lays out what
// is already on disk and lets you inspect it.
import { invoke } from "../../shared/tauri.js";
import { escapeHtml, markdownToHtml } from "../../shared/markdown.js";
import { timeAgo } from "../../shared/format.js";
import { showToast } from "../lib/toast.js";
import { getMode } from "../lib/mode.js";
import {
  el,
  searchInput,
  notesCount,
  memoryFiltersEl,
  memoryListEl,
  memoryCanvas,
  memoryCtx,
  memoryGraphWrap,
  memoryEmptyEl,
  memoryDetailEl,
} from "../lib/dom.js";

let memories = []; // raw MemoryNode[] from list_memories
let memoryTypeFilter = "all";
let memoryProjectFilter = "all";
let memorySelected = null; // selected node's name, or null

const MEMORY_TYPE_COLORS = {
  user: "#7fc3e8",
  feedback: "#e8b04b",
  project: "#7bc79e",
  reference: "#c792ea",
  note: "#9aa0a6",
  ghost: "#4d4d52",
};

function memoryTypeColor(type) {
  return MEMORY_TYPE_COLORS[type] || MEMORY_TYPE_COLORS.note;
}

export async function loadMemories() {
  memories = await invoke("list_memories");
  // Most-recently-touched memories are the ones most likely to matter right
  // now — surface those first instead of the backend's project/name order.
  memories.sort((a, b) => b.updated_at - a.updated_at);
  renderMemoryFilters();
  renderMemoryList();
  renderMemoryLegend();
  buildGraph();
  updateMemoryCounts();
}

function updateMemoryCounts() {
  const n = memories.length;
  notesCount.textContent = n + (n === 1 ? " memory" : " memories");
  el("memoryGraphCount").textContent = n ? n + (n === 1 ? " node" : " nodes") + " · " + graphEdges.length + " links" : "";
}

/* ---------------- filters + sidebar list ---------------- */
function renderMemoryFilters() {
  const types = ["all", ...new Set(memories.map((m) => m.type))];
  const projects = ["all", ...new Set(memories.map((m) => m.project))];

  const typeRow =
    '<div class="memory-filter-row">' +
    types
      .map((t) => {
        const count = t === "all" ? memories.length : memories.filter((m) => m.type === t).length;
        return (
          '<button class="memory-chip ' +
          (memoryTypeFilter === t ? "active" : "") +
          '" data-kind="type" data-value="' +
          escapeHtml(t) +
          '">' +
          escapeHtml(t === "all" ? "All types" : t) +
          '<span class="chip-count">' +
          count +
          "</span></button>"
        );
      })
      .join("") +
    "</div>";

  const projectRow =
    '<div class="memory-filter-row">' +
    projects
      .map((p) => {
        const count = p === "all" ? memories.length : memories.filter((m) => m.project === p).length;
        return (
          '<button class="memory-chip ' +
          (memoryProjectFilter === p ? "active" : "") +
          '" data-kind="project" data-value="' +
          escapeHtml(p) +
          '">' +
          escapeHtml(p === "all" ? "All projects" : prettifyProjectSlug(p)) +
          '<span class="chip-count">' +
          count +
          "</span></button>"
        );
      })
      .join("") +
    "</div>";

  memoryFiltersEl.innerHTML = typeRow + projectRow;
  memoryFiltersEl.querySelectorAll(".memory-chip").forEach((chip) => {
    chip.addEventListener("click", () => {
      if (chip.dataset.kind === "type") memoryTypeFilter = chip.dataset.value;
      else memoryProjectFilter = chip.dataset.value;
      renderMemoryFilters();
      renderMemoryList();
      buildGraph();
      updateMemoryCounts();
    });
  });
}

function prettifyProjectSlug(slug) {
  const parts = slug.split("-").filter(Boolean);
  return parts.length ? parts[parts.length - 1] : slug;
}

function memoryMatchesFilters(m) {
  if (memoryTypeFilter !== "all" && m.type !== memoryTypeFilter) return false;
  if (memoryProjectFilter !== "all" && m.project !== memoryProjectFilter) return false;
  return true;
}

function memoryMatchesSearch(m) {
  const q = searchInput.value.trim().toLowerCase();
  if (!q) return true;
  return (
    m.name.toLowerCase().includes(q) ||
    m.description.toLowerCase().includes(q) ||
    m.body.toLowerCase().includes(q)
  );
}

export function renderMemoryList() {
  const list = memories.filter((m) => memoryMatchesFilters(m) && memoryMatchesSearch(m));

  if (memories.length === 0) {
    memoryListEl.innerHTML =
      '<div class="empty-list">No memories yet.<br>Claude writes these as it learns things worth remembering.</div>';
    return;
  }
  if (list.length === 0) {
    memoryListEl.innerHTML = '<div class="empty-list">No matches found.</div>';
    return;
  }

  memoryListEl.innerHTML = list
    .map(
      (m) =>
        '<div class="memory-item ' +
        (m.name === memorySelected ? "active" : "") +
        '" data-name="' +
        escapeHtml(m.name) +
        '">' +
        '<div class="memory-item-top">' +
        '<span class="memory-item-dot" style="background:' +
        memoryTypeColor(m.type) +
        '"></span>' +
        '<span class="memory-item-name">' +
        escapeHtml(m.name) +
        "</span>" +
        '<span class="memory-item-time">' +
        timeAgo(m.updated_at) +
        "</span>" +
        "</div>" +
        '<span class="memory-item-desc">' +
        escapeHtml(m.description) +
        "</span>" +
        '<span class="memory-item-project">' +
        escapeHtml(m.type) +
        '<span class="dot-sep"></span>' +
        escapeHtml(prettifyProjectSlug(m.project)) +
        "</span>" +
        "</div>"
    )
    .join("");

  memoryListEl.querySelectorAll(".memory-item").forEach((item) => {
    item.addEventListener("click", () => selectMemory(item.dataset.name, true));
  });
}

function renderMemoryLegend() {
  const legendEl = el("memoryLegend");
  const typesPresent = [...new Set(memories.map((m) => m.type))];
  if (typesPresent.length === 0) {
    legendEl.innerHTML = "";
    legendEl.style.display = "none";
    return;
  }
  legendEl.style.display = "flex";
  legendEl.innerHTML = typesPresent
    .sort()
    .map(
      (t) =>
        '<div class="memory-legend-row"><span class="memory-legend-dot" style="background:' +
        memoryTypeColor(t) +
        '"></span>' +
        escapeHtml(t) +
        "</div>"
    )
    .join("");
}

/* ---------------- graph model ---------------- */
let graphNodes = []; // {name, real, x, y, vx, vy, fx, fy, r, data}
let graphEdges = []; // {a, b} — indices into graphNodes
let simAlpha = 1;
let simRunning = false;

function buildGraph() {
  const visible = memories.filter((m) => memoryMatchesFilters(m));
  const visibleNames = new Set(visible.map((m) => m.name));
  const prevByName = new Map(graphNodes.map((n) => [n.name, n]));

  const nodes = [];
  const nodeIndex = new Map();

  function addNode(name, real, data) {
    if (nodeIndex.has(name)) return nodeIndex.get(name);
    const prev = prevByName.get(name);
    const angle = Math.random() * Math.PI * 2;
    const radius = 40 + Math.random() * 60;
    const node = prev
      ? { ...prev, real, data }
      : {
          name,
          real,
          data,
          x: Math.cos(angle) * radius,
          y: Math.sin(angle) * radius,
          vx: 0,
          vy: 0,
          fx: null,
          fy: null,
          r: real ? 10 : 7,
        };
    node.real = real;
    node.data = data;
    node.r = real ? 10 : 7;
    nodeIndex.set(name, nodes.length);
    nodes.push(node);
    return nodes.length - 1;
  }

  for (const m of visible) addNode(m.name, true, m);
  // Ghost nodes for links pointing at a memory that doesn't exist (yet, or
  // renamed) — rendered dashed rather than silently dropping the edge,
  // matching Obsidian's own unresolved-link convention.
  for (const m of visible) {
    for (const link of m.links) {
      if (!visibleNames.has(link)) addNode(link, false, null);
    }
  }

  const edges = [];
  for (const m of visible) {
    const a = nodeIndex.get(m.name);
    for (const link of m.links) {
      const b = nodeIndex.get(link);
      if (b !== undefined && a !== b) edges.push({ a, b });
    }
  }

  // Well-connected memories read as visually more important — a subtle size
  // bump per edge (capped) rather than a flat radius for every real node.
  const degree = new Array(nodes.length).fill(0);
  for (const edge of edges) {
    degree[edge.a]++;
    degree[edge.b]++;
  }
  nodes.forEach((node, i) => {
    node.r = node.real ? 9 + Math.min(degree[i], 6) * 1.3 : 7;
  });

  graphNodes = nodes;
  graphEdges = edges;
  simAlpha = 1;
  startSimLoop();

  memoryEmptyEl.style.display = memories.length === 0 ? "flex" : "none";
}

function stepSimulation() {
  const n = graphNodes.length;
  if (n === 0) return;

  // Tuned empirically against real data (a handful to a few dozen nodes):
  // the first pass used DAMPING 0.85 with REPEL 2200, which killed ~85% of
  // each step's velocity before it could accumulate any real separation —
  // the graph visibly froze fully clustered near the center well before
  // alpha decayed to its stop threshold. REPEL is now an order of magnitude
  // higher and DAMPING much lighter so nodes actually reach a comfortably
  // spread layout within the same decay schedule.
  const REPEL = 26000;
  const SPRING_LEN = 130;
  const SPRING_K = 0.02;
  const CENTER_K = 0.004;
  const DAMPING = 0.94;

  for (let i = 0; i < n; i++) {
    const node = graphNodes[i];
    if (node.fx !== null) continue;
    let fx = -node.x * CENTER_K;
    let fy = -node.y * CENTER_K;

    for (let j = 0; j < n; j++) {
      if (i === j) continue;
      const other = graphNodes[j];
      let dx = node.x - other.x;
      let dy = node.y - other.y;
      let distSq = dx * dx + dy * dy || 0.01;
      const dist = Math.sqrt(distSq);
      const force = (REPEL * simAlpha) / distSq;
      fx += (dx / dist) * force;
      fy += (dy / dist) * force;
    }

    node._fx = fx;
    node._fy = fy;
  }

  for (const edge of graphEdges) {
    const a = graphNodes[edge.a];
    const b = graphNodes[edge.b];
    const dx = b.x - a.x;
    const dy = b.y - a.y;
    const dist = Math.sqrt(dx * dx + dy * dy) || 0.01;
    const force = (dist - SPRING_LEN) * SPRING_K * simAlpha;
    const fx = (dx / dist) * force;
    const fy = (dy / dist) * force;
    if (a.fx === null) {
      a._fx += fx;
      a._fy += fy;
    }
    if (b.fx === null) {
      b._fx -= fx;
      b._fy -= fy;
    }
  }

  for (const node of graphNodes) {
    if (node.fx !== null) {
      node.x = node.fx;
      node.y = node.fy;
      node.vx = 0;
      node.vy = 0;
      continue;
    }
    node.vx = (node.vx + (node._fx || 0)) * DAMPING;
    node.vy = (node.vy + (node._fy || 0)) * DAMPING;
    node.x += node.vx;
    node.y += node.vy;
  }

  simAlpha *= 0.985;
}

/* ---------------- view transform (pan/zoom) ---------------- */
const memoryView = { cx: 0, cy: 0, zoom: 1 };
let hoveredNode = null;
let draggingNode = null;
let dragMoved = false;
let panState = null;

export function resizeMemoryCanvas() {
  const rect = memoryGraphWrap.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  memoryCanvas.width = rect.width * dpr;
  memoryCanvas.height = rect.height * dpr;
  memoryCanvas.style.width = rect.width + "px";
  memoryCanvas.style.height = rect.height + "px";
  memoryCtx.setTransform(dpr, 0, 0, dpr, 0, 0);
}

function worldToScreen(x, y) {
  const rect = memoryGraphWrap.getBoundingClientRect();
  return {
    x: (x - memoryView.cx) * memoryView.zoom + rect.width / 2,
    y: (y - memoryView.cy) * memoryView.zoom + rect.height / 2,
  };
}

function screenToWorld(x, y) {
  const rect = memoryGraphWrap.getBoundingClientRect();
  return {
    x: (x - rect.width / 2) / memoryView.zoom + memoryView.cx,
    y: (y - rect.height / 2) / memoryView.zoom + memoryView.cy,
  };
}

function nodeAt(screenX, screenY) {
  for (let i = graphNodes.length - 1; i >= 0; i--) {
    const node = graphNodes[i];
    const p = worldToScreen(node.x, node.y);
    const dx = screenX - p.x;
    const dy = screenY - p.y;
    if (dx * dx + dy * dy <= (node.r + 4) * (node.r + 4)) return node;
  }
  return null;
}

function draw() {
  const rect = memoryGraphWrap.getBoundingClientRect();
  memoryCtx.clearRect(0, 0, rect.width, rect.height);

  const q = searchInput.value.trim().toLowerCase();
  const searching = getMode() === "memory" && q.length > 0;
  const activeNames = new Set();
  if (hoveredNode || memorySelected) {
    const focus = hoveredNode ? hoveredNode.name : memorySelected;
    activeNames.add(focus);
    for (const edge of graphEdges) {
      const a = graphNodes[edge.a];
      const b = graphNodes[edge.b];
      if (a.name === focus) activeNames.add(b.name);
      if (b.name === focus) activeNames.add(a.name);
    }
  }

  for (const edge of graphEdges) {
    const a = graphNodes[edge.a];
    const b = graphNodes[edge.b];
    const pa = worldToScreen(a.x, a.y);
    const pb = worldToScreen(b.x, b.y);
    const dimmed = (hoveredNode || memorySelected) && !(activeNames.has(a.name) && activeNames.has(b.name));
    const highlighted = (hoveredNode || memorySelected) && activeNames.has(a.name) && activeNames.has(b.name);
    memoryCtx.lineWidth = highlighted ? 1.6 : 1;
    memoryCtx.strokeStyle = dimmed ? "rgba(120,120,130,0.08)" : highlighted ? "rgba(200,200,210,0.55)" : "rgba(150,150,160,0.28)";
    memoryCtx.beginPath();
    memoryCtx.moveTo(pa.x, pa.y);
    memoryCtx.lineTo(pb.x, pb.y);
    memoryCtx.stroke();
  }

  // Labels are collected and drawn in a second pass, after every node
  // circle — otherwise a node drawn later would paint its solid circle
  // right on top of an earlier node's label text.
  const labels = [];

  for (const node of graphNodes) {
    const p = worldToScreen(node.x, node.y);
    const matches = !searching || (node.real && memoryMatchesSearch(node.data));
    const dimmedByFocus = (hoveredNode || memorySelected) && !activeNames.has(node.name);
    let opacity = 1;
    if (!matches) opacity = 0.12;
    else if (dimmedByFocus) opacity = 0.3;
    else if (!node.real) opacity = 0.55;

    const color = node.real ? memoryTypeColor(node.data.type) : MEMORY_TYPE_COLORS.ghost;

    memoryCtx.globalAlpha = opacity;

    // Soft glow behind the node instead of a flat, cheap-looking disc — a
    // radial gradient reaching just past the node's own radius, and (for
    // the currently hovered/selected node) an actual canvas shadow blur so
    // it visibly lifts off the dotted background.
    if (node.name === memorySelected || node === hoveredNode) {
      memoryCtx.shadowColor = color;
      memoryCtx.shadowBlur = 18;
    }
    const glowR = node.r * 2.2;
    const glow = memoryCtx.createRadialGradient(p.x, p.y, 0, p.x, p.y, glowR);
    glow.addColorStop(0, color + "33");
    glow.addColorStop(1, color + "00");
    memoryCtx.fillStyle = glow;
    memoryCtx.beginPath();
    memoryCtx.arc(p.x, p.y, glowR, 0, Math.PI * 2);
    memoryCtx.fill();
    memoryCtx.shadowBlur = 0;

    // The node itself: a radial gradient (lighter core, true type color at
    // the rim) reads as an actual glossy bubble instead of a flat sticker.
    const body = memoryCtx.createRadialGradient(
      p.x - node.r * 0.35,
      p.y - node.r * 0.35,
      node.r * 0.1,
      p.x,
      p.y,
      node.r
    );
    body.addColorStop(0, lightenColor(color, 0.35));
    body.addColorStop(1, color);

    memoryCtx.beginPath();
    memoryCtx.arc(p.x, p.y, node.r, 0, Math.PI * 2);
    if (!node.real) {
      memoryCtx.setLineDash([3, 3]);
      memoryCtx.strokeStyle = MEMORY_TYPE_COLORS.ghost;
      memoryCtx.lineWidth = 1.5;
      memoryCtx.stroke();
      memoryCtx.setLineDash([]);
    } else {
      memoryCtx.fillStyle = body;
      memoryCtx.fill();
      // A thin, slightly darker rim gives the bubble a defined edge against
      // the dotted background instead of the fill just fading into it.
      memoryCtx.lineWidth = 1;
      memoryCtx.strokeStyle = "rgba(0,0,0,0.35)";
      memoryCtx.stroke();
    }
    if (node.name === memorySelected) {
      memoryCtx.lineWidth = 2;
      memoryCtx.strokeStyle = "#fff";
      memoryCtx.stroke();
    }
    memoryCtx.globalAlpha = 1;

    if (memoryView.zoom > 0.55 || node.name === memorySelected || node === hoveredNode) {
      labels.push({ node, p, opacity: matches ? (dimmedByFocus ? 0.45 : 1) : 0.15 });
    }
  }

  memoryCtx.font = "600 11px -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
  memoryCtx.textAlign = "center";
  memoryCtx.textBaseline = "middle";
  for (const { node, p, opacity } of labels) {
    const labelY = p.y + node.r + 14;
    const textWidth = memoryCtx.measureText(node.name).width;
    const padX = 6;
    const boxW = textWidth + padX * 2;
    const boxH = 16;

    memoryCtx.globalAlpha = opacity;
    // A soft pill behind the text is what actually fixes overlapping labels
    // reading as illegible noise — plain text floating over the dotted
    // background (and over other labels/edges) was the main "looks cheap"
    // complaint. A translucent backdrop keeps every label readable at a
    // glance regardless of what's directly behind it.
    memoryCtx.fillStyle = "rgba(10,10,12,0.72)";
    roundRect(memoryCtx, p.x - boxW / 2, labelY - boxH / 2, boxW, boxH, 8);
    memoryCtx.fill();

    memoryCtx.fillStyle = node.name === memorySelected ? "#fff" : "#c8c8ce";
    memoryCtx.fillText(node.name, p.x, labelY + 0.5);
    memoryCtx.globalAlpha = 1;
  }
}

function roundRect(ctx, x, y, w, h, r) {
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
}

// Lightens a "#rrggbb" hex color toward white by `amount` (0-1) — used to
// give each node's radial gradient a brighter core without a second color
// constant to keep in sync with MEMORY_TYPE_COLORS.
function lightenColor(hex, amount) {
  const n = parseInt(hex.slice(1), 16);
  const r = (n >> 16) & 0xff;
  const g = (n >> 8) & 0xff;
  const b = n & 0xff;
  const mix = (c) => Math.round(c + (255 - c) * amount);
  return "rgb(" + mix(r) + "," + mix(g) + "," + mix(b) + ")";
}

function startSimLoop() {
  if (simRunning) return;
  simRunning = true;
  function tick() {
    if (getMode() !== "memory") {
      simRunning = false;
      return;
    }
    if (simAlpha > 0.01 || draggingNode) stepSimulation();
    draw();
    requestAnimationFrame(tick);
  }
  requestAnimationFrame(tick);
}

/* ---------------- interactions ---------------- */
memoryCanvas.addEventListener("mousedown", (e) => {
  const rect = memoryCanvas.getBoundingClientRect();
  const x = e.clientX - rect.left;
  const y = e.clientY - rect.top;
  const hit = nodeAt(x, y);
  dragMoved = false;
  if (hit) {
    draggingNode = hit;
    simAlpha = Math.max(simAlpha, 0.3);
  } else {
    panState = { startX: e.clientX, startY: e.clientY, cx: memoryView.cx, cy: memoryView.cy };
  }
});

window.addEventListener("mousemove", (e) => {
  if (getMode() !== "memory") return;
  const rect = memoryCanvas.getBoundingClientRect();
  const x = e.clientX - rect.left;
  const y = e.clientY - rect.top;

  if (draggingNode) {
    dragMoved = true;
    const world = screenToWorld(x, y);
    draggingNode.fx = world.x;
    draggingNode.fy = world.y;
    draggingNode.x = world.x;
    draggingNode.y = world.y;
    el("memoryTooltip").classList.remove("visible");
    return;
  }
  if (panState) {
    memoryView.cx = panState.cx - (e.clientX - panState.startX) / memoryView.zoom;
    memoryView.cy = panState.cy - (e.clientY - panState.startY) / memoryView.zoom;
    el("memoryTooltip").classList.remove("visible");
    return;
  }
  hoveredNode = nodeAt(x, y);
  memoryCanvas.style.cursor = hoveredNode ? "pointer" : "grab";
  updateMemoryTooltip(x, y);
});

function updateMemoryTooltip(x, y) {
  const tooltip = el("memoryTooltip");
  if (!hoveredNode) {
    tooltip.classList.remove("visible");
    return;
  }
  if (hoveredNode.real) {
    const m = hoveredNode.data;
    tooltip.innerHTML =
      '<div class="tt-name">' +
      escapeHtml(m.name) +
      "</div>" +
      (m.description ? '<div class="tt-desc">' + escapeHtml(m.description) + "</div>" : "") +
      '<div class="tt-meta">' +
      escapeHtml(m.type) +
      " · " +
      escapeHtml(prettifyProjectSlug(m.project)) +
      "</div>";
  } else {
    tooltip.innerHTML =
      '<div class="tt-name">' + escapeHtml(hoveredNode.name) + "</div>" + '<div class="tt-meta">Not written yet</div>';
  }
  const wrapRect = memoryGraphWrap.getBoundingClientRect();
  let left = x + 16;
  let top = y + 16;
  // Keep the tooltip on-screen when hovering a node near the wrap's right or
  // bottom edge instead of letting it spill out and get clipped.
  const maxLeft = wrapRect.width - 250;
  const maxTop = wrapRect.height - 90;
  if (left > maxLeft) left = x - 250;
  if (top > maxTop) top = y - 90;
  tooltip.style.left = Math.max(4, left) + "px";
  tooltip.style.top = Math.max(4, top) + "px";
  tooltip.classList.add("visible");
}

window.addEventListener("mouseup", () => {
  if (draggingNode && !dragMoved) {
    // A plain click (no drag distance) on a node selects it instead of
    // pinning it in place — pinning is reserved for an actual drag gesture.
    selectMemory(draggingNode.name, false);
    draggingNode.fx = null;
    draggingNode.fy = null;
  }
  draggingNode = null;
  panState = null;
});

memoryCanvas.addEventListener("mouseleave", () => {
  hoveredNode = null;
  el("memoryTooltip").classList.remove("visible");
});

memoryCanvas.addEventListener("wheel", (e) => {
  e.preventDefault();
  const rect = memoryCanvas.getBoundingClientRect();
  const before = screenToWorld(e.clientX - rect.left, e.clientY - rect.top);
  const factor = e.deltaY < 0 ? 1.1 : 0.9;
  memoryView.zoom = Math.min(3, Math.max(0.25, memoryView.zoom * factor));
  const after = screenToWorld(e.clientX - rect.left, e.clientY - rect.top);
  memoryView.cx += before.x - after.x;
  memoryView.cy += before.y - after.y;
}, { passive: false });

el("memoryZoomInBtn").addEventListener("click", () => {
  memoryView.zoom = Math.min(3, memoryView.zoom * 1.2);
});
el("memoryZoomOutBtn").addEventListener("click", () => {
  memoryView.zoom = Math.max(0.25, memoryView.zoom / 1.2);
});
el("memoryResetViewBtn").addEventListener("click", () => {
  memoryView.cx = 0;
  memoryView.cy = 0;
  memoryView.zoom = 1;
});

window.addEventListener("resize", () => {
  if (getMode() === "memory") resizeMemoryCanvas();
});

/* ---------------- detail panel ---------------- */
function selectMemory(name, centerView) {
  memorySelected = name;
  renderMemoryList();
  const m = memories.find((x) => x.name === name);
  if (!m) {
    memoryDetailEl.classList.remove("visible");
    return;
  }
  el("memoryDetailType").textContent = m.type;
  el("memoryDetailType").style.background = memoryTypeColor(m.type) + "26";
  el("memoryDetailType").style.color = memoryTypeColor(m.type);
  el("memoryDetailName").textContent = m.name;
  el("memoryDetailDesc").textContent = m.description;
  el("memoryDetailMeta").textContent = prettifyProjectSlug(m.project) + " · " + timeAgo(m.updated_at);
  el("memoryDetailBody").innerHTML = markdownToHtml(m.body);
  memoryDetailEl.classList.add("visible");

  if (centerView) {
    const node = graphNodes.find((n) => n.name === name);
    if (node) {
      memoryView.cx = node.x;
      memoryView.cy = node.y;
    }
  }
}

/* ---------------- memory sources (extra ~/.claude/projects-shaped roots) ---------------- */
const memorySourcesPanel = el("memorySourcesPanel");

async function renderMemorySources() {
  const roots = await invoke("get_memory_roots");
  const list = el("memorySourcesList");

  const defaultRow =
    '<div class="memory-source-row ' +
    (roots.default_root_exists ? "" : "missing") +
    '">' +
    '<div class="memory-source-info">' +
    '<span class="memory-source-label">' +
    (roots.default_root_exists ? "Default" : "Default (not found)") +
    "</span>" +
    '<span class="memory-source-path">' +
    escapeHtml(roots.default_root || "unresolved") +
    "</span>" +
    "</div>" +
    "</div>";

  const extraRows = roots.extra_roots
    .map(
      (path) =>
        '<div class="memory-source-row" data-path="' +
        escapeHtml(path) +
        '">' +
        '<div class="memory-source-info">' +
        '<span class="memory-source-label">Added</span>' +
        '<span class="memory-source-path">' +
        escapeHtml(path) +
        "</span>" +
        "</div>" +
        '<button class="memory-source-remove" title="Remove this source">' +
        '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M6 6l12 12M18 6L6 18"/></svg>' +
        "</button>" +
        "</div>"
    )
    .join("");

  list.innerHTML = defaultRow + extraRows;
  list.querySelectorAll(".memory-source-remove").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const path = btn.closest(".memory-source-row").dataset.path;
      await invoke("remove_memory_root", { path });
      await renderMemorySources();
      loadMemories();
    });
  });
}

el("memorySourcesBtn").addEventListener("click", () => {
  memorySourcesPanel.classList.toggle("visible");
  if (memorySourcesPanel.classList.contains("visible")) renderMemorySources();
});
el("closeMemorySourcesBtn").addEventListener("click", () => {
  memorySourcesPanel.classList.remove("visible");
});
el("addMemoryRootBtn").addEventListener("click", async () => {
  const path = await invoke("add_memory_root");
  if (!path) return; // user cancelled the folder picker
  await renderMemorySources();
  loadMemories();
  showToast("Added memory source");
});

el("closeMemoryDetailBtn").addEventListener("click", () => {
  memorySelected = null;
  memoryDetailEl.classList.remove("visible");
  renderMemoryList();
});

el("copyMemoryBodyBtn").addEventListener("click", () => {
  const m = memories.find((x) => x.name === memorySelected);
  if (!m) return;
  navigator.clipboard.writeText(m.body).then(() => showToast("Memory copied"));
});

el("memoryDetailBody").addEventListener("click", (e) => {
  const link = e.target.closest(".wiki-link");
  if (!link) return;
  const target = memories.find((m) => m.name.toLowerCase() === link.dataset.note.toLowerCase());
  if (target) selectMemory(target.name, true);
  else showToast("Memory not written yet: " + link.dataset.note);
});
