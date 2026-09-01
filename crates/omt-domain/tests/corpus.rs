//! Behavioral corpus — Rust leg (plan U3).
//!
//! Loads every `corpus/scenarios/*.json` document (sorted by filename for a
//! deterministic order) and executes it against the pure Rust domain via
//! in-memory ports. The scenario files are the frozen specification: any
//! divergence FAILS. Run:
//!
//! ```bash
//! CARGO_HOME=$PWD/.cargo-home cargo test -p omt-domain --test corpus -- --nocapture
//! ```

mod common;

use common::{run_scenario, ScenarioSummary};
use serde_json::Value;
use std::path::PathBuf;

fn scenario_dir() -> PathBuf {
    // crates/omt-domain/tests → ../../corpus/scenarios
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/scenarios")
}

fn load_scenarios() -> Vec<(String, Value)> {
    let dir = scenario_dir();
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("scenario directory missing at {dir:?}: {error}"))
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let name = path.file_name()?.to_str()?.to_string();
            name.ends_with(".json").then_some(name)
        })
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no scenario documents found in {}",
        dir.display()
    );
    files
        .into_iter()
        .map(|file| {
            let path = dir.join(&file);
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("unreadable scenario {file}: {error}"));
            let doc = serde_json::from_str::<Value>(&text)
                .unwrap_or_else(|error| panic!("invalid JSON in {file}: {error}"));
            (file, doc)
        })
        .collect()
}

#[test]
fn scenario_inventory_is_non_trivial() {
    let files = load_scenarios();
    assert!(
        files.len() >= 40,
        "expected >= 40 scenarios, found {}",
        files.len()
    );
}

/// One driver iterating all scenarios in deterministic filename order,
/// printing a PASS line per scenario and failing with per-scenario counts.
#[test]
fn rust_leg_matches_frozen_corpus_zero_divergence() {
    let files = load_scenarios();
    let mut total_checks = 0usize;
    let mut failed: Vec<(String, usize, Vec<String>)> = Vec::new();

    for (file, doc) in &files {
        let summary: ScenarioSummary = run_scenario(doc);
        total_checks += summary.checks;
        if summary.ok {
            println!(
                "PASS {:<48} — {} ({} checks)",
                file, summary.name, summary.checks
            );
        } else {
            println!("FAIL {file} — {} ({} checks)", summary.name, summary.checks);
            for failure in &summary.failures {
                println!("      {failure}");
            }
            if std::env::var("OMT_DEBUG").is_ok() {
                if let Some(results) = &summary.debug_results {
                    for (index, result) in results.iter().enumerate() {
                        println!(
                            "      result[{index}] = {}",
                            serde_json::to_string(result).unwrap_or_default()
                        );
                    }
                }
            }
            failed.push((
                file.clone(),
                summary.failures.len(),
                summary.failures.clone(),
            ));
        }
    }

    println!();
    println!(
        "corpus: {} scenarios, {} invariant checks, {} failed scenario file(s)",
        files.len(),
        total_checks,
        failed.len()
    );
    assert!(
        failed.is_empty(),
        "{} divergence(s) across {} scenario file(s):\n{}",
        failed.iter().map(|(_, count, _)| count).sum::<usize>(),
        failed.len(),
        failed
            .iter()
            .map(|(file, count, _)| format!("  {file}: {count} failure(s)"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
