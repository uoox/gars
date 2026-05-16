#!/usr/bin/env node
// Copies manifest, html, and icons into dist/, then zips dist into dist.zip
// using the standard `zip` cli (any platform with zip available).

import { copyFileSync, existsSync, mkdirSync, readdirSync, statSync } from "node:fs";
import { join, dirname } from "node:path";
import { execSync } from "node:child_process";

const root = dirname(new URL(import.meta.url).pathname);
const dist = join(root, "dist");
mkdirSync(dist, { recursive: true });

function copy(src, dest) {
  if (statSync(src).isDirectory()) {
    mkdirSync(dest, { recursive: true });
    for (const name of readdirSync(src)) {
      copy(join(src, name), join(dest, name));
    }
  } else {
    mkdirSync(dirname(dest), { recursive: true });
    copyFileSync(src, dest);
  }
}

copy(join(root, "manifest.json"), join(dist, "manifest.json"));
copy(join(root, "src", "popup.html"), join(dist, "popup.html"));
if (existsSync(join(root, "icons"))) {
  copy(join(root, "icons"), join(dist, "icons"));
}

// Zip
const zipPath = join(root, "dist.zip");
try {
  // Remove old zip if any
  try {
    execSync(`rm -f "${zipPath}"`);
  } catch {
    /* ignore */
  }
  execSync(`cd "${dist}" && zip -r "${zipPath}" .`, { stdio: "inherit" });
  console.log(`packaged ${zipPath}`);
} catch (e) {
  console.error("zip failed:", e);
  process.exit(1);
}
