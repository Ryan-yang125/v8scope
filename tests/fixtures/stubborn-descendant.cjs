'use strict';

const fs = require('node:fs');
const { spawn } = require('node:child_process');

const child = spawn(
  process.execPath,
  ['-e', `require('node:fs').writeFileSync(${JSON.stringify(process.argv[2])}, String(process.pid)); process.on('SIGINT', () => {}); setInterval(() => {}, 1000)`],
  { stdio: 'ignore' },
);
child.unref();
process.on('SIGINT', () => process.exit(0));

const deadline = Date.now() + 2_000;
while (!fs.existsSync(process.argv[2]) && Date.now() < deadline) {}
setInterval(() => {}, 1_000);
