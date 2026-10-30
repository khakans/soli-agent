use crate::{
    ApiResult, App, bad,
    data::{Data, Sample, amount_field},
    now,
};
use axum::{
    Json,
    extract::{Path, State},
};
use futures_util::{SinkExt, StreamExt};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use soli_agent::USDC;
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Strategy {
    pub version: String,
    pub min_liquidity_usd: f64,
    pub min_age_secs: u64,
    pub min_momentum_pct: f64,
    pub max_momentum_pct: f64,
    pub min_volume_ratio: f64,
    pub min_buy_sell_ratio: f64,
    pub stop_loss_pct: f64,
    pub take_profit_pct: f64,
    pub max_hold_secs: u64,
    pub entry_usdc: u64,
    pub max_data_age_secs: u64,
}
impl Default for Strategy {
    fn default() -> Self {
        Self {
            version: "momentum-v1".into(),
            min_liquidity_usd: 100_000.0,
            min_age_secs: 3600,
            min_momentum_pct: 2.0,
            max_momentum_pct: 20.0,
            min_volume_ratio: 1.5,
            min_buy_sell_ratio: 1.2,
            stop_loss_pct: 15.0,
            take_profit_pct: 30.0,
            max_hold_secs: 21600,
            entry_usdc: 10_000_000,
            max_data_age_secs: 90,
        }
    }
}
impl Strategy {
    pub fn from_env() -> Result<Self, String> {
        let strategy: Self = match std::env::var("SOLI_STRATEGY_FILE")
            .ok()
            .filter(|p| !p.is_empty())
        {
            Some(path) => serde_json::from_slice(
                &std::fs::read(path).map_err(|_| "strategy file unavailable")?,
            )
            .map_err(|_| "invalid strategy JSON")?,
            None => Self::default(),
        };
        strategy.validate()?;
        Ok(strategy)
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.version.is_empty()
            || !self.min_liquidity_usd.is_finite()
            || self.min_liquidity_usd < 1000.0
            || !(0.0..100.0).contains(&self.stop_loss_pct)
            || !(0.0..1000.0).contains(&self.take_profit_pct)
            || self.entry_usdc == 0
            || self.max_data_age_secs > 300
            || self.max_data_age_secs == 0
            || self.min_momentum_pct < 0.0
            || self.max_momentum_pct < self.min_momentum_pct
            || !self.max_momentum_pct.is_finite()
            || !self.min_volume_ratio.is_finite()
            || self.min_volume_ratio < 1.0
            || !self.min_buy_sell_ratio.is_finite()
            || self.min_buy_sell_ratio < 1.0
        {
            return Err("invalid strategy bounds".into());
        }
        Ok(())
    }
    pub fn fingerprint(&self) -> String {
        format!("{:x}", Sha256::digest(serde_json::to_vec(self).unwrap()))
    }
    pub fn entry(&self, old: &Sample, new: &Sample, stamp: u64) -> bool {
        if old.mint != new.mint
            || stamp < new.timestamp
            || stamp - new.timestamp > self.max_data_age_secs
            || new.timestamp.saturating_sub(old.timestamp) < 240
            || new.timestamp - old.timestamp > 420
            || stamp.saturating_sub(new.pair_created_at) < self.min_age_secs
            || new.liquidity_usd < self.min_liquidity_usd
            || old.price_usd <= 0.0
            || old.volume_5m_usd <= 0.0
        {
            return false;
        }
        let momentum = (new.price_usd / old.price_usd - 1.0) * 100.0;
        momentum >= self.min_momentum_pct
            && momentum <= self.max_momentum_pct
            && new.volume_5m_usd / old.volume_5m_usd >= self.min_volume_ratio
            && new.buys_5m as f64 / (new.sells_5m.max(1) as f64) >= self.min_buy_sell_ratio
    }
    pub fn exit(&self, cost: u64, liquidation: u64, age: u64) -> Option<&'static str> {
        let pct = (liquidation as f64 / cost.max(1) as f64 - 1.0) * 100.0;
        if pct <= -self.stop_loss_pct {
            Some("stop_loss")
        } else if pct >= self.take_profit_pct {
            Some("take_profit")
        } else if age >= self.max_hold_secs {
            Some("time_exit")
        } else {
            None
        }
    }
}
pub fn init(app: &App) -> Result<(), String> {
    let s = app.store.lock().map_err(|_| "store unavailable")?;
    s.db.execute_batch("CREATE TABLE IF NOT EXISTS live_candidates(mint TEXT,strategy_hash TEXT,detected_at INTEGER,expires_at INTEGER,data TEXT,PRIMARY KEY(mint,strategy_hash));").map_err(|_|"live candidate schema failed")?;
    s.db.execute_batch("CREATE TABLE IF NOT EXISTS daily_reports(day INTEGER PRIMARY KEY,generated_at INTEGER,data TEXT NOT NULL);").map_err(|_|"daily report schema failed")?;
    s.db.execute_batch("CREATE TABLE IF NOT EXISTS samples(mint TEXT,ts INTEGER,data TEXT,PRIMARY KEY(mint,ts)); CREATE TABLE IF NOT EXISTS research_events(ts INTEGER,kind TEXT,data TEXT); CREATE TABLE IF NOT EXISTS equity(ts INTEGER PRIMARY KEY,cash INTEGER,liquidation INTEGER,equity INTEGER,complete INTEGER); CREATE TABLE IF NOT EXISTS strategy_versions(hash TEXT PRIMARY KEY,data TEXT);").map_err(|_|"research schema failed")?;
    s.db.execute(
        "INSERT OR IGNORE INTO strategy_versions VALUES(?1,?2)",
        params![
            app.strategy.fingerprint(),
            serde_json::to_string(&app.strategy).unwrap()
        ],
    )
    .map_err(|_| "strategy snapshot failed")?;
    Ok(())
}
fn save_live_candidate(
    db: &rusqlite::Connection,
    mint: &str,
    hash: &str,
    source: &str,
    amount: u64,
    stamp: u64,
    ttl: u64,
) -> Result<(), String> {
    let data = json!({"mint":mint,"strategy_hash":hash,"source":source,"amount_usdc_atomic":amount,"detected_at":stamp,"expires_at":stamp+ttl,"execution_requires_owner_approval":true,"next":"Request a fresh live buy preview, then review and approve its exact digest. A candidate is not an order."});
    let tx = db
        .unchecked_transaction()
        .map_err(|_| "candidate transaction unavailable")?;
    let changed=tx.execute("INSERT INTO live_candidates VALUES(?1,?2,?3,?4,?5) ON CONFLICT(mint,strategy_hash) DO UPDATE SET detected_at=excluded.detected_at,expires_at=excluded.expires_at,data=excluded.data WHERE live_candidates.expires_at<=?3",params![mint,hash,stamp,stamp+ttl,data.to_string()]).map_err(|_|"candidate persistence failed")?;
    if changed > 0 {
        crate::notify::enqueue(
            &tx,
            &format!("live-entry:{mint}:{hash}:{stamp}"),
            "live_entry_candidate",
            &data,
        )?;
    }
    tx.execute(
        "DELETE FROM live_candidates WHERE expires_at<?1",
        [stamp.saturating_sub(604800)],
    )
    .map_err(|_| "candidate cleanup failed")?;
    tx.commit().map_err(|_| "candidate commit failed".into())
}
pub fn live_candidate(app: &App, mint: &str, source: &str) -> Result<(), String> {
    if !app.live_enabled {
        return Ok(());
    }
    let s = app.store.lock().map_err(|_| "store unavailable")?;
    save_live_candidate(
        &s.db,
        mint,
        &app.strategy.fingerprint(),
        source,
        app.strategy.entry_usdc,
        now(),
        app.strategy.max_data_age_secs.clamp(1, 120),
    )
}
pub async fn live_candidates(State(app): State<App>) -> ApiResult {
    let s = app.store.lock().map_err(|_| bad("store unavailable"))?;
    let mut q=s.db.prepare("SELECT data FROM live_candidates WHERE expires_at>?1 AND strategy_hash=?2 ORDER BY detected_at DESC LIMIT 100").map_err(|_|bad("candidate queue unavailable"))?;
    let rows = q
        .query_map(params![now(), app.strategy.fingerprint()], |r| {
            r.get::<_, String>(0)
        })
        .map_err(|_| bad("candidate queue unavailable"))?;
    let mut candidates = vec![];
    for row in rows {
        candidates.push(
            serde_json::from_str::<Value>(&row.map_err(|_| bad("candidate read failed"))?)
                .map_err(|_| bad("invalid candidate record"))?,
        );
    }
    Ok(Json(
        json!({"mode":if app.live_enabled{"live"}else{"dry_run"},"candidates":candidates,"execution":"isolated signer; exact approval in manual/semi, expiring strategy/amount mandate in full","owner_authorization_required":true}),
    ))
}
pub fn event(app: &App, kind: &str, data: Value) {
    if let Ok(s) = app.store.lock() {
        let _ = s.db.execute(
            "INSERT INTO research_events VALUES(?1,?2,?3)",
            params![now(), kind, data.to_string()],
        );
        crate::notify::event(&s.db, kind, &data);
    }
}
fn save_sample(app: &App, sample: &Sample) -> Result<(), String> {
    let s = app.store.lock().map_err(|_| "store unavailable")?;
    s.db.execute(
        "INSERT OR IGNORE INTO samples VALUES(?1,?2,?3)",
        params![
            sample.mint,
            sample.timestamp,
            serde_json::to_string(sample).unwrap()
        ],
    )
    .map_err(|_| "sample write failed")?;
    Ok(())
}
fn old_sample(app: &App, mint: &str, ts: u64) -> Option<Sample> {
    let s = app.store.lock().ok()?;
    let text: String =
        s.db.query_row(
            "SELECT data FROM samples WHERE mint=?1 AND ts<=?2 ORDER BY ts DESC LIMIT 1",
            params![mint, ts.saturating_sub(300)],
            |r| r.get(0),
        )
        .ok()?;
    serde_json::from_str(&text).ok()
}

pub async fn risk(State(app): State<App>, Path(mint): Path<String>) -> ApiResult {
    let d = app
        .data
        .as_ref()
        .ok_or_else(|| bad("SOLI_RPC_URL not configured"))?;
    let r = d.risk(&mint).await.map_err(bad)?;
    event(&app, "risk", serde_json::to_value(&r).unwrap());
    Ok(Json(serde_json::to_value(r).unwrap()))
}
pub async fn history(State(app): State<App>, Path(mint): Path<String>) -> ApiResult {
    if !soli_agent::valid_mint(&mint) {
        return Err(bad("invalid mint"));
    }
    let s = app.store.lock().map_err(|_| bad("store unavailable"))?;
    let mut query =
        s.db.prepare("SELECT data FROM samples WHERE mint=?1 ORDER BY ts DESC LIMIT 500")
            .map_err(|_| bad("query failed"))?;
    let rows = query
        .query_map([mint], |r| r.get::<_, String>(0))
        .map_err(|_| bad("query failed"))?;
    let samples: Vec<Value> = rows
        .filter_map(Result::ok)
        .filter_map(|v| serde_json::from_str(&v).ok())
        .collect();
    Ok(Json(json!({"samples":samples})))
}
pub async fn replay(State(app): State<App>, Path(mint): Path<String>) -> ApiResult {
    let rows = history(State(app.clone()), Path(mint)).await?.0;
    let mut samples: Vec<Sample> = serde_json::from_value(rows["samples"].clone())
        .map_err(|_| bad("invalid sample history"))?;
    samples.reverse();
    let mut signals = vec![];
    for n in &samples {
        if let Some(old) = samples
            .iter()
            .rev()
            .find(|s| s.timestamp <= n.timestamp.saturating_sub(300))
            && app.strategy.entry(old, n, n.timestamp)
        {
            signals.push(n.timestamp);
        }
    }
    Ok(Json(
        json!({"strategy":app.strategy,"hash":app.strategy.fingerprint(),"signals":signals,"note":"historical signal replay only; no invented retrospective executable fills or profit"}),
    ))
}
pub async fn import(State(app): State<App>, Json(samples): Json<Vec<Sample>>) -> ApiResult {
    if samples.len() > 100 {
        return Err(bad("maximum 100 samples per import"));
    }
    for s in &samples {
        if !soli_agent::valid_mint(&s.mint)
            || s.timestamp > now()
            || s.timestamp == 0
            || !s.price_usd.is_finite()
            || s.price_usd <= 0.0
            || !s.liquidity_usd.is_finite()
            || s.liquidity_usd < 0.0
            || !s.volume_5m_usd.is_finite()
            || s.volume_5m_usd < 0.0
            || s.pair_created_at > s.timestamp
        {
            return Err(bad("invalid historical sample"));
        }
    }
    for s in &samples {
        let mut s = s.clone();
        s.source = format!("imported:{}", s.source);
        save_sample(&app, &s).map_err(bad)?;
    }
    Ok(Json(
        json!({"imported":samples.len(),"provenance":"user supplied; not independently verified"}),
    ))
}

pub async fn report(State(app): State<App>) -> ApiResult {
    let live_report = if app.live_enabled {
        match app
            .http
            .get("http://127.0.0.1:8788/v1/live/status")
            .bearer_auth(&app.token)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r
                .json::<Value>()
                .await
                .unwrap_or(json!({"error":"invalid signer response"})),
            _ => json!({"error":"isolated signer unavailable; live marks unverified"}),
        }
    } else {
        Value::Null
    };
    let store = app.store.lock().map_err(|_| bad("store unavailable"))?;
    let now = now();
    let start = now / 86400 * 86400;
    let latest = store
        .db
        .query_row(
            "SELECT ts,equity,complete FROM equity ORDER BY ts DESC LIMIT 1",
            [],
            |r| {
                Ok((
                    r.get::<_, u64>(0)?,
                    r.get::<_, u64>(1)?,
                    r.get::<_, bool>(2)?,
                ))
            },
        )
        .ok();
    let peak = store
        .db
        .query_row("SELECT MAX(equity) FROM equity WHERE complete=1", [], |r| {
            r.get::<_, Option<u64>>(0)
        })
        .ok()
        .flatten();
    let fees: u64 = store
        .ledger
        .fills
        .iter()
        .filter(|f| f.timestamp >= start)
        .map(|f| f.estimated_fee_usdc)
        .sum();
    let fills: Vec<_> = store
        .ledger
        .fills
        .iter()
        .filter(|f| f.timestamp >= start)
        .collect();
    let drawdown = latest.and_then(|(_, e, c)| {
        peak.filter(|p| *p > 0 && c)
            .map(|p| 100.0 * (1.0 - e as f64 / p as f64).max(0.0))
    });
    let mut query = store
        .db
        .prepare("SELECT ts,kind,data FROM research_events ORDER BY ts DESC LIMIT 20")
        .map_err(|_| bad("events query failed"))?;
    let events:Vec<Value>=query.query_map([],|r|Ok(json!({"timestamp":r.get::<_,u64>(0)?,"kind":r.get::<_,String>(1)?,"data":serde_json::from_str::<Value>(&r.get::<_,String>(2)?).unwrap_or(Value::Null)}))).map_err(|_|bad("events query failed"))?.filter_map(Result::ok).collect();
    let paper = json!({"date_utc":chrono::DateTime::from_timestamp(now as i64,0).unwrap().format("%Y-%m-%d").to_string(),"mode":if app.live_enabled{"live"}else{"dry_run"},"paper_cash_usdc_atomic":store.ledger.cash_usdc,"paper_realized_pnl_today_usdc_atomic":if store.ledger.day==now/86400{store.ledger.daily_realized_pnl_usdc}else{0},"paper_estimated_fees_today_usdc_atomic":fees,"paper_fills_today":fills,"latest_paper_equity":latest,"peak_to_current_drawdown_pct":drawdown,"strategy_hash":app.strategy.fingerprint(),"events":events,"warning":"equity is stale if timestamp is old or complete=false; daily boundary UTC"});
    let mut paper = paper;
    paper["live"] = live_report;
    Ok(Json(paper))
}
pub async fn daily(State(app): State<App>) -> ApiResult {
    let s = app.store.lock().map_err(|_| bad("store unavailable"))?;
    let mut q =
        s.db.prepare("SELECT data FROM daily_reports ORDER BY day DESC LIMIT 31")
            .map_err(|_| bad("report query failed"))?;
    let rows = q
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|_| bad("report query failed"))?
        .filter_map(Result::ok)
        .filter_map(|s| serde_json::from_str::<Value>(&s).ok())
        .collect::<Vec<_>>();
    Ok(Json(json!({"daily_reports":rows})))
}

pub async fn calibrate(State(app): State<App>) -> ApiResult {
    let data = app
        .data
        .as_ref()
        .ok_or_else(|| bad("RPC and Jupiter required"))?;
    let fees = data
        .rpc("getRecentPrioritizationFees", json!([]))
        .await
        .map_err(bad)?;
    let mut values: Vec<u64> = fees
        .as_array()
        .ok_or_else(|| bad("missing fee samples"))?
        .iter()
        .filter_map(|v| v["prioritizationFee"].as_u64())
        .collect();
    values.sort_unstable();
    let median = values.get(values.len() / 2).copied().unwrap_or(0);
    let rent = data
        .rpc("getMinimumBalanceForRentExemption", json!([165]))
        .await
        .map_err(bad)?
        .as_u64()
        .ok_or_else(|| bad("missing rent"))?;
    let lamports = 5000u64
        .saturating_add(median.saturating_mul(400_000).div_ceil(1_000_000))
        .saturating_add(rent.saturating_mul(2));
    let quote = data
        .quote(
            "So11111111111111111111111111111111111111112",
            USDC,
            1_000_000_000,
        )
        .await
        .map_err(bad)?;
    let sol_price = amount_field(&quote, "otherAmountThreshold").map_err(bad)?;
    let estimated = (u128::from(lamports) * u128::from(sol_price))
        .div_ceil(1_000_000_000)
        .min(1_000_000) as u64;
    let result = json!({"timestamp":now(),"estimated_usdc_atomic":estimated,"lamports_estimate":lamports,"median_priority_micro_lamports_per_cu":median,"compute_units_assumed":400000,"rent_accounts_assumed":2,"note":"conservative calibration estimate, not guaranteed transaction fee"});
    event(&app, "fee_calibration", result.clone());
    Ok(Json(result))
}
pub fn fee(app: &App) -> u64 {
    let recent=app.store.lock().ok().and_then(|s|s.db.query_row("SELECT data FROM research_events WHERE kind='fee_calibration' AND ts>=?1 ORDER BY ts DESC LIMIT 1",[now().saturating_sub(3600)],|r|r.get::<_,String>(0)).ok()).and_then(|s|serde_json::from_str::<Value>(&s).ok());
    recent
        .and_then(|v| v["estimated_usdc_atomic"].as_u64())
        .unwrap_or(app.fee)
}

pub fn start(app: App) {
    if let Some(data) = app.data.clone() {
        let worker = app.clone();
        let entry_data = data.clone();
        tokio::spawn(async move {
            let mut clock = tokio::time::interval(Duration::from_secs(30));
            loop {
                clock.tick().await;
                if let Err(reason) = tick(&worker, &entry_data).await {
                    event(&worker, "tick_failed", json!({"reason":reason}));
                }
            }
        });
        let mark_app = app.clone();
        let mark_data = data.clone();
        tokio::spawn(async move {
            let mut clock = tokio::time::interval(Duration::from_secs(15));
            let mut calibration_due = 0;
            loop {
                clock.tick().await;
                if now() >= calibration_due {
                    let _ = calibrate(State(mark_app.clone())).await;
                    calibration_due = now() + 3600;
                }
                if let Err(reason) = mark_and_exits(&mark_app, &mark_data).await {
                    event(&mark_app, "mark_failed", json!({"reason":reason}));
                }
                if let Ok(Json(summary)) = report(State(mark_app.clone())).await
                    && let Ok(s) = mark_app.store.lock()
                {
                    let _ = s.db.execute(
                        "INSERT OR REPLACE INTO daily_reports VALUES(?1,?2,?3)",
                        params![now() / 86400, now(), summary.to_string()],
                    );
                }
            }
        });
        if let Ok(ws) = std::env::var("SOLI_WS_URL") {
            let watched = app.watchlist.clone();
            tokio::spawn(async move {
                websocket(app, ws, watched).await;
            });
        }
    }
}
async fn tick(app: &App, data: &Data) -> Result<(), String> {
    let _ = crate::learning::review(app, true).await;
    if app.entry_trigger == "wallet_convergence" && !app.smart_wallets.is_empty() {
        crate::signals::tick(app, data).await?;
    }
    for mint in &app.watchlist {
        let sample = match data.sample(mint).await {
            Ok(s) => s,
            Err(e) => {
                event(app, "data_unavailable", json!({"mint":mint,"reason":e}));
                continue;
            }
        };
        let old = old_sample(app, mint, sample.timestamp);
        save_sample(app, &sample)?;
        if old
            .as_ref()
            .is_some_and(|old| app.strategy.entry(old, &sample, now()))
        {
            let risk = data.risk(mint).await?;
            event(
                app,
                "entry_signal",
                json!({"mint":mint,"accepted":risk.accepted,"risk":risk,"strategy_hash":app.strategy.fingerprint()}),
            );
            if risk.accepted && app.entry_trigger == "momentum" {
                live_candidate(app, mint, "momentum")?;
            }
            if risk.accepted
                && crate::paper_cycle::entry(app)
                && !app.live_enabled
                && app.entry_trigger == "momentum"
            {
                let q = super::preview(
                    State(app.clone()),
                    Json(super::QuoteRequest {
                        side: "buy".into(),
                        mint: mint.clone(),
                        amount: crate::paper_cycle::amount(app),
                    }),
                )
                .await;
                if let Ok(Json(q)) = q {
                    let id = q["preview"]["id"].as_str().ok_or("missing preview id")?;
                    let _ = super::order(
                        State(app.clone()),
                        Json(super::OrderRequest {
                            quote_id: id.into(),
                        }),
                    )
                    .await;
                }
            }
        }
    }
    Ok(())
}
async fn mark_and_exits(app: &App, data: &Data) -> Result<(), String> {
    if app.live_enabled {
        return Ok(());
    } // Live automation belongs to the isolated signer's owner-authorized worker.
    let ledger = {
        app.store
            .lock()
            .map_err(|_| "store unavailable")?
            .ledger
            .clone()
    };
    let mut liquidation = 0u64;
    let mut complete = true;
    for (mint, position) in &ledger.positions {
        match data.quote(mint, USDC, position.amount).await {
            Ok(q) => {
                let credit = amount_field(&q, "otherAmountThreshold")?.saturating_sub(fee(app));
                liquidation = liquidation.checked_add(credit).ok_or("equity overflow")?;
                let entry = ledger
                    .fills
                    .iter()
                    .rev()
                    .find(|f| f.mint == *mint && f.side == "buy")
                    .map_or(now(), |f| f.timestamp);
                if let Some(reason) =
                    app.strategy
                        .exit(position.cost_usdc, credit, now().saturating_sub(entry))
                {
                    event(
                        app,
                        "exit_signal",
                        json!({"mint":mint,"reason":reason,"liquidation_usdc":credit}),
                    );
                    if crate::paper_cycle::exit(app) {
                        let q = super::preview(
                            State(app.clone()),
                            Json(super::QuoteRequest {
                                side: "sell".into(),
                                mint: mint.clone(),
                                amount: position.amount,
                            }),
                        )
                        .await;
                        if let Ok(Json(q)) = q
                            && let Some(id) = q["preview"]["id"].as_str()
                        {
                            let _ = super::order(
                                State(app.clone()),
                                Json(super::OrderRequest {
                                    quote_id: id.into(),
                                }),
                            )
                            .await;
                        }
                    }
                }
            }
            Err(e) => {
                complete = false;
                event(
                    app,
                    "exit_quote_unavailable",
                    json!({"mint":mint,"reason":e}),
                );
            }
        }
    }
    let equity = ledger
        .cash_usdc
        .checked_add(liquidation)
        .ok_or("equity overflow")?;
    {
        let mut store = app.store.lock().map_err(|_| "store unavailable")?;
        // Do not mix quotes from a previous position set with a newly filled ledger.
        if serde_json::to_string(&store.ledger.fills).unwrap()
            == serde_json::to_string(&ledger.fills).unwrap()
        {
            store
                .db
                .execute(
                    "INSERT OR REPLACE INTO equity VALUES(?1,?2,?3,?4,?5)",
                    params![now(), ledger.cash_usdc, liquidation, equity, complete],
                )
                .map_err(|_| "equity write failed")?;
            let baseline: Option<u64> = store
                .db
                .query_row(
                    "SELECT equity FROM equity WHERE complete=1 AND ts>=?1 ORDER BY ts ASC LIMIT 1",
                    [now() / 86400 * 86400],
                    |r| r.get(0),
                )
                .ok();
            if complete
                && baseline.is_some_and(|b| equity.saturating_mul(100) < b.saturating_mul(97))
            {
                let mut next = store.ledger.clone();
                next.halted = true;
                super::persist(&mut store, next)?;
            }
        }
    }
    Ok(())
}
async fn websocket(app: App, url: String, mints: Vec<String>) {
    loop {
        match tokio_tungstenite::connect_async(&url).await {
            Ok((mut ws, _)) => {
                let mut subscriptions = std::collections::HashMap::<u64, String>::new();
                for (i, mint) in mints.iter().enumerate() {
                    let _=ws.send(tokio_tungstenite::tungstenite::Message::Text(json!({"jsonrpc":"2.0","id":i+1,"method":"accountSubscribe","params":[mint,{"encoding":"jsonParsed","commitment":"confirmed"}]}).to_string().into())).await;
                }
                for (i, wallet) in app.smart_wallets.iter().enumerate() {
                    let _=ws.send(tokio_tungstenite::tungstenite::Message::Text(json!({"jsonrpc":"2.0","id":1000+i,"method":"logsSubscribe","params":[{"mentions":[wallet]},{"commitment":"confirmed"}]}).to_string().into())).await;
                }
                let mut ping = tokio::time::interval(Duration::from_secs(30));
                loop {
                    tokio::select! {
                     _=ping.tick()=>{if ws.send(tokio_tungstenite::tungstenite::Message::Ping(vec![].into())).await.is_err(){break;}}
                     msg=ws.next()=>{match msg{Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t)))=>{if let Ok(v)=serde_json::from_str::<Value>(&t){
                        if let (Some(id),Some(subscription))=(v["id"].as_u64(),v["result"].as_u64())&&id>=1000&&let Some(wallet)=app.smart_wallets.get((id-1000)as usize){subscriptions.insert(subscription,wallet.clone());}
                        if v["method"]=="logsNotification"&&v["params"]["result"]["value"]["err"].is_null()&&let Some(sub)=v["params"]["subscription"].as_u64()&&let Some(wallet)=subscriptions.get(&sub)&&let Some(sig)=v["params"]["result"]["value"]["signature"].as_str()&&let Ok(s)=app.store.lock(){let _=s.db.execute("INSERT OR IGNORE INTO wallet_queue VALUES(?1,?2,?3)",params![wallet,sig,now()]);}
                        event(&app,"websocket",json!({"received_at":now(),"message":v}));
                     }},Some(Ok(_))=>{},_=>break}}
                    }
                }
            }
            Err(_) => event(&app, "websocket_disconnected", json!({"retry_seconds":15})),
        }
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn live_candidate_queue_is_durable_deduplicated_and_expires() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("candidates.db");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE live_candidates(mint TEXT,strategy_hash TEXT,detected_at INTEGER,expires_at INTEGER,data TEXT,PRIMARY KEY(mint,strategy_hash));").unwrap();
        save_live_candidate(&db, "mint", "hash", "momentum", 1000000, 100, 30).unwrap();
        save_live_candidate(&db, "mint", "hash", "momentum", 9999999, 101, 30).unwrap();
        let text: String = db
            .query_row("SELECT data FROM live_candidates", [], |r| r.get(0))
            .unwrap();
        let data: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(data["amount_usdc_atomic"], 1000000);
        drop(db);
        let db = rusqlite::Connection::open(path).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM live_candidates WHERE expires_at>131",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
            0
        );
        save_live_candidate(&db, "mint", "hash", "momentum", 2000000, 132, 30).unwrap();
        assert_eq!(
            db.query_row("SELECT detected_at FROM live_candidates", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            132
        );
    }
    use super::*;
    fn s(ts: u64, price: f64, vol: f64) -> Sample {
        Sample {
            mint: "m".into(),
            timestamp: ts,
            price_usd: price,
            liquidity_usd: 200000.0,
            volume_5m_usd: vol,
            buys_5m: 20,
            sells_5m: 10,
            pair_created_at: 1,
            source: "test".into(),
        }
    }
    #[test]
    fn versioned_strategy_rejects_stale_and_enforces_exits() {
        let p = Strategy::default();
        p.validate().unwrap();
        assert!(p.entry(&s(4000, 1.0, 100.0), &s(4300, 1.05, 200.0), 4301));
        assert!(!p.entry(&s(4000, 1.0, 100.0), &s(4300, 1.05, 200.0), 4400));
        assert_eq!(p.exit(100, 80, 0), Some("stop_loss"));
        assert_eq!(p.exit(100, 140, 0), Some("take_profit"));
        assert_eq!(p.exit(100, 100, 21600), Some("time_exit"));
        assert_eq!(p.fingerprint(), p.fingerprint());
    }
}
