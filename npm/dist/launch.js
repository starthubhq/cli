#!/usr/bin/env node
const path = require("path");
const { spawn } = require("child_process");
const exe = process.platform === "win32" ? "starthub.exe" : "starthub";

// allow override for dev
const override = process.env.STARTHUB_CLI_BIN;
const bin = override || path.join(__dirname, "bin", exe);

const child = spawn(bin, process.argv.slice(2), { stdio: "inherit" });
child.on("exit", (code) => process.exit(code ?? 1));
child.on("error", (err) => {
  console.error(`[starthub] failed to start: ${err.message}`);
  process.exit(1);
});
