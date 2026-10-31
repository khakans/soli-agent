//! Wallet-convergence trigger for spot swaps. Independent implementation inspired by Meridian's architecture.
use crate::{App, data::Data, now, research};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
const WSOL: &str = "So11111111111111111111111111111111111111112";
const JUPITER: &str = "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4";
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Buy {
    pub prior_amount: u64,
    pub signature: String,
    pub wallet: String,
    pub mint: String,
    pub timestamp: u64,
    pub received: u64,
    pub spent_usdc: u64,
    pub spent_lamports: u64,
}
fn balances(v: &Value, wallet: &str) -> Result<BTreeMap<String, u64>, String> {
    let mut out = BTreeMap::new();
    for b in v.as_array().ok_or("token balance metadata unavailable")? {
        if b["owner"] == wallet {
            let mint = b["mint"].as_str().ok_or("missing mint")?;
            let amount = b["uiTokenAmount"]["amount"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or("invalid token amount")?;
            let n = out.entry(mint.into()).or_insert(0u64);
            *n = (*n).checked_add(amount).ok_or("balance overflow")?;
        }
    }
    Ok(out)
}
pub fn parse_buy(
    tx: &Value,
    wallet: &str,
    signature: &str,
    stamp: u64,
) -> Result<Option<Buy>, String> {
    let Some(ts) = tx["blockTime"].as_u64() else {
        return Ok(None);
    };
    if ts > stamp || stamp - ts > 120 || !tx["meta"]["err"].is_null() {
        return Ok(None);
    }
    let keys = tx["transaction"]["message"]["accountKeys"]
        .as_array()
        .ok_or("missing transaction keys")?;
    let Some(index) = keys
        .iter()
        .position(|v| v["pubkey"] == wallet && v["signer"] == true)
    else {
        return Ok(None);
    };
    let direct = tx["transaction"]["message"]["instructions"]
        .as_array()
        .ok_or("missing instructions")?
        .iter()
        .any(|v| v["programId"] == JUPITER);
    if !direct {
        return Ok(None);
    } // Ignore transfers, LP deposits, unverified logs and unrelated program invocations.
    let pre = balances(&tx["meta"]["preTokenBalances"], wallet)?;
    let post = balances(&tx["meta"]["postTokenBalances"], wallet)?;
    let mints: BTreeSet<_> = pre.keys().chain(post.keys()).cloned().collect();
    let mut bought = vec![];
    for mint in mints {
        if [WSOL, soli_agent::USDC].contains(&mint.as_str()) {
            continue;
        }
        let old = pre.get(&mint).copied().unwrap_or(0);
        let new = post.get(&mint).copied().unwrap_or(0);
        if new < old {
            return Ok(None);
        } // Exclude token-to-token rotations instead of misclassifying them as fresh capital.
        if new > old {
            bought.push((mint, new - old));
        }
    }
    if bought.len() != 1 {
        return Ok(None);
    }
    let spent_usdc = pre
        .get(soli_agent::USDC)
        .copied()
        .unwrap_or(0)
        .saturating_sub(post.get(soli_agent::USDC).copied().unwrap_or(0));
    let raw_sol = tx["meta"]["preBalances"][index]
        .as_u64()
        .ok_or("missing pre SOL balance")?
        .saturating_sub(
            tx["meta"]["postBalances"][index]
                .as_u64()
                .ok_or("missing post SOL balance")?,
        );
    let fee = if index == 0 {
        tx["meta"]["fee"].as_u64().ok_or("missing fee")?
    } else {
        0
    };
    let new_accounts = post.keys().filter(|mint| !pre.contains_key(*mint)).count() as u64;
    let spent_lamports = raw_sol
        .saturating_sub(fee)
        .saturating_sub(new_accounts.saturating_mul(2_039_280))
        .saturating_add(
            pre.get(WSOL)
                .copied()
                .unwrap_or(0)
                .saturating_sub(post.get(WSOL).copied().unwrap_or(0)),
        );
    if spent_usdc < 1_000_000 && spent_lamports < 50_000_000 {
        return Ok(None);
    }
    let (mint, received) = bought.pop().unwrap();
    Ok(Some(Buy {
        prior_amount: pre.get(&mint).copied().unwrap_or(0),
        signature: signature.into(),
        wallet: wallet.into(),
        mint,
        timestamp: ts,
        received,
        spent_usdc,
        spent_lamports,
    }))
}
pub fn init(app: &App) -> Result<(), String> {
    let s = app.store.lock().map_err(|_| "store unavailable")?;
    s.db.execute_batch("CREATE TABLE IF NOT EXISTS wallet_cursors(wallet TEXT PRIMARY KEY,signature TEXT NOT NULL); CREATE TABLE IF NOT EXISTS wallet_queue(wallet TEXT,signature TEXT,queued INTEGER,PRIMARY KEY(wallet,signature)); CREATE TABLE IF NOT EXISTS wallet_buys(wallet TEXT,signature TEXT,mint TEXT,ts INTEGER,data TEXT,PRIMARY KEY(wallet,signature,mint)); CREATE TABLE IF NOT EXISTS candidate_queue(mint TEXT PRIMARY KEY,first_seen INTEGER,last_try INTEGER DEFAULT 0); CREATE TABLE IF NOT EXISTS cooldowns(mint TEXT PRIMARY KEY,until_ts INTEGER,reason TEXT);").map_err(|_|"signal schema failed")?;
    Ok(())
}

pub async fn tick(app: &App, data: &Data) -> Result<(), String> {
    if crate::paper_cycle::paused(app) {
        return Ok(());
    }
    for wallet in &app.smart_wallets {
        if crate::paper_cycle::paused(app) {
            return Ok(());
        }
        let cursor = {
            let s = app.store.lock().map_err(|_| "store unavailable")?;
            s.db.query_row(
                "SELECT signature FROM wallet_cursors WHERE wallet=?1",
                [wallet],
                |r| r.get::<_, String>(0),
            )
            .ok()
        };
        let mut before: Option<String> = None;
        let mut newest = None;
        let mut signatures = vec![];
        let mut complete = false;
        for _ in 0..5 {
            if crate::paper_cycle::paused(app) {
                return Ok(());
            }
            let mut options = json!({"limit":100,"commitment":"confirmed"});
            if let Some(before) = &before {
                options["before"] = json!(before);
            }
            let page = data
                .rpc("getSignaturesForAddress", json!([wallet, options]))
                .await?;
            if crate::paper_cycle::paused(app) {
                return Ok(());
            }
            let page = page.as_array().ok_or("missing signature history")?;
            if newest.is_none() {
                newest = page
                    .first()
                    .and_then(|v| v["signature"].as_str())
                    .map(String::from);
            }
            if cursor.is_none() {
                complete = true;
                break;
            } // First observation is a baseline, never historical buying conviction.
            for row in page {
                let sig = row["signature"].as_str().ok_or("signature missing")?;
                if cursor.as_deref() == Some(sig)
                    || row["blockTime"]
                        .as_u64()
                        .is_some_and(|t| now().saturating_sub(t) > 120)
                {
                    complete = true;
                    break;
                }
                if row["err"].is_null() {
                    signatures.push(sig.to_string());
                }
            }
            if complete || page.len() < 100 {
                complete = true;
                break;
            }
            before = page
                .last()
                .and_then(|v| v["signature"].as_str())
                .map(String::from);
        }
        if !complete {
            return Err(
                "wallet backfill limit reached; entries paused until history catches up".into(),
            );
        }
        if crate::paper_cycle::paused(app) {
            return Ok(());
        }
        if let Some(newest) = newest {
            let s = app.store.lock().map_err(|_| "store unavailable")?;
            for sig in signatures {
                s.db.execute(
                    "INSERT OR IGNORE INTO wallet_queue VALUES(?1,?2,?3)",
                    params![wallet, sig, now()],
                )
                .map_err(|_| "queue write failed")?;
            }
            // Queue writes precede cursor advancement. A crash can replay IDs, never lose an acknowledged signature.
            s.db.execute(
                "INSERT OR REPLACE INTO wallet_cursors VALUES(?1,?2)",
                params![wallet, newest],
            )
            .map_err(|_| "cursor write failed")?;
        }
    }
    let queued = {
        let s = app.store.lock().map_err(|_| "store unavailable")?;
        let mut q = s
            .db
            .prepare("SELECT wallet,signature,queued FROM wallet_queue ORDER BY queued LIMIT 30")
            .map_err(|_| "queue query failed")?;
        q.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u64>(2)?,
            ))
        })
        .map_err(|_| "queue query failed")?
        .filter_map(Result::ok)
        .collect::<Vec<_>>()
    };
    for (wallet, sig, queued) in queued {
        if crate::paper_cycle::paused(app) {
            return Ok(());
        }
        let tx=data.rpc("getTransaction",json!([sig,{"encoding":"jsonParsed","commitment":"confirmed","maxSupportedTransactionVersion":0}])).await?;
        if crate::paper_cycle::paused(app) {
            return Ok(());
        }
        if tx.is_null() && now().saturating_sub(queued) < 120 {
            continue;
        }
        let buy = if tx.is_null() {
            None
        } else {
            parse_buy(&tx, &wallet, &sig, now())?
        };
        let s = app.store.lock().map_err(|_| "store unavailable")?;
        if let Some(b) = buy {
            s.db.execute(
                "INSERT OR IGNORE INTO wallet_buys VALUES(?1,?2,?3,?4,?5)",
                params![
                    b.wallet,
                    b.signature,
                    b.mint,
                    b.timestamp,
                    serde_json::to_string(&b).unwrap()
                ],
            )
            .map_err(|_| "signal write failed")?;
            let count: u64 =
                s.db.query_row(
                    "SELECT COUNT(DISTINCT wallet) FROM wallet_buys WHERE mint=?1 AND ts>=?2",
                    params![b.mint, now().saturating_sub(120)],
                    |r| r.get(0),
                )
                .map_err(|_| "convergence query failed")?;
            if count >= 2 {
                s.db.execute(
                    "INSERT OR IGNORE INTO candidate_queue(mint,first_seen) VALUES(?1,?2)",
                    params![b.mint, now()],
                )
                .map_err(|_| "candidate queue failed")?;
            }
        }
        s.db.execute(
            "DELETE FROM wallet_queue WHERE wallet=?1 AND signature=?2",
            params![wallet, sig],
        )
        .map_err(|_| "queue acknowledgement failed")?;
    }
    let candidates = {
        let s = app.store.lock().map_err(|_| "store unavailable")?;
        let mut q =
            s.db.prepare("SELECT mint,first_seen FROM candidate_queue WHERE last_try<=?1 LIMIT 10")
                .map_err(|_| "candidate query failed")?;
        q.query_map([now().saturating_sub(15)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?))
        })
        .map_err(|_| "candidate query failed")?
        .filter_map(Result::ok)
        .collect::<Vec<_>>()
    };
    for (mint, first) in candidates {
        if crate::paper_cycle::paused(app) {
            return Ok(());
        }
        let cooldown = {
            let s = app.store.lock().map_err(|_| "store unavailable")?;
            s.db.query_row(
                "SELECT until_ts FROM cooldowns WHERE mint=?1",
                [&mint],
                |r| r.get::<_, u64>(0),
            )
            .ok()
            .is_some_and(|t| t > now())
        };
        if cooldown || now().saturating_sub(first) > 120 {
            let s = app.store.lock().map_err(|_| "store unavailable")?;
            s.db.execute("DELETE FROM candidate_queue WHERE mint=?1", [&mint])
                .map_err(|_| "candidate cleanup failed")?;
            continue;
        }
        let observations = {
            let s = app.store.lock().map_err(|_| "store unavailable")?;
            let mut q = s
                .db
                .prepare("SELECT data FROM wallet_buys WHERE mint=?1 AND ts>=?2 ORDER BY ts DESC")
                .map_err(|_| "observation query failed")?;
            q.query_map(params![mint, now().saturating_sub(120)], |r| {
                r.get::<_, String>(0)
            })
            .map_err(|_| "observation query failed")?
            .filter_map(Result::ok)
            .filter_map(|s| serde_json::from_str::<Buy>(&s).ok())
            .collect::<Vec<_>>()
        };
        let mut distinct = BTreeMap::new();
        for b in observations {
            distinct.entry(b.wallet.clone()).or_insert(b);
        }
        let mut still_holding = 0;
        for (wallet, b) in distinct.iter().take(10) {
            if crate::paper_cycle::paused(app) {
                return Ok(());
            }
            let holdings=data.rpc("getTokenAccountsByOwner",json!([wallet,{"mint":mint},{"encoding":"jsonParsed","commitment":"confirmed"}])).await?;
            if crate::paper_cycle::paused(app) {
                return Ok(());
            }
            let held = holdings["value"]
                .as_array()
                .ok_or("missing convergence holdings")?
                .iter()
                .try_fold(0u64, |n, a| {
                    n.checked_add(
                        a["account"]["data"]["parsed"]["info"]["tokenAmount"]["amount"]
                            .as_str()
                            .and_then(|v| v.parse().ok())
                            .ok_or("invalid holder balance")?,
                    )
                    .ok_or("holder overflow")
                })?;
            if held >= b.prior_amount.saturating_add(b.received.div_ceil(2)) {
                still_holding += 1;
            }
        }
        if crate::paper_cycle::paused(app) {
            return Ok(());
        }
        let screening = data.risk(&mint).await;
        if crate::paper_cycle::paused(app) {
            return Ok(());
        }
        let sample = data.sample(&mint).await;
        if crate::paper_cycle::paused(app) {
            return Ok(());
        }
        let accepted = still_holding >= 2
            && now().saturating_sub(first) <= 120
            && screening.as_ref().is_ok_and(|r| r.accepted)
            && sample.as_ref().is_ok_and(|s| {
                s.liquidity_usd >= app.strategy.min_liquidity_usd
                    && now().saturating_sub(s.pair_created_at) >= app.strategy.min_age_secs
            });
        research::event(
            app,
            "wallet_convergence_decision",
            json!({"mint":mint,"accepted":accepted,"wallets_still_holding":still_holding,"risk":screening,"market":sample,"strategy_hash":app.strategy.fingerprint(),"deadline":first+120}),
        );
        if accepted {
            research::live_candidate(app, &mint, "wallet_convergence")?;
            if crate::paper_cycle::entry(app) && !app.live_enabled {
                let q = crate::preview(
                    axum::extract::State(app.clone()),
                    axum::Json(crate::QuoteRequest {
                        side: "buy".into(),
                        mint: mint.clone(),
                        amount: crate::paper_cycle::amount(app),
                    }),
                )
                .await;
                if let Ok(axum::Json(q)) = q
                    && let Some(id) = q["preview"]["id"].as_str()
                {
                    let _ = crate::order(
                        axum::extract::State(app.clone()),
                        axum::Json(crate::OrderRequest {
                            quote_id: id.into(),
                        }),
                    )
                    .await;
                }
            }
            let s = app.store.lock().map_err(|_| "store unavailable")?;
            s.db.execute(
                "INSERT OR REPLACE INTO cooldowns VALUES(?1,?2,'convergence processed')",
                params![mint, now() + 1800],
            )
            .map_err(|_| "cooldown write failed")?;
            s.db.execute("DELETE FROM candidate_queue WHERE mint=?1", [mint])
                .map_err(|_| "candidate acknowledgement failed")?;
        } else {
            let s = app.store.lock().map_err(|_| "store unavailable")?;
            s.db.execute(
                "UPDATE candidate_queue SET last_try=?1 WHERE mint=?2",
                params![now(), mint],
            )
            .map_err(|_| "retry checkpoint failed")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn pause_all_stops_wallet_history_audits() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE paper_cycle(id INTEGER PRIMARY KEY,data TEXT);")
            .unwrap();
        let settings = crate::paper_cycle::Settings {
            mode: "full".into(),
            paused: true,
            expires_at: u64::MAX,
            entry_usdc: 1_000_000,
        };
        db.execute(
            "INSERT INTO paper_cycle VALUES(1,?1)",
            [serde_json::to_string(&settings).unwrap()],
        )
        .unwrap();
        let http = reqwest::Client::new();
        let app = App {
            smart_wallets: vec!["wallet".into()],
            entry_trigger: "wallet_convergence".into(),
            data: None,
            strategy: crate::research::Strategy::default(),
            watchlist: vec![],
            auto_paper: false,
            live_enabled: false,
            store: std::sync::Arc::new(std::sync::Mutex::new(crate::Store {
                db,
                ledger: soli_agent::Ledger::new(100_000_000).unwrap(),
                previews: Default::default(),
            })),
            http: http.clone(),
            token: String::new(),
            jupiter_key: None,
            fee: 0,
        };
        init(&app).unwrap();
        let data = Data {
            jupiter_url: String::new(),
            http,
            rpc: "http://127.0.0.1:1".into(),
            jupiter_key: String::new(),
            goplus: crate::goplus::Auth::from_env(),
        };

        tick(&app, &data).await.unwrap();

        assert_eq!(
            app.store
                .lock()
                .unwrap()
                .db
                .query_row("SELECT COUNT(*) FROM wallet_cursors", [], |r| r
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn transfers_and_unsigned_wallets_are_not_buy_signals() {
        let tx = json!({"blockTime":100,"meta":{"err":null},"transaction":{"message":{"accountKeys":[{"pubkey":"w","signer":false}],"instructions":[]}}});
        assert!(parse_buy(&tx, "w", "sig", 101).unwrap().is_none());
        let mut t = tx;
        t["transaction"]["message"]["accountKeys"][0]["signer"] = json!(true);
        assert!(parse_buy(&t, "w", "sig", 101).unwrap().is_none());
    }
    #[test]
    fn distinct_wallet_count_does_not_count_repeated_transactions_twice() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE buys(wallet TEXT,mint TEXT,ts INTEGER); INSERT INTO buys VALUES('a','m',100),('a','m',101),('b','m',102);").unwrap();
        let count: u64 = db
            .query_row(
                "SELECT COUNT(DISTINCT wallet) FROM buys WHERE mint='m' AND ts>=90",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }
}
