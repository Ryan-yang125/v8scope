'use strict'

const retained = []
const objects = []
const deadline = Date.now() + 650

function work () {
  let value = 0
  for (let index = 0; index < 200000; index++) value += Math.sqrt(index)
  retained.push(Buffer.alloc(64 * 1024, Math.floor(value) & 255))
  objects.push(new Array(10000).fill({ value }))
  Promise.resolve(value).then(() => {})
  if (Date.now() < deadline) setImmediate(work)
}

work()
