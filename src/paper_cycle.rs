//! Persistent paper controls mirror live semantics without any signing authority.
use crate::{ApiResult, App, bad};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::json;
#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    pub mode: String,
    pub paused: bool,
    pub expires_at: u64,
    pub entry_usdc: u64,
}
pub fn settings(app: &App) -> Result<Settings, String> {
    let s = app.store.lock().map_err(|_| "store unavailable")?;
    settings_from_db(&s.db, app)
}
pub(crate) fn settings_from_db(db: &rusqlite::Connection, app: &App) -> Result<Settings, String> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS paper_cycle(id INTEGER PRIMARY KEY,data TEXT);")
        .map_err(|_| "paper control unavailable")?;
    use rusqlite::OptionalExtension;
    let value = db
        .query_row("SELECT data FROM paper_cycle WHERE id=1", [], |r| {
            r.get::<_, String>(0)
        })
        .optional()
        .map_err(|_| "paper control unavailable")?;
    match value {
        Some(v) => serde_json::from_str(&v).map_err(|_| "invalid paper control".into()),
        None => Ok(Settings {
            mode: if app.auto_paper { "full" } else { "manual" }.into(),
            paused: false,
            expires_at: u64::MAX,
            entry_usdc: app.strategy.entry_usdc,
        }),
    }
}
pub fn entry(app: &App) -> bool {
    settings(app).is_ok_and(|s| !s.paused && s.mode == "full" && s.expires_at > crate::now())
}
pub fn exit(app: &App) -> bool {
    settings(app).is_ok_and(|s| {
        !s.paused && ["semi", "full"].contains(&s.mode.as_str()) && s.expires_at > crate::now()
    })
}
pub fn amount(app: &App) -> u64 {
    settings(app).map_or(app.strategy.entry_usdc, |s| {
        s.entry_usdc.min(app.strategy.entry_usdc)
    })
}
pub async fn status(State(app): State<App>) -> ApiResult {
    Ok(Json(
        json!({"mode":"dry_run","lifecycle":settings(&app).map_err(bad)?,"virtual":true}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Control {
    action: String,
    args: Vec<String>,
}
pub async fn control(State(app): State<App>, Json(r): Json<Control>) -> ApiResult {
    if app.live_enabled {
        return Err(bad("paper controls require dry run"));
    }
    if r.action == "force-close" || r.action == "close-all" {
        let target = match (r.action.as_str(), r.args.as_slice()) {
            ("close-all", [c]) if c == "CONFIRM" => "all",
            ("force-close", [m, c]) if c == "CONFIRM" => m.as_str(),
            _ => return Err(bad("explicit CONFIRM required")),
        };
        let positions = {
            let mut s = app.store.lock().map_err(|_| bad("store unavailable"))?;
            if target == "all" {
                let mut ledger = s.ledger.clone();
                ledger.halted = true;
                crate::persist(&mut s, ledger).map_err(bad)?;
            }
            s.ledger
                .positions
                .iter()
                .filter(|(m, _)| target == "all" || m.as_str() == target)
                .map(|(m, p)| (m.clone(), p.amount))
                .collect::<Vec<_>>()
        };
        if positions.is_empty() && target != "all" {
            return Err(bad("position not tracked"));
        }
        let mut outcomes = vec![];
        for (mint, amount) in positions {
            let result = async {
                let q = crate::preview(
                    State(app.clone()),
                    Json(crate::QuoteRequest {
                        side: "sell".into(),
                        mint: mint.clone(),
                        amount,
                    }),
                )
                .await?
                .0;
                crate::order(
                    State(app.clone()),
                    Json(crate::OrderRequest {
                        quote_id: q["preview"]["id"]
                            .as_str()
                            .ok_or_else(|| bad("preview missing"))?
                            .into(),
                    }),
                )
                .await
                .map(|v| v.0)
            }
            .await;
            outcomes.push(json!({"mint":mint,"result":result.map_err(|(_,v)|v.0)}));
        }
        return Ok(Json(json!({"virtual":true,"outcomes":outcomes})));
    }
    if r.action == "force-entry" {
        let [mint] = r.args.as_slice() else {
            return Err(bad("force-entry requires one valid mint"));
        };
        if !soli_agent::valid_mint(mint) || mint == soli_agent::USDC {
            return Err(bad("force-entry requires one valid non-USDC mint"));
        }
        let state = settings(&app).map_err(bad)?;
        if state.paused {
            return Err(bad("paper lifecycle paused"));
        }
        let quote = crate::preview(
            State(app.clone()),
            Json(crate::QuoteRequest {
                side: "buy".into(),
                mint: mint.clone(),
                amount: state.entry_usdc.min(app.strategy.entry_usdc),
            }),
        )
        .await?;
        let preview = quote.0["preview"].clone();
        let quote_id = preview["id"]
            .as_str()
            .ok_or_else(|| bad("preview missing"))?
            .to_owned();
        let fill = crate::force_entry_order(app, quote_id).await?;
        return Ok(Json(
            json!({"virtual":true,"mode":"dry_run","entry_type":"force","signal_analysis_skipped":true,"token_safety_audit_performed":false,"preview":preview,"fill":fill.0["fill"]}),
        ));
    }
    let mut state = settings(&app).map_err(bad)?;
    match (r.action.as_str(), r.args.as_slice()) {
        ("mode", [mode, hours, amount]) if ["manual", "semi", "full"].contains(&mode.as_str()) => {
            let hours = hours.parse::<u64>().map_err(|_| bad("invalid hours"))?;
            let amount = amount.parse::<u64>().map_err(|_| bad("invalid USDC"))?;
            if !(1..=168).contains(&hours) || !(1..=100).contains(&amount) {
                return Err(bad("hours 1..168 and entry USDC 1..100 required"));
            }
            state.mode = mode.clone();
            state.expires_at = crate::now() + hours * 3600;
            state.entry_usdc = amount * 1_000_000;
        }
        ("pause" | "resume-cycle", [scope]) if scope == "all" => state.paused = r.action == "pause",
        ("pause" | "resume-cycle", [scope]) if scope == "entries" => {
            let mut s = app.store.lock().map_err(|_| bad("store unavailable"))?;
            let mut ledger = s.ledger.clone();
            ledger.halted = r.action == "pause";
            crate::persist(&mut s, ledger).map_err(bad)?;
        }
        _ => return Err(bad("unsupported paper control")),
    }
    let s = app.store.lock().map_err(|_| bad("store unavailable"))?;
    s.db.execute(
        "INSERT OR REPLACE INTO paper_cycle VALUES(1,?1)",
        [serde_json::to_string(&state).map_err(|_| bad("settings encoding failed"))?],
    )
    .map_err(|_| bad("paper control save failed"))?;
    Ok(Json(json!({"virtual":true,"lifecycle":state})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Store, research::Strategy};
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn app(paused: bool, live_enabled: bool) -> App {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE paper_cycle(id INTEGER PRIMARY KEY,data TEXT);")
            .unwrap();
        let settings = Settings {
            mode: "manual".into(),
            paused,
            expires_at: u64::MAX,
            entry_usdc: 1_000_000,
        };
        db.execute(
            "INSERT INTO paper_cycle VALUES(1,?1)",
            [serde_json::to_string(&settings).unwrap()],
        )
        .unwrap();
        App {
            smart_wallets: vec![],
            entry_trigger: "momentum".into(),
            data: None,
            strategy: Strategy::default(),
            watchlist: vec![],
            auto_paper: false,
            live_enabled,
            store: Arc::new(Mutex::new(Store {
                db,
                ledger: soli_agent::Ledger::new(100_000_000).unwrap(),
                previews: Default::default(),
            })),
            http: reqwest::Client::new(),
            token: String::new(),
            jupiter_key: None,
            fee: 0,
        }
    }

    fn error_text(result: ApiResult) -> String {
        result.unwrap_err().1.0["error"].as_str().unwrap().into()
    }

    #[tokio::test]
    async fn force_entry_respects_pause_and_dry_run_boundary() {
        let mint = "So11111111111111111111111111111111111111112".to_string();
        let paused = control(
            State(app(true, false)),
            Json(Control {
                action: "force-entry".into(),
                args: vec![mint.clone()],
            }),
        )
        .await;
        assert_eq!(error_text(paused), "paper lifecycle paused");

        let live = control(
            State(app(false, true)),
            Json(Control {
                action: "force-entry".into(),
                args: vec![mint],
            }),
        )
        .await;
        assert_eq!(error_text(live), "paper controls require dry run");
    }

    #[tokio::test]
    async fn force_fill_rechecks_pause_after_quote_creation() {
        let app = app(false, false);
        let quote = soli_agent::Preview {
            id: "fresh-quote".into(),
            side: "buy".into(),
            mint: "So11111111111111111111111111111111111111112".into(),
            input_amount: 1_000_000,
            quoted_output: 1_000,
            minimum_output: 900,
            estimated_fee_usdc: 0,
            created_at: crate::now(),
            price_impact_pct: 0.1,
        };
        {
            let mut store = app.store.lock().unwrap();
            store.previews.insert(quote.id.clone(), quote);
            let paused = Settings {
                mode: "manual".into(),
                paused: true,
                expires_at: u64::MAX,
                entry_usdc: 1_000_000,
            };
            store
                .db
                .execute(
                    "UPDATE paper_cycle SET data=?1 WHERE id=1",
                    [serde_json::to_string(&paused).unwrap()],
                )
                .unwrap();
        }
        let result = crate::force_entry_order(app.clone(), "fresh-quote".into()).await;
        assert_eq!(error_text(result), "paper lifecycle paused");
        assert!(app.store.lock().unwrap().ledger.fills.is_empty());
    }

    #[tokio::test]
    async fn force_entry_requests_a_fresh_quote_without_strategy_signal() {
        let result = control(
            State(app(false, false)),
            Json(Control {
                action: "force-entry".into(),
                args: vec!["So11111111111111111111111111111111111111112".into()],
            }),
        )
        .await;
        assert_eq!(error_text(result), "JUPITER_API_KEY is not configured");
    }
}
