import { execFile, execFileSync, spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { access, cp, mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { constants as fsConstants } from "node:fs";
import { createServer } from "node:net";
import { cpus, platform, release, tmpdir, totalmem } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import autocannon from "autocannon";

const execFileAsync = promisify(execFile);
const scriptDir = dirname(fileURLToPath(import.meta.url));
const benchmarkRoot = resolve(scriptDir, "..");
const repositoryRoot = resolve(benchmarkRoot, "..");
const fixture = join(benchmarkRoot, "fixtures", "http-workload.mjs");
const packageBin = (name) => join(benchmarkRoot, "node_modules", ".bin", name);

function parseArgs(argv) {
  const options = {
    startupIterations: 30,
    reportIterations: 10,
    warmup: 1,
    captureDuration: 5,
    results: join(benchmarkRoot, "results", "local-control-plane"),
    label: undefined,
    commit: process.env.V8SCOPE_COMMIT,
    v8scopeLauncher: process.env.V8SCOPE_LAUNCHER,
    v8scopeNative: process.env.V8SCOPE_NATIVE,
    clinicBin: process.env.CLINIC_BIN,
    v8scopePrefix: process.env.V8SCOPE_PREFIX,
    clinicPrefix: process.env.CLINIC_PREFIX,
    skipFootprint: false,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    const next = () => {
      const argument = argv[index + 1];
      if (!argument) throw new Error(`${value} requires a value`);
      index += 1;
      return argument;
    };
    if (value === "--startup-iterations") options.startupIterations = Number.parseInt(next(), 10);
    else if (value === "--report-iterations") options.reportIterations = Number.parseInt(next(), 10);
    else if (value === "--warmup") options.warmup = Number.parseInt(next(), 10);
    else if (value === "--capture-duration") options.captureDuration = Number.parseInt(next(), 10);
    else if (value === "--results") options.results = resolve(next());
    else if (value === "--label") options.label = next();
    else if (value === "--commit") options.commit = next();
    else if (value === "--v8scope-launcher") options.v8scopeLauncher = resolve(next());
    else if (value === "--v8scope-native") options.v8scopeNative = resolve(next());
    else if (value === "--clinic-bin") options.clinicBin = resolve(next());
    else if (value === "--v8scope-prefix") options.v8scopePrefix = resolve(next());
    else if (value === "--clinic-prefix") options.clinicPrefix = resolve(next());
    else if (value === "--skip-footprint") options.skipFootprint = true;
    else throw new Error(`unknown argument: ${value}`);
  }

  for (const [name, number] of Object.entries({
    startupIterations: options.startupIterations,
    reportIterations: options.reportIterations,
    warmup: options.warmup,
    captureDuration: options.captureDuration,
  })) {
    if (!Number.isInteger(number) || number < (name === "warmup" ? 0 : 1)) {
      throw new Error(`${name} must be a positive integer`);
    }
  }
  if (!options.skipFootprint && (!options.v8scopePrefix || !options.clinicPrefix)) {
    throw new Error("separate --v8scope-prefix and --clinic-prefix are required for footprint data");
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

async function firstExecutable(paths) {
  for (const path of paths.filter(Boolean)) {
    if (await executable(path)) return path;
  }
  return undefined;
}

async function resolveBinaries(options) {
  const builtBinary = join(repositoryRoot, "target", "release", "v8scope");
  const v8scopeLauncher = await firstExecutable([
    options.v8scopeLauncher,
    packageBin("v8scope"),
    builtBinary,
  ]);
  const v8scopeNative = await firstExecutable([options.v8scopeNative, builtBinary, v8scopeLauncher]);
  const clinicBin = await firstExecutable([options.clinicBin, packageBin("clinic")]);
  if (!v8scopeLauncher || !v8scopeNative || !clinicBin) {
    throw new Error("V8Scope launcher, V8Scope native binary, and Clinic.js binary are required");
  }
  return { v8scopeLauncher, v8scopeNative, clinicBin };
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

async function commandVersion(command) {
  try {
    const { stdout, stderr } = await execFileAsync(command, ["--version"], { maxBuffer: 1024 * 1024 });
    return `${stdout}${stderr}`.trim().split("\n").at(-1);
  } catch {
    return "unknown";
  }
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

function delay(milliseconds) {
  return new Promise((resolveDelay) => setTimeout(resolveDelay, milliseconds));
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

function killGroup(processGroup, signal) {
  try {
    process.kill(-processGroup, signal);
  } catch (error) {
    if (error.code !== "ESRCH") throw error;
  }
}

function groupExists(processGroup) {
  try {
    process.kill(-processGroup, 0);
    return true;
  } catch (error) {
    return error.code !== "ESRCH";
  }
}

async function stopDetached(childProcess) {
  killGroup(childProcess.child.pid, "SIGTERM");
  try {
    await waitWithTimeout(childProcess.exited, 2_000);
  } catch {}
  await delay(250);
  if (groupExists(childProcess.child.pid)) {
    killGroup(childProcess.child.pid, "SIGKILL");
  }
  await childProcess.exited;
}

async function waitWithTimeout(promise, milliseconds) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`command timed out after ${milliseconds} ms`)), milliseconds);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function spawnCaptured(command, args, options = {}) {
  const child = spawn(command, args, {
    cwd: options.cwd,
    env: options.env ?? process.env,
    detached: options.detached ?? false,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", (chunk) => {
    stdout = `${stdout}${chunk}`.slice(-16_000);
  });
  child.stderr.on("data", (chunk) => {
    stderr = `${stderr}${chunk}`.slice(-16_000);
  });
  const exited = new Promise((resolveExit, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => resolveExit({ code, signal }));
  });
  return { child, exited, output: () => ({ stdout, stderr }) };
}

async function captureSeeds({ binaries, options, temporaryRoot }) {
  const v8scopeOutput = join(temporaryRoot, "v8scope-seed");
  const clinicOutput = join(temporaryRoot, "clinic-seed");
  await mkdir(v8scopeOutput, { recursive: true });
  await mkdir(clinicOutput, { recursive: true });

  const capture = async ({ family, command, args, output, port }) => {
    const childProcess = await spawnCaptured(command, args, {
      cwd: output,
      detached: true,
      env: { ...process.env, CI: "1", NO_INSIGHT: "1", PORT: String(port) },
    });
    let exitInfo;
    void childProcess.exited.then((value) => {
      exitInfo = value;
    });
    try {
      await waitForReady(`http://127.0.0.1:${port}/health`, () => Boolean(exitInfo));
      const load = await autocannon({
        url: `http://127.0.0.1:${port}/work`,
        connections: 20,
        duration: Math.max(1, options.captureDuration - 2),
        pipelining: 1,
      });
      if (load.errors || load.timeouts || load.non2xx) throw new Error(`${family} seed load failed`);
      if (family === "clinic") childProcess.child.kill("SIGINT");
      const result = await waitWithTimeout(childProcess.exited, 45_000);
      if (result.code !== 0) throw new Error(`${family} seed capture exited ${JSON.stringify(result)}`);
    } catch (error) {
      if (!exitInfo) await stopDetached(childProcess);
      throw error;
    }
    await delay(25);
    if (groupExists(childProcess.child.pid)) {
      await stopDetached(childProcess);
      throw new Error(`${family} seed capture left a process group running`);
    }
    const { stdout, stderr } = childProcess.output();
    return { stdout: redactMachinePaths(stdout), stderr: redactMachinePaths(stderr) };
  };

  const v8scopePort = await freePort();
  const v8scopeLog = await capture({
    family: "v8scope",
    command: binaries.v8scopeLauncher,
    args: [
      "diagnose",
      "--duration",
      `${options.captureDuration}s`,
      "--no-report",
      "--output",
      v8scopeOutput,
      "--name",
      "run",
      "--",
      process.execPath,
      fixture,
    ],
    output: v8scopeOutput,
    port: v8scopePort,
  });
  const v8scopeDirectories = (await readdir(v8scopeOutput, { withFileTypes: true }))
    .filter((entry) => entry.isDirectory())
    .map((entry) => join(v8scopeOutput, entry.name));
  if (v8scopeDirectories.length !== 1) throw new Error("expected one V8Scope seed run");

  const clinicPort = await freePort();
  const clinicLog = await capture({
    family: "clinic",
    command: binaries.clinicBin,
    args: [
      "doctor",
      "--open=false",
      "--dest",
      clinicOutput,
      "--name",
      "run",
      "--",
      process.execPath,
      fixture,
    ],
    output: clinicOutput,
    port: clinicPort,
  });
  const clinicDirectories = (await readdir(clinicOutput, { withFileTypes: true }))
    .filter((entry) => entry.isDirectory() && entry.name.endsWith(".clinic-doctor"))
    .map((entry) => join(clinicOutput, entry.name));
  if (clinicDirectories.length !== 1) throw new Error("expected one Clinic Doctor seed run");

  return {
    v8scope: v8scopeDirectories[0],
    clinic: clinicDirectories[0],
    logs: { v8scope: v8scopeLog, clinic: clinicLog },
  };
}

async function runTimed(command, args, cwd) {
  const timeOutput = join(cwd, `.time-${randomUUID()}.txt`);
  const startedAt = performance.now();
  const childProcess = await spawnCaptured("/usr/bin/time", ["-v", "-o", timeOutput, command, ...args], {
    cwd,
    env: { ...process.env, CI: "1", NO_INSIGHT: "1" },
    detached: true,
  });
  let exit;
  try {
    exit = await waitWithTimeout(childProcess.exited, 60_000);
  } catch (error) {
    await stopDetached(childProcess);
    throw error;
  }
  const elapsedMs = performance.now() - startedAt;
  const timing = await readFile(timeOutput, "utf8");
  await rm(timeOutput);
  const rssMatch = timing.match(/Maximum resident set size \(kbytes\):\s*(\d+)/);
  const { stdout, stderr } = childProcess.output();
  await delay(25);
  const lingeringProcessGroup = groupExists(childProcess.child.pid);
  if (lingeringProcessGroup) await stopDetached(childProcess);
  return {
    exit,
    elapsedMs,
    peakRssKb: rssMatch ? Number(rssMatch[1]) : undefined,
    lingeringProcessGroup,
    stdout: redactMachinePaths(stdout.trim()),
    stderr: redactMachinePaths(stderr.trim()),
  };
}

function rotated(items, round) {
  const offset = round % items.length;
  const order = [...items.slice(offset), ...items.slice(0, offset)];
  return round % 2 === 0 ? order : order.reverse();
}

async function benchmarkStartup({ binaries, options, temporaryRoot }) {
  const variants = [
    { id: "clinic-cli", command: binaries.clinicBin },
    { id: "v8scope-npm", command: binaries.v8scopeLauncher },
    { id: "v8scope-native", command: binaries.v8scopeNative },
  ];
  const samples = [];
  for (let round = 0; round < options.warmup + options.startupIterations; round += 1) {
    for (const variant of rotated(variants, round)) {
      const working = join(temporaryRoot, `startup-${round}-${variant.id}`);
      await mkdir(working, { recursive: true });
      const result = await runTimed(variant.command, ["--version"], working);
      if (result.exit.code !== 0) throw new Error(`${variant.id} startup failed: ${result.stderr}`);
      if (result.lingeringProcessGroup) throw new Error(`${variant.id} startup left a process group running`);
      if (round >= options.warmup) samples.push({ variant: variant.id, round: round - options.warmup, ...result });
    }
  }
  return samples;
}

async function benchmarkReportRebuild({ binaries, options, seeds, temporaryRoot }) {
  const variants = [
    { id: "clinic-doctor", command: binaries.clinicBin },
    { id: "v8scope-npm", command: binaries.v8scopeLauncher },
    { id: "v8scope-native", command: binaries.v8scopeNative },
  ];
  const samples = [];
  for (let round = 0; round < options.warmup + options.reportIterations; round += 1) {
    for (const variant of rotated(variants, round)) {
      const working = join(temporaryRoot, `report-${round}-${variant.id}`);
      await mkdir(working, { recursive: true });
      let input;
      let args;
      if (variant.id === "clinic-doctor") {
        input = join(working, basename(seeds.clinic));
        await cp(seeds.clinic, input, { recursive: true });
        args = ["doctor", "--visualize-only", input, "--open=false"];
      } else {
        input = join(working, "run");
        await cp(seeds.v8scope, input, { recursive: true });
        args = ["analyze", input];
      }
      const result = await runTimed(variant.command, args, working);
      if (result.exit.code !== 0) throw new Error(`${variant.id} report rebuild failed: ${result.stderr}`);
      if (result.lingeringProcessGroup) throw new Error(`${variant.id} report rebuild left a process group running`);
      let report;
      if (variant.id === "clinic-doctor") {
        const reports = (await readdir(working, { withFileTypes: true }))
          .filter((entry) => entry.isFile() && entry.name.endsWith(".html"))
          .map((entry) => join(working, entry.name));
        if (reports.length !== 1) {
          const entries = (await readdir(working)).join(", ");
          throw new Error(
            `Clinic report rebuild produced ${reports.length} HTML files; entries=${entries}; stdout=${result.stdout}; stderr=${result.stderr}`,
          );
        }
        [report] = reports;
      } else {
        report = join(input, "report", "index.html");
      }
      if ((await stat(report)).size === 0) throw new Error(`${variant.id} produced an empty report`);
      if (round >= options.warmup) samples.push({ variant: variant.id, round: round - options.warmup, ...result });
    }
  }
  return samples;
}

async function directoryMetrics(root) {
  let bytes = 0;
  let files = 0;
  async function visit(path) {
    for (const entry of await readdir(path, { withFileTypes: true })) {
      const child = join(path, entry.name);
      if (entry.isDirectory()) await visit(child);
      else if (entry.isFile()) {
        files += 1;
        bytes += (await stat(child)).size;
      }
    }
  }
  await visit(root);
  return { bytes, files };
}

async function dependencyCount(prefix) {
  const { stdout } = await execFileAsync(
    "npm",
    ["ls", "--prefix", prefix, "--all", "--omit=dev", "--parseable"],
    { maxBuffer: 16 * 1024 * 1024 },
  );
  return Math.max(0, stdout.trim().split("\n").filter(Boolean).length - 1);
}

async function vulnerabilityCount(prefix) {
  try {
    await execFileAsync("npm", ["audit", "--prefix", prefix, "--omit=dev", "--json"], {
      maxBuffer: 16 * 1024 * 1024,
    });
    return 0;
  } catch (error) {
    const audit = JSON.parse(String(error.stdout));
    return audit.metadata?.vulnerabilities?.total;
  }
}

async function measureFootprint(prefix) {
  const nodeModules = join(prefix, "node_modules");
  return {
    ...(await directoryMetrics(nodeModules)),
    packages: await dependencyCount(prefix),
    productionVulnerabilities: await vulnerabilityCount(prefix),
  };
}

function quantile(values, probability) {
  const sorted = [...values].sort((left, right) => left - right);
  const position = (sorted.length - 1) * probability;
  const lower = Math.floor(position);
  const upper = Math.ceil(position);
  if (lower === upper) return sorted[lower];
  return sorted[lower] + (sorted[upper] - sorted[lower]) * (position - lower);
}

function metric(values) {
  const finite = values.filter(Number.isFinite);
  return {
    median: quantile(finite, 0.5),
    q1: quantile(finite, 0.25),
    q3: quantile(finite, 0.75),
    min: Math.min(...finite),
    max: Math.max(...finite),
    samples: finite.length,
  };
}

function summarize(samples) {
  return Object.fromEntries(
    [...new Set(samples.map(({ variant }) => variant))].map((variant) => {
      const selected = samples.filter((sample) => sample.variant === variant);
      return [variant, {
        runs: selected.length,
        elapsedMs: metric(selected.map(({ elapsedMs }) => elapsedMs)),
        peakRssKb: metric(selected.map(({ peakRssKb }) => peakRssKb)),
      }];
    }),
  );
}

function fixed(value, digits = 1) {
  return Number.isFinite(value) ? value.toFixed(digits) : "—";
}

function markdown(result) {
  const startup = result.summary.startup;
  const report = result.summary.reportRebuild;
  const footprint = result.footprint;
  const lines = [
    `# ${result.environment.label} control-plane benchmark`,
    "",
    `Startup uses ${result.parameters.startupIterations} measured runs after ${result.parameters.warmup} warmup. Report rebuilding uses ${result.parameters.reportIterations} measured runs after ${result.parameters.warmup} warmup against one fresh copy of each tool's equivalent ${result.parameters.captureDurationSeconds}-second diagnosis capture per run.`,
    "",
    `- Host: ${result.environment.os} ${result.environment.osRelease}, ${result.environment.arch}, ${result.environment.cpuCount} CPU(s), ${Math.round(result.environment.totalMemoryBytes / 1024 / 1024 / 1024)} GiB RAM`,
    `- CPU: ${result.environment.cpuModel}`,
    `- Node: ${result.environment.node}`,
    `- V8Scope: ${result.environment.v8scope}`,
    `- Clinic.js: ${result.environment.clinic}`,
    `- Commit: \`${result.environment.commit ?? "unknown"}\``,
    "",
    "## CLI startup",
    "",
    "| Entry point | Median | Q1–Q3 | Peak RSS median |",
    "| --- | ---: | ---: | ---: |",
  ];
  for (const id of ["clinic-cli", "v8scope-npm", "v8scope-native"]) {
    const item = startup[id];
    lines.push(`| ${id} | ${fixed(item.elapsedMs.median)} ms | ${fixed(item.elapsedMs.q1)}–${fixed(item.elapsedMs.q3)} ms | ${fixed(item.peakRssKb.median / 1024)} MiB |`);
  }
  lines.push(
    "",
    "`v8scope-npm` is the public npm launcher; `v8scope-native` is the same released Rust binary after platform selection.",
    "",
    "## Offline report rebuild",
    "",
    "| Workflow | Median | Q1–Q3 | Peak RSS median | Input bytes |",
    "| --- | ---: | ---: | ---: | ---: |",
  );
  for (const id of ["clinic-doctor", "v8scope-npm", "v8scope-native"]) {
    const item = report[id];
    const input = id === "clinic-doctor" ? result.inputs.clinic : result.inputs.v8scope;
    lines.push(`| ${id} | ${fixed(item.elapsedMs.median)} ms | ${fixed(item.elapsedMs.q1)}–${fixed(item.elapsedMs.q3)} ms | ${fixed(item.peakRssKb.median / 1024)} MiB | ${input.bytes} |`);
  }
  if (footprint) {
    lines.push(
      "",
      "## Installed production surface",
      "",
      "| Package | Installed bytes | Files | npm dependency nodes | npm audit findings |",
      "| --- | ---: | ---: | ---: | ---: |",
      `| Clinic.js 13.0.0 | ${footprint.clinic.bytes} | ${footprint.clinic.files} | ${footprint.clinic.packages} | ${footprint.clinic.productionVulnerabilities} |`,
      `| V8Scope 0.2.0 | ${footprint.v8scope.bytes} | ${footprint.v8scope.files} | ${footprint.v8scope.packages} | ${footprint.v8scope.productionVulnerabilities} |`,
    );
  }
  lines.push(
    "",
    "Wall time includes process startup and the public command path. GNU time records peak RSS. Input copying is excluded from report timing. Raw samples and audit counts are retained in `control-plane.json`.",
    "",
  );
  return lines.join("\n");
}

function redactMachinePaths(value) {
  return value
    .replace(/\/tmp\/v8scope-control-[^/\s]+/g, "<control-host>")
    .replace(/\/(?:private\/)?tmp\/v8scope-control-[^/\s]+/g, "<control-run>")
    .replaceAll(benchmarkRoot, "<benchmark>")
    .replaceAll(repositoryRoot, "<repository>");
}

async function main() {
  if (platform() !== "linux") throw new Error("the canonical control-plane benchmark requires Linux and GNU time");
  const options = parseArgs(process.argv.slice(2));
  await ensureEmptyOutput(options.results);
  const binaries = await resolveBinaries(options);
  const temporaryRoot = await mkdtemp(join(tmpdir(), "v8scope-control-"));
  try {
    process.stdout.write("capturing fixed report inputs\n");
    const seeds = await captureSeeds({ binaries, options, temporaryRoot });
    process.stdout.write("measuring CLI startup\n");
    const startup = await benchmarkStartup({ binaries, options, temporaryRoot });
    process.stdout.write("measuring offline report rebuild\n");
    const reportRebuild = await benchmarkReportRebuild({
      binaries,
      options,
      seeds,
      temporaryRoot,
    });
    const footprint = options.skipFootprint ? undefined : {
      clinic: await measureFootprint(options.clinicPrefix),
      v8scope: await measureFootprint(options.v8scopePrefix),
    };
    const cpuList = cpus();
    const result = {
      schemaVersion: 1,
      generatedAt: new Date().toISOString(),
      environment: {
        label: options.label ?? `linux-${process.arch}-node${process.versions.node.split(".")[0]}`,
        os: platform(),
        osRelease: release(),
        arch: process.arch,
        cpuModel: cpuList[0]?.model ?? "unknown",
        cpuCount: cpuList.length,
        totalMemoryBytes: totalmem(),
        node: process.version,
        v8: process.versions.v8,
        v8scope: await commandVersion(binaries.v8scopeLauncher),
        clinic: await commandVersion(binaries.clinicBin),
        commit: options.commit ?? gitCommit(),
      },
      parameters: {
        startupIterations: options.startupIterations,
        reportIterations: options.reportIterations,
        warmup: options.warmup,
        captureDurationSeconds: options.captureDuration,
        scheduling: "rotated each round and reversed on odd rounds",
      },
      inputs: {
        clinic: await directoryMetrics(seeds.clinic),
        v8scope: await directoryMetrics(seeds.v8scope),
      },
      footprint,
      summary: {
        startup: summarize(startup),
        reportRebuild: summarize(reportRebuild),
      },
      samples: { startup, reportRebuild },
      captureLogs: seeds.logs,
    };
    await writeFile(join(options.results, "control-plane.json"), `${JSON.stringify(result, null, 2)}\n`);
    await writeFile(join(options.results, "control-plane.md"), markdown(result));
    process.stdout.write(`wrote ${options.results}\n`);
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  process.stderr.write(`${error.stack ?? error}\n`);
  process.exitCode = 1;
});
