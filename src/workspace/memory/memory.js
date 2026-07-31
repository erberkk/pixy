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

  // While a search is running the node/link total is not the number anyone is
  // looking at — how many of them matched is.
  const q = searchInput.value.trim();
  if (q && getMode() === "memory") {
    const hits = memories.filter((m) => memoryMatchesFilters(m) && memoryMatchesSearch(m)).length;
    el("memoryGraphCount").textContent = hits + (hits === 1 ? " match" : " matches");
    return;
  }
  // Trees and what is in them, rather than nodes and links — the shape on
  // screen is now a set of project trees, so that is what the count describes.
  const trees = graphNodes.filter((node) => node.kind === "root").length;
  const shown = graphNodes.filter((node) => node.kind === "memory").length;
  el("memoryGraphCount").textContent = n
    ? shown + (shown === 1 ? " memory" : " memories") + " · " + trees + (trees === 1 ? " project" : " projects")
    : "";
}

// Which of the two empty states applies, if either: nothing on disk at all, or
// nothing the current filters let through. An empty canvas explains neither, and
// the second one is a dead end the user can actually undo.
function updateMemoryEmptyState() {
  const hasNone = memories.length === 0;
  const filteredOut = !hasNone && graphNodes.length === 0;
  memoryEmptyEl.style.display = hasNone || filteredOut ? "flex" : "none";
  if (!filteredOut) {
    el("memoryEmptyTitle").textContent = "No memories yet";
    el("memoryEmptyBody").textContent =
      "Claude will write memory files here as it learns things worth remembering across sessions.";
    return;
  }
  el("memoryEmptyTitle").textContent = "Nothing matches these filters";
  el("memoryEmptyBody").textContent =
    "All " + memories.length + " memories are filtered out. Clear a chip in the sidebar to bring them back.";
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
        '" role="button" tabindex="0" aria-label="' +
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
    // Rows are divs (they carry three stacked lines, which a button would fight
    // with), so the keyboard half of being clickable has to be added by hand.
    item.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        selectMemory(item.dataset.name, true);
      }
    });
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
  let rows = typesPresent
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

  // A dashed empty circle is the one thing on the canvas the type colours don't
  // explain, so it gets a row of its own — but only when there is one to see.
  if (memories.some((m) => m.links.some((link) => !memories.find((x) => x.name === link)))) {
    rows +=
      '<div class="memory-legend-row"><span class="memory-legend-dot ghost"></span>linked, not written yet</div>';
  }
  legendEl.innerHTML = rows;
}

/* ---------------- graph model: one skill tree per project ----------------
   Memories are laid out as a tidy tree per project rather than by a physics
   simulation. The simulation was replaced outright: it shook every node around
   for a second and a half on open, put the same memory somewhere different
   every time (its starting angle was random), and the resulting cloud said
   nothing that this doesn't. Here a project is the root of its own tree and its
   memories branch downward from it, so position means something — depth is
   distance from the project, and a subtree is a topic.

   Nothing moves after the layout is computed. The only motion is the entrance
   in draw(): branches grow out from their parent into the position they will
   keep. Re-running a layout (a filter change) tweens from wherever each node
   already was to its new slot, so nothing jumps either. */
let graphNodes = []; // {name, kind, real, data, project, depth, x, y, r, fromX, fromY}
let graphEdges = []; // {a, b, kind} — indices into graphNodes
let animStart = 0; // performance.now() when the current entrance began
const ANIM_MS = 460;

// Slot size, in world units. NODE_GAP_X is the horizontal distance between two
// adjacent leaves; the tree is centred over its own leaves from there.
const NODE_GAP_X = 104;
const LEVEL_GAP_Y = 92;
const PROJECT_GAP = 1.4; // in leaf slots, between one project's tree and the next

function nodeRadius(kind, depth) {
  if (kind === "root") return 17;
  if (kind === "ghost") return 6.5;
  return depth <= 1 ? 11 : 9.5;
}

// Picks each memory's parent inside its own project. A memory linked to by
// another memory in the same project hangs off it; anything unlinked hangs off
// the project root. Breadth-first from the roots so the first (shallowest)
// claim on a memory wins, which keeps the tree shallow and stops a link cycle
// from building an infinitely deep branch.
function parentsWithinProject(group) {
  const byName = new Map(group.map((m) => [m.name, m]));
  const incoming = new Map(group.map((m) => [m.name, 0]));
  for (const m of group) {
    for (const link of m.links) {
      if (byName.has(link) && link !== m.name) incoming.set(link, incoming.get(link) + 1);
    }
  }

  // Newest first among the entry points, so the tree's leftmost branch is the
  // thing most recently worked on.
  let queue = group.filter((m) => incoming.get(m.name) === 0).sort((x, y) => y.updated_at - x.updated_at);
  // Every memory in this project links to another one (a cycle): nothing has an
  // in-degree of zero, so the most recently touched memory is made the entry
  // point rather than leaving the whole group unreachable.
  if (queue.length === 0 && group.length > 0) queue = [group[0]];

  const parent = new Map(queue.map((m) => [m.name, null])); // null = child of the project root
  const order = [...queue];
  for (let i = 0; i < order.length; i++) {
    const m = order[i];
    for (const link of m.links) {
      if (!byName.has(link) || parent.has(link)) continue;
      parent.set(link, m.name);
      order.push(byName.get(link));
    }
  }
  // Unreachable leftovers (only linked to from inside a cycle) still belong to
  // the project — they hang off its root rather than disappearing.
  for (const m of group) if (!parent.has(m.name)) parent.set(m.name, null);
  return parent;
}

function buildGraph() {
  const visible = memories.filter((m) => memoryMatchesFilters(m));
  const visibleNames = new Set(visible.map((m) => m.name));
  // Where each node currently sits on screen, so a rebuild slides from there
  // instead of restarting the entrance from nothing.
  const prevByName = new Map(graphNodes.map((n) => [n.name, n]));

  const byProject = new Map();
  for (const m of visible) {
    if (!byProject.has(m.project)) byProject.set(m.project, []);
    byProject.get(m.project).push(m);
  }
  // Biggest tree first, then alphabetical: a stable left-to-right order, so the
  // same data always draws the same picture.
  const projects = [...byProject.keys()].sort((x, y) => {
    const size = byProject.get(y).length - byProject.get(x).length;
    return size !== 0 ? size : x.localeCompare(y);
  });

  const nodes = [];
  const nodeIndex = new Map();
  const edges = [];
  let slotCursor = 0; // running leaf slot, shared across every project's tree

  function addNode(node) {
    nodeIndex.set(node.name, nodes.length);
    nodes.push(node);
    return nodes.length - 1;
  }

  for (const project of projects) {
    const group = byProject.get(project);
    const parentOf = parentsWithinProject(group);
    const childrenOf = new Map();
    for (const m of group) {
      const key = parentOf.get(m.name) ?? "\u0000root";
      if (!childrenOf.has(key)) childrenOf.set(key, []);
      childrenOf.get(key).push(m);
    }
    // Siblings newest first, matching the entry-point order above.
    for (const list of childrenOf.values()) list.sort((x, y) => y.updated_at - x.updated_at);

    const rootName = "project:" + project;
    const rootIndex = addNode({
      name: rootName,
      kind: "root",
      real: false,
      data: null,
      project,
      depth: 0,
      x: 0,
      y: 0,
      r: nodeRadius("root", 0),
      label: prettifyProjectSlug(project),
    });
    const treeStartSlot = slotCursor;

    // Depth-first walk: leaves take the next slot, a parent centres itself over
    // the children it just placed. The classic tidy-tree assignment, and the
    // reason nothing overlaps without any repulsion to push it apart.
    function place(memory, depth, parentIndex) {
      const index = addNode({
        name: memory.name,
        kind: "memory",
        real: true,
        data: memory,
        project,
        depth,
        x: 0,
        y: depth * LEVEL_GAP_Y,
        r: nodeRadius("memory", depth),
      });
      edges.push({ a: parentIndex, b: index, kind: "tree" });

      const kids = childrenOf.get(memory.name) || [];
      const ghosts = memory.links.filter((link) => !visibleNames.has(link));
      const firstSlot = slotCursor;

      for (const kid of kids) place(kid, depth + 1, index);
      for (const ghost of ghosts) {
        // A link to something not written yet: a leaf stub on the branch that
        // asked for it. Names are suffixed with their parent so two branches
        // waiting on the same missing memory each get their own stub rather
        // than one being silently dropped by the name index.
        const ghostIndex = addNode({
          name: ghost + "\u0000" + memory.name,
          kind: "ghost",
          real: false,
          data: null,
          project,
          depth: depth + 1,
          label: ghost,
          x: slotCursor * NODE_GAP_X,
          y: (depth + 1) * LEVEL_GAP_Y,
          r: nodeRadius("ghost", depth + 1),
        });
        edges.push({ a: index, b: ghostIndex, kind: "tree" });
        slotCursor += 1;
      }

      const node = nodes[index];
      if (slotCursor === firstSlot) {
        node.x = slotCursor * NODE_GAP_X; // a leaf: take a slot of its own
        slotCursor += 1;
      } else {
        node.x = ((firstSlot + slotCursor - 1) / 2) * NODE_GAP_X; // centre over the subtree
      }
    }

    for (const top of childrenOf.get("\u0000root") || []) place(top, 1, rootIndex);

    const root = nodes[rootIndex];
    if (slotCursor === treeStartSlot) {
      root.x = slotCursor * NODE_GAP_X; // a project with no memories left after filtering
      slotCursor += 1;
    } else {
      root.x = ((treeStartSlot + slotCursor - 1) / 2) * NODE_GAP_X;
    }
    root.y = -LEVEL_GAP_Y * 0.55; // lifted clear of its first row of branches
    slotCursor += PROJECT_GAP;
  }

  // Links that the tree itself doesn't already draw: a memory pointing at one
  // in another project, or a second link into a branch that already has a
  // parent. Drawn as faint arcs so a tree stays readable as a tree.
  for (const m of visible) {
    const a = nodeIndex.get(m.name);
    if (a === undefined) continue;
    for (const link of m.links) {
      const b = nodeIndex.get(link);
      if (b === undefined || a === b) continue;
      const alreadyDrawn = edges.some(
        (e) => e.kind === "tree" && ((e.a === a && e.b === b) || (e.a === b && e.b === a))
      );
      if (!alreadyDrawn) edges.push({ a, b, kind: "cross" });
    }
  }

  // Centre the whole forest on the origin, so "reset" and the initial view have
  // something predictable to aim at.
  if (nodes.length > 0) {
    const midX = (Math.min(...nodes.map((n) => n.x)) + Math.max(...nodes.map((n) => n.x))) / 2;
    for (const node of nodes) node.x -= midX;
  }

  // Where each node enters from: its own previous position if it was already on
  // screen, otherwise its parent's spot — which is what makes a new branch look
  // like it grew out of the one it hangs off.
  const parentIndexOf = new Map();
  for (const edge of edges) if (edge.kind === "tree") parentIndexOf.set(edge.b, edge.a);
  nodes.forEach((node, i) => {
    const previous = prevByName.get(node.name);
    if (previous) {
      node.fromX = previous.x;
      node.fromY = previous.y;
      return;
    }
    const parent = nodes[parentIndexOf.get(i)];
    node.fromX = parent ? parent.x : node.x;
    node.fromY = parent ? parent.y : node.y;
  });

  graphNodes = nodes;
  graphEdges = edges;
  animStart = performance.now();
  // The layout is already final, so the view can be fitted to it immediately
  // rather than waiting for anything to settle — unless the user has taken the
  // view over (see clearPendingFit).
  pendingFit = true;
  startRenderLoop();
  requestDraw();

  updateMemoryEmptyState();
}

// 0..1 for the whole entrance, and per node with a small stagger by depth so a
// tree unfolds outward from its root instead of appearing all at once.
function animProgress() {
  return Math.min(1, (performance.now() - animStart) / ANIM_MS);
}

function nodeProgress(node, overall) {
  const delay = Math.min(0.5, node.depth * 0.07);
  const t = (overall - delay) / (1 - delay);
  if (t <= 0) return 0;
  if (t >= 1) return 1;
  return 1 - Math.pow(1 - t, 3); // ease-out cubic
}

// Where a node is drawn right now: on its way in, between where it entered from
// and the slot the layout gave it. Once the entrance is done this is just the
// node's own position, every frame, forever.
function nodePos(node, overall) {
  const t = nodeProgress(node, overall);
  if (t >= 1) return { x: node.x, y: node.y, t };
  return { x: node.fromX + (node.x - node.fromX) * t, y: node.fromY + (node.y - node.fromY) * t, t };
}

/* ---------------- view transform (pan/zoom) ---------------- */
const memoryView = { cx: 0, cy: 0, zoom: 1 };
const ZOOM_MIN = 0.25;
const ZOOM_MAX = 3;
let hoveredNode = null;
let pressedNode = null; // node the mouse went down on, for click-vs-pan
let dragMoved = false;
let panState = null;
let pendingFit = false;

// The canvas is only repainted when something has actually changed. It used to
// redraw every frame for as long as Memory mode was open, which kept a whole
// core busy drawing an identical picture of a settled graph.
let needsDraw = true;

function requestDraw() {
  needsDraw = true;
}

// Any deliberate view change cancels the automatic fit — once you have chosen
// where to look, having the graph re-frame itself under you is a bug.
function clearPendingFit() {
  pendingFit = false;
}

export function resizeMemoryCanvas() {
  const rect = memoryGraphWrap.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  memoryCanvas.width = rect.width * dpr;
  memoryCanvas.height = rect.height * dpr;
  memoryCanvas.style.width = rect.width + "px";
  memoryCanvas.style.height = rect.height + "px";
  memoryCtx.setTransform(dpr, 0, 0, dpr, 0, 0);
  requestDraw();
}

// Frames every node, rather than returning to the origin at 1× — the layout
// drifts as it settles, so "reset" used to mean "look at wherever the middle
// happens to be", which on a spread-out graph is empty space.
function fitView(padding = 0.86) {
  if (graphNodes.length === 0) {
    memoryView.cx = 0;
    memoryView.cy = 0;
    memoryView.zoom = 1;
    requestDraw();
    return;
  }
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const node of graphNodes) {
    // The node's own radius and its label sit outside its centre point, so the
    // box is grown by them or the outermost nodes end up half off-screen.
    minX = Math.min(minX, node.x - node.r - 30);
    minY = Math.min(minY, node.y - node.r - 10);
    maxX = Math.max(maxX, node.x + node.r + 30);
    maxY = Math.max(maxY, node.y + node.r + 26);
  }
  memoryView.cx = (minX + maxX) / 2;
  memoryView.cy = (minY + maxY) / 2;

  const rect = memoryGraphWrap.getBoundingClientRect();
  const spanX = Math.max(maxX - minX, 1);
  const spanY = Math.max(maxY - minY, 1);
  const fit = Math.min(rect.width / spanX, rect.height / spanY) * padding;
  // A single memory would otherwise be fitted to a zoom of about 8× and fill
  // the screen with one bubble.
  memoryView.zoom = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, Math.min(fit, 1.4)));
  requestDraw();
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
  const overall = animProgress();
  for (let i = graphNodes.length - 1; i >= 0; i--) {
    const node = graphNodes[i];
    const pos = nodePos(node, overall);
    const p = worldToScreen(pos.x, pos.y);
    const dx = screenX - p.x;
    const dy = screenY - p.y;
    // The drawn radius, not the world one: nodes are scaled by the zoom, so a
    // hit box in world units missed every node while zoomed in and swallowed
    // half the canvas while zoomed out.
    const hit = node.r * memoryView.zoom + 5;
    if (dx * dx + dy * dy <= hit * hit) return node;
  }
  return null;
}

// Each project tree gets its own hue, so two trees side by side read as two
// things rather than as one graph that happens to have a gap in it. Derived
// from the project name so it is stable across restarts and never needs a
// colour table to be kept in sync with anything.
function projectHue(project) {
  let hash = 0;
  for (let i = 0; i < project.length; i++) hash = (hash * 31 + project.charCodeAt(i)) >>> 0;
  return hash % 360;
}

// The elbow connector that makes this read as a tree: straight down out of the
// parent, across at the midpoint, straight down into the child. Corners are
// rounded by hand — a diagonal line between the two would be a graph edge
// again, which is exactly what the layout is trying to stop looking like.
function branchPath(ctx, pa, pb, ra, rb) {
  const top = pa.y + ra;
  const bottom = pb.y - rb;
  const mid = top + (bottom - top) * 0.5;
  const corner = Math.min(14, Math.abs(pb.x - pa.x) / 2, Math.abs(mid - top), Math.abs(bottom - mid));
  const dir = pb.x > pa.x ? 1 : -1;

  ctx.beginPath();
  ctx.moveTo(pa.x, top);
  if (corner < 1.5) {
    // Directly underneath: one straight line, no corners to round.
    ctx.lineTo(pb.x, bottom);
    return;
  }
  ctx.lineTo(pa.x, mid - corner);
  ctx.quadraticCurveTo(pa.x, mid, pa.x + corner * dir, mid);
  ctx.lineTo(pb.x - corner * dir, mid);
  ctx.quadraticCurveTo(pb.x, mid, pb.x, mid + corner);
  ctx.lineTo(pb.x, bottom);
}

function draw() {
  const rect = memoryGraphWrap.getBoundingClientRect();
  memoryCtx.clearRect(0, 0, rect.width, rect.height);

  const overall = animProgress();
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

  // Screen position + entrance progress, once per node per frame.
  const drawn = graphNodes.map((node) => {
    const pos = nodePos(node, overall);
    const p = worldToScreen(pos.x, pos.y);
    return { node, p, t: pos.t };
  });

  for (const edge of graphEdges) {
    const a = drawn[edge.a];
    const b = drawn[edge.b];
    // A branch arrives with the node it feeds, so nothing is ever drawn
    // pointing at a node that isn't there yet.
    const t = Math.min(a.t, b.t);
    if (t <= 0) continue;

    const focused = hoveredNode || memorySelected;
    const highlighted = focused && activeNames.has(a.node.name) && activeNames.has(b.node.name);
    const dimmed = focused && !highlighted;

    memoryCtx.globalAlpha = t;
    if (edge.kind === "cross") {
      // Not part of any tree — a link across to another branch or another
      // project. Dashed and faint, so it reads as a reference rather than as
      // structure.
      memoryCtx.setLineDash([3, 4]);
      memoryCtx.lineWidth = highlighted ? 1.4 : 1;
      memoryCtx.strokeStyle = dimmed
        ? "rgba(120,120,130,0.06)"
        : highlighted
          ? "rgba(200,200,210,0.42)"
          : "rgba(150,150,160,0.16)";
      const bend = (b.p.y - a.p.y) * 0.25;
      memoryCtx.beginPath();
      memoryCtx.moveTo(a.p.x, a.p.y);
      memoryCtx.quadraticCurveTo((a.p.x + b.p.x) / 2, (a.p.y + b.p.y) / 2 + bend, b.p.x, b.p.y);
      memoryCtx.stroke();
      memoryCtx.setLineDash([]);
    } else {
      memoryCtx.lineWidth = highlighted ? 2 : 1.4;
      memoryCtx.strokeStyle = dimmed
        ? "rgba(120,120,130,0.08)"
        : highlighted
          ? "rgba(210,210,220,0.6)"
          : "rgba(150,150,160,0.3)";
      branchPath(memoryCtx, a.p, b.p, a.node.r * memoryView.zoom, b.node.r * memoryView.zoom);
      memoryCtx.stroke();
    }
    memoryCtx.globalAlpha = 1;
  }

  // Labels are collected and drawn in a second pass, after every node
  // circle — otherwise a node drawn later would paint its solid circle
  // right on top of an earlier node's label text.
  const labels = [];

  for (const { node, p, t } of drawn) {
    if (t <= 0) continue;
    // A project head is context rather than a result, so a search never dims it
    // — the trees have to stay legible while you look through them.
    const matches =
      !searching || node.kind === "root" || (node.real && memoryMatchesSearch(node.data));
    const dimmedByFocus = (hoveredNode || memorySelected) && !activeNames.has(node.name);
    let opacity = 1;
    if (!matches) opacity = 0.12;
    else if (dimmedByFocus) opacity = 0.3;
    else if (node.kind === "ghost") opacity = 0.55;

    const color =
      node.kind === "root"
        ? "hsl(" + projectHue(node.project) + " 62% 62%)"
        : node.real
          ? memoryTypeColor(node.data.type)
          : MEMORY_TYPE_COLORS.ghost;

    // Radius grows with the entrance, so a new branch scales up out of its
    // parent rather than popping in at full size.
    const r = node.r * memoryView.zoom * (0.55 + 0.45 * t);

    memoryCtx.globalAlpha = opacity * t;

    if (node.name === memorySelected || node === hoveredNode) {
      memoryCtx.shadowColor = color;
      memoryCtx.shadowBlur = 18;
    }
    const glowR = r * 2.2;
    const glow = memoryCtx.createRadialGradient(p.x, p.y, 0, p.x, p.y, glowR);
    glow.addColorStop(0, hexOrHslWithAlpha(color, 0.2));
    glow.addColorStop(1, hexOrHslWithAlpha(color, 0));
    memoryCtx.fillStyle = glow;
    memoryCtx.beginPath();
    memoryCtx.arc(p.x, p.y, glowR, 0, Math.PI * 2);
    memoryCtx.fill();
    memoryCtx.shadowBlur = 0;

    if (node.kind === "root") {
      // The head of a tree, drawn as a hexagon: it is not one of the memories,
      // and a bigger circle would only say "a very well-connected memory".
      memoryCtx.beginPath();
      for (let i = 0; i < 6; i++) {
        const angle = (Math.PI / 3) * i - Math.PI / 2;
        const px = p.x + Math.cos(angle) * r;
        const py = p.y + Math.sin(angle) * r;
        if (i === 0) memoryCtx.moveTo(px, py);
        else memoryCtx.lineTo(px, py);
      }
      memoryCtx.closePath();
      memoryCtx.fillStyle = "rgba(14,14,16,0.92)";
      memoryCtx.fill();
      memoryCtx.lineWidth = 2;
      memoryCtx.strokeStyle = color;
      memoryCtx.stroke();
    } else if (node.kind === "ghost") {
      memoryCtx.beginPath();
      memoryCtx.arc(p.x, p.y, r, 0, Math.PI * 2);
      memoryCtx.setLineDash([3, 3]);
      memoryCtx.strokeStyle = MEMORY_TYPE_COLORS.ghost;
      memoryCtx.lineWidth = 1.5;
      memoryCtx.stroke();
      memoryCtx.setLineDash([]);
    } else {
      const body = memoryCtx.createRadialGradient(p.x - r * 0.35, p.y - r * 0.35, r * 0.1, p.x, p.y, r);
      body.addColorStop(0, lightenColor(color, 0.35));
      body.addColorStop(1, color);
      memoryCtx.beginPath();
      memoryCtx.arc(p.x, p.y, r, 0, Math.PI * 2);
      memoryCtx.fillStyle = body;
      memoryCtx.fill();
      memoryCtx.lineWidth = 1;
      memoryCtx.strokeStyle = "rgba(0,0,0,0.35)";
      memoryCtx.stroke();
    }

    if (node.name === memorySelected) {
      memoryCtx.beginPath();
      memoryCtx.arc(p.x, p.y, r + 3, 0, Math.PI * 2);
      memoryCtx.lineWidth = 2;
      memoryCtx.strokeStyle = "#fff";
      memoryCtx.stroke();
    }
    memoryCtx.globalAlpha = 1;

    // A project's name is the one label that is always worth the space — it is
    // the title of the tree under it.
    const isRoot = node.kind === "root";
    if (isRoot || memoryView.zoom > 0.55 || node.name === memorySelected || node === hoveredNode) {
      labels.push({
        node,
        p,
        r,
        isRoot,
        text: node.label || node.name,
        opacity: (matches ? (dimmedByFocus ? 0.45 : 1) : 0.15) * t,
      });
    }
  }

  // Roots first and biggest-first after that, so when labels compete for the
  // same space the survivor is the more important node rather than whichever
  // happened to be later in the array.
  labels.sort((a, b) => (b.isRoot ? 1 : 0) - (a.isRoot ? 1 : 0) || b.node.r - a.node.r);

  memoryCtx.textAlign = "center";
  memoryCtx.textBaseline = "middle";
  // Placed labels, so a label that would land on top of one already drawn is
  // dropped instead. Zoomed out over a few dozen memories every label used to
  // be drawn, and a pile of overlapping pills is less readable than fewer
  // labels — the hovered, selected and project ones are always kept, which is
  // how you read the rest.
  const placed = [];
  for (const { node, p, r, isRoot, text, opacity } of labels) {
    memoryCtx.font = isRoot
      ? "700 12.5px -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif"
      : "600 11px -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
    // A project's name sits above its hexagon (it heads the tree); everything
    // else hangs under its own node.
    const labelY = isRoot ? p.y - r - 14 : p.y + r + 14;
    const textWidth = memoryCtx.measureText(text).width;
    const padX = isRoot ? 9 : 6;
    const boxW = textWidth + padX * 2;
    const boxH = isRoot ? 20 : 16;

    const box = { x: p.x - boxW / 2, y: labelY - boxH / 2, w: boxW, h: boxH };
    const forced = isRoot || node.name === memorySelected || node === hoveredNode;
    if (!forced && placed.some((o) => box.x < o.x + o.w && box.x + box.w > o.x && box.y < o.y + o.h && box.y + box.h > o.y)) {
      continue;
    }
    placed.push(box);

    memoryCtx.globalAlpha = opacity;
    memoryCtx.fillStyle = isRoot ? "rgba(10,10,12,0.88)" : "rgba(10,10,12,0.72)";
    roundRect(memoryCtx, box.x, box.y, boxW, boxH, isRoot ? 7 : 8);
    memoryCtx.fill();
    if (isRoot) {
      memoryCtx.lineWidth = 1;
      memoryCtx.strokeStyle = "hsl(" + projectHue(node.project) + " 62% 62% / 0.5)";
      memoryCtx.stroke();
    }

    memoryCtx.fillStyle = isRoot
      ? "hsl(" + projectHue(node.project) + " 62% 78%)"
      : node.name === memorySelected
        ? "#fff"
        : "#c8c8ce";
    memoryCtx.fillText(text, p.x, labelY + 0.5);
    memoryCtx.globalAlpha = 1;
  }

  // Keep painting while the entrance is still running.
  if (overall < 1) requestDraw();
}

// The node colours are hex from MEMORY_TYPE_COLORS but a project root's is an
// hsl() string, and the glow gradient needs both with an alpha applied.
function hexOrHslWithAlpha(color, alpha) {
  if (color.startsWith("#")) {
    return (
      color +
      Math.round(alpha * 255)
        .toString(16)
        .padStart(2, "0")
    );
  }
  return color.replace(")", " / " + alpha + ")");
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

// A frame pump, not a simulation: it only exists so the entrance animation and
// pan/zoom have somewhere to draw from. It idles at nothing once needsDraw is
// clear, and stops entirely when the user leaves Memory mode.
let renderLoopRunning = false;

function startRenderLoop() {
  if (renderLoopRunning) return;
  renderLoopRunning = true;
  function tick() {
    if (getMode() !== "memory") {
      renderLoopRunning = false;
      return;
    }
    if (pendingFit) {
      // The layout is computed up front, so there is nothing to wait for.
      pendingFit = false;
      fitView();
    }
    if (needsDraw) {
      needsDraw = false;
      draw();
    }
    requestAnimationFrame(tick);
  }
  requestAnimationFrame(tick);
}

/* ---------------- interactions ---------------- */
memoryCanvas.addEventListener("mousedown", (e) => {
  const rect = memoryCanvas.getBoundingClientRect();
  const x = e.clientX - rect.left;
  const y = e.clientY - rect.top;
  // Dragging is always a pan now. A node's position is the layout's statement
  // about where it belongs in its tree, so letting it be dragged somewhere else
  // would only produce a tree that lies about itself.
  pressedNode = nodeAt(x, y);
  dragMoved = false;
  panState = { startX: e.clientX, startY: e.clientY, cx: memoryView.cx, cy: memoryView.cy };
  clearPendingFit();
  // Clicking the graph is a way of dismissing the sources panel sitting over it,
  // which otherwise needs its own close button hunted down.
  memorySourcesPanel.classList.remove("visible");
});

// Double-clicking the background re-frames everything — the same as the toolbar
// button, where the mouse already is.
memoryCanvas.addEventListener("dblclick", (e) => {
  const rect = memoryCanvas.getBoundingClientRect();
  if (!nodeAt(e.clientX - rect.left, e.clientY - rect.top)) fitView();
});

window.addEventListener("mousemove", (e) => {
  if (getMode() !== "memory") return;
  const rect = memoryCanvas.getBoundingClientRect();
  const x = e.clientX - rect.left;
  const y = e.clientY - rect.top;

  if (panState) {
    if (Math.abs(e.clientX - panState.startX) > 3 || Math.abs(e.clientY - panState.startY) > 3) dragMoved = true;
    memoryView.cx = panState.cx - (e.clientX - panState.startX) / memoryView.zoom;
    memoryView.cy = panState.cy - (e.clientY - panState.startY) / memoryView.zoom;
    el("memoryTooltip").classList.remove("visible");
    memoryCanvas.style.cursor = "grabbing";
    requestDraw();
    return;
  }
  const previous = hoveredNode;
  hoveredNode = nodeAt(x, y);
  memoryCanvas.style.cursor = hoveredNode ? "pointer" : "grab";
  // Only the change matters: hovering dims everything unconnected, so a repaint
  // is needed when the hover moves on or off a node, not on every mouse move.
  if (previous !== hoveredNode) requestDraw();
  updateMemoryTooltip(x, y);
});

function updateMemoryTooltip(x, y) {
  const tooltip = el("memoryTooltip");
  if (!hoveredNode) {
    tooltip.classList.remove("visible");
    return;
  }
  if (hoveredNode.kind === "root") {
    const count = memories.filter((m) => m.project === hoveredNode.project).length;
    tooltip.innerHTML =
      '<div class="tt-name">' +
      escapeHtml(hoveredNode.label) +
      "</div>" +
      '<div class="tt-desc">' +
      count +
      (count === 1 ? " memory" : " memories") +
      " in this project</div>" +
      '<div class="tt-meta">' +
      (memoryProjectFilter === hoveredNode.project ? "Click to show every project" : "Click to show only this tree") +
      "</div>";
  } else if (hoveredNode.real) {
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
      '<div class="tt-name">' +
      escapeHtml(hoveredNode.label) +
      "</div>" +
      '<div class="tt-meta">Not written yet</div>';
  }
  // Measured rather than assumed: the old fixed 250x90 guess flipped a
  // one-line tooltip early and still let a three-line one hang off the bottom.
  // Reading offsetWidth here is a deliberate layout flush — the content was
  // just written, and it is one read per hover, not per frame.
  const wrapRect = memoryGraphWrap.getBoundingClientRect();
  const tipW = tooltip.offsetWidth;
  const tipH = tooltip.offsetHeight;
  const GAP = 16;
  let left = x + GAP;
  let top = y + GAP;
  if (left + tipW > wrapRect.width - 4) left = x - GAP - tipW;
  if (top + tipH > wrapRect.height - 4) top = y - GAP - tipH;
  tooltip.style.left = Math.max(4, Math.min(left, wrapRect.width - tipW - 4)) + "px";
  tooltip.style.top = Math.max(4, Math.min(top, wrapRect.height - tipH - 4)) + "px";
  tooltip.classList.add("visible");
}

window.addEventListener("mouseup", () => {
  // A click is a press and release on the same node without panning in between;
  // anything else was a drag of the view and selects nothing.
  if (pressedNode && !dragMoved) {
    if (pressedNode.kind === "root") {
      // The head of a tree isn't a memory to read — it stands for the project,
      // so clicking it narrows everything to that project (and clicking it
      // again, now the only tree on screen, widens back out).
      memoryProjectFilter = memoryProjectFilter === pressedNode.project ? "all" : pressedNode.project;
      renderMemoryFilters();
      renderMemoryList();
      buildGraph();
      updateMemoryCounts();
    } else if (pressedNode.kind === "memory") {
      selectMemory(pressedNode.name, false);
    }
  }
  pressedNode = null;
  panState = null;
  if (getMode() === "memory") {
    memoryCanvas.style.cursor = hoveredNode ? "pointer" : "grab";
    requestDraw();
  }
});

memoryCanvas.addEventListener("mouseleave", () => {
  hoveredNode = null;
  el("memoryTooltip").classList.remove("visible");
  requestDraw();
});

memoryCanvas.addEventListener("wheel", (e) => {
  e.preventDefault();
  const rect = memoryCanvas.getBoundingClientRect();
  const before = screenToWorld(e.clientX - rect.left, e.clientY - rect.top);
  const factor = e.deltaY < 0 ? 1.1 : 0.9;
  memoryView.zoom = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, memoryView.zoom * factor));
  const after = screenToWorld(e.clientX - rect.left, e.clientY - rect.top);
  memoryView.cx += before.x - after.x;
  memoryView.cy += before.y - after.y;
  clearPendingFit();
  requestDraw();
}, { passive: false });

function zoomBy(factor) {
  memoryView.zoom = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, memoryView.zoom * factor));
  clearPendingFit();
  requestDraw();
}

el("memoryZoomInBtn").addEventListener("click", () => zoomBy(1.2));
el("memoryZoomOutBtn").addEventListener("click", () => zoomBy(1 / 1.2));
el("memoryResetViewBtn").addEventListener("click", () => fitView());

// The graph is a canvas, so nothing in it is reachable by tab or arrow key on
// its own — without this the whole mode is mouse-only. Scoped to the canvas
// having focus so it never competes with the sidebar search box or Ctrl+K.
memoryCanvas.addEventListener("keydown", (e) => {
  const PAN = e.shiftKey ? 120 : 40;
  const pan = (dx, dy) => {
    memoryView.cx += dx / memoryView.zoom;
    memoryView.cy += dy / memoryView.zoom;
    clearPendingFit();
    requestDraw();
  };
  switch (e.key) {
    case "ArrowLeft": pan(-PAN, 0); break;
    case "ArrowRight": pan(PAN, 0); break;
    case "ArrowUp": pan(0, -PAN); break;
    case "ArrowDown": pan(0, PAN); break;
    case "+":
    case "=": zoomBy(1.2); break;
    case "-":
    case "_": zoomBy(1 / 1.2); break;
    case "0": fitView(); break;
    default: return;
  }
  e.preventDefault();
});

window.addEventListener("resize", () => {
  if (getMode() === "memory") resizeMemoryCanvas();
});

// The picture depends on the search box (non-matching nodes are dimmed rather
// than removed), so a keystroke there has to reach the canvas too.
searchInput.addEventListener("input", () => {
  if (getMode() !== "memory") return;
  updateMemoryCounts();
  requestDraw();
});

/* ---------------- detail panel ---------------- */
function selectMemory(name, centerView) {
  memorySelected = name;
  renderMemoryList();
  requestDraw();
  const m = memories.find((x) => x.name === name);
  if (!m) {
    // A ghost node — nothing to read, so the panel closes rather than showing
    // the last memory's body under a name that isn't its own.
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

  // Reading a memory and managing the folders they come from are two different
  // jobs, and on a narrow window the two panels are over each other.
  memorySourcesPanel.classList.remove("visible");
  // Opening a second memory should start at its top, not at the scroll offset
  // the previous one was left at.
  memoryDetailEl.querySelector(".memory-detail-scroll")?.scrollTo(0, 0);

  if (centerView) {
    const node = graphNodes.find((n) => n.name === name);
    if (node) {
      memoryView.cx = node.x;
      memoryView.cy = node.y;
      clearPendingFit();
      requestDraw();
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

function closeMemoryDetail() {
  memorySelected = null;
  memoryDetailEl.classList.remove("visible");
  renderMemoryList();
  requestDraw();
}

el("closeMemoryDetailBtn").addEventListener("click", closeMemoryDetail);

// Escape closes whichever panel is over the graph, innermost first. Registered
// on the capture phase and stopping propagation only when it actually closes
// something, so it runs before notes.js's own document-level Escape (command
// palette / find bar / focus mode) instead of racing it on import order — and
// leaves that handler untouched when there is nothing here to close.
document.addEventListener(
  "keydown",
  (e) => {
    if (e.key !== "Escape" || getMode() !== "memory") return;
    if (memorySourcesPanel.classList.contains("visible")) {
      memorySourcesPanel.classList.remove("visible");
      e.stopPropagation();
      return;
    }
    if (memoryDetailEl.classList.contains("visible")) {
      closeMemoryDetail();
      e.stopPropagation();
    }
  },
  true
);

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
