//! Real native install/restart/HTTP scope acceptance; no external providers/funds.
use serde_json::{Value, json};
use std::{path::PathBuf, process::Command};
struct Cleanup(PathBuf);
#[test]
fn native_wallet_check_reads_private_env_without_live_startup() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path();
    std::fs::create_dir(home.join("bin")).unwrap();
    let binary = home.join("bin").join(if cfg!(windows) {
        "soli-agent.exe"
    } else {
        "soli-agent"
    });
    std::fs::copy(env!("CARGO_BIN_EXE_soli-agent"), binary).unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&[17; 32]);
    let wallet = bs58::encode(key.verifying_key().to_bytes()).into_string();
    let file = home.join(".env");
    std::fs::write(
        &file,
        format!(
            "SOLI_DRY_RUN=true\nSOLI_PRIVATE_KEY={}\nSOLI_EXPECTED_WALLET={wallet}\n",
            bs58::encode([17; 32]).into_string()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let result = Command::new(env!("CARGO_BIN_EXE_soli-agent"))
        .arg("wallet-check")
        .env("SOLI_HOME", home)
        .output()
        .unwrap();
    assert!(result.status.success(), "wallet check failed");
    let text = String::from_utf8(result.stdout).unwrap();
    assert!(text.contains(&wallet));
    assert!(!text.contains(&bs58::encode([17; 32]).into_string()));
    assert!(!home.join("signer.pid.json").exists());
}
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
    std::fs::create_dir_all(&home).unwrap();
    let env_file = home.join(".env");
    std::fs::write(&env_file, include_str!("../.env.example")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&env_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    command(&["install", "test"]);
    command(&["down"]);
    assert!(!home.join("engine.pid.json").exists());
    command(&["up"]);
    command(&["up"]); // Idempotent start never creates a duplicate engine.
    command(&["restart"]);
    command(&["doctor", "--agent", "test"]);
    let credential: Value =
        serde_json::from_slice(&std::fs::read(home.join("credentials/test.json")).unwrap())
            .unwrap();
    let active: std::collections::BTreeMap<String, String> = dotenvy::from_path_iter(&env_file)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(!active["SOLI_API_TOKEN"].starts_with("replace-"));
    let profile = serde_json::to_value(active).unwrap();
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
    // Model credentials can inspect controls, but cannot grant automation authority.
    command(&["agent-add", "test", "paper"]);
    assert_eq!(
        http.post("http://127.0.0.1:8787/v1/paper/orders")
            .bearer_auth(credential["token"].as_str().unwrap())
            .json(&json!({"quote_id":"none"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let control_url = "http://127.0.0.1:8787/v1/lifecycle/control";
    let mode = json!({"action":"mode","args":["semi","24","1"]});
    assert_eq!(
        http.post(control_url)
            .bearer_auth(credential["token"].as_str().unwrap())
            .json(&mode)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let operator = profile["SOLI_API_TOKEN"].as_str().unwrap();
    assert!(
        http.post(control_url)
            .bearer_auth(operator)
            .json(&json!({"action":"mode","args":["full","169","1"]}))
            .send()
            .await
            .unwrap()
            .status()
            .is_client_error()
    );
    assert!(
        http.post(control_url)
            .bearer_auth(operator)
            .json(&mode)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
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
    assert!(
        http.post(control_url)
            .bearer_auth(operator)
            .json(&json!({"action":"pause","args":["all"]}))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
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
    let cycle: Value = http
        .get("http://127.0.0.1:8787/v1/lifecycle")
        .bearer_auth(credential["token"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cycle["lifecycle"]["mode"], "semi");
    assert_eq!(cycle["lifecycle"]["paused"], true);
    assert_eq!(cycle["lifecycle"]["entry_usdc"], 1_000_000);
    // An owner settings change reloads the native child, preserving pause and ledger.
    let before: Value =
        serde_json::from_slice(&std::fs::read(home.join("engine.pid.json")).unwrap()).unwrap();
    let settings_url = "http://127.0.0.1:8787/v1/settings";
    let update = json!({"field":"stop-loss","value":"14"});
    assert_eq!(
        http.post(settings_url)
            .bearer_auth(credential["token"].as_str().unwrap())
            .json(&update)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert!(
        http.post(settings_url)
            .bearer_auth(operator)
            .json(&json!({"field":"stop-loss","value":"NaN"}))
            .send()
            .await
            .unwrap()
            .status()
            .is_client_error()
    );
    assert!(
        http.post(settings_url)
            .bearer_auth(operator)
            .json(&update)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let mut reloaded = false;
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if let Ok(r) = http.get(settings_url).bearer_auth(operator).send().await
            && let Ok(v) = r.json::<Value>().await
            && v["strategy"]["stop_loss_pct"] == 14.0
        {
            reloaded = true;
            break;
        }
    }
    assert!(reloaded, "supervisor failed to reload validated settings");
    let mut after: Value = before.clone();
    for _ in 0..50 {
        after =
            serde_json::from_slice(&std::fs::read(home.join("engine.pid.json")).unwrap()).unwrap();
        if before["child"] != after["child"] {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(before["wrapper"], after["wrapper"]);
    assert_ne!(before["child"], after["child"]);
    let preserved: Value = http
        .get("http://127.0.0.1:8787/v1/status")
        .bearer_auth(operator)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(preserved["ledger"]["halted"], true);
    assert_eq!(preserved["paper_lifecycle"]["paused"], true);
    assert!(
        http.post(control_url)
            .bearer_auth(operator)
            .json(&json!({"action":"close-all","args":[]}))
            .send()
            .await
            .unwrap()
            .status()
            .is_client_error()
    );
    assert!(
        http.post(control_url)
            .bearer_auth(operator)
            .json(&json!({"action":"resume-cycle","args":["all"]}))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
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
    command(&["down"]);
    assert!(
        http.get("http://127.0.0.1:8787/health")
            .send()
            .await
            .is_err(),
        "stop left an orphan engine"
    );
    drop(cleanup);
}
