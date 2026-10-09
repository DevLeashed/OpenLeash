#!/usr/bin/env node
// Run from the repository root. No dependency, Cargo, git, or network commands:
// npm's `version` lifecycle has already bumped its manifests before --sync runs.
import { readFileSync, writeFileSync } from "node:fs";
import process from "node:process";

const numeric = "(?:0|[1-9][0-9]*)";
const prerelease = "(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)";
const semver = new RegExp(`^${numeric}\\.${numeric}\\.${numeric}(?:-${prerelease}(?:\\.${prerelease})*)?(?:\\+[0-9A-Za-z-]+(?:\\.[0-9A-Za-z-]+)*)?$`);
const usage = "Usage: node scripts/version.mjs <X.Y.Z> | --sync | --check [--tag vX.Y.Z]";

function validVersion(value, label) {
  // Comparing the whole match also rejects a trailing newline (JS's $ permits it).
  if (typeof value !== "string" || semver.exec(value)?.[0] !== value) {
    throw new Error(`${label}: expected a valid SemVer version, got ${JSON.stringify(value)}`);
  }
  return value;
}

function argumentsFor(args) {
  if (args[0] === "--check" && (args.length === 1 || (args.length === 3 && args[1] === "--tag"))) {
    const tag = args[2];
    if (tag !== undefined) {
      if (!tag.startsWith("v")) throw new Error("Release tag must start with v");
      validVersion(tag.slice(1), "Release tag");
    }
    return { mode: "check", tag };
  }
  if (args.length !== 1) throw new Error(usage);
  if (args[0] === "--sync") return { mode: "sync" };
  return { mode: "set", version: validVersion(args[0], "Requested version") };
}

// JSON.parse validates syntax; this small walk locates exact value spans without
// reserializing the document, changing indentation, or touching dependency versions.
function jsonDocument(file) {
  const text = readFileSync(file, "utf8");
  const data = JSON.parse(text);
  const strings = new Map();
  let cursor = 0;
  const whitespace = () => { while (/\s/.test(text[cursor] ?? "") && cursor < text.length) cursor++; };
  function stringToken() {
    const start = cursor++;
    while (text[cursor] !== '"') {
      if (text[cursor] === "\\") cursor++;
      cursor++;
    }
    cursor++;
    return { start, end: cursor, value: JSON.parse(text.slice(start, cursor)) };
  }
  function walk(path) {
    whitespace();
    if (text[cursor] === '"') {
      strings.set(JSON.stringify(path), stringToken());
    } else if (text[cursor] === "{") {
      cursor++;
      whitespace();
      const keys = new Set();
      while (text[cursor] !== "}") {
        const key = stringToken().value;
        if (keys.has(key)) throw new Error(`${file}: duplicate JSON key ${JSON.stringify(key)}`);
        keys.add(key);
        whitespace();
        cursor++; // colon (syntax already validated)
        walk([...path, key]);
        whitespace();
        if (text[cursor] !== ",") break;
        cursor++;
        whitespace();
      }
      cursor++;
    } else if (text[cursor] === "[") {
      cursor++;
      whitespace();
      let index = 0;
      while (text[cursor] !== "]") {
        walk([...path, index++]);
        whitespace();
        if (text[cursor] !== ",") break;
        cursor++;
      }
      cursor++;
    } else {
      while (cursor < text.length && !/[\s,}\]]/.test(text[cursor])) cursor++;
    }
  }
  walk([]);
  return { file, text, data, strings, edits: [] };
}

function jsonVersion(document, path) {
  const token = document.strings.get(JSON.stringify(path));
  if (!token) throw new Error(`${document.file}: missing string ${path.join(".")}`);
  validVersion(token.value, `${document.file} ${path.join(".")}`);
  return { document, ...token, quote: '"' };
}

function tomlDocument(file) {
  const text = readFileSync(file, "utf8");
  // Scope edits to exact sections rather than replacing every version assignment.
  const headings = [...text.matchAll(/^[ \t]*(\[\[?[^\]\r\n]+\]\]?)[ \t]*(?:#[^\r\n]*)?\r?$/gm)];
  const sections = headings.map((heading, index) => ({
    header: heading[1],
    start: heading.index + heading[0].length,
    end: headings[index + 1]?.index ?? text.length,
  }));
  return { file, text, sections, edits: [] };
}

function tomlString(document, section, key) {
  const block = document.text.slice(section.start, section.end);
  const assignments = [...block.matchAll(new RegExp(`^[ \\t]*${key}[ \\t]*=.*$`, "gm"))];
  if (assignments.length !== 1) throw new Error(`${document.file}: expected one ${key} in ${section.header}`);
  const assignment = assignments[0];
  const match = new RegExp(`^([ \\t]*${key}[ \\t]*=[ \\t]*)("[^"\\\\\\r\\n]*"|'[^'\\r\\n]*')[ \\t]*(?:#[^\\r\\n]*)?\\r?$`).exec(assignment[0]);
  if (!match) throw new Error(`${document.file}: malformed ${key} assignment`);
  const start = section.start + assignment.index + match[1].length;
  return { document, start, end: start + match[2].length, value: match[2].slice(1, -1), quote: match[2][0] };
}

function cargoVersion(document, header) {
  const sections = document.sections.filter(section => section.header === header);
  const own = sections.filter(section => tomlString(document, section, "name").value === "openleash");
  if (own.length !== 1) throw new Error(`${document.file}: expected one openleash ${header} section`);
  const version = tomlString(document, own[0], "version");
  validVersion(version.value, document.file);
  return version;
}

function run() {
  const options = argumentsFor(process.argv.slice(2));
  // Read and validate ALL inputs before preparing or applying any writes. A bad
  // lockfile or Tauri pointer must never leave just the first manifests updated.
  const pkg = jsonDocument("package.json");
  const lock = jsonDocument("package-lock.json");
  const tauri = jsonDocument("src-tauri/tauri.conf.json");
  const cargo = tomlDocument("src-tauri/Cargo.toml");
  const cargoLock = tomlDocument("src-tauri/Cargo.lock");
  if (tauri.data.version !== "../package.json") {
    throw new Error('src-tauri/tauri.conf.json version must point to "../package.json"');
  }
  const versions = [
    jsonVersion(pkg, ["version"]),
    jsonVersion(lock, ["version"]),
    jsonVersion(lock, ["packages", "", "version"]),
    cargoVersion(cargo, "[package]"),
    cargoVersion(cargoLock, "[[package]]"),
  ];
  const target = options.mode === "set" ? options.version : versions[0].value;
  if (options.mode === "check") {
    const drift = versions.filter(version => version.value !== target);
    if (drift.length) throw new Error(`Version drift: expected ${target}; ${drift.map(version => `${version.document.file} has ${version.value}`).join("; ")}`);
    if (options.tag !== undefined && options.tag !== `v${target}`) {
      throw new Error(`Release tag ${options.tag} does not match package.json version v${target}`);
    }
    process.stdout.write(`Version metadata aligned: ${target}\n`);
    return;
  }
  for (const version of versions) {
    if (version.value !== target) version.document.edits.push({ ...version, replacement: `${version.quote}${target}${version.quote}` });
  }
  const changes = [pkg, lock, cargo, cargoLock].filter(document => document.edits.length).map(document => {
    let text = document.text;
    for (const edit of document.edits.sort((a, b) => b.start - a.start)) {
      text = text.slice(0, edit.start) + edit.replacement + text.slice(edit.end);
    }
    return { file: document.file, text };
  });
  for (const change of changes) writeFileSync(change.file, change.text, "utf8");
  process.stdout.write(`Version metadata synchronized: ${target} (${changes.length} files changed)\n`);
}

try {
  run();
} catch (error) {
  process.stderr.write(`version: ${error instanceof Error ? error.message : String(error)}\n`);
  process.exitCode = 1;
}
