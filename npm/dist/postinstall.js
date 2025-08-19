/* eslint-disable no-console */
const fs = require("fs");
const os = require("os");
const path = require("path");
const https = require("https");
const { createGunzip } = require("zlib");
const tar = require("tar");

const version = require("../../package.json").version; // keep in sync with Cargo.toml
const repo = process.env.STARTHUB_CLI_REPO || "starthubhq/cli";
const destDir = path.join(__dirname, "bin");
const exe = process.platform === "win32" ? "starthub.exe" : "starthub";

function rustTarget() {
  const p = process.platform;
  const a = process.arch;
  if (p === "darwin" && a === "x64") return "x86_64-apple-darwin";
  if (p === "darwin" && a === "arm64") return "aarch64-apple-darwin";
  if (p === "linux" && a === "x64") return "x86_64-unknown-linux-gnu";
  if (p === "linux" && a === "arm64") return "aarch64-unknown-linux-gnu";
  if (p === "win32" && a === "x64") return "x86_64-pc-windows-msvc";
  throw new Error(`Unsupported platform: ${p} ${a}`);
}

function download(url, destPath, redirectsLeft = 5) {
  return new Promise((resolve, reject) => {
    const file = fs.createWriteStream(destPath);
    const req = https.get(url, { headers: { "User-Agent": "starthub-cli-installer" } }, (res) => {
      // follow redirects
      if ([301, 302, 303, 307, 308].includes(res.statusCode)) {
        if (!res.headers.location || redirectsLeft <= 0) {
          return reject(new Error(`Too many redirects or missing Location for ${url}`));
        }
        file.close(() => fs.unlink(destPath, () => {})); // cleanup partial
        const next = new URL(res.headers.location, url).toString();
        return download(next, destPath, redirectsLeft - 1).then(resolve, reject);
      }

      if (res.statusCode !== 200) {
        return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
      }

      res.pipe(file);
      file.on("finish", () => file.close(() => resolve()));
    });
    req.on("error", (err) => {
      file.close(() => fs.unlink(destPath, () => reject(err)));
    });
  });
}
(async () => {
  try {
    fs.mkdirSync(destDir, { recursive: true });
    const target = rustTarget();
    const assetName = `starthub-v${version}-${target}.tar.gz`;
    const url = `https://github.com/${repo}/releases/download/v${version}/${assetName}`;
    const tgz = path.join(os.tmpdir(), assetName);

    console.log(`[starthub] downloading ${assetName} …`);
    await download(url, tgz);

    await tar.x({
      file: tgz,
      cwd: destDir,
      gzip: true
    });

    const binPath = path.join(destDir, exe);
    if (process.platform !== "win32") {
      fs.chmodSync(binPath, 0o755);
    }
    console.log(`[starthub] installed binary -> ${binPath}`);
  } catch (err) {
    console.error(`[starthub] install failed: ${err.message}`);
    console.error("You can build from source with Rust: https://rustup.rs/");
    process.exit(1);
  }
})();
