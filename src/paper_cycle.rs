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
    s.db.execute_batch("CREATE TABLE IF NOT EXISTS paper_cycle(id INTEGER PRIMARY KEY,data TEXT);")
        .map_err(|_| "paper control unavailable")?;
    use rusqlite::OptionalExtension;
    let value =
        s.db.query_row("SELECT data FROM paper_cycle WHERE id=1", [], |r| {
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
