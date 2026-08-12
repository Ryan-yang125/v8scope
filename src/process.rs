use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use serde::Serialize;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;

#[derive(Clone, Default)]
pub struct ProcessTracker {
    identities: Arc<Mutex<BTreeMap<u32, u64>>>,
    environment_marker: Option<OsString>,
    baseline: Arc<BTreeMap<u32, u64>>,
}

impl ProcessTracker {
    pub fn with_environment_marker(marker: impl Into<OsString>) -> Self {
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::All, true);
        Self {
            identities: Arc::default(),
            environment_marker: Some(marker.into()),
            baseline: Arc::new(
                system
                    .processes()
                    .iter()
                    .map(|(pid, process)| (pid.as_u32(), process.start_time()))
                    .collect(),
            ),
        }
    }

    fn record(&self, pid: u32, started_at_epoch_s: u64) {
        self.identities
            .lock()
            .expect("process tracker lock was poisoned")
            .insert(pid, started_at_epoch_s);
    }

    fn snapshot(&self) -> BTreeMap<u32, u64> {
        self.identities
            .lock()
            .expect("process tracker lock was poisoned")
            .clone()
    }
}

fn refresh_tree(system: &mut System, root_pid: u32, tracker: &ProcessTracker) -> BTreeSet<Pid> {
    system.refresh_processes(ProcessesToUpdate::All, true);
    if let Some(marker) = &tracker.environment_marker {
        let candidates = system
            .processes()
            .iter()
            .filter(|(pid, process)| {
                tracker.baseline.get(&pid.as_u32()) != Some(&process.start_time())
            })
            .map(|(pid, _)| *pid)
            .collect::<Vec<_>>();
        if !candidates.is_empty() {
            system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&candidates),
                false,
                ProcessRefreshKind::nothing().with_environ(UpdateKind::OnlyIfNotSet),
            );
        }
        for pid in candidates {
            if let Some(process) = system.process(pid)
                && process.environ().iter().any(|value| value == marker)
            {
                tracker.record(pid.as_u32(), process.start_time());
            }
        }
    }
    if let Some(root) = system.process(Pid::from_u32(root_pid)) {
        tracker.record(root_pid, root.start_time());
    }
    let mut tree = tracker
        .snapshot()
        .into_iter()
        .filter_map(|(pid, start_time)| {
            let pid = Pid::from_u32(pid);
            system
                .process(pid)
                .is_some_and(|process| process.start_time() == start_time)
                .then_some(pid)
        })
        .collect::<BTreeSet<_>>();
    let mut changed = true;
    while changed {
        changed = false;
        for (pid, process) in system.processes() {
            if process
                .parent()
                .is_some_and(|parent| tree.contains(&parent))
                && tree.insert(*pid)
            {
                tracker.record(pid.as_u32(), process.start_time());
                changed = true;
            }
        }
    }
    tree
}

#[derive(Debug, Serialize)]
struct ProcessSample {
    timestamp_ms: u64,
    pid: u32,
    parent_pid: Option<u32>,
    started_at_epoch_s: u64,
    cpu_percent: f32,
    rss_bytes: u64,
    virtual_bytes: u64,
    name: String,
}

pub async fn sample_tree(
    root_pid: u32,
    path: PathBuf,
    interval: Duration,
    mut stop: watch::Receiver<bool>,
    tracker: ProcessTracker,
) -> anyhow::Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
        .with_context(|| format!("failed to open {}", path.display()))?;
    let mut system = System::new();
    let started = Instant::now();
    let mut last_write = None;

    loop {
        let tree = refresh_tree(&mut system, root_pid, &tracker);

        if last_write.is_none_or(|last: Instant| last.elapsed() >= interval) {
            for pid in tree {
                if let Some(process) = system.process(pid) {
                    let sample = ProcessSample {
                        timestamp_ms: started.elapsed().as_millis() as u64,
                        pid: pid.as_u32(),
                        parent_pid: process.parent().map(Pid::as_u32),
                        started_at_epoch_s: process.start_time(),
                        cpu_percent: process.cpu_usage(),
                        rss_bytes: process.memory(),
                        virtual_bytes: process.virtual_memory(),
                        name: process.name().to_string_lossy().into_owned(),
                    };
                    let mut line = serde_json::to_vec(&sample)?;
                    line.push(b'\n');
                    file.write_all(&line).await?;
                }
            }
            file.flush().await?;
            last_write = Some(Instant::now());
        }

        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() { break; }
            }
            _ = tokio::time::sleep(Duration::from_millis(25)) => {}
        }
    }
    Ok(())
}

pub async fn monitor_tree(root_pid: u32, tracker: ProcessTracker, mut stop: watch::Receiver<bool>) {
    let mut system = System::new();
    loop {
        refresh_tree(&mut system, root_pid, &tracker);
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() { break; }
            }
            _ = tokio::time::sleep(Duration::from_millis(10)) => {}
        }
    }
}

pub fn record_telemetry_processes(
    telemetry_path: &Path,
    tracker: &ProcessTracker,
) -> anyhow::Result<()> {
    if !telemetry_path.is_file() {
        return Ok(());
    }
    let mut pids = BTreeSet::new();
    for line in BufReader::new(std::fs::File::open(telemetry_path)?).lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line?) else {
            continue;
        };
        if value.get("event").and_then(serde_json::Value::as_str) == Some("start")
            && let Some(pid) = value.get("pid").and_then(serde_json::Value::as_u64)
            && let Ok(pid) = u32::try_from(pid)
        {
            pids.insert(pid);
        }
    }
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    for pid in pids {
        if let Some(process) = system.process(Pid::from_u32(pid)) {
            tracker.record(pid, process.start_time());
        }
    }
    Ok(())
}

#[cfg(unix)]
pub async fn settle_tracked_processes(
    tracker: &ProcessTracker,
    root_pid: u32,
    grace: Duration,
) -> anyhow::Result<bool> {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid as NixPid;

    let refresh_and_alive = || {
        let mut system = System::new();
        refresh_tree(&mut system, root_pid, tracker);
        tracker
            .snapshot()
            .iter()
            .filter(|(pid, _)| **pid != root_pid)
            .filter_map(|(pid, start_time)| {
                let process = system.process(Pid::from_u32(*pid))?;
                (process.start_time() == *start_time).then_some(*pid)
            })
            .collect::<Vec<_>>()
    };

    let mut remaining = refresh_and_alive();
    let mut interrupted = BTreeSet::new();
    let deadline = tokio::time::Instant::now() + grace;
    let mut empty_checks = 0;
    while tokio::time::Instant::now() < deadline {
        for pid in &remaining {
            if !interrupted.insert(*pid) {
                continue;
            }
            match kill(NixPid::from_raw(*pid as i32), Signal::SIGINT) {
                Ok(()) | Err(Errno::ESRCH) => {}
                Err(error) => {
                    return Err(error).context("failed to interrupt tracked descendant");
                }
            }
        }
        if remaining.is_empty() {
            empty_checks += 1;
            if empty_checks >= 3 {
                break;
            }
        } else {
            empty_checks = 0;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
        remaining = refresh_and_alive();
    }
    let forced = !remaining.is_empty();
    for pid in &remaining {
        match kill(NixPid::from_raw(*pid as i32), Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(error) => return Err(error).context("failed to terminate tracked descendant"),
        }
    }
    if forced {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while !refresh_and_alive().is_empty() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    Ok(forced)
}

#[cfg(windows)]
pub async fn settle_tracked_processes(
    _tracker: &ProcessTracker,
    _root_pid: u32,
    _grace: Duration,
) -> anyhow::Result<bool> {
    Ok(false)
}

#[cfg(unix)]
pub fn interrupt_group(root_pid: u32) -> anyhow::Result<()> {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid as NixPid;
    killpg(NixPid::from_raw(root_pid as i32), Signal::SIGINT)
        .context("failed to send SIGINT to target process group")
}

#[cfg(unix)]
pub async fn settle_group(root_pid: u32, grace: Duration) -> anyhow::Result<bool> {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid as NixPid;

    let group = NixPid::from_raw(root_pid as i32);
    let alive = || match killpg(group, None) {
        Ok(()) | Err(Errno::EPERM) => true,
        Err(Errno::ESRCH) => false,
        Err(_) => true,
    };
    if !alive() {
        return Ok(false);
    }
    let _ = killpg(group, Signal::SIGINT);
    let deadline = tokio::time::Instant::now() + grace;
    while alive() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let forced = alive();
    if forced {
        killpg(group, Signal::SIGKILL).context("failed to terminate remaining process group")?;
        let kill_deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while alive() && tokio::time::Instant::now() < kill_deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    Ok(forced)
}

#[cfg(windows)]
pub fn interrupt_group(root_pid: u32) -> anyhow::Result<()> {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
    let result = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, root_pid) };
    if result == 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to send Ctrl+Break to target process group");
    }
    Ok(())
}
