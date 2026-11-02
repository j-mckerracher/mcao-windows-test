// Build-time guard for the worker image's pi tree: every installed copy of undici must be the fixed version.
// Usage: node check-undici.js <install-root> <fixed-version>
//
// A copy is any installed package whose package.json name is "undici", wherever npm placed it: at the top
// level, nested under another package or under undici itself, under an npm alias directory, or reached
// through a symlink. Identity comes from the manifest name, never the directory name.
const fs = require("node:fs");
const path = require("node:path");

const [root, fixed] = process.argv.slice(2);
if (!root || !fixed) {
  console.error("usage: node check-undici.js <install-root> <fixed-version>");
  process.exit(2);
}

const copies = [];
const failures = [];
const visited = new Set();

function scanNodeModules(nodeModules) {
  for (const entry of fs.readdirSync(nodeModules)) {
    const location = path.join(nodeModules, entry);
    if (!entry.startsWith("@")) {
      scanPackage(location);
      continue;
    }
    for (const scoped of fs.readdirSync(location)) scanPackage(path.join(location, scoped));
  }
}

function scanPackage(location) {
  let real;
  try {
    real = fs.realpathSync(location);
  } catch (error) {
    failures.push(`cannot resolve ${location}: ${error.code}`);
    return;
  }
  if (!fs.statSync(real).isDirectory() || visited.has(real)) return;
  visited.add(real);

  const manifest = path.join(real, "package.json");
  if (fs.existsSync(manifest)) {
    try {
      const pkg = JSON.parse(fs.readFileSync(manifest, "utf8"));
      if (pkg.name === "undici") copies.push({ location, version: pkg.version });
    } catch (error) {
      failures.push(`unreadable manifest ${manifest}: ${error.message}`);
      return;
    }
  }

  const nested = path.join(real, "node_modules");
  if (fs.existsSync(nested)) scanNodeModules(nested);
}

const topLevel = path.join(root, "node_modules");
if (!fs.existsSync(topLevel)) {
  console.error(`no node_modules under ${root}`);
  process.exit(1);
}

scanNodeModules(topLevel);

if (failures.length > 0) {
  for (const failure of failures) console.error(failure);
  process.exit(1);
}
if (copies.length === 0) {
  console.error(`no undici under ${root}`);
  process.exit(1);
}

const stale = copies.filter((copy) => copy.version !== fixed);
if (stale.length > 0) {
  for (const copy of stale) console.error(`undici ${copy.version} at ${copy.location}; expected ${fixed}`);
  process.exit(1);
}
console.log(`${copies.length} undici ${copies.length === 1 ? "copy" : "copies"}, all ${fixed}`);
