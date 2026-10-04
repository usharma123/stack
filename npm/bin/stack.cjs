#!/usr/bin/env node
'use strict';
const { spawn } = require('node:child_process');
const path = require('node:path');
const fs = require('node:fs');

const platform = `${process.platform}-${process.arch}`;
const binary = path.join(__dirname, '..', 'binaries', platform, 'stack');
if (!['darwin-x64', 'darwin-arm64', 'linux-x64', 'linux-arm64'].includes(platform)) {
  console.error(`stack: unsupported platform ${platform}. Build from source: https://github.com/usharma123/stack`);
  process.exit(1);
}
if (process.platform === 'linux' && !process.report.getReport().header.glibcVersionRuntime) {
  console.error('stack: Linux requires glibc 2.39 or newer. Alpine/musl is not supported by this package.');
  process.exit(1);
}
if (!fs.existsSync(binary)) {
  console.error(`stack: missing packaged binary for ${platform}. Reinstall @ushawarma/stack.`);
  process.exit(1);
}
const child = spawn(binary, process.argv.slice(2), { stdio: 'inherit' });
const signals = ['SIGINT', 'SIGTERM', 'SIGHUP'];
for (const signal of signals) process.on(signal, () => child.kill(signal));
child.on('error', error => {
  console.error(`stack: cannot start ${platform} binary: ${error.message}`);
  process.exitCode = 1;
});
child.on('exit', (code, signal) => {
  if (signal) {
    process.removeAllListeners(signal);
    process.kill(process.pid, signal);
  } else process.exitCode = code ?? 1;
});
