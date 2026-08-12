'use strict';

const fs = require('node:fs');
const { spawn } = require('node:child_process');

const child = spawn(
  process.execPath,
  ['-e', `require('node:fs').writeFileSync(${JSON.stringify(process.argv[2])}, String(process.pid)); setInterval(() => {}, 1000)`],
  { stdio: 'ignore' },
);
child.unref();

const deadline = Date.now() + 2_000;
while (!fs.existsSync(process.argv[2]) && Date.now() < deadline) {}
process.exit(0);
