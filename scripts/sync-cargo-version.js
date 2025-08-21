// scripts/sync-cargo-version.js
const fs = require('fs');
const path = require('path');

const pkgPath = path.resolve(process.cwd(), 'package.json');
const cargoPath = path.resolve(process.cwd(), 'Cargo.toml');

function die(msg) {
  console.error(msg);
  process.exit(1);
}

if (!fs.existsSync(pkgPath)) die('package.json not found.');
if (!fs.existsSync(cargoPath)) die('Cargo.toml not found.');

const pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
const version = pkg.version;
if (!version) die('No version field in package.json.');

let cargo = fs.readFileSync(cargoPath, 'utf8');

// Replace the first occurrence of a version line like: version = "0.1.0"
const re = /^version\s*=\s*"(.*?)"/m;
if (!re.test(cargo)) {
  die('Could not find `version = "..."` in Cargo.toml.');
}
cargo = cargo.replace(re, `version = "${version}"`);

fs.writeFileSync(cargoPath, cargo);

// Stage Cargo.toml so npm’s auto-commit (from `npm version`) includes it.
try {
  require('child_process').execSync('git add Cargo.toml', { stdio: 'inherit' });
} catch (e) {
  die('Failed to git add Cargo.toml.');
}

console.log(`Synchronized Cargo.toml to version ${version}`);
