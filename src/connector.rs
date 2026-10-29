//! Host-neutral trusted connector entry point; identity comes from the native host.
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use std::io::Read;
pub async fn command() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    std::io::stdin().take(8192).read_to_string(&mut input)?;
    let mut event: crate::bridge::Event = serde_json::from_str(&input)?;
    if !["slack", "telegram", "discord"].contains(&event.platform.as_str())
        || event.user_id.is_empty()
        || event.conversation_id.is_empty()
    {
        return Err("verified transport identity required".into());
    }
    event.event_id = uuid::Uuid::new_v4().to_string();
    let secrets: Value = serde_json::from_str(&std::env::var("SOLI_BRIDGE_SECRETS_JSON")?)?;
    let secret = secrets[&event.platform]
        .as_str()
        .filter(|s| s.len() >= 32)
        .ok_or("trusted connector signing key missing")?;
    let stamp = crate::now().to_string();
    let body = serde_json::to_vec(&event)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())?;
    mac.update(stamp.as_bytes());
    mac.update(b".");
    mac.update(&body);
    let response = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(180))
        .build()?
        .post("http://127.0.0.1:8787/v1/bridge/events")
        .header("content-type", "application/json")
        .header("x-soli-time", stamp)
        .header("x-soli-signature", hex::encode(mac.finalize().into_bytes()))
        .body(body)
        .send()
        .await
        .map_err(|_| "local engine unavailable")?;
    let result: Value = response
        .json()
        .await
        .map_err(|_| "invalid engine response")?;
    let expected = json!({"platform":event.platform,"conversation_id":event.conversation_id,"thread_id":event.thread_id});
    if result["route"] != expected {
        return Err("response routing mismatch; delivery suppressed".into());
    }
    println!("{}", result["result"]);
    Ok(())
}
