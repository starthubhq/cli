#!/usr/bin/env node
const fs = require("fs");
const path = require("path");
const { spawnSync } = require("child_process");
const { ensureBinary } = require("./shared-download");

const exe = process.platform === "win32" ? "starthub.exe" : "starthub";
const binDir = path.join(__dirname, "bin");
const binPath = path.join(binDir, exe);

(async () => {
  // 1) Local override for dev
  if (process.env.STARTHUB_CLI_BIN && !fs.existsSync(binPath)) {
    fs.mkdirSync(binDir, { recursive: true });
    fs.copyFileSync(path.resolve(process.env.STARTHUB_CLI_BIN), binPath);
    if (process.platform !== "win32") fs.chmodSync(binPath, 0o755);
    console.log(`[starthub] using local binary override: ${process.env.STARTHUB_CLI_BIN}`);
  }

  // 2) Download if missing
  if (!fs.existsSync(binPath)) {
    await ensureBinary({ binDir, exe });
  }

  // 3) Exec
  const result = spawnSync(binPath, process.argv.slice(2), { stdio: "inherit" });
  process.exit(result.status ?? 1);
})();
