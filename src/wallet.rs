//! Public wallet inspection. This module never loads private keys.
use crate::{ApiResult, App, bad};
use axum::{
    Json,
    extract::{Path, State},
};
use serde_json::{Value, json};
pub async fn status(State(app): State<App>) -> ApiResult {
    let address = std::env::var("SOLI_WALLET_ADDRESS")
        .map_err(|_| bad("configure public SOLI_WALLET_ADDRESS"))?;
    if !soli_agent::valid_mint(&address) {
        return Err(bad("invalid public wallet address"));
    }
    let data = app.data.as_ref().ok_or_else(|| bad("RPC not configured"))?;
    data.check_fresh_chain().await.map_err(bad)?;
    let native = data
        .rpc("getBalance", json!([address,{"commitment":"confirmed"}]))
        .await
        .map_err(bad)?;
    let mut positions = vec![];
    for program in [crate::data::TOKEN, crate::data::TOKEN_2022] {
        let result=data.rpc("getTokenAccountsByOwner",json!([address,{"programId":program},{"encoding":"jsonParsed","commitment":"confirmed"}])).await.map_err(bad)?;
        for a in result["value"]
            .as_array()
            .ok_or_else(|| bad("token accounts unavailable"))?
        {
            let info = &a["account"]["data"]["parsed"]["info"];
            if info["tokenAmount"]["amount"]
                .as_str()
                .is_some_and(|n| n != "0")
            {
                positions.push(json!({"token_account":a["pubkey"],"mint":info["mint"],"balance":info["tokenAmount"],"program":program,"execution_supported":program==crate::data::TOKEN}));
            }
        }
    }
    let live = if app.live_enabled {
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
                .unwrap_or(json!({"error":"invalid live status"})),
            _ => json!({"error":"live signer unavailable","pnl":null}),
        }
    } else {
        json!({"mode":"dry_run","pnl":null,"note":"real-wallet PnL unavailable before live receipt history; paper ledger is separate"})
    };
    let paper = {
        let store = app.store.lock().map_err(|_| bad("store unavailable"))?;
        json!({"virtual":true,"cash_usdc_atomic":store.ledger.cash_usdc,"positions":store.ledger.positions,"realized_pnl_today_usdc_atomic":if store.ledger.day==crate::now()/86400{store.ledger.daily_realized_pnl_usdc}else{0}})
    };
    Ok(Json(
        json!({"wallet":address,"balance_sol_lamports":native["value"],"token_accounts":positions,"live":live,"paper":paper,"checked_at":crate::now()}),
    ))
}
pub async fn position(State(app): State<App>, Path(mint): Path<String>) -> ApiResult {
    if !soli_agent::valid_mint(&mint) {
        return Err(bad("invalid mint"));
    }
    let data = status(State(app)).await?.0;
    let holdings = data["token_accounts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["mint"] == mint)
        .cloned()
        .collect::<Vec<_>>();
    Ok(Json(
        json!({"wallet":data["wallet"],"mint":mint,"holdings":holdings,"live_details":data["live"]["marks"]["positions"].as_array().map(|rows|rows.iter().filter(|p|p["mint"]==mint).cloned().collect::<Vec<_>>()),"pnl":data["live"]["pnl"]}),
    ))
}
pub const MENU: &str = "Soli commands\n/soli-menu — menu\n/soli wallet — real wallet balance, positions and PnL\n/soli positions | pnl\n/soli position MINT — details\n/soli candidates — live signals\n/soli mode semi|full|manual HOURS ENTRY_USDC — owner only, max 168h\n/soli pause entries|all\n/soli resume-cycle entries|all\n/soli force-close MINT CONFIRM — owner only\n/soli close-all CONFIRM — all tracked positions, sequential\n/soli learning — learning evidence\n/soli report\n/soli preview buy|sell MINT ATOMIC_AMOUNT\n/soli approve ORDER_ID DIGEST\n/soli reconcile\nPause keeps reconciliation active; owner force close still works. Commands do not authorize live activation.";
