import { stat } from "node:fs/promises";
import { createServer } from "node:http";
import { fileURLToPath } from "node:url";

const fixturePath = fileURLToPath(import.meta.url);
const port = Number.parseInt(process.env.PORT ?? "3000", 10);

function cpuWork(seed) {
  let value = seed | 0;
  for (let index = 0; index < 18_000; index += 1) {
    value = Math.imul(value ^ index, 1_664_525) + 1_013_904_223;
  }
  return value >>> 0;
}

function allocationWork(seed) {
  const values = Array.from({ length: 96 }, (_, index) => ({
    id: index,
    value: (seed + index).toString(36),
  }));
  return Buffer.byteLength(JSON.stringify(values));
}

async function asyncWork() {
  await stat(fixturePath);
  await new Promise((resolve) => setTimeout(resolve, 2));
}

const server = createServer(async (request, response) => {
  if (request.url === "/health") {
    response.writeHead(200, { "content-type": "text/plain" });
    response.end("ok");
    return;
  }

  try {
    const seed = Number.parseInt(new URL(request.url, "http://localhost").searchParams.get("seed") ?? "1", 10);
    const checksum = cpuWork(seed) ^ allocationWork(seed);
    await asyncWork();
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify({ checksum }));
  } catch (error) {
    response.writeHead(500, { "content-type": "application/json" });
    response.end(JSON.stringify({ error: error.message }));
  }
});

function shutdown() {
  server.close(() => process.exit(0));
  setTimeout(() => process.exit(1), 5_000).unref();
}

process.once("SIGINT", shutdown);
process.once("SIGTERM", shutdown);
server.listen(port, "127.0.0.1", () => {
  process.stdout.write(`ready:${port}\n`);
});
