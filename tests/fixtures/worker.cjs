'use strict'

const { isMainThread, Worker } = require('node:worker_threads')

function burn () {
  const deadline = Date.now() + 450
  let value = 0
  while (Date.now() < deadline) {
    for (let index = 0; index < 100000; index++) value += Math.sqrt(index)
  }
  return value
}

if (isMainThread) {
  const worker = new Worker(__filename)
  burn()
  worker.once('exit', () => {})
} else {
  burn()
}
