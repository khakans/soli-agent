//! Host-neutral trusted connector entry point; identity comes from the native host.
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use std::io::Read;
pub async fn command() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    std::io::stdin().take(8192).read_to_string(&mut input)?;
    let mut event: crate::bridge::Event = serde_json::from_str(&input)?;
    simplify(&mut event)?;
    if ![
        "slack",
        "telegram",
        "discord",
        "whatsapp",
        "whatsapp_cloud",
        "agent",
    ]
    .contains(&event.platform.as_str())
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

fn simplify(e: &mut crate::bridge::Event) -> Result<(), &'static str> {
    match (e.command.as_str(), e.arguments.as_slice()) {
        ("pause" | "resume-cycle", []) => e.arguments.push("all".into()),
        ("mode", [mode]) if ["semi", "full", "manual"].contains(&mode.as_str()) => {
            e.arguments.extend(["24".into(), "1".into()]);
        }
        ("close", [mint, confirm]) if confirm == "CONFIRM" => {
            e.command = "force-close".into();
            e.arguments = vec![mint.clone(), confirm.clone()];
        }
        ("preview", [side, _, amount]) if side == "buy" && amount.ends_with("USDC") => {
            let number = amount.trim_end_matches("USDC");
            let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
            if fraction.len() > 6 || !fraction.bytes().all(|c| c.is_ascii_digit()) {
                return Err("USDC supports at most six decimal places");
            }
            let units = whole
                .parse::<u64>()
                .ok()
                .and_then(|v| v.checked_mul(1_000_000))
                .and_then(|v| {
                    format!("{fraction:0<6}")
                        .parse::<u64>()
                        .ok()
                        .and_then(|f| v.checked_add(f))
                })
                .filter(|v| *v > 0)
                .ok_or("invalid USDC amount")?;
            e.arguments[2] = units.to_string();
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn friendly_commands_preserve_exact_amounts_and_confirmation() {
        let mut e = crate::bridge::Event {
            event_id: "test".into(),
            platform: "agent".into(),
            user_id: "owner".into(),
            conversation_id: "chat".into(),
            thread_id: None,
            command: "preview".into(),
            arguments: vec!["buy".into(), "mint".into(), "1.000001USDC".into()],
        };
        simplify(&mut e).unwrap();
        assert_eq!(e.arguments[2], "1000001");
        e.arguments[2] = "1.0000001USDC".into();
        assert!(simplify(&mut e).is_err());
        e.command = "mode".into();
        e.arguments = vec!["full".into()];
        simplify(&mut e).unwrap();
        assert_eq!(e.arguments, ["full", "24", "1"]);
        e.command = "close".into();
        e.arguments = vec!["all".into()];
        simplify(&mut e).unwrap();
        assert_eq!(e.command, "close");
    }
}
