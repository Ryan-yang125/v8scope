'use strict'

const fs = require('node:fs')
const { spawn } = require('node:child_process')

const pidFile = process.argv[2]
const child = spawn(
  process.execPath,
  ['-e', 'setInterval(() => {}, 1000)'],
  {
    detached: true,
    env: process.env,
    stdio: 'ignore'
  }
)

fs.writeFileSync(pidFile, String(child.pid))
child.unref()
setTimeout(() => {}, 100)
