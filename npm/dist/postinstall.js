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

  if (p === "darwin" && a === "x64")   return "x86_64-apple-darwin";
  if (p === "darwin" && a === "arm64") return "aarch64-apple-darwin";

  if (p === "linux" && a === "x64")    return "x86_64-unknown-linux-gnu";
  // optionally support musl if you want:
  // if (p === "linux" && a === "x64" && process.env.MUSL) return "x86_64-unknown-linux-musl";

  if (p === "win32" && a === "x64")    return "x86_64-pc-windows-msvc";

  throw new Error(`Unsupported platform: ${p} ${a}`);
}


function download(url, destPath, redirectsLeft = 5) {
  const tmpPath = destPath + ".part";

  return new Promise((resolve, reject) => {
    const doReq = (u, redirects) => {
      const req = https.get(
        u,
        { headers: { "User-Agent": "starthub-cli-installer" } },
        (res) => {
          // Handle redirects
          if ([301, 302, 303, 307, 308].includes(res.statusCode)) {
            if (!res.headers.location || redirects <= 0) {
              return reject(
                new Error(`Too many redirects or missing Location for ${u}`)
              );
            }
            const next = new URL(res.headers.location, u).toString();
            // restart request (ensure we don't keep a stale file)
            try { fs.unlinkSync(tmpPath); } catch {}
            return doReq(next, redirects - 1);
          }

          if (res.statusCode !== 200) {
            return reject(new Error(`HTTP ${res.statusCode} for ${u}`));
          }

          const file = fs.createWriteStream(tmpPath);
          res.pipe(file);
          file.on("finish", () => {
            file.close(() => {
              try {
                fs.renameSync(tmpPath, destPath); // atomic
                resolve();
              } catch (err) {
                reject(err);
              }
            });
          });
          file.on("error", (err) => {
            try { fs.unlinkSync(tmpPath); } catch {}
            reject(err);
          });
        }
      );

      req.on("error", (err) => {
        try { fs.unlinkSync(tmpPath); } catch {}
        reject(err);
      });
    };

    doReq(url, redirectsLeft);
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

    if (!fs.existsSync(tgz)) {
      throw new Error(`Downloaded file missing at ${tgz}`);
    }

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
