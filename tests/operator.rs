//! Real native install/restart/HTTP scope acceptance; no external providers/funds.
use serde_json::{Value, json};
use std::{path::PathBuf, process::Command};
struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_soli-agent"))
            .args(["stop", "engine"])
            .env("SOLI_HOME", &self.0)
            .status();
    }
}
#[tokio::test]
async fn native_install_scopes_revocation_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("state");
    std::fs::create_dir(root.path().join("config")).unwrap();
    std::fs::write(
        root.path().join("config/strategy.json"),
        include_str!("../config/strategy.json"),
    )
    .unwrap();
    let command = |args: &[&str]| {
        let result = Command::new(env!("CARGO_BIN_EXE_soli-agent"))
            .args(args)
            .env("SOLI_HOME", &home)
            .env(
                "SOLI_OWNER_TOKEN",
                "disposable-parent-secret-must-not-reach-engine",
            )
            .env(
                "VAULT_TOKEN",
                "disposable-parent-vault-secret-must-not-reach-engine",
            )
            .current_dir(root.path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "native operator command failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    let cleanup = Cleanup(home.clone());
    command(&["install", "test"]);
    command(&["stop", "engine"]);
    assert!(!home.join("engine.pid.json").exists());
    command(&["start", "engine"]);
    command(&["doctor", "--agent", "test"]);
    let credential: Value =
        serde_json::from_slice(&std::fs::read(home.join("credentials/test.json")).unwrap())
            .unwrap();
    let profile: Value =
        serde_json::from_slice(&std::fs::read(home.join("engine.json")).unwrap()).unwrap();
    // Match the native MCP adapter's per-call HTTP connection, including after a forced restart.
    let http = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();
    let status: Value = http
        .get("http://127.0.0.1:8787/v1/status")
        .bearer_auth(credential["token"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["mode"], "dry_run");
    assert_eq!(
        http.post("http://127.0.0.1:8787/v1/paper/orders")
            .bearer_auth(credential["token"].as_str().unwrap())
            .json(&json!({"quote_id":"none"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let r: Value = http
        .get("http://127.0.0.1:8787/v1/readiness")
        .bearer_auth(credential["token"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["ready"], false);
    assert!(
        http.post("http://127.0.0.1:8787/v1/halt")
            .bearer_auth(profile["SOLI_API_TOKEN"].as_str().unwrap())
            .json(&json!({"halted":true}))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    command(&["stop", "engine"]);
    command(&["start", "engine"]);
    let r: Value = http
        .get("http://127.0.0.1:8787/v1/status")
        .bearer_auth(credential["token"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["ledger"]["halted"], true);
    command(&["agent-revoke", "test"]);
    assert_eq!(
        http.get("http://127.0.0.1:8787/v1/status")
            .bearer_auth(credential["token"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    drop(cleanup);
}
