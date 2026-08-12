'use strict';

const fs = require('node:fs');

const marker = process.argv[2];
process.once('SIGINT', () => {
  setTimeout(() => {
    fs.writeFileSync(marker, 'clean\n');
    process.exit(0);
  }, 200);
});
setInterval(() => {}, 1_000);
