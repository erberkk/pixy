// Pulls the compiled defaults straight out of tunables.rs so the harness never
// carries its own copy — the same rule shared/tunables.js enforces at runtime.
import { readFile } from "node:fs/promises";

export async function tunableDefaults(path = "src-tauri/src/tunables.rs") {
  const src = await readFile(path, "utf8");
  const values = {};
  // id, then the first Kind::… block that follows it
  const entry = /"([a-z_]+\.[a-z_]+)"\s*,\s*"[^"]*"\s*,[\s\S]*?Kind::(\w+)\s*\{([^}]*)\}/g;
  let m;
  while ((m = entry.exec(src))) {
    const [, id, kind, body] = m;
    const def = /default:\s*("(?:[^"\\]|\\.)*"|[^,}]+)/.exec(body);
    if (!def) continue;
    const raw = def[1].trim();
    let value;
    if (raw.startsWith('"')) value = JSON.parse(raw);
    else if (raw === "true" || raw === "false") value = raw === "true";
    // Rust writes large literals as 60_000; JS's Number() gives NaN for those.
    else value = Number(raw.replace(/_/g, ""));
    if (typeof value === "number" && Number.isNaN(value)) continue;
    values[id] = value;
  }
  return values;
}

const { pathToFileURL } = await import("node:url");
if (process.argv[1] && pathToFileURL(process.argv[1]).href === import.meta.url) {
  const v = await tunableDefaults();
  console.log(`${Object.keys(v).length} tunables extracted`);
  console.log(JSON.stringify(v, null, 2));
}
