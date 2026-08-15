#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";

const PLATFORMS: Readonly<Record<string, string>> = {
  "darwin-arm64": "@alecghost/lighter-darwin-arm64",
  "darwin-x64": "@alecghost/lighter-darwin-x64",
  "linux-x64": "@alecghost/lighter-linux-x64",
  "win32-x64": "@alecghost/lighter-win32-x64",
};

function getBinaryPath(): string {
  const customPath = process.env["LIGHTER_BINARY_PATH"];
  if (customPath) {
    if (!existsSync(customPath)) {
      console.error(
        `Error: LIGHTER_BINARY_PATH does not exist: ${customPath}`,
      );
      process.exit(1);
    }
    return customPath;
  }

  const platformKey = `${process.platform}-${process.arch}`;
  const packageName = PLATFORMS[platformKey];

  if (!packageName) {
    console.error(
      `Error: Unsupported platform: ${process.platform} (${process.arch}).\n` +
        `Supported: ${Object.keys(PLATFORMS).join(", ")}.`,
    );
    process.exit(1);
  }

  let pkgJsonPath: string;
  try {
    pkgJsonPath = require.resolve(`${packageName}/package.json`);
  } catch {
    console.error(
      `Error: Could not find "${packageName}".\n` +
        `The optional dependency for your platform was not installed.\n` +
        `Try reinstalling @alecghost/lighter or install "${packageName}" directly.`,
    );
    process.exit(1);
  }

  const binName = process.platform === "win32" ? "lighter.exe" : "lighter";
  const binPath = join(dirname(pkgJsonPath), "bin", binName);

  if (!existsSync(binPath)) {
    console.error(
      `Error: Binary not found at ${binPath}.\n` +
        `Try reinstalling "${packageName}" or set LIGHTER_BINARY_PATH.`,
    );
    process.exit(1);
  }

  return binPath;
}

function main(): void {
  const binaryPath = getBinaryPath();
  const result = spawnSync(binaryPath, process.argv.slice(2), {
    stdio: "inherit",
  });

  if (result.error) {
    console.error(`Error executing lighter: ${result.error.message}`);
    process.exit(1);
  }

  if (result.signal) {
    process.kill(process.pid, result.signal);
  }

  process.exit(result.status ?? 0);
}

main();
