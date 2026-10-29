//! Read-only provider acceptance and observed forward-paper evidence.
use crate::{ApiResult, App, bad, now};
use axum::{Json, extract::State};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use soli_agent::USDC;
use std::time::Duration;

pub async fn inspect(State(app): State<App>) -> ApiResult {
    let mut checks = serde_json::Map::new();
    if let Some(data) = &app.data {
        checks.insert(
            "rpc_fresh_mainnet".into(),
            outcome(data.check_fresh_chain().await),
        );
        let mint = "So11111111111111111111111111111111111111112";
        let routes = async {
            let buy = data.quote(USDC, mint, 1_000_000).await?;
            let amount = crate::data::amount_field(&buy, "outAmount")?;
            data.quote(mint, USDC, amount).await?;
            Ok::<(), String>(())
        }
        .await;
        checks.insert("jupiter_two_way_quotes".into(), outcome(routes));
        // Native wrapped SOL reports zero mint supply; it is a route probe,
        // not a valid holder-concentration fixture. BONK is a read-only probe;
        // base USDC is deliberately excluded from the engine's target-mint API.
        let coverage = data
            .risk("DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263")
            .await
            .map(|_| ());
        checks.insert("goplus_and_chain_response".into(), outcome(coverage));
    } else {
        checks.insert(
            "providers".into(),
            json!({"ok":false,"reason":"RPC/Jupiter not configured"}),
        );
    }
    let ws = async {
        let url =
            std::env::var("SOLI_WS_URL").map_err(|_| "SOLI_WS_URL not configured".to_string())?;
        let parsed = reqwest::Url::parse(&url).map_err(|_| "invalid WebSocket URL".to_string())?;
        if parsed.scheme() != "wss" {
            return Err("provider WebSocket requires WSS".into());
        }
        let (mut stream, _) = tokio_tungstenite::connect_async(&url)
            .await
            .map_err(|_| "WebSocket connection failed".to_string())?;
        stream
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"jsonrpc":"2.0","id":1,"method":"slotSubscribe"})
                    .to_string()
                    .into(),
            ))
            .await
            .map_err(|_| "WebSocket send failed".to_string())?;
        while let Some(message) = stream.next().await {
            if let Ok(tokio_tungstenite::tungstenite::Message::Text(text)) = message {
                let value: Value = serde_json::from_str(&text)
                    .map_err(|_| "invalid subscription response".to_string())?;
                if value["id"] == 1 {
                    if value["result"].as_u64().is_some() {
                        let _ = stream.close(None).await;
                        return Ok(());
                    }
                    return Err("WebSocket subscription rejected".into());
                }
            }
        }
        Err("WebSocket closed before subscription acknowledgement".into())
    };
    checks.insert(
        "websocket_subscription".into(),
        outcome(
            tokio::time::timeout(Duration::from_secs(15), ws)
                .await
                .unwrap_or(Err("WebSocket acceptance timed out".into())),
        ),
    );
    let evidence = performance(State(app.clone())).await?.0;
    let ready = checks.values().all(|v| v["ok"] == true);
    Ok(Json(
        json!({"checked_at":now(),"ready":ready,"scope":"read-only provider acceptance; no transaction broadcast or profitability claim","strategy_hash":app.strategy.fingerprint(),"checks":checks,"paper_evidence":evidence}),
    ))
}
fn outcome(result: Result<(), String>) -> Value {
    match result {
        Ok(()) => json!({"ok":true}),
        Err(reason) => json!({"ok":false,"reason":reason}),
    }
}
pub async fn performance(State(app): State<App>) -> ApiResult {
    let s = app.store.lock().map_err(|_| bad("store unavailable"))?;
    let (count, first, last): (u64, Option<u64>, Option<u64>) =
        s.db.query_row("SELECT COUNT(*),MIN(ts),MAX(ts) FROM samples", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(|_| bad("sample evidence unavailable"))?;
    let failed:u64=s.db.query_row("SELECT COUNT(*) FROM research_events WHERE kind IN ('tick_failed','mark_failed','exit_quote_unavailable')",[],|r|r.get(0)).unwrap_or(0);
    let fills = &s.ledger.fills;
    let fee: u128 = fills.iter().map(|f| f.estimated_fee_usdc as u128).sum();
    let elapsed = first.zip(last).map_or(0, |(a, b)| b.saturating_sub(a));
    let closed = fills.iter().filter(|f| f.side == "sell").count();
    let net_realized: i128 = fills
        .iter()
        .map(|f| {
            if f.side == "sell" {
                f.output_amount as i128 - f.estimated_fee_usdc as i128
            } else {
                -(f.input_amount as i128) - (f.estimated_fee_usdc as i128)
            }
        })
        .sum::<i128>()
        + s.ledger
            .positions
            .values()
            .map(|p| p.cost_usdc as i128)
            .sum::<i128>();
    Ok(Json(
        json!({"strategy_hash":app.strategy.fingerprint(),"sample_count":count,"first_sample_at":first,"last_sample_at":last,"observed_seconds":elapsed,
        "forward_paper_fills":fills.len(),"closed_round_trips":closed,"estimated_fees_usdc_atomic":fee.to_string(),"realized_pnl_usdc_atomic":net_realized.to_string(),
        "processing_failures":failed,"observation_threshold_met":elapsed>=86400 && count>=100 && closed>=10,
        "threshold":"at least 24h, 100 samples and 10 closed paper positions; sufficiency does not establish profitability",
        "note":"Forward fills use conservative executable quote thresholds and estimated costs. Open positions require fresh liquidation marks from /v1/report. No fabricated historical fills."}),
    ))
}
