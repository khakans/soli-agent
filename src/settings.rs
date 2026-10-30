//! Validated owner settings. Live mandates remain bound to their strategy snapshot.
use crate::{ApiResult, App, bad, research::Strategy};
use axum::{Json, extract::State};
use serde::Deserialize;
use serde_json::json;
pub async fn read(State(app): State<App>) -> ApiResult {
    let notification =
        crate::notify::schedule(&app.store.lock().map_err(|_| bad("store unavailable"))?.db)
            .map_err(bad)?;
    Ok(Json(
        json!({"notifications":notification,"mode":if app.live_enabled{"live"}else{"dry_run"},"strategy":app.strategy,"strategy_hash":app.strategy.fingerprint(),"trigger":app.entry_trigger,"watch_mints":app.watchlist,"smart_wallets":app.smart_wallets,"live_changes":"maintenance and renewed owner mandate required"}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    field: String,
    value: String,
}
fn change(s: &mut Strategy, field: &str, value: &str) -> Result<(), String> {
    match field {
        "entry" => {
            let (w, f) = value
                .trim_end_matches("USDC")
                .split_once('.')
                .unwrap_or((value.trim_end_matches("USDC"), ""));
            if f.len() > 6 || !f.bytes().all(|c| c.is_ascii_digit()) {
                return Err("entry supports six decimals".into());
            }
            s.entry_usdc = w
                .parse::<u64>()
                .ok()
                .and_then(|v| v.checked_mul(1_000_000))
                .and_then(|v| {
                    format!("{f:0<6}")
                        .parse::<u64>()
                        .ok()
                        .and_then(|n| v.checked_add(n))
                })
                .filter(|n| (1_000_000..=100_000_000).contains(n))
                .ok_or("entry must be 1..100 USDC")?;
        }
        "stop-loss" => s.stop_loss_pct = value.parse().map_err(|_| "invalid stop loss")?,
        "take-profit" => s.take_profit_pct = value.parse().map_err(|_| "invalid take profit")?,
        "max-hold-hours" => {
            s.max_hold_secs = value
                .parse::<u64>()
                .ok()
                .filter(|v| (1..=168).contains(v))
                .and_then(|v| v.checked_mul(3600))
                .ok_or("holding hours must be 1..168")?
        }
        _ => return Err("settings: entry | stop-loss | take-profit | max-hold-hours".into()),
    }
    s.validate()
}
pub async fn update(State(app): State<App>, Json(r): Json<Update>) -> ApiResult {
    if r.field == "report-minutes" {
        let settings = crate::notify::set_schedule(
            &app.store.lock().map_err(|_| bad("store unavailable"))?.db,
            &r.value,
        )
        .map_err(bad)?;
        return Ok(Json(
            json!({"saved":true,"notifications":settings,"restart_required":false}),
        ));
    }
    if app.live_enabled {
        return Err(bad(
            "live settings require maintenance and a renewed owner mandate",
        ));
    }
    let path =
        std::env::var("SOLI_STRATEGY_FILE").map_err(|_| bad("explicit strategy path required"))?;
    let strategy = {
        let _guard = app.store.lock().map_err(|_| bad("store unavailable"))?;
        let mut s: Strategy = serde_json::from_slice(
            &std::fs::read(&path).map_err(|_| bad("strategy file unavailable"))?,
        )
        .map_err(|_| bad("invalid strategy file"))?;
        change(&mut s, &r.field, &r.value).map_err(bad)?;
        s.version = format!("owner-{}-{}", crate::now(), uuid::Uuid::new_v4().simple());
        crate::learning::save(std::path::Path::new(&path), &s).map_err(bad)?;
        s
    };
    crate::research::event(
        &app,
        "owner_settings",
        json!({"field":r.field,"strategy_hash":strategy.fingerprint()}),
    );
    let supervised = std::env::var("SOLI_SUPERVISED").as_deref() == Ok("true");
    if supervised {
        tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            std::process::exit(75)
        });
    }
    Ok(Json(
        json!({"saved":true,"virtual":true,"strategy":strategy,"reload_scheduled":supervised,"restart":if supervised{"native supervisor reloads settings; positions and journals preserved"}else{"restart the unsupervised engine to apply; positions and journals preserved"}}),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn setting_bounds_and_exact_units_reject_unsafe_values() {
        let mut s = Strategy::default();
        change(&mut s, "entry", "1.250001USDC").unwrap();
        assert_eq!(s.entry_usdc, 1_250_001);
        assert!(change(&mut s, "entry", "0.1").is_err());
        assert!(change(&mut s, "stop-loss", "NaN").is_err());
        assert!(change(&mut s, "max-hold-hours", "18446744073709551615").is_err());
        assert!(change(&mut s, "SOLI_PRIVATE_KEY", "anything").is_err());
    }
}
