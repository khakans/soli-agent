//! Signed transport context from a trusted connector, never from model arguments.
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use hmac::{Hmac, Mac};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub event_id: String,
    pub platform: String,
    pub user_id: String,
    pub conversation_id: String,
    pub thread_id: Option<String>,
    pub command: String,
    pub arguments: Vec<String>,
}
pub struct Security {
    secrets: BTreeMap<String, String>,
    owners: BTreeMap<String, Vec<String>>,
    db: Mutex<Connection>,
}
impl Security {
    pub fn from_env(path: &std::path::Path) -> Result<Option<Arc<Self>>, String> {
        let Some(raw) = std::env::var("SOLI_BRIDGE_SECRETS_JSON")
            .ok()
            .filter(|s| !s.is_empty())
        else {
            return Ok(None);
        };
        let secrets: BTreeMap<String, String> =
            serde_json::from_str(&raw).map_err(|_| "invalid bridge secrets JSON")?;
        let owners: BTreeMap<String, Vec<String>> = serde_json::from_str(
            &std::env::var("SOLI_OWNERS_JSON").map_err(|_| "SOLI_OWNERS_JSON required")?,
        )
        .map_err(|_| "invalid owner mapping")?;
        if secrets.is_empty()
            || secrets.iter().any(|(p, s)| {
                !supported(p) || s.len() < 32 || owners.get(p).is_none_or(|v| v.is_empty())
            })
        {
            return Err(
                "configure platform-specific secrets >=32 characters and explicit owner IDs".into(),
            );
        }
        let db = Connection::open(path).map_err(|_| "bridge journal unavailable")?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS bridge_events(platform TEXT,id TEXT,ts INTEGER,PRIMARY KEY(platform,id));").map_err(|_|"bridge schema error")?;
        Ok(Some(Arc::new(Self {
            secrets,
            owners,
            db: Mutex::new(db),
        })))
    }
    pub fn verify(&self, headers: &HeaderMap, body: &[u8]) -> Result<Event, String> {
        let stamp = headers
            .get("x-soli-time")
            .and_then(|v| v.to_str().ok())
            .ok_or("missing event timestamp")?;
        let parsed = stamp.parse::<u64>().map_err(|_| "invalid timestamp")?;
        if super::now().abs_diff(parsed) > 60 {
            return Err("event timestamp expired".into());
        }
        let e: Event = serde_json::from_slice(body).map_err(|_| "invalid event")?;
        if !supported(&e.platform)
            || e.event_id.is_empty()
            || e.event_id.len() > 200
            || e.conversation_id.is_empty()
            || e.arguments.len() > 8
        {
            return Err("invalid transport metadata".into());
        }
        let secret = self
            .secrets
            .get(&e.platform)
            .ok_or("platform not configured")?;
        let supplied = headers
            .get("x-soli-signature")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| hex::decode(s).ok())
            .ok_or("missing event signature")?;
        let mut h = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
            .map_err(|_| "invalid signing secret")?;
        h.update(stamp.as_bytes());
        h.update(b".");
        h.update(body);
        h.verify_slice(&supplied)
            .map_err(|_| "invalid event signature")?;
        if !self
            .owners
            .get(&e.platform)
            .is_some_and(|ids| ids.contains(&e.user_id))
        {
            return Err("user is not an authorized owner".into());
        }
        let db = self.db.lock().map_err(|_| "bridge journal unavailable")?;
        db.execute(
            "DELETE FROM bridge_events WHERE ts<?1",
            [super::now().saturating_sub(300)],
        )
        .map_err(|_| "bridge cleanup failed")?;
        db.execute(
            "INSERT INTO bridge_events VALUES(?1,?2,?3)",
            params![e.platform, e.event_id, parsed],
        )
        .map_err(|_| "duplicate event")?;
        Ok(e)
    }
}
fn supported(p: &str) -> bool {
    [
        "slack",
        "telegram",
        "discord",
        "whatsapp",
        "whatsapp_cloud",
        "agent",
    ]
    .contains(&p)
}
type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;
fn error(s: impl ToString) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":s.to_string()})),
    )
}
#[derive(Clone)]
struct Bridge {
    security: Arc<Security>,
    http: reqwest::Client,
    token: String,
    live: bool,
}
async fn event(State(b): State<Bridge>, headers: HeaderMap, body: Bytes) -> Reply {
    let e = b.security.verify(&headers, &body).map_err(error)?;
    let (method, path, payload) = command(&e, b.live).map_err(error)?;
    let mut r = b
        .http
        .request(method, format!("http://127.0.0.1:8787{path}"))
        .bearer_auth(&b.token);
    if b.live
        && [
            "preview",
            "approve",
            "halt",
            "resume",
            "cancel",
            "reconcile",
            "force-close",
            "close-all",
            "pause",
            "resume-cycle",
            "mode",
        ]
        .contains(&e.command.as_str())
    {
        r = b
            .http
            .post("http://127.0.0.1:8788/v1/owner-event")
            .headers(headers)
            .body(body);
    } else if let Some(payload) = payload {
        r = r.json(&payload);
    }
    let response = r
        .send()
        .await
        .map_err(|_| error("Soli backend unavailable"))?;
    let status = response.status();
    let result: Value = response
        .json()
        .await
        .map_err(|_| error("invalid backend response"))?;
    // Reply routing is derived exclusively from signed gateway identity.
    Ok(Json(
        json!({"route":{"platform":e.platform,"conversation_id":e.conversation_id,"thread_id":e.thread_id},"ok":status.is_success(),"result":result}),
    ))
}
fn command(e: &Event, live: bool) -> Result<(reqwest::Method, String, Option<Value>), String> {
    let get = |p: String| Ok((reqwest::Method::GET, p, None));
    let post = |p: &str, v: Value| Ok((reqwest::Method::POST, p.into(), Some(v)));
    match (e.command.as_str(),e.arguments.as_slice()) {
  ("status",[])=>get(if live{"/v1/live/status"}else{"/v1/status"}.into()),
  ("report",[])=>get("/v1/report".into()),
  ("settings",[])=>get("/v1/settings".into()),
  ("settings",[action,field,value]) if action=="set" && (!live || field=="report-minutes")=>post("/v1/settings",json!({"field":field,"value":value})),
  ("lifecycle",[]) if !live=>get("/v1/lifecycle".into()),
  ("mode"|"pause"|"resume-cycle"|"force-close"|"close-all",args) if !live=>post("/v1/lifecycle/control",json!({"action":e.command,"args":args})),
  ("wallet"|"positions"|"pnl",[])=>get("/v1/wallet".into()),
  ("position",[mint]) if soli_agent::valid_mint(mint)=>get(format!("/v1/wallet/positions/{mint}")),
  ("learning",[])=>get("/v1/learning".into()),
  ("candidates",[])=>get("/v1/live/candidates".into()),
  ("force-close",[mint,c]) if live && c=="CONFIRM" && (soli_agent::valid_mint(mint)||mint=="all")=>post("/v1/owner-event",json!({})),
  ("close-all",[c]) if live && c=="CONFIRM"=>post("/v1/owner-event",json!({})),
  ("pause"|"resume-cycle",[scope]) if live && ["entries","all"].contains(&scope.as_str())=>post("/v1/owner-event",json!({})),
  ("mode",[mode,_,_]) if live && ["manual","semi","full"].contains(&mode.as_str())=>post("/v1/owner-event",json!({})),
  ("risk",[mint]) if soli_agent::valid_mint(mint)=>get(format!("/v1/risk/{mint}")),
  ("market",[mint]) if soli_agent::valid_mint(mint)=>get(format!("/v1/market/{mint}")),
  ("preview",[side,mint,amount]) if ["buy","sell"].contains(&side.as_str())&&soli_agent::valid_mint(mint)=>post(if live{"/v1/live/previews"}else{"/v1/paper/previews"},json!({"side":side,"mint":mint,"amount":amount.parse::<u64>().map_err(|_|"amount must be atomic integer")?})),
  ("fill",[id]) if !live=>post("/v1/paper/orders",json!({"quote_id":id})),
  ("approve",[id,digest]) if live=>post("/v1/owner-event",json!({"order_id":id,"digest":digest})),
  ("halt"|"resume"|"cancel"|"reconcile",[]) if live=>post("/v1/owner-event",json!({})),
  ("halt",[]) if !live=>post("/v1/halt",json!({"halted":true})),
  _=>Err("commands: status | report | risk MINT | market MINT | preview buy/sell MINT ATOMIC_AMOUNT | fill QUOTE_ID (paper) | approve ORDER_ID DIGEST (live) | halt (paper)".into()),
 }
}
pub fn routes(security: Arc<Security>, token: String, live: bool) -> Router {
    Router::new()
        .route("/v1/bridge/events", post(event))
        .layer(axum::extract::DefaultBodyLimit::max(4096))
        .with_state(Bridge {
            security,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(180))
                .build()
                .unwrap(),
            token,
            live,
        })
}
pub fn owner_route(live: Arc<super::live::Live>, security: Arc<Security>) -> Router {
    #[derive(Clone)]
    struct Owner {
        live: Arc<super::live::Live>,
        security: Arc<Security>,
    }
    async fn approve(State(s): State<Owner>, headers: HeaderMap, body: Bytes) -> Reply {
        let e = s.security.verify(&headers, &body).map_err(error)?;
        let result = super::live::from_event(s.live, &e).await.map_err(error)?;
        Ok(Json(result))
    }
    Router::new()
        .route("/v1/owner-event", post(approve))
        .layer(axum::extract::DefaultBodyLimit::max(4096))
        .with_state(Owner { live, security })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transport_rules_and_command_scopes() {
        for p in [
            "slack",
            "telegram",
            "discord",
            "whatsapp",
            "whatsapp_cloud",
            "agent",
        ] {
            assert!(supported(p));
        }
        assert!(!supported("cli"));
        let e = Event {
            event_id: "e".into(),
            platform: "slack".into(),
            user_id: "u".into(),
            conversation_id: "c".into(),
            thread_id: None,
            command: "fill".into(),
            arguments: vec!["id".into()],
        };
        assert!(command(&e, false).is_ok());
        assert!(command(&e, true).is_err());
    }
    #[test]
    fn signed_identity_timestamp_and_replay_are_enforced() {
        for platform in [
            "slack",
            "telegram",
            "discord",
            "whatsapp",
            "whatsapp_cloud",
            "agent",
        ] {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch("CREATE TABLE bridge_events(platform TEXT,id TEXT,ts INTEGER,PRIMARY KEY(platform,id));").unwrap();
            let s = Security {
                secrets: BTreeMap::from([(platform.into(), "secret".into())]),
                owners: BTreeMap::from([(platform.into(), vec!["42".into()])]),
                db: Mutex::new(db),
            };
            let e = Event {
                event_id: "e".into(),
                platform: platform.into(),
                user_id: "42".into(),
                conversation_id: "123".into(),
                thread_id: Some("9".into()),
                command: "status".into(),
                arguments: vec![],
            };
            let body = serde_json::to_vec(&e).unwrap();
            let ts = super::super::now().to_string();
            let mut h = Hmac::<Sha256>::new_from_slice(b"secret").unwrap();
            h.update(ts.as_bytes());
            h.update(b".");
            h.update(&body);
            let mut headers = HeaderMap::new();
            headers.insert("x-soli-time", ts.parse().unwrap());
            headers.insert(
                "x-soli-signature",
                hex::encode(h.finalize().into_bytes()).parse().unwrap(),
            );
            let mut tampered = body.clone();
            tampered.push(b' ');
            assert!(s.verify(&headers, &tampered).is_err());
            assert_eq!(
                s.verify(&headers, &body).unwrap().thread_id.as_deref(),
                Some("9")
            );
            assert!(s.verify(&headers, &body).is_err());
        }
    }
}
