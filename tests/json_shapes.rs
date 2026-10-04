//! The `--json` surface, exercised end to end against the real binary.
//!
//! The unit tests beside each document builder hold the exact keys
//! without running anything; these run `kuma` so the wiring between
//! the two is tested as well: the flag reaches the verb, one JSON
//! document reaches stdout, and a failure is the one failure document
//! rather than silence. What is here is what a sandbox can reach: the
//! verbs whose documents do not depend on a bootc deployment, a
//! snapshot store, or a registry round trip. The fixture declaration
//! is `schema_version = 1` alone, which validates, so nothing here
//! reads anything the test does not own.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static FIXTURES: AtomicUsize = AtomicUsize::new(0);

/// A declaration this test owns, unique per call: tests run in
/// parallel, and add and remove edit the file they are pointed at, so
/// sharing one would be a race two of them would lose.
fn fixture() -> String {
    let n = FIXTURES.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("kuma-shape-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the fixture directory is created");
    let path = dir.join("kuma.toml");
    std::fs::write(&path, "schema_version = 1\n").expect("the fixture declaration is written");
    path.display().to_string()
}

/// One JSON document on stdout, or the test names what came out
/// instead: prose in the middle of a document is the failure the
/// one-document promise exists to prevent.
fn kuma(args: &[&str]) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_kuma"))
        .args(args)
        .output()
        .expect("the test binary runs kuma");
    let stdout = String::from_utf8(out.stdout).expect("stdout is utf-8");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("`kuma {args:?}` stdout was not one JSON document ({e}): {stdout:?}")
    })
}

/// The exact keys a document carries. The map is a BTreeMap, so the
/// order is the alphabet's; the set is the promise.
fn shape(doc: &serde_json::Value, want: &[&str]) {
    let mut got: Vec<&str> =
        doc.as_object().expect("a JSON document").keys().map(String::as_str).collect();
    got.sort_unstable();
    let mut want = want.to_vec();
    want.sort_unstable();
    assert_eq!(got, want, "a verb's keys changed; the contract reads them");
}

/// The probe is the root resource: state, facts, and actions, whatever
/// state this machine turns out to be in.
#[test]
fn the_probe_is_the_root_resource() {
    let doc = kuma(&["--json"]);
    shape(&doc, &["state", "headline", "facts", "actions"]);
    shape(&doc["facts"], &["config", "image", "machine"]);
    for action in doc["actions"].as_array().expect("actions is an array") {
        shape(action, &["rel", "cmd", "why"]);
    }
}

#[test]
fn check_json_carries_the_verdict() {
    let fixture = fixture();
    let doc = kuma(&["check", "--json", "--config", &fixture]);
    shape(&doc, &["actions", "config", "declares", "ok", "valid"]);
    assert_eq!(doc["ok"], true);
    for action in doc["actions"].as_array().expect("actions is an array") {
        shape(action, &["rel", "cmd", "why"]);
    }
}

/// The one-document promise under the answer that hides best: a failing
/// verb that has already printed its own document. Two documents back
/// to back is the one shape no caller can parse, and it is exactly what
/// a `bail!` after a `println!` used to produce.
#[test]
fn a_broken_declaration_is_still_one_document() {
    let dir = std::env::temp_dir().join(format!("kuma-shape-broken-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the fixture directory is created");
    let path = dir.join("kuma.toml");
    std::fs::write(&path, "schema_version = 1\nno_such_key = true\n")
        .expect("the broken fixture is written");
    let doc = kuma(&["check", "--json", "--config", &path.display().to_string()]);
    shape(&doc, &["actions", "config", "error", "ok", "valid"]);
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["valid"], false);
    assert!(doc["error"].is_string());
}

#[test]
fn doctor_json_carries_findings_with_fixes() {
    let doc = kuma(&["doctor", "--json"]);
    // The verdict decides the shape: a failing doctor adds `error`
    // naming the counts, and this machine is whatever it is — a CI
    // runner grades unclean, a kuma deployment does not — so the shape
    // is read off the verdict, not assumed of the machine.
    let ok = doc["ok"].as_bool().expect("the verdict is a bool");
    let mut want = vec!["checks", "ok", "summary"];
    if !ok {
        want.push("error");
        assert!(doc["error"].is_string(), "a failing doctor names its counts");
    }
    shape(&doc, &want);
    shape(&doc["summary"], &["fails", "warns"]);
    let checks = doc["checks"].as_array().expect("checks is an array");
    assert!(!checks.is_empty(), "doctor always grades something");
    for check in checks {
        shape(check, &["detail", "fix", "grade", "name"]);
        let fix = &check["fix"];
        assert!(
            fix.is_null() || fix.as_object().is_some_and(|f| f.contains_key("cmd")),
            "a fix is an action or nothing, never prose: {fix}"
        );
    }
}

#[test]
fn diff_json_carries_drift_sections_and_actions() {
    let fixture = fixture();
    let doc = kuma(&["diff", "--json", "--config", &fixture]);
    shape(
        &doc,
        &[
            "actions",
            "adhoc_brews",
            "adhoc_flatpaks",
            "config",
            "drift",
            "image_declaration_stale",
            "sections",
        ],
    );
    for action in doc["actions"].as_array().expect("actions is an array") {
        shape(action, &["rel", "cmd", "why"]);
    }
}

#[test]
fn the_dry_runs_mark_themselves() {
    let fixture = fixture();
    for verb in [
        vec!["switch", "--json", "--config", &fixture],
        vec!["rollback", "--json", "--config", &fixture],
    ] {
        let doc = kuma(&verb);
        assert_eq!(doc["dry_run"], serde_json::Value::Bool(true), "`{verb:?}` is a preview");
        assert_eq!(doc["ok"], serde_json::Value::Bool(true));
    }
}

#[test]
fn switch_json_tells_the_truth_about_the_image() {
    let fixture = fixture();
    let doc = kuma(&["switch", "--json", "--config", &fixture]);
    shape(&doc, &["actions", "dry_run", "image_built", "ok", "tag"]);
}

#[test]
fn rollback_json_names_what_it_would_run() {
    let fixture = fixture();
    let doc = kuma(&["rollback", "--json", "--config", &fixture]);
    shape(&doc, &["actions", "dry_run", "ok", "would_run"]);
    assert_eq!(doc["would_run"], "bootc rollback");
}

#[test]
fn hibernate_json_answers_machine_readably() {
    let fixture = fixture();
    let doc = kuma(&["hibernate", "--json", "--config", &fixture]);
    // What this run can report depends on the machine running the test:
    // a machine whose root device can be named gets the swapfile report,
    // and a sandbox whose root is an overlay gets the one failure
    // document, because there is nowhere it could honestly propose
    // putting a swapfile. Both are the contract; the report's exact keys
    // are held by the unit tests beside the builder.
    if doc["ok"] == serde_json::Value::Bool(true) {
        shape(&doc, &["actions", "device", "dry_run", "ok", "repairing", "swap_mib", "warnings"]);
    } else {
        shape(&doc, &["ok", "error"]);
    }
}

#[test]
fn snapshot_json_lists_the_store() {
    let fixture = fixture();
    let doc = kuma(&["snapshot", "--json", "--config", &fixture]);
    shape(
        &doc,
        &["actions", "declared", "keep_daily", "keep_recent", "ok", "snapshots", "store", "target"],
    );
}

#[test]
fn backup_json_answers_without_touching_the_network() {
    let fixture = fixture();
    let doc = kuma(&["backup", "--json", "--config", &fixture]);
    shape(
        &doc,
        &[
            "actions",
            "covers",
            "declared",
            "interval",
            "last_completed",
            "network_connections",
            "ok",
            "provisioned",
            "repo",
            "secret",
            "secret_path",
        ],
    );
}

#[test]
fn capture_json_is_one_document_whatever_the_machine_runs() {
    let fixture = fixture();
    let doc = kuma(&["capture", "--json", "--config", &fixture]);
    // Which of capture's three documents this is depends on what the
    // machine runs; all three carry ok and written, and the exact key
    // sets are held by the unit tests beside their builders.
    assert_eq!(doc["ok"], serde_json::Value::Bool(true));
    assert!(doc.get("written").is_some());
}

#[test]
fn add_json_reports_the_declaration_edit() {
    let fixture = fixture();
    let doc = kuma(&["add", "--json", "--rpm", "shape-test-package", "--config", &fixture]);
    shape(&doc, &["actions", "already_declared", "declared", "list", "note", "ok"]);
    for action in doc["actions"].as_array().expect("actions is an array") {
        shape(action, &["rel", "cmd", "why"]);
    }
}

#[test]
fn remove_json_reports_the_declaration_edit() {
    let fixture = fixture();
    // Adding first so the removal has something to remove: the two
    // verbs are one edit's two directions, and the fixture is the
    // test's own file, not a machine's.
    kuma(&["add", "--json", "--rpm", "shape-test-package", "--config", &fixture]);
    let doc = kuma(&["remove", "--json", "shape-test-package", "--config", &fixture]);
    shape(&doc, &["actions", "note", "ok", "removed"]);
}

#[test]
fn a_failure_is_one_document_with_an_error() {
    let fixture = fixture();
    let failures = [
        // A mutating verb whose declaration cannot be read.
        vec!["add", "--json", "--rpm", "x", "--config", "/nonexistent/kuma.toml"],
        // A read verb whose declaration cannot be read.
        vec!["diff", "--json", "--config", "/nonexistent/kuma.toml"],
        // A read verb refusing its argument.
        vec!["snapshot", "--json", "--restore", "/etc/hostname", "--config", &fixture],
    ];
    for args in &failures {
        let out = Command::new(env!("CARGO_BIN_EXE_kuma"))
            .args(args)
            .output()
            .expect("the test binary runs kuma");
        assert!(!out.status.success(), "`{args:?}` should fail");
        let stdout = String::from_utf8(out.stdout).expect("stdout is utf-8");
        let doc: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!("a failing `kuma {args:?}` left stdout unparsable ({e}): {stdout:?}")
        });
        shape(&doc, &["ok", "error"]);
        assert_eq!(doc["ok"], serde_json::Value::Bool(false));
    }
}
