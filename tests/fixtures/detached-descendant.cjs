'use strict'

const fs = require('node:fs')
const { spawn } = require('node:child_process')

const pidFile = process.argv[2]
const readyFile = `${pidFile}.ready`
const child = spawn(
  process.execPath,
  [
    '-e',
    `
      const fs = require('node:fs')
      const sampleUntil = Date.now() + 500
      while (Date.now() < sampleUntil) Math.sqrt(Date.now())
      fs.writeFileSync(process.argv[1], 'ready')
      setInterval(() => {}, 1000)
    `,
    readyFile
  ],
  {
    detached: true,
    env: process.env,
    stdio: 'ignore'
  }
)

fs.writeFileSync(pidFile, String(child.pid))
child.unref()

const deadline = Date.now() + 5000
const ready = setInterval(() => {
  if (fs.existsSync(readyFile)) {
    clearInterval(ready)
    return
  }
  if (Date.now() >= deadline) {
    clearInterval(ready)
    process.exitCode = 1
  }
}, 10)
