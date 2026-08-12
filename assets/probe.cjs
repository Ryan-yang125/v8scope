'use strict';

const fs = require('node:fs');
const process = require('node:process');
const {
  PerformanceObserver,
  monitorEventLoopDelay,
  performance,
} = require('node:perf_hooks');
const { threadId } = require('node:worker_threads');

const telemetryPath = process.env.V8SCOPE_TELEMETRY_PATH;
if (!telemetryPath) return;

const intervalMs = Math.max(10, Number(process.env.V8SCOPE_SAMPLE_INTERVAL_MS || 100));
const telemetryFd = fs.openSync(telemetryPath, 'a');
const started = process.hrtime.bigint();
let sequence = 0;
let finished = false;
let stopRequested = false;
let stopTimer = null;
let previousCpu = process.cpuUsage();
let previousSample = process.hrtime.bigint();
let previousElu = performance.eventLoopUtilization();

function timestampNs() {
  return Number(process.hrtime.bigint() - started);
}

function write(fd, value) {
  try {
    fs.writeSync(fd, `${JSON.stringify(value)}\n`);
  } catch {
    // Profiling must never crash the target process.
  }
}

function base(event) {
  return {
    event,
    sequence: sequence++,
    timestamp_ns: timestampNs(),
    pid: process.pid,
    thread_id: threadId,
  };
}

write(telemetryFd, {
  ...base('start'),
  node: process.version,
  v8: process.versions.v8,
});

const delayResolutionMs = Math.min(intervalMs, 10);
const delayResolutionNs = delayResolutionMs * 1e6;
const delay = monitorEventLoopDelay({ resolution: delayResolutionMs });
delay.enable();

const gcObserver = new PerformanceObserver((list) => {
  for (const entry of list.getEntries()) {
    write(telemetryFd, {
      ...base('gc'),
      duration_ms: entry.duration,
      kind: entry.detail && entry.detail.kind,
      flags: entry.detail && entry.detail.flags,
    });
  }
});
gcObserver.observe({ entryTypes: ['gc'] });

function countResources() {
  const counts = Object.create(null);
  if (typeof process.getActiveResourcesInfo !== 'function') return counts;
  for (const resource of process.getActiveResourcesInfo()) {
    counts[resource] = (counts[resource] || 0) + 1;
  }
  return counts;
}

function sample(final = false) {
  const now = process.hrtime.bigint();
  const elapsedUs = Math.max(1, Number(now - previousSample) / 1000);
  const cpu = process.cpuUsage(previousCpu);
  previousCpu = process.cpuUsage();
  previousSample = now;
  const currentElu = performance.eventLoopUtilization();
  const elu = performance.eventLoopUtilization(currentElu, previousElu);
  previousElu = currentElu;
  const memory = process.memoryUsage();

  write(telemetryFd, {
    ...base('sample'),
    final,
    cpu_percent: ((cpu.user + cpu.system) / elapsedUs) * 100,
    cpu_user_us: cpu.user,
    cpu_system_us: cpu.system,
    event_loop_utilization: elu.utilization,
    event_loop_active_ms: elu.active,
    event_loop_idle_ms: elu.idle,
    delay_p50_ns: adjustedDelay(delay.percentile(50)),
    delay_p95_ns: adjustedDelay(delay.percentile(95)),
    delay_p99_ns: adjustedDelay(delay.percentile(99)),
    delay_max_ns: adjustedDelay(delay.max),
    rss_bytes: memory.rss,
    heap_total_bytes: memory.heapTotal,
    heap_used_bytes: memory.heapUsed,
    external_bytes: memory.external,
    array_buffers_bytes: memory.arrayBuffers,
    active_resources: countResources(),
  });
  delay.reset();
}

function finite(value) {
  return Number.isFinite(value) ? value : 0;
}

function adjustedDelay(value) {
  return Math.max(0, finite(value) - delayResolutionNs);
}

const timer = setInterval(sample, intervalMs);
timer.unref();

let asyncState = null;
if (process.env.V8SCOPE_ASYNC_PATH) {
  asyncState = enableAsyncProbe(process.env.V8SCOPE_ASYNC_PATH);
}

function enableAsyncProbe(path) {
  const asyncHooks = require('node:async_hooks');
  const fd = fs.openSync(path, 'a');
  const maxEvents = Math.max(1, Number(process.env.V8SCOPE_ASYNC_MAX_EVENTS || 1000000));
  const buffer = [];
  const resources = new Map();
  const callbackStarted = new Map();
  let events = 0;
  let dropped = 0;
  let suppress = false;

  function push(value) {
    if (suppress) return;
    if (events >= maxEvents) {
      dropped++;
      return;
    }
    events++;
    buffer.push({ ...base(value.event), ...value });
  }

  function flush() {
    if (buffer.length === 0) return;
    suppress = true;
    try {
      fs.writeSync(fd, `${buffer.map((value) => JSON.stringify(value)).join('\n')}\n`);
      buffer.length = 0;
    } catch {
      dropped += buffer.length;
      buffer.length = 0;
    } finally {
      suppress = false;
    }
  }

  suppress = true;
  const flushTimer = setInterval(flush, 50);
  flushTimer.unref();
  suppress = false;

  const hook = asyncHooks.createHook({
    init(asyncId, type, triggerAsyncId) {
      if (suppress) return;
      resources.set(asyncId, { type, waitingSince: process.hrtime.bigint() });
      const stack = new Error().stack
        ?.split('\n')
        .slice(2, 10)
        .filter((line) => !line.includes('node:async_hooks'));
      push({ event: 'init', async_id: asyncId, trigger_async_id: triggerAsyncId, type, stack });
    },
    before(asyncId) {
      const resource = resources.get(asyncId);
      if (resource) {
        const started = process.hrtime.bigint();
        callbackStarted.set(asyncId, { started, waitNs: Number(started - resource.waitingSince) });
      }
    },
    after(asyncId) {
      const callback = callbackStarted.get(asyncId);
      if (callback) {
        callbackStarted.delete(asyncId);
        const finished = process.hrtime.bigint();
        const resource = resources.get(asyncId);
        if (resource) resource.waitingSince = finished;
        push({ event: 'callback', async_id: asyncId, wait_ns: callback.waitNs, duration_ns: Number(finished - callback.started) });
      }
    },
    destroy(asyncId) {
      const resource = resources.get(asyncId);
      if (resource) {
        resources.delete(asyncId);
        callbackStarted.delete(asyncId);
        push({ event: 'destroy', async_id: asyncId, type: resource.type });
      }
    },
    promiseResolve(asyncId) {
      if (resources.has(asyncId)) push({ event: 'promise_resolve', async_id: asyncId });
    },
  });
  hook.enable();

  return {
    finish() {
      hook.disable();
      clearInterval(flushTimer);
      flush();
      write(fd, { ...base('async_summary'), events, dropped, live_resources: resources.size });
    },
  };
}

function finish() {
  if (finished) return;
  finished = true;
  if (stopTimer) clearInterval(stopTimer);
  clearInterval(timer);
  sample(true);
  delay.disable();
  gcObserver.disconnect();
  if (asyncState) asyncState.finish();
  write(telemetryFd, { ...base('finish') });
}

process.once('beforeExit', finish);
process.once('exit', finish);

// CLI-triggered stops need an orderly Node exit so V8 can flush profile files.
// Application handlers retain ownership of shutdown and may finish async work.
const signalHandlers = new Map();
for (const signal of ['SIGINT', 'SIGTERM']) {
  const handler = () => {
    const applicationOwnsSignal = process.listeners(signal).some((listener) => listener !== handler);
    finish();
    if (applicationOwnsSignal) return;
    process.removeListener(signal, handler);
    process.kill(process.pid, signal);
  };
  signalHandlers.set(signal, handler);
  process.on(signal, handler);
}

function requestStop() {
  if (stopRequested) return;
  stopRequested = true;
  if (stopTimer) clearInterval(stopTimer);
  const signal = 'SIGINT';
  const handler = signalHandlers.get(signal);
  const applicationOwnsSignal = process.listeners(signal).some((listener) => listener !== handler);
  finish();
  if (applicationOwnsSignal) {
    process.emit(signal, signal);
    return;
  }
  process.exit(130);
}

const stopPath = process.env.V8SCOPE_STOP_PATH;
if (stopPath) {
  stopTimer = setInterval(() => {
    try {
      if (fs.existsSync(stopPath)) requestStop();
    } catch {
      // A failed control-file check must not crash the target process.
    }
  }, 25);
  stopTimer.unref();
}
