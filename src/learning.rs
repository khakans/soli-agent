//! Measured feedback from completed forward trades, never fabricated backtest fills.
use crate::{ApiResult, App, bad, research::Strategy};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
#[derive(Default, Serialize, Deserialize)]
pub struct Evidence {
    pub source: String,
    pub closed: u64,
    pub wins: u64,
    pub net_usdc_atomic: i128,
    pub first: u64,
    pub last: u64,
    pub complete: bool,
}
fn candidate(e: &Evidence, s: &Strategy) -> Option<Strategy> {
    if !e.complete
        || e.closed < 10
        || e.last.saturating_sub(e.first) < 86400
        || e.net_usdc_atomic >= 0
    {
        return None;
    }
    let mut next = s.clone();
    next.entry_usdc = (s.entry_usdc / 2).max(1_000_000).min(s.entry_usdc);
    next.min_momentum_pct = (s.min_momentum_pct + 0.5)
        .min(s.max_momentum_pct - 0.1)
        .max(s.min_momentum_pct);
    if next.entry_usdc == s.entry_usdc && next.min_momentum_pct == s.min_momentum_pct {
        return None;
    }
    next.version = format!("learned-{}", crate::now());
    next.validate().ok()?;
    Some(next)
}
pub async fn review(app: &App, allow_apply: bool) -> Result<Value, String> {
    let evidence = if app.live_enabled {
        let r = app
            .http
            .get("http://127.0.0.1:8788/v1/live/status")
            .bearer_auth(&app.token)
            .send()
            .await
            .map_err(|_| "live evidence unavailable")?;
        if !r.status().is_success() {
            return Err("live evidence unavailable".into());
        }
        let v = r
            .json::<Value>()
            .await
            .map_err(|_| "invalid live evidence")?;
        serde_json::from_value::<Evidence>(v["learning_evidence"].clone())
            .map_err(|_| "live receipt evidence incomplete")?
    } else {
        let s = app.store.lock().map_err(|_| "store unavailable")?;
        let mut costs = BTreeMap::new();
        let mut e = Evidence {
            source: "forward_paper".into(),
            complete: true,
            ..Default::default()
        };
        for f in &s.ledger.fills {
            if e.first == 0 {
                e.first = f.timestamp;
            }
            e.last = f.timestamp;
            if f.side == "buy" {
                costs.insert(
                    f.mint.clone(),
                    f.input_amount as i128 + f.estimated_fee_usdc as i128,
                );
            } else if let Some(cost) = costs.remove(&f.mint) {
                let pnl = f.output_amount as i128 - f.estimated_fee_usdc as i128 - cost;
                e.closed += 1;
                e.wins += u64::from(pnl > 0);
                e.net_usdc_atomic += pnl;
            } else {
                e.complete = false;
            }
        }
        e
    };
    let next = candidate(&evidence, &app.strategy);
    let id = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&evidence).map_err(|_| "evidence encoding failed")?)
    );
    let auto =
        std::env::var("SOLI_LEARNING_AUTO_APPLY").as_deref() == Ok("true") && !app.live_enabled;
    let report = json!({"evidence":evidence,"strategy_hash":app.strategy.fingerprint(),"candidate":next,"automatic_apply_allowed":auto,"live_auto_apply":false,"policy":"at least ten closed forward trades and 24h; losing evidence reduces entry size and tightens momentum, never raises spend caps; candidate is not proof of better future performance"});
    if !allow_apply {
        return Ok(report);
    }
    let changed = {
        let s = app.store.lock().map_err(|_| "store unavailable")?;
        s.db.execute_batch(
            "CREATE TABLE IF NOT EXISTS learning_runs(id TEXT PRIMARY KEY,ts INTEGER,data TEXT);",
        )
        .map_err(|_| "learning schema failed")?;
        s.db.execute(
            "INSERT OR IGNORE INTO learning_runs VALUES(?1,?2,?3)",
            rusqlite::params![id, crate::now(), report.to_string()],
        )
        .map_err(|_| "learning save failed")?
    };
    if changed > 0
        && let Some(next) = next
    {
        let home = crate::operator::home().map_err(|_| "learning home unavailable")?;
        let file = home.join("learning-candidate.json");
        save(&file, &next)?;
        if auto {
            let active = std::env::var("SOLI_STRATEGY_FILE")
                .map_err(|_| "explicit strategy file required for learning auto-apply")?;
            save(std::path::Path::new(&active), &next)?;
            // Only an explicitly opted-in paper service restarts; journal is already durable.
            std::process::exit(75);
        }
    }
    Ok(report)
}
pub(crate) fn save(path: &std::path::Path, s: &Strategy) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("strategy parent missing")?;
    let mut f = tempfile::NamedTempFile::new_in(parent).map_err(|_| "strategy staging failed")?;
    f.write_all(&serde_json::to_vec_pretty(s).map_err(|_| "strategy encoding failed")?)
        .map_err(|_| "strategy staging failed")?;
    f.as_file().sync_all().map_err(|_| "strategy sync failed")?;
    f.persist(path).map_err(|_| "strategy replacement failed")?;
    Ok(())
}
pub async fn status(State(app): State<App>) -> ApiResult {
    review(&app, false).await.map(Json).map_err(bad)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn learning_requires_real_forward_evidence_and_only_reduces_entry_risk() {
        let s = Strategy::default();
        let mut e = Evidence {
            closed: 10,
            first: 1,
            last: 86401,
            complete: true,
            net_usdc_atomic: -100,
            source: "forward_paper".into(),
            ..Default::default()
        };
        let next = candidate(&e, &s).unwrap();
        assert!(next.entry_usdc <= s.entry_usdc);
        assert!(next.min_momentum_pct >= s.min_momentum_pct);
        assert_eq!(next.stop_loss_pct, s.stop_loss_pct);
        let mut small = s.clone();
        small.entry_usdc = 500_000;
        assert_eq!(candidate(&e, &small).unwrap().entry_usdc, 500_000);
        e.closed = 9;
        assert!(candidate(&e, &s).is_none());
        e.closed = 10;
        e.last = 10;
        assert!(candidate(&e, &s).is_none());
        e.last = 86401;
        e.complete = false;
        assert!(candidate(&e, &s).is_none());
    }
}
