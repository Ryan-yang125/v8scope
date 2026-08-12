'use strict'

const http = require('node:http')
const port = Number(process.argv[2])

http.createServer((request, response) => {
  let value = 0
  for (let index = 0; index < 10000; index++) value += Math.sqrt(index)
  response.setHeader('content-type', 'application/json')
  response.end(JSON.stringify({ value }))
}).listen(port, '127.0.0.1')
