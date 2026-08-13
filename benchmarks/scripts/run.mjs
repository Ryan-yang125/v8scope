import { execFile, execFileSync, spawn } from "node:child_process";
import { access, mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { constants as fsConstants } from "node:fs";
import { createServer } from "node:net";
import { cpus, freemem, loadavg, platform, release, tmpdir, totalmem, uptime } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import autocannon from "autocannon";

const execFileAsync = promisify(execFile);
const scriptDir = dirname(fileURLToPath(import.meta.url));
const benchmarkRoot = resolve(scriptDir, "..");
const repositoryRoot = resolve(benchmarkRoot, "..");
const fixture = join(benchmarkRoot, "fixtures", "http-workload.mjs");
const packageBin = (name) => join(benchmarkRoot, "node_modules", ".bin", name);

const variants = [
  { id: "baseline", family: "Node", mode: "baseline" },
  { id: "clinic-doctor", family: "Clinic.js", mode: "doctor", clinic: "doctor" },
  { id: "v8scope-diagnose", family: "V8Scope", mode: "doctor", v8scope: "diagnose" },
  { id: "clinic-flame", family: "Clinic.js", mode: "cpu", clinic: "flame" },
  { id: "v8scope-cpu", family: "V8Scope", mode: "cpu", v8scope: "cpu" },
  { id: "clinic-heapprofiler", family: "Clinic.js", mode: "heap", clinic: "heapprofiler" },
  { id: "v8scope-heap", family: "V8Scope", mode: "heap", v8scope: "heap" },
  { id: "clinic-bubbleprof", family: "Clinic.js", mode: "async", clinic: "bubbleprof" },
  { id: "v8scope-async", family: "V8Scope", mode: "async", v8scope: "async" },
];

function parseArgs(argv) {
  const options = {
    iterations: 10,
    warmup: 1,
    duration: 5,
    connections: 20,
    results: join(benchmarkRoot, "results", "local"),
    selected: variants.map(({ id }) => id),
    timeout: 120,
    label: undefined,
    commit: process.env.V8SCOPE_COMMIT,
    v8scopeBin: process.env.V8SCOPE_BIN,
    clinicBin: process.env.CLINIC_BIN,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    const next = () => {
      const argument = argv[index + 1];
      if (!argument) throw new Error(`${value} requires a value`);
      index += 1;
      return argument;
    };
    if (value === "--iterations") options.iterations = Number.parseInt(next(), 10);
    else if (value === "--warmup") options.warmup = Number.parseInt(next(), 10);
    else if (value === "--duration") options.duration = Number.parseInt(next(), 10);
    else if (value === "--connections") options.connections = Number.parseInt(next(), 10);
    else if (value === "--timeout") options.timeout = Number.parseInt(next(), 10);
    else if (value === "--results") options.results = resolve(next());
    else if (value === "--variants") options.selected = next().split(",").filter(Boolean);
    else if (value === "--label") options.label = next();
    else if (value === "--commit") options.commit = next();
    else if (value === "--v8scope-bin") options.v8scopeBin = resolve(next());
    else if (value === "--clinic-bin") options.clinicBin = resolve(next());
    else throw new Error(`unknown argument: ${value}`);
  }

  for (const [name, number] of Object.entries({
    iterations: options.iterations,
    warmup: options.warmup,
    duration: options.duration,
    connections: options.connections,
    timeout: options.timeout,
  })) {
    if (!Number.isInteger(number) || number < (name === "warmup" ? 0 : 1)) {
      throw new Error(`${name} must be a positive integer`);
    }
  }
  const known = new Set(variants.map(({ id }) => id));
  for (const id of options.selected) {
    if (!known.has(id)) throw new Error(`unknown variant: ${id}`);
  }
  return options;
}

async function executable(path) {
  try {
    await access(path, fsConstants.X_OK);
    return true;
  } catch {
    return false;
  }
}

async function resolveBinaries(options) {
  const v8scopeCandidates = [
    options.v8scopeBin,
    join(repositoryRoot, "target", "release", "v8scope"),
    packageBin("v8scope"),
  ].filter(Boolean);
  const clinicCandidates = [options.clinicBin, packageBin("clinic")].filter(Boolean);
  const v8scopeBin = await firstExecutable(v8scopeCandidates);
  const clinicBin = await firstExecutable(clinicCandidates);
  const selected = new Set(options.selected);
  if ([...selected].some((id) => id.startsWith("v8scope-")) && !v8scopeBin) {
    throw new Error(`v8scope binary not found; checked ${v8scopeCandidates.join(", ")}`);
  }
  if ([...selected].some((id) => id.startsWith("clinic-")) && !clinicBin) {
    throw new Error(`Clinic.js binary not found; run npm ci in ${benchmarkRoot}`);
  }
  return { v8scopeBin, clinicBin };
}

async function firstExecutable(candidates) {
  for (const candidate of candidates) {
    if (await executable(candidate)) return candidate;
  }
  return undefined;
}

function commandVersion(binary) {
  if (!binary) return undefined;
  try {
    return execFileSync(binary, ["--version"], { encoding: "utf8" }).trim();
  } catch {
    return "unknown";
  }
}

function gitCommit() {
  try {
    return execFileSync("git", ["rev-parse", "HEAD"], {
      cwd: repositoryRoot,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
  } catch {
    return undefined;
  }
}

async function freePort() {
  return new Promise((resolvePort, reject) => {
    const server = createServer();
    server.unref();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(() => resolvePort(address.port));
    });
  });
}

function buildCommand(variant, binaries, output) {
  if (variant.id === "baseline") return { command: process.execPath, args: [fixture] };
  if (variant.clinic) {
    return {
      command: binaries.clinicBin,
      args: [
        variant.clinic,
        "--open=false",
        "--dest",
        output,
        "--name",
        "run",
        "--",
        process.execPath,
        fixture,
      ],
    };
  }
  return {
    command: binaries.v8scopeBin,
    args: [variant.v8scope, "--output", output, "--name", "run", "--", process.execPath, fixture],
  };
}

async function waitForReady(url, exited, timeoutMs = 20_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (exited()) throw new Error("profiler exited before the fixture became ready");
    try {
      const response = await fetch(url, { signal: AbortSignal.timeout(500) });
      if (response.ok) return;
    } catch {}
    await delay(50);
  }
  throw new Error(`fixture did not become ready within ${timeoutMs} ms`);
}

async function processTable() {
  const { stdout } = await execFileAsync("ps", ["-Ao", "pid=,ppid=,pgid=,rss=,command="], {
    maxBuffer: 8 * 1024 * 1024,
  });
  return stdout
    .trim()
    .split("\n")
    .map((line) => {
      const match = line.trim().match(/^(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(.+)$/);
      return match
        ? {
            pid: Number(match[1]),
            ppid: Number(match[2]),
            pgid: Number(match[3]),
            rssKb: Number(match[4]),
            command: match[5],
          }
        : undefined;
    })
    .filter(Boolean);
}

function descendants(table, rootPid) {
  const children = new Map();
  for (const process of table) {
    const list = children.get(process.ppid) ?? [];
    list.push(process.pid);
    children.set(process.ppid, list);
  }
  const selected = new Set([rootPid]);
  const queue = [rootPid];
  while (queue.length) {
    const current = queue.shift();
    for (const child of children.get(current) ?? []) {
      if (!selected.has(child)) {
        selected.add(child);
        queue.push(child);
      }
    }
  }
  return table.filter(({ pid }) => selected.has(pid));
}

function startMonitor(pid) {
  const metrics = { peakRssKb: 0, peakProcesses: 0, samples: 0 };
  const processGroups = new Set([pid]);
  let stopped = false;
  let sampling = false;
  const sample = async () => {
    if (stopped || sampling) return;
    sampling = true;
    try {
      const tree = descendants(await processTable(), pid);
      for (const process of tree) processGroups.add(process.pgid);
      metrics.peakRssKb = Math.max(metrics.peakRssKb, tree.reduce((sum, item) => sum + item.rssKb, 0));
      metrics.peakProcesses = Math.max(metrics.peakProcesses, tree.length);
      metrics.samples += 1;
    } catch {}
    sampling = false;
  };
  const timer = setInterval(sample, 100);
  timer.unref();
  void sample();
  return {
    processGroups,
    async stop() {
      stopped = true;
      clearInterval(timer);
      while (sampling) await delay(5);
      return metrics;
    },
  };
}

function groupExists(processGroup) {
  try {
    process.kill(-processGroup, 0);
    return true;
  } catch (error) {
    return error.code !== "ESRCH";
  }
}

function killGroup(processGroup, signal) {
  try {
    process.kill(-processGroup, signal);
  } catch (error) {
    if (error.code !== "ESRCH") throw error;
  }
}

async function interruptProfiler(child, variant) {
  if (variant.v8scope) {
    try {
      const tree = descendants(await processTable(), child.pid);
      const native = tree.find(({ command }) => {
        const executable = command.trim().split(/\s+/, 1)[0];
        return /(^|\/)v8scope$/.test(executable);
      });
      if (native) {
        process.kill(native.pid, "SIGINT");
        return;
      }
    } catch {}
  }
  child.kill("SIGINT");
}

async function directoryFiles(root) {
  const files = [];
  async function visit(directory) {
    let entries;
    try {
      entries = await readdir(directory, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) await visit(path);
      else if (entry.isFile()) files.push(path);
    }
  }
  await visit(root);
  return files;
}

async function inspectArtifacts(variant, output) {
  if (variant.id === "baseline") return { bytes: 0, report: true, complete: true, files: 0 };
  const files = await directoryFiles(output);
  let bytes = 0;
  for (const file of files) bytes += (await stat(file)).size;
  if (variant.clinic) {
    return {
      bytes,
      report: files.some((file) => file.endsWith(".html")),
      complete: files.length > 0,
      files: files.length,
    };
  }
  const manifestPath = files.find((file) => file.endsWith("/manifest.json"));
  let manifest;
  if (manifestPath) {
    try {
      manifest = JSON.parse(await readFile(manifestPath, "utf8"));
    } catch {}
  }
  return {
    bytes,
    report: files.some((file) => file.endsWith("/report/index.html")),
    complete: Boolean(manifest && !manifest.completeness?.partial),
    files: files.length,
  };
}

async function runVariant({ variant, binaries, options, temporaryRoot, phase, round }) {
  const port = await freePort();
  const output = join(temporaryRoot, `${phase}-${String(round).padStart(2, "0")}-${variant.id}`);
  await mkdir(output, { recursive: true });
  const { command, args } = buildCommand(variant, binaries, output);
  const environment = {
    ...process.env,
    CI: "1",
    NO_INSIGHT: "1",
    PORT: String(port),
  };
  const startedAt = performance.now();
  const child = spawn(command, args, {
    cwd: output,
    env: environment,
    detached: true,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", (chunk) => {
    stdout = `${stdout}${chunk}`.slice(-8_000);
  });
  child.stderr.on("data", (chunk) => {
    stderr = `${stderr}${chunk}`.slice(-8_000);
  });
  let exitInfo;
  const exited = new Promise((resolveExit) => {
    child.once("exit", (code, signal) => {
      exitInfo = { code, signal };
      resolveExit(exitInfo);
    });
    child.once("error", (error) => {
      exitInfo = { code: null, signal: null, error: error.message };
      resolveExit(exitInfo);
    });
  });
  const monitor = startMonitor(child.pid);
  let load;
  let error;
  let finalizeMs;
  let timedOut = false;

  try {
    await waitForReady(`http://127.0.0.1:${port}/health`, () => Boolean(exitInfo));
    load = await autocannon({
      url: `http://127.0.0.1:${port}/work?seed=${round}`,
      connections: options.connections,
      duration: options.duration,
      pipelining: 1,
    });
    const loadFinishedAt = performance.now();
    if (!exitInfo) await interruptProfiler(child, variant);
    timedOut = await didTimeout(exited, options.timeout * 1_000);
    if (!exitInfo) {
      killGroup(child.pid, "SIGTERM");
      await didTimeout(exited, 2_000);
    }
    if (!exitInfo) {
      killGroup(child.pid, "SIGKILL");
      await exited;
    }
    finalizeMs = performance.now() - loadFinishedAt;
  } catch (caught) {
    error = caught instanceof Error ? caught.message : String(caught);
    if (!exitInfo) {
      killGroup(child.pid, "SIGTERM");
      await didTimeout(exited, 2_000);
    }
    if (!exitInfo) {
      killGroup(child.pid, "SIGKILL");
      await exited;
    }
  }

  const processMetrics = await monitor.stop();
  await delay(250);
  const lingeringProcessGroups = [...monitor.processGroups].filter((processGroup) =>
    groupExists(processGroup),
  );
  for (const processGroup of lingeringProcessGroups) killGroup(processGroup, "SIGTERM");
  if (lingeringProcessGroups.length) await delay(250);
  for (const processGroup of lingeringProcessGroups) {
    if (groupExists(processGroup)) killGroup(processGroup, "SIGKILL");
  }
  const artifacts = await inspectArtifacts(variant, output);
  const lingeringProcessGroup = lingeringProcessGroups.length > 0;
  const loadSucceeded = Boolean(load && load.errors === 0 && load.timeouts === 0 && load.non2xx === 0);
  const expectedExit = variant.v8scope
    ? exitInfo?.code === 0 || exitInfo?.code === 130
    : exitInfo?.code === 0;
  const reportSucceeded = Boolean(
    loadSucceeded &&
      !error &&
      !timedOut &&
      expectedExit &&
      artifacts.report &&
      artifacts.complete &&
      !lingeringProcessGroup,
  );

  return {
    variant: variant.id,
    family: variant.family,
    mode: variant.mode,
    phase,
    round,
    loadSucceeded,
    reportSucceeded,
    expectedExit,
    exit: exitInfo,
    error,
    timedOut,
    lingeringProcessGroup,
    lingeringProcessGroups,
    elapsedMs: performance.now() - startedAt,
    finalizeMs,
    peakRssKb: processMetrics.peakRssKb,
    peakProcesses: processMetrics.peakProcesses,
    monitorSamples: processMetrics.samples,
    artifacts,
    load: load
      ? {
          requestsAverage: load.requests.average,
          requestsTotal: load.requests.total,
          latencyAverageMs: load.latency.average,
          latencyP50Ms: load.latency.p50,
          latencyP99Ms: load.latency.p99,
          throughputAverageBytes: load.throughput.average,
          errors: load.errors,
          timeouts: load.timeouts,
          non2xx: load.non2xx,
          durationSeconds: load.duration,
        }
      : undefined,
    stdout: redactMachinePaths(stdout.trim()),
    stderr: redactMachinePaths(stderr.trim()),
  };
}

function quantile(values, probability) {
  if (!values.length) return undefined;
  const sorted = [...values].sort((left, right) => left - right);
  const position = (sorted.length - 1) * probability;
  const lower = Math.floor(position);
  const upper = Math.ceil(position);
  if (lower === upper) return sorted[lower];
  return sorted[lower] + (sorted[upper] - sorted[lower]) * (position - lower);
}

function metric(values) {
  const finite = values.filter(Number.isFinite);
  if (!finite.length) return undefined;
  return {
    median: quantile(finite, 0.5),
    q1: quantile(finite, 0.25),
    q3: quantile(finite, 0.75),
    min: Math.min(...finite),
    max: Math.max(...finite),
    samples: finite.length,
  };
}

function summarize(runs, selectedVariants) {
  const byVariant = {};
  for (const variant of selectedVariants) {
    const samples = runs.filter((run) => run.variant === variant.id);
    byVariant[variant.id] = {
      family: variant.family,
      mode: variant.mode,
      runs: samples.length,
      loadSuccesses: samples.filter(({ loadSucceeded }) => loadSucceeded).length,
      reportSuccesses: samples.filter(({ reportSucceeded }) => reportSucceeded).length,
      lingeringProcessGroups: samples.filter(({ lingeringProcessGroup }) => lingeringProcessGroup).length,
      requestsPerSecond: metric(samples.map((run) => run.load?.requestsAverage)),
      latencyP99Ms: metric(samples.map((run) => run.load?.latencyP99Ms)),
      peakRssKb: metric(samples.map(({ peakRssKb }) => peakRssKb)),
      finalizeMs: metric(samples.map(({ finalizeMs }) => finalizeMs)),
      artifactBytes: metric(samples.map(({ artifacts }) => artifacts.bytes)),
    };
  }
  const baseline = byVariant.baseline;
  if (baseline?.requestsPerSecond && baseline?.latencyP99Ms) {
    for (const summary of Object.values(byVariant)) {
      if (summary.requestsPerSecond) {
        summary.throughputDeltaPercent =
          ((summary.requestsPerSecond.median / baseline.requestsPerSecond.median) - 1) * 100;
      }
      if (summary.latencyP99Ms) {
        summary.latencyP99DeltaPercent =
          ((summary.latencyP99Ms.median / baseline.latencyP99Ms.median) - 1) * 100;
      }
    }
  }
  return byVariant;
}

function fixed(value, digits = 1) {
  return Number.isFinite(value) ? value.toFixed(digits) : "—";
}

function redactMachinePaths(value) {
  return value
    .replace(/\/tmp\/v8scope-benchmark-[^/\s]+/g, "<benchmark-host>")
    .replace(/\/(?:private\/)?tmp\/v8scope-bench-[^/\s]+/g, "<run>")
    .replaceAll(benchmarkRoot, "<benchmark>")
    .replaceAll(repositoryRoot, "<repository>");
}

function markdown(result) {
  const lines = [
    `# ${result.environment.label} benchmark`,
    "",
    `Measured ${result.parameters.iterations} times after ${result.parameters.warmup} warmup run(s) per variant. Each measurement used ${result.parameters.connections} connections for ${result.parameters.durationSeconds} seconds.`,
    "",
    `- Host: ${result.environment.os} ${result.environment.osRelease}, ${result.environment.arch}, ${result.environment.cpuCount} CPU(s), ${Math.round(result.environment.totalMemoryBytes / 1024 / 1024 / 1024)} GiB RAM`,
    `- CPU: ${result.environment.cpuModel}`,
    `- Node: ${result.environment.node}`,
    `- V8Scope: ${result.environment.v8scope}`,
    `- Clinic.js: ${result.environment.clinic}`,
    `- Commit: \`${result.environment.commit ?? "unknown"}\``,
    "",
    "| Variant | Report success | Requests/s median | vs baseline | p99 median | Peak tree RSS | Finalize |",
    "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
  ];
  for (const id of result.parameters.variantOrder) {
    const summary = result.summary[id];
    lines.push(
      `| ${id} | ${summary.reportSuccesses}/${summary.runs} | ${fixed(summary.requestsPerSecond?.median, 0)} | ${id === "baseline" ? "—" : `${fixed(summary.throughputDeltaPercent)}%`} | ${fixed(summary.latencyP99Ms?.median)} ms | ${fixed((summary.peakRssKb?.median ?? Number.NaN) / 1024)} MiB | ${fixed(summary.finalizeMs?.median / 1_000, 2)} s |`,
    );
  }
  lines.push(
    "",
    "Values are medians. `raw.json` includes every sample plus Q1/Q3, exits, failures, artifact size, and cleanup state. Report success requires the expected exit contract, a complete report, and no remaining process group.",
    "",
  );
  return lines.join("\n");
}

function rotatedOrder(items, round) {
  const offset = round % items.length;
  const rotated = [...items.slice(offset), ...items.slice(0, offset)];
  return round % 2 === 0 ? rotated : rotated.reverse();
}

async function ensureEmptyOutput(path) {
  try {
    const entries = await readdir(path);
    if (entries.length) throw new Error(`results directory is not empty: ${path}`);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  await mkdir(path, { recursive: true });
}

function delay(milliseconds) {
  return new Promise((resolveDelay) => setTimeout(resolveDelay, milliseconds));
}

async function didTimeout(promise, milliseconds) {
  let timer;
  try {
    return await Promise.race([
      promise.then(() => false),
      new Promise((resolveTimeout) => {
        timer = setTimeout(() => resolveTimeout(true), milliseconds);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function main() {
  if (!["darwin", "linux"].includes(platform())) {
    throw new Error("the benchmark harness currently supports macOS and Linux");
  }
  const options = parseArgs(process.argv.slice(2));
  await ensureEmptyOutput(options.results);
  const binaries = await resolveBinaries(options);
  const selectedVariants = variants.filter(({ id }) => options.selected.includes(id));
  const temporaryRoot = await mkdtemp(join(tmpdir(), "v8scope-bench-"));
  const warmups = [];
  const runs = [];
  const cpuList = cpus();
  const environment = {
    label: options.label ?? `${platform()}-${process.arch}-node${process.versions.node.split(".")[0]}`,
    os: platform(),
    osRelease: release(),
    arch: process.arch,
    cpuModel: cpuList[0]?.model ?? "unknown",
    cpuCount: cpuList.length,
    totalMemoryBytes: totalmem(),
    freeMemoryBytesAtStart: freemem(),
    loadAverageAtStart: loadavg(),
    hostUptimeSeconds: uptime(),
    node: process.version,
    v8: process.versions.v8,
    v8scope: commandVersion(binaries.v8scopeBin),
    clinic: commandVersion(binaries.clinicBin),
    autocannon: JSON.parse(
      await readFile(join(benchmarkRoot, "node_modules", "autocannon", "package.json"), "utf8"),
    ).version,
    commit: options.commit ?? gitCommit(),
  };

  try {
    for (let round = 0; round < options.warmup; round += 1) {
      for (const variant of rotatedOrder(selectedVariants, round)) {
        process.stdout.write(`[warmup ${round + 1}/${options.warmup}] ${variant.id}\n`);
        warmups.push(await runVariant({ variant, binaries, options, temporaryRoot, phase: "warmup", round }));
      }
    }
    for (let round = 0; round < options.iterations; round += 1) {
      for (const variant of rotatedOrder(selectedVariants, round)) {
        process.stdout.write(`[run ${round + 1}/${options.iterations}] ${variant.id}\n`);
        const run = await runVariant({ variant, binaries, options, temporaryRoot, phase: "measured", round });
        runs.push(run);
        process.stdout.write(
          `  load=${run.loadSucceeded ? "ok" : "failed"} report=${run.reportSucceeded ? "ok" : "failed"} rps=${fixed(run.load?.requestsAverage, 0)}\n`,
        );
      }
    }

    const result = {
      schemaVersion: 1,
      generatedAt: new Date().toISOString(),
      environment,
      parameters: {
        iterations: options.iterations,
        warmup: options.warmup,
        durationSeconds: options.duration,
        connections: options.connections,
        timeoutSeconds: options.timeout,
        variantOrder: selectedVariants.map(({ id }) => id),
        scheduling: "rotated each round and reversed on odd rounds",
      },
      warmups: warmups.map(({ variant, loadSucceeded, reportSucceeded, error }) => ({
        variant,
        loadSucceeded,
        reportSucceeded,
        error,
      })),
      summary: summarize(runs, selectedVariants),
      runs,
    };
    await writeFile(join(options.results, "raw.json"), `${JSON.stringify(result, null, 2)}\n`);
    await writeFile(join(options.results, "summary.md"), markdown(result));
    process.stdout.write(`wrote ${options.results}\n`);

    const failedV8Scope = runs.filter((run) => run.family === "V8Scope" && !run.reportSucceeded);
    if (failedV8Scope.length) {
      throw new Error(`${failedV8Scope.length} measured V8Scope run(s) failed`);
    }
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  process.stderr.write(`${error.stack ?? error}\n`);
  process.exitCode = 1;
});
