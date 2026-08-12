'use strict'

const { spawn } = require('node:child_process')

const output = `${process.env.V8SCOPE_RUN_DIR}/detached-late.txt`
const child = spawn(
  process.execPath,
  [
    '-e',
    'setTimeout(() => require("node:fs").writeFileSync(process.env.OUTPUT, "late"), 2000)'
  ],
  {
    detached: true,
    env: { ...process.env, OUTPUT: output },
    stdio: 'ignore'
  }
)
child.unref()
