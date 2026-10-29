//! Real isolated signer/Vault validation with a local RPC that cannot broadcast.
use axum::{Json, Router, routing::post};
use serde_json::{Value, json};
use std::{
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
struct Stop(Child);
impl Drop for Stop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
pub async fn signer(sealed: bool) -> Result<(), Box<dyn std::error::Error>> {
    let unexpected = Arc::new(AtomicUsize::new(0));
    let seen = unexpected.clone();
    let router = Router::new().route(
        "/",
        post(move |Json(r): Json<Value>| {
            let seen = seen.clone();
            async move {
                let result = match r["method"].as_str().unwrap_or("") {
                    "getGenesisHash" => json!("5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d"),
                    "getSlot" => json!(123),
                    "getBlockTime" => json!(crate::now()),
                    "getTokenAccountsByOwner" => json!({"value":[]}),
                    _ => {
                        seen.fetch_add(1, Ordering::SeqCst);
                        Value::Null
                    }
                };
                Json(json!({"jsonrpc":"2.0","id":1,"result":result}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let directory = tempfile::tempdir()?;
    let agent = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let owner = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?;
    let mut wallet = None;
    for _ in 0..if sealed { 1 } else { 2 } {
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.arg("signer")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (key, _) in std::env::vars().filter(|(k, _)| k.starts_with("SOLI_")) {
            cmd.env_remove(key);
        }
        cmd.env("SOLI_DRY_RUN", "false")
            .env("SOLI_API_TOKEN", &agent)
            .env("SOLI_OWNER_TOKEN", &owner)
            .env("SOLI_RPC_URL", format!("http://{address}/"))
            .env("JUPITER_API_KEY", "disposable-test")
            .env("VAULT_TRANSIT_KEY", "soli-validation")
            .env(
                "SOLI_STRATEGY_FILE",
                std::env::current_dir()?.join("config/strategy.json"),
            )
            .env("SOLI_SIGNER_DB", directory.path().join("live.db"))
            .env("SOLI_LIVE_MAX_POSITION_USDC", "1000000")
            .env("SOLI_LIVE_DAILY_SPEND_USDC", "5000000");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        let mut child = Stop(cmd.spawn()?);
        let mut status = None;
        for _ in 0..150 {
            if let Some(exit) = child.0.try_wait()? {
                if sealed && !exit.success() {
                    println!("PASS: live signer rejects sealed Vault");
                    server.abort();
                    return Ok(());
                }
                return Err("actual live signer exited unexpectedly".into());
            }
            if !sealed
                && let Ok(r) = http
                    .get("http://127.0.0.1:8788/v1/live/status")
                    .bearer_auth(&agent)
                    .send()
                    .await
                && r.status().is_success()
            {
                status = Some(r.json::<Value>().await?);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let status = status.ok_or("signer acceptance timed out")?;
        if status["mode"] != "live" || status["strategy_hash"].is_null() {
            return Err("unexpected signer mode or strategy".into());
        }
        if let Some(previous) = &wallet
            && previous != &status["wallet"]
        {
            return Err("journal restart changed wallet".into());
        }
        wallet = Some(status["wallet"].clone());
        if http
            .get("http://127.0.0.1:8788/v1/live/approval")
            .bearer_auth(&agent)
            .send()
            .await?
            .status()
            != 401
        {
            return Err("agent obtained owner access".into());
        }
        if !http
            .get("http://127.0.0.1:8788/v1/live/approval")
            .bearer_auth(&owner)
            .send()
            .await?
            .status()
            .is_success()
        {
            return Err("owner review unavailable".into());
        }
        drop(child);
    }
    server.abort();
    if unexpected.load(Ordering::SeqCst) != 0 {
        return Err("unexpected RPC or broadcast attempt".into());
    }
    println!(
        "PASS: actual dry_run=false signer, real Vault challenge, owner boundary and journal restart; no broadcast"
    );
    Ok(())
}
