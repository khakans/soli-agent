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
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct PaperStrategyUpdate {
    pub min_liquidity_usd: Option<f64>,
    pub min_age_secs: Option<u64>,
    pub min_momentum_pct: Option<f64>,
    pub max_momentum_pct: Option<f64>,
    pub min_volume_ratio: Option<f64>,
    pub min_buy_sell_ratio: Option<f64>,
    pub evidence: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    field: String,
    value: String,
}
fn apply_paper_strategy(s: &mut Strategy, r: &PaperStrategyUpdate) -> Result<(), String> {
    if !(12..=500).contains(&r.evidence.trim().chars().count())
        || r.evidence.chars().any(char::is_control)
    {
        return Err("evidence must be 12..500 printable characters".into());
    }
    if [
        r.min_liquidity_usd.is_some(),
        r.min_age_secs.is_some(),
        r.min_momentum_pct.is_some(),
        r.max_momentum_pct.is_some(),
        r.min_volume_ratio.is_some(),
        r.min_buy_sell_ratio.is_some(),
    ]
    .iter()
    .all(|present| !present)
    {
        return Err("at least one paper strategy filter is required".into());
    }
    if let Some(v) = r.min_liquidity_usd {
        if !v.is_finite() || !(1_000.0..=10_000_000.0).contains(&v) {
            return Err("min_liquidity_usd must be 1000..10000000".into());
        }
        s.min_liquidity_usd = v;
    }
    if let Some(v) = r.min_age_secs {
        if !(60..=86_400).contains(&v) {
            return Err("min_age_secs must be 60..86400".into());
        }
        s.min_age_secs = v;
    }
    if let Some(v) = r.min_momentum_pct {
        if !v.is_finite() || !(0.1..=50.0).contains(&v) {
            return Err("min_momentum_pct must be 0.1..50".into());
        }
        s.min_momentum_pct = v;
    }
    if let Some(v) = r.max_momentum_pct {
        if !v.is_finite() || !(0.5..=100.0).contains(&v) {
            return Err("max_momentum_pct must be 0.5..100".into());
        }
        s.max_momentum_pct = v;
    }
    if let Some(v) = r.min_volume_ratio {
        if !v.is_finite() || !(1.0..=10.0).contains(&v) {
            return Err("min_volume_ratio must be 1..10".into());
        }
        s.min_volume_ratio = v;
    }
    if let Some(v) = r.min_buy_sell_ratio {
        if !v.is_finite() || !(1.0..=10.0).contains(&v) {
            return Err("min_buy_sell_ratio must be 1..10".into());
        }
        s.min_buy_sell_ratio = v;
    }
    s.validate()
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
pub async fn paper_strategy(
    State(app): State<App>,
    Json(r): Json<PaperStrategyUpdate>,
) -> ApiResult {
    if app.live_enabled {
        return Err(bad("paper strategy updates are disabled in live mode"));
    }
    let path =
        std::env::var("SOLI_STRATEGY_FILE").map_err(|_| bad("explicit strategy path required"))?;
    let strategy = {
        let _guard = app.store.lock().map_err(|_| bad("store unavailable"))?;
        let mut s: Strategy = serde_json::from_slice(
            &std::fs::read(&path).map_err(|_| bad("strategy file unavailable"))?,
        )
        .map_err(|_| bad("invalid strategy file"))?;
        let old_hash = s.fingerprint();
        apply_paper_strategy(&mut s, &r).map_err(bad)?;
        if old_hash == s.fingerprint() {
            return Ok(Json(
                json!({"saved":true,"changed":false,"virtual":true,"strategy":s,"strategy_hash":old_hash,"reload_scheduled":false}),
            ));
        }
        s.version = format!(
            "paper-adaptive-{}-{}",
            crate::now(),
            uuid::Uuid::new_v4().simple()
        );
        crate::learning::save(std::path::Path::new(&path), &s).map_err(bad)?;
        s
    };
    let fields = [
        r.min_liquidity_usd.map(|_| "min_liquidity_usd"),
        r.min_age_secs.map(|_| "min_age_secs"),
        r.min_momentum_pct.map(|_| "min_momentum_pct"),
        r.max_momentum_pct.map(|_| "max_momentum_pct"),
        r.min_volume_ratio.map(|_| "min_volume_ratio"),
        r.min_buy_sell_ratio.map(|_| "min_buy_sell_ratio"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let strategy_hash = strategy.fingerprint();
    crate::research::event(
        &app,
        "paper_strategy_update",
        json!({"fields":fields,"strategy_hash":strategy_hash,"rationale_source":"agent_supplied","evidence":r.evidence.trim()}),
    );
    let supervised = std::env::var("SOLI_SUPERVISED").as_deref() == Ok("true");
    if supervised {
        tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            std::process::exit(75)
        });
    }
    Ok(Json(
        json!({"saved":true,"changed":true,"virtual":true,"strategy":strategy,"strategy_hash":strategy_hash,"reload_scheduled":supervised,"restart_required":!supervised}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paper_strategy_fields_are_bounded_and_risk_fields_are_rejected() {
        let mut strategy = Strategy::default();
        let update = PaperStrategyUpdate {
            min_liquidity_usd: Some(15_000.0),
            min_age_secs: Some(300),
            min_momentum_pct: Some(0.5),
            max_momentum_pct: Some(35.0),
            min_volume_ratio: Some(1.1),
            min_buy_sell_ratio: Some(1.05),
            evidence: "recent paper sample supports wider entry filters".into(),
        };
        apply_paper_strategy(&mut strategy, &update).unwrap();
        assert_eq!(strategy.min_liquidity_usd, 15_000.0);
        assert_eq!(strategy.min_age_secs, 300);
        assert_eq!(strategy.min_momentum_pct, 0.5);
        assert_eq!(strategy.max_momentum_pct, 35.0);
        assert_eq!(strategy.min_volume_ratio, 1.1);
        assert_eq!(strategy.min_buy_sell_ratio, 1.05);
        let invalid = PaperStrategyUpdate {
            min_liquidity_usd: Some(999.0),
            ..update.clone()
        };
        assert!(apply_paper_strategy(&mut strategy, &invalid).is_err());
        let invalid = PaperStrategyUpdate {
            min_momentum_pct: Some(36.0),
            ..update.clone()
        };
        assert!(apply_paper_strategy(&mut strategy, &invalid).is_err());
        let invalid = PaperStrategyUpdate {
            min_volume_ratio: Some(0.99),
            ..update.clone()
        };
        assert!(apply_paper_strategy(&mut strategy, &invalid).is_err());
        let invalid = PaperStrategyUpdate {
            min_liquidity_usd: None,
            min_age_secs: None,
            min_momentum_pct: None,
            max_momentum_pct: None,
            min_volume_ratio: None,
            min_buy_sell_ratio: None,
            ..update
        };
        assert!(apply_paper_strategy(&mut strategy, &invalid).is_err());
        assert!(
            serde_json::from_value::<PaperStrategyUpdate>(json!({
                "stop_loss_pct":5,
                "evidence":"recent paper sample supports wider entry filters"
            }))
            .is_err()
        );
    }

    #[tokio::test]
    async fn paper_strategy_update_is_refused_when_engine_is_live() {
        let app = App {
            smart_wallets: vec![],
            entry_trigger: "momentum".into(),
            data: None,
            strategy: Strategy::default(),
            watchlist: vec![],
            auto_paper: false,
            live_enabled: true,
            store: std::sync::Arc::new(std::sync::Mutex::new(crate::Store {
                db: rusqlite::Connection::open_in_memory().unwrap(),
                ledger: soli_agent::Ledger::new(1_000_000).unwrap(),
                previews: Default::default(),
            })),
            http: reqwest::Client::new(),
            token: String::new(),
            jupiter_key: None,
            fee: 0,
        };
        let update = PaperStrategyUpdate {
            min_liquidity_usd: Some(15_000.0),
            min_age_secs: None,
            min_momentum_pct: None,
            max_momentum_pct: None,
            min_volume_ratio: None,
            min_buy_sell_ratio: None,
            evidence: "recent paper sample supports this filter change".into(),
        };
        assert!(paper_strategy(State(app), Json(update)).await.is_err());
    }

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
