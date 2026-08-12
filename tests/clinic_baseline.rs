use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
struct UpstreamLock {
    repositories: Vec<RepositoryLock>,
}

#[derive(Deserialize)]
struct RepositoryLock {
    name: String,
    url: String,
    commit: String,
    tests: usize,
    inventory_sha256: String,
}

#[test]
fn every_upstream_clinic_test_has_a_v8scope_baseline() {
    let matrix = include_str!("clinic-baseline.tsv");
    let lock: UpstreamLock =
        serde_json::from_str(include_str!("clinic-upstream-lock.json")).unwrap();
    let coverage = include_str!("clinic-coverage.tsv")
        .lines()
        .skip(1)
        .map(|line| {
            let (target, tests) = line.split_once('\t').unwrap();
            assert!(!tests.is_empty(), "{target} has no executable Rust tests");
            (target, tests)
        })
        .collect::<HashMap<_, _>>();
    let mut identities = HashSet::new();
    let mut inventories: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let coverage_targets = HashSet::from([
        "async-causality-analysis",
        "cli-contract",
        "cpu-profile-analysis",
        "diagnostic-findings",
        "doctor-cpu",
        "e2e",
        "event-loop-analysis",
        "heap-profile-analysis",
        "offline-report",
        "run-lifecycle",
        "telemetry-contract",
    ]);
    for (index, row) in matrix.lines().skip(1).enumerate() {
        let columns = row.split('\t').collect::<Vec<_>>();
        assert_eq!(columns.len(), 3, "invalid matrix row {}", index + 2);
        assert!(
            identities.insert((columns[0], columns[1])),
            "duplicate upstream test {}",
            columns[1]
        );
        inventories.entry(columns[0]).or_default().push(columns[1]);
        assert!(
            coverage_targets.contains(columns[2]),
            "unknown V8Scope coverage target {} for {}",
            columns[2],
            columns[1],
        );
        assert!(
            coverage.contains_key(columns[2]),
            "coverage target {} has no concrete Rust tests",
            columns[2]
        );
    }
    assert_eq!(identities.len(), 141);
    assert_eq!(lock.repositories.len(), inventories.len());
    for repository in lock.repositories {
        assert!(repository.url.starts_with("https://github.com/clinicjs/"));
        assert_eq!(repository.commit.len(), 40);
        let mut paths = inventories.remove(repository.name.as_str()).unwrap();
        paths.sort_unstable();
        assert_eq!(paths.len(), repository.tests);
        let inventory = format!("{}\n", paths.join("\n"));
        assert_eq!(
            hex::encode(Sha256::digest(inventory.as_bytes())),
            repository.inventory_sha256,
            "pinned inventory changed for {}",
            repository.name
        );
    }
    assert!(inventories.is_empty());
}
