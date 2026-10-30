//! Durable, deduplicated notification outbox. Remote delivery is at-least-once.
use crate::{App, now};
use hmac::{Hmac, Mac};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub id: String,
    pub kind: String,
    pub destination: String,
    pub secret_env: String,
}
fn targets() -> Result<Vec<Target>, String> {
    match std::env::var("SOLI_NOTIFICATIONS_FILE")
        .ok()
        .filter(|p| !p.is_empty())
    {
        Some(p) => serde_json::from_slice(
            &std::fs::read(p).map_err(|_| "notification configuration unavailable")?,
        )
        .map_err(|_| "invalid notification configuration".into()),
        None => Ok(vec![]),
    }
}
pub fn init(db: &Connection) -> Result<(), String> {
    schema(db)?;
    validate_targets()
}
fn schema(db: &Connection) -> Result<(), String> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS outbox(id TEXT PRIMARY KEY,target TEXT,data TEXT,attempts INTEGER DEFAULT 0,next_at INTEGER,delivered_at INTEGER,last_error TEXT);")
        .map_err(|_|"outbox schema failed")?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS notification_schedule(id INTEGER PRIMARY KEY CHECK(id=1),minutes INTEGER NOT NULL,next_at INTEGER NOT NULL,started_at INTEGER NOT NULL);")
        .map_err(|_| "notification schedule schema failed")?;
    db.execute(
        "INSERT OR IGNORE INTO notification_schedule VALUES(1,0,0,?1)",
        [now()],
    )
    .map_err(|_| "notification schedule initialization failed")?;
    Ok(())
}
pub fn schedule(db: &Connection) -> Result<Value, String> {
    db.query_row(
        "SELECT minutes,next_at FROM notification_schedule WHERE id=1",
        [],
        |r| {
            let minutes: u64 = r.get(0)?;
            Ok(json!({"minutes":minutes,"enabled":minutes>0,"next_at":r.get::<_,u64>(1)?}))
        },
    )
    .map_err(|_| "notification schedule unavailable".into())
}
pub fn set_schedule(db: &Connection, value: &str) -> Result<Value, String> {
    let minutes = if value == "off" {
        0
    } else {
        value
            .parse::<u64>()
            .map_err(|_| "report-minutes: off or 5..1440")?
    };
    if minutes != 0 && !(5..=1440).contains(&minutes) {
        return Err("report-minutes: off or 5..1440".into());
    }
    // Applying a new frequency sends one current snapshot, then schedules from that point.
    let tx = db
        .unchecked_transaction()
        .map_err(|_| "notification schedule transaction failed")?;
    tx.execute("UPDATE outbox SET attempts=12,last_error='superseded by report frequency change' WHERE delivered_at IS NULL AND json_extract(data,'$.kind')='lifecycle_report'",[])
        .map_err(|_| "notification schedule cancellation failed")?;
    tx.execute(
        "UPDATE notification_schedule SET minutes=?1,next_at=?2 WHERE id=1",
        params![minutes, now()],
    )
    .map_err(|_| "notification schedule save failed")?;
    tx.commit()
        .map_err(|_| "notification schedule commit failed")?;
    schedule(db)
}
fn periodic(
    db: &Connection,
    stamp: u64,
    data: &Value,
    configured: &[Target],
) -> Result<(), String> {
    let tx = db
        .unchecked_transaction()
        .map_err(|_| "notification schedule transaction failed")?;
    let (minutes, next): (u64, u64) = tx
        .query_row(
            "SELECT minutes,next_at FROM notification_schedule WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| "notification schedule unavailable")?;
    if minutes > 0 && stamp >= next && !configured.is_empty() {
        for target in configured {
            enqueue_target(
                &tx,
                target,
                &format!("lifecycle:{}", uuid::Uuid::new_v4()),
                "lifecycle_report",
                data,
            )?;
        }
        // Coalesce downtime into one current report; never replay hourly backlog.
        tx.execute(
            "UPDATE notification_schedule SET next_at=?1 WHERE id=1",
            [stamp + minutes * 60],
        )
        .map_err(|_| "notification schedule advance failed")?;
    }
    tx.commit()
        .map_err(|_| "notification schedule commit failed".into())
}
fn validate_targets() -> Result<(), String> {
    for t in targets()? {
        if t.id.is_empty()
            || !["slack", "telegram", "discord", "webhook"].contains(&t.kind.as_str())
            || std::env::var(&t.secret_env).is_err()
        {
            return Err(
                "notification target requires valid kind, id and secret environment".into(),
            );
        }
        if t.kind == "discord" {
            let url = reqwest::Url::parse(&std::env::var(&t.secret_env).unwrap())
                .map_err(|_| "invalid Discord webhook")?;
            if url.scheme() != "https"
                || url.host_str() != Some("discord.com")
                || !url.path().starts_with("/api/webhooks/")
            {
                return Err("invalid Discord webhook destination".into());
            }
        }
        if t.kind == "webhook" {
            let url = reqwest::Url::parse(&t.destination).map_err(|_| "invalid webhook")?;
            if url.scheme() != "https" {
                return Err("webhook requires HTTPS".into());
            }
        }
    }
    Ok(())
}
pub fn enqueue(db: &Connection, key: &str, kind: &str, data: &Value) -> Result<(), String> {
    for target in targets()? {
        enqueue_target(db, &target, key, kind, data)?;
    }
    Ok(())
}
fn enqueue_target(
    db: &Connection,
    target: &Target,
    key: &str,
    kind: &str,
    data: &Value,
) -> Result<(), String> {
    let id = format!("{:x}", Sha256::digest(format!("{}:{key}", target.id)));
    let payload = json!({"id":id,"kind":kind,"timestamp":now(),"data":data});
    db.execute(
        "INSERT OR IGNORE INTO outbox(id,target,data,next_at) VALUES(?1,?2,?3,?4)",
        params![
            id,
            serde_json::to_string(&target).map_err(|_| "target encoding failed")?,
            payload.to_string(),
            now()
        ],
    )
    .map_err(|_| "notification enqueue failed")?;
    Ok(())
}
pub fn event(db: &Connection, kind: &str, data: &Value) {
    let important = matches!(
        kind,
        "exit_signal" | "tick_failed" | "mark_failed" | "data_unavailable" | "drawdown_halt"
    ) || (["wallet_convergence_decision", "entry_signal"].contains(&kind)
        && data["accepted"] == true);
    if important {
        let key = format!("{kind}:{}:{}", data["mint"], now() / 1800);
        if enqueue(db, &key, kind, data).is_err() {
            eprintln!("Notification enqueue failed; inspect outbox configuration/storage");
        }
    }
}
pub async fn status(axum::extract::State(app): axum::extract::State<App>) -> crate::ApiResult {
    let s = app
        .store
        .lock()
        .map_err(|_| crate::bad("store unavailable"))?;
    let mut q=s.db.prepare("SELECT id,attempts,next_at,delivered_at,last_error FROM outbox ORDER BY next_at DESC LIMIT 100").map_err(|_|crate::bad("outbox unavailable"))?;
    let rows=q.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"attempts":r.get::<_,u64>(1)?,"next_at":r.get::<_,u64>(2)?,"delivered_at":r.get::<_,Option<u64>>(3)?,"last_error":r.get::<_,Option<String>>(4)?}))).map_err(|_|crate::bad("outbox unavailable"))?.filter_map(Result::ok).collect::<Vec<_>>();
    Ok(axum::Json(
        json!({"deliveries":rows,"delivery_semantics":"at-least-once; unknown platform acknowledgements may duplicate; webhook receivers must deduplicate event ID"}),
    ))
}
pub async fn retry(
    axum::extract::State(app): axum::extract::State<App>,
    axum::Json(v): axum::Json<Value>,
) -> crate::ApiResult {
    let id = v["id"]
        .as_str()
        .ok_or_else(|| crate::bad("delivery id required"))?;
    let s = app
        .store
        .lock()
        .map_err(|_| crate::bad("store unavailable"))?;
    let n =
        s.db.execute(
            "UPDATE outbox SET attempts=0,next_at=?1 WHERE id=?2 AND delivered_at IS NULL",
            params![now(), id],
        )
        .map_err(|_| crate::bad("outbox retry failed"))?;
    Ok(axum::Json(json!({"requeued":n})))
}
async fn deliver(
    http: &reqwest::Client,
    t: &Target,
    payload: &Value,
    secret: &str,
) -> Result<(), String> {
    let text = crate::menu::notification(payload, now());
    let request=match t.kind.as_str() {
        "slack"=>http.post("https://slack.com/api/chat.postMessage").bearer_auth(secret).json(&json!({"channel":t.destination,"text":text,"unfurl_links":false,"unfurl_media":false})),
        "telegram"=>http.post(format!("https://api.telegram.org/bot{secret}/sendMessage")).json(&json!({"chat_id":t.destination,"text":text.chars().take(3900).collect::<String>()})),
        "discord"=>http.post(secret).query(&[("wait","true")]).json(&json!({"content":text.chars().take(1900).collect::<String>(),"allowed_mentions":{"parse":[]}})),
        "webhook"=>{
            let body=payload.to_string();
            let stamp=now().to_string();
            let mut mac=Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_|"invalid webhook secret")?;
            mac.update(format!("{stamp}.{body}").as_bytes());
            http.post(&t.destination).header("content-type","application/json").header("x-soli-event-id",payload["id"].as_str().unwrap_or("")).header("x-soli-time",stamp).header("x-soli-signature",hex::encode(mac.finalize().into_bytes())).body(body)
        },
        _=>return Err("unsupported delivery kind".into())
    };
    let response = request
        .send()
        .await
        .map_err(|_| "delivery network error; outcome unknown")?;
    if !response.status().is_success() {
        return Err(format!("delivery HTTP {}", response.status().as_u16()));
    }
    if t.kind == "slack" || t.kind == "telegram" {
        let v: Value = response
            .json()
            .await
            .map_err(|_| "delivery acknowledgement invalid")?;
        if v["ok"] != true {
            return Err("platform rejected delivery".into());
        }
    }
    Ok(())
}
pub fn start(app: App) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            let pending = {
                let Ok(s) = app.store.lock() else { continue };
                let Ok(mut q)=s.db.prepare("SELECT id,target,data,attempts FROM outbox WHERE delivered_at IS NULL AND attempts<12 AND next_at<=?1 ORDER BY next_at LIMIT 10") else {continue};
                let Ok(rows) = q.query_map([now()], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, u32>(3)?,
                    ))
                }) else {
                    continue;
                };
                rows.filter_map(Result::ok).collect::<Vec<_>>()
            };
            for (id, target, data, attempts) in pending {
                let result = match (
                    serde_json::from_str::<Target>(&target),
                    serde_json::from_str::<Value>(&data),
                ) {
                    (Ok(t), Ok(v)) => match std::env::var(&t.secret_env) {
                        Ok(secret)
                            if targets().is_ok_and(|all| {
                                all.iter().any(|active| {
                                    active.id == t.id
                                        && active.kind == t.kind
                                        && active.destination == t.destination
                                        && active.secret_env == t.secret_env
                                })
                            }) =>
                        {
                            deliver(&app.http, &t, &v, &secret).await
                        }
                        _ => Err("notification target removed or secret unavailable".into()),
                    },
                    _ => Err("invalid stored delivery".into()),
                };
                if let Ok(s) = app.store.lock() {
                    match result {
                        Ok(()) => {
                            let _ = s.db.execute(
                                "UPDATE outbox SET delivered_at=?1,last_error=NULL WHERE id=?2",
                                params![now(), id],
                            );
                        }
                        Err(reason) => {
                            let wait = (10u64 * 2u64.pow(attempts.min(9))).min(3600);
                            let _=s.db.execute("UPDATE outbox SET attempts=attempts+1,next_at=?1,last_error=?2 WHERE id=?3",params![now()+wait,reason,id]);
                        }
                    }
                }
            }
            if let Ok(axum::Json(report)) =
                crate::research::report(axum::extract::State(app.clone())).await
                && let Ok(axum::Json(status)) =
                    crate::status(axum::extract::State(app.clone())).await
                && let Ok(s) = app.store.lock()
            {
                let latest_sample: Option<u64> =
                    s.db.query_row("SELECT MAX(ts) FROM samples", [], |r| r.get(0))
                        .ok()
                        .flatten();
                if let Err(e) = targets().and_then(|t| {
                    periodic(
                        &s.db,
                        now(),
                        &json!({"status":status,"report":report,"latest_sample_at":latest_sample}),
                        &t,
                    )
                }) {
                    eprintln!("Lifecycle notification: {e}");
                }
                let since =
                    s.db.query_row(
                        "SELECT started_at FROM notification_schedule WHERE id=1",
                        [],
                        |r| r.get::<_, u64>(0),
                    )
                    .unwrap_or(u64::MAX);
                for fill in s.ledger.fills.iter().filter(|f| f.timestamp >= since) {
                    let _ = enqueue(
                        &s.db,
                        &format!("paper-fill:{}", fill.id),
                        if fill.side == "buy" {
                            "paper_entry_filled"
                        } else {
                            "paper_exit_filled"
                        },
                        &json!(fill),
                    );
                }
                // Send the completed prior UTC day's persisted snapshot once, including after downtime.
                let day = now() / 86400;
                let previous: Option<String> =
                    s.db.query_row(
                        "SELECT data FROM daily_reports WHERE day=?1",
                        [day.saturating_sub(1)],
                        |r| r.get(0),
                    )
                    .ok();
                if let Some(old) = previous.and_then(|v| serde_json::from_str::<Value>(&v).ok()) {
                    let _ = enqueue(&s.db, &format!("daily:{}", day - 1), "daily_report", &old);
                }
                if let Some(positions) = report["live"]["marks"]["positions"].as_array() {
                    for p in positions
                        .iter()
                        .filter(|p| p["exit_recommendation"].is_string())
                    {
                        let _ = enqueue(
                            &s.db,
                            &format!(
                                "live-exit:{}:{}:{}",
                                p["mint"],
                                p["exit_recommendation"],
                                now() / 1800
                            ),
                            "live_exit_recommendation",
                            p,
                        );
                    }
                }
                if let Some(completed) = report["live"]["completed_order_sample"].as_array() {
                    for order in completed {
                        let key = format!("live-receipt:{}", order);
                        let _ = enqueue(&s.db, &key, "live_receipt", order);
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schedule_survives_restart_coalesces_downtime_and_can_be_disabled() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let db = Connection::open(file.path()).unwrap();
        schema(&db).unwrap();
        assert!(set_schedule(&db, "1").is_err());
        assert!(set_schedule(&db, "1441").is_err());
        assert!(set_schedule(&db, "-5").is_err());
        let t = Target {
            id: "dm".into(),
            kind: "slack".into(),
            destination: "D1".into(),
            secret_env: "TEST_ONLY".into(),
        };
        let schedule = set_schedule(&db, "60").unwrap();
        let stamp = schedule["next_at"].as_u64().unwrap();
        periodic(&db, stamp, &json!({}), &[]).unwrap();
        assert_eq!(super::schedule(&db).unwrap()["next_at"], stamp);
        periodic(&db, stamp, &json!({}), std::slice::from_ref(&t)).unwrap();
        drop(db);
        let db = Connection::open(file.path()).unwrap();
        schema(&db).unwrap();
        periodic(&db, stamp + 3599, &json!({}), std::slice::from_ref(&t)).unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get::<_, u64>(0))
                .unwrap(),
            1
        );
        periodic(&db, stamp + 86400, &json!({}), std::slice::from_ref(&t)).unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get::<_, u64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            super::schedule(&db).unwrap()["next_at"],
            stamp + 86400 + 3600
        );
        set_schedule(&db, "off").unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM outbox WHERE attempts<12", [], |r| r
                .get::<_, u64>(
                0
            ))
            .unwrap(),
            0
        );
        periodic(&db, stamp + 172800, &json!({}), &[t]).unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get::<_, u64>(0))
                .unwrap(),
            2
        );
    }
    #[test]
    fn outbox_keeps_retry_and_receipt_state_on_disk() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let db = Connection::open(file.path()).unwrap();
        schema(&db).unwrap();
        let target = Target {
            id: "one".into(),
            kind: "webhook".into(),
            destination: "https://example.invalid/hook".into(),
            secret_env: "UNUSED_TEST_SECRET".into(),
        };
        enqueue_target(&db, &target, "same-event", "test", &json!({"safe":true})).unwrap();
        drop(db);
        let db = Connection::open(file.path()).unwrap();
        enqueue_target(&db, &target, "same-event", "test", &json!({"safe":true})).unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get::<_, u64>(0))
                .unwrap(),
            1
        );
        db.execute("UPDATE outbox SET attempts=12", []).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM outbox WHERE attempts<12 AND delivered_at IS NULL",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
            0
        );
    }
    #[tokio::test]
    async fn webhook_signature_and_failed_acknowledgement_are_verified() {
        use axum::{
            Router,
            body::Bytes,
            http::{HeaderMap, StatusCode},
            routing::post,
        };
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new().route(
            "/hook",
            post(move |headers: HeaderMap, body: Bytes| {
                let count = observed.clone();
                async move {
                    let mut mac =
                        Hmac::<Sha256>::new_from_slice(b"disposable-test-secret").unwrap();
                    mac.update(headers["x-soli-time"].as_bytes());
                    mac.update(b".");
                    mac.update(&body);
                    mac.verify_slice(
                        &hex::decode(headers["x-soli-signature"].to_str().unwrap()).unwrap(),
                    )
                    .unwrap();
                    assert_eq!(headers["x-soli-event-id"], "event-id");
                    if count.fetch_add(1, Ordering::SeqCst) == 0 {
                        StatusCode::TOO_MANY_REQUESTS
                    } else {
                        StatusCode::OK
                    }
                }
            }),
        );
        let handle = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let target = Target {
            id: "fixture".into(),
            kind: "webhook".into(),
            destination: format!("http://{address}/hook"),
            secret_env: "unused".into(),
        };
        let payload = json!({"id":"event-id","kind":"test","data":{}});
        let http = reqwest::Client::new();
        assert!(
            deliver(&http, &target, &payload, "disposable-test-secret")
                .await
                .is_err()
        );
        assert!(
            deliver(&http, &target, &payload, "disposable-test-secret")
                .await
                .is_ok()
        );
        assert_eq!(count.load(Ordering::SeqCst), 2);
        handle.abort();
    }
}
