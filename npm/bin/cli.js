#!/usr/bin/env node

const { execFileSync } = require('child_process');
const path = require('path');
const fs = require('fs');

const isWindows = process.platform === 'win32';
const binaryName = isWindows ? 'ccnest.exe' : 'ccnest';
const binaryPath = path.join(__dirname, binaryName);

if (!fs.existsSync(binaryPath)) {
  console.error('ccnest binary not found. Try reinstalling: npm install -g ccnest-cli');
  process.exit(1);
}

try {
  execFileSync(binaryPath, process.argv.slice(2), {
    stdio: 'inherit',
    env: process.env,
  });
} catch (err) {
  if (err.status !== null) {
    process.exit(err.status);
  }
}
