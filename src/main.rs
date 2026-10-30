mod acceptance;
mod access;
mod bridge;
mod connector;
mod data;
mod goplus;
mod jupiter_wire;
mod learning;
mod live;
mod local_key;
mod mcp;
mod notify;
mod operator;
mod paper_cycle;
mod proxy;
mod readiness;
mod research;
mod signals;
mod vault_admin;
mod wallet;
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::{Value, json};
use soli_agent::{Ledger, Preview, QUOTE_TTL, USDC, valid_mint};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct Store {
    db: Connection,
    ledger: Ledger,
    previews: HashMap<String, Preview>,
}
#[derive(Clone)]
struct App {
    smart_wallets: Vec<String>,
    entry_trigger: String,
    data: Option<data::Data>,
    strategy: research::Strategy,
    watchlist: Vec<String>,
    auto_paper: bool,
    live_enabled: bool,
    store: Arc<Mutex<Store>>,
    http: reqwest::Client,
    token: String,
    jupiter_key: Option<String>,
    fee: u64,
}
type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;
fn error(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error":message})))
}
fn bad(message: impl AsRef<str>) -> (StatusCode, Json<Value>) {
    error(StatusCode::BAD_REQUEST, message.as_ref())
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_secs()
}
fn persist(store: &mut Store, ledger: Ledger) -> Result<(), String> {
    let data = serde_json::to_string(&ledger).map_err(|_| "ledger serialization failed")?;
    store
        .db
        .execute("UPDATE ledger SET data=?1 WHERE id=1", params![data])
        .map_err(|_| "ledger persistence failed")?;
    store.ledger = ledger;
    Ok(())
}

async fn authorize(State(app): State<App>, request: Request, next: Next) -> Response {
    let actual = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let identity = match access::Access::from_env(&app.token).identify(
        actual,
        request.method().as_str(),
        request.uri().path(),
    ) {
        Ok(id) => id,
        Err(reason) => {
            return error(
                if reason.contains("scope") {
                    StatusCode::FORBIDDEN
                } else {
                    StatusCode::UNAUTHORIZED
                },
                reason,
            )
            .into_response();
        }
    };
    let path = request.uri().path().to_owned();
    let method = request.method().to_string();
    if identity != "operator"
        && method == "POST"
        && path == "/v1/paper/orders"
        && paper_cycle::settings(&app).is_ok_and(|s| s.mode == "semi")
    {
        return error(
            StatusCode::FORBIDDEN,
            "semi mode requires owner paper-fill approval",
        )
        .into_response();
    }
    let response = next.run(request).await;
    research::event(
        &app,
        "agent_request",
        json!({"agent":identity,"method":method,"path":path,"status":response.status().as_u16()}),
    );
    response
}

async fn status(State(app): State<App>) -> ApiResult {
    let lifecycle = if app.live_enabled {
        Value::Null
    } else {
        serde_json::to_value(paper_cycle::settings(&app).map_err(bad)?)
            .map_err(|_| bad("lifecycle encoding failed"))?
    };
    let store = app
        .store
        .lock()
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "store unavailable"))?;
    Ok(Json(
        json!({"agent":"Soli","chain":"solana-mainnet","mode":if app.live_enabled {"live"}else{"dry_run"}, "live_enabled":app.live_enabled,
        "vault":if app.live_enabled {"Transit connected; owner-only live API"}else{"not connected in dry run"}, "jupiter_configured":app.jupiter_key.is_some(),
        "risk":{"max_positions":2,"position_cap_pct":5,"total_cost_cap_pct":10,"daily_realized_loss_pct":3},
        "ledger":store.ledger,"paper_lifecycle":lifecycle,"valuation":"cash and cost basis only; not marked-to-market equity"}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuoteRequest {
    side: String,
    mint: String,
    amount: u64,
}

async fn preview(State(app): State<App>, Json(input): Json<QuoteRequest>) -> ApiResult {
    if app.live_enabled {
        return Err(bad("paper endpoints disabled in live mode"));
    }
    if !valid_mint(&input.mint)
        || input.mint == USDC
        || input.amount == 0
        || !["buy", "sell"].contains(&input.side.as_str())
    {
        return Err(bad(
            "expected buy/sell, valid non-USDC mint and positive atomic amount",
        ));
    }
    let key = app.jupiter_key.as_ref().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "JUPITER_API_KEY is not configured",
        )
    })?;
    let (input_mint, output_mint) = if input.side == "buy" {
        (USDC, input.mint.as_str())
    } else {
        (input.mint.as_str(), USDC)
    };
    let response = app
        .http
        .get("https://api.jup.ag/swap/v2/order")
        .header("x-api-key", key)
        .query(&[
            ("inputMint", input_mint),
            ("outputMint", output_mint),
            ("amount", &input.amount.to_string()),
            ("slippageBps", "100"),
        ])
        .send()
        .await
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "Jupiter request failed"))?;
    if !response.status().is_success() {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "Jupiter rejected quote request",
        ));
    }
    let value: Value = response
        .json()
        .await
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "invalid Jupiter response"))?;
    if value.get("error").is_some_and(|v| !v.is_null())
        || value
            .get("errorCode")
            .and_then(Value::as_i64)
            .is_some_and(|v| v != 0)
    {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "Jupiter returned quote error",
        ));
    }
    if value["inputMint"] != input_mint || value["outputMint"] != output_mint {
        return Err(error(StatusCode::BAD_GATEWAY, "quote mint mismatch"));
    }
    let number = |name: &str| value[name].as_str().and_then(|v| v.parse::<u64>().ok());
    if number("inAmount") != Some(input.amount) {
        return Err(error(StatusCode::BAD_GATEWAY, "quote input mismatch"));
    }
    let output = number("outAmount")
        .filter(|v| *v > 0)
        .ok_or_else(|| error(StatusCode::BAD_GATEWAY, "missing quote output"))?;
    let minimum = number("otherAmountThreshold")
        .filter(|v| data::within_slippage(output, *v))
        .ok_or_else(|| {
            error(
                StatusCode::BAD_GATEWAY,
                "missing conservative output threshold",
            )
        })?;
    let impact = value
        .get("priceImpactPct")
        .and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|v| v.parse().ok()))
        })
        .ok_or_else(|| error(StatusCode::BAD_GATEWAY, "missing price impact percentage"))?;
    if !impact.is_finite() || impact.abs() > 1.0 {
        return Err(bad("price impact exceeds 1%"));
    }
    let p = Preview {
        id: uuid::Uuid::new_v4().to_string(),
        side: input.side,
        mint: input.mint,
        input_amount: input.amount,
        quoted_output: output,
        minimum_output: minimum,
        estimated_fee_usdc: research::fee(&app),
        created_at: now(),
        price_impact_pct: impact,
    };
    let mut store = app
        .store
        .lock()
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "store unavailable"))?;
    let mut candidate = store.ledger.clone();
    candidate.fill(&p, now()).map_err(|v| bad(&v))?;
    store
        .previews
        .retain(|_, p| now().saturating_sub(p.created_at) <= QUOTE_TTL);
    if store.previews.len() >= 100 {
        return Err(error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many pending previews",
        ));
    }
    store.previews.insert(p.id.clone(), p.clone());
    Ok(Json(
        json!({"mode":"dry_run","preview":p,"expires_in_seconds":QUOTE_TTL,
        "warning":"quote is not a guaranteed fill; no token safety approval has been performed"}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderRequest {
    quote_id: String,
}
async fn order(State(app): State<App>, Json(input): Json<OrderRequest>) -> ApiResult {
    if app.live_enabled {
        return Err(bad("paper endpoints disabled in live mode"));
    }
    let mut store = app
        .store
        .lock()
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "store unavailable"))?;
    if let Some(fill) = store
        .ledger
        .fills
        .iter()
        .find(|f| f.quote_id == input.quote_id)
    {
        return Ok(Json(json!({"mode":"dry_run","fill":fill,"replayed":true})));
    }
    let preview = store
        .previews
        .get(&input.quote_id)
        .cloned()
        .ok_or_else(|| bad("unknown preview; request a fresh quote"))?;
    let mut ledger = store.ledger.clone();
    let fill = ledger.fill(&preview, now()).map_err(|e| bad(&e))?;
    persist(&mut store, ledger).map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, &e))?;
    store.previews.remove(&input.quote_id);
    Ok(Json(json!({"mode":"dry_run","fill":fill,"replayed":false})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HaltRequest {
    halted: bool,
}
async fn halt(State(app): State<App>, Json(input): Json<HaltRequest>) -> ApiResult {
    if app.live_enabled {
        return Err(bad("use owner-only live-halt/live-resume in live mode"));
    }
    let mut store = app
        .store
        .lock()
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "store unavailable"))?;
    let mut ledger = store.ledger.clone();
    ledger.halted = input.halted;
    persist(&mut store, ledger).map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, &e))?;
    Ok(Json(json!({"halted":input.halted,"exits_allowed":true})))
}

async fn market(State(app): State<App>, Path(mint): Path<String>) -> ApiResult {
    if !valid_mint(&mint) {
        return Err(bad("invalid Solana mint"));
    }
    let r = app
        .http
        .get(format!(
            "https://api.dexscreener.com/token-pairs/v1/solana/{mint}"
        ))
        .send()
        .await
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "market data unavailable"))?;
    if !r.status().is_success() {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "market provider rejected request",
        ));
    }
    let data: Value = r
        .json()
        .await
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "invalid market response"))?;
    Ok(Json(
        json!({"source":"dexscreener","fetched_at":now(),"data":data,"trusted_instructions":false}),
    ))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args
        .first()
        .is_some_and(|a| ["menu", "soli-menu"].contains(&a.as_str()))
    {
        println!("{}", wallet::MENU);
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "verify-local-wallet") {
        return local_key::check().map_err(Into::into);
    }
    if args.first().is_none_or(|a| a != "signer")
        && std::env::var("SOLI_PRIVATE_KEY").is_ok_and(|s| !s.is_empty())
    {
        return Err("SOLI_PRIVATE_KEY is only permitted in the isolated signer process".into());
    }
    if args.first().is_some_and(|a| a == "check-signer") {
        return acceptance::signer(args.iter().any(|a| a == "--expect-startup-failure")).await;
    }
    if args.first().is_some_and(|a| a == "connector") {
        return connector::command().await;
    }
    if args.first().is_some_and(|a| operator::handles(a)) {
        return operator::command(&args).await;
    }
    if args.first().is_some_and(|a| a == "mcp") {
        return mcp::serve().await;
    }
    if args.first().is_some_and(|a| a == "vault-key") {
        return vault_admin::command(&args).await;
    }
    let live_enabled = match std::env::var("SOLI_DRY_RUN").as_deref() {
        Ok("false") => true,
        Ok("true") | Err(_) => false,
        _ => return Err("SOLI_DRY_RUN must be true or false".into()),
    };
    let token = mcp::credential()?;
    if token.len() < 32 || token.starts_with("replace-") {
        return Err("use a random SOLI_API_TOKEN of at least 32 characters".into());
    }
    if args.first().is_some_and(|a| a == "signer") {
        if !live_enabled {
            return Err("signer requires explicit SOLI_DRY_RUN=false; dry run never opens a signing service".into());
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:8788").await?;
        let path = std::path::PathBuf::from(
            std::env::var("SOLI_SIGNER_DB").unwrap_or("signer-data/live.db".into()),
        );
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let live = live::Live::connect(&path, &token).await?;
        let mut routes = live::routes(live.clone()).route(
            "/health",
            get(|| async { Json(json!({"status":"ok","role":"isolated-signer"})) }),
        );
        if let Some(security) = bridge::Security::from_env(&path)? {
            routes = routes.merge(bridge::owner_route(live, security));
        }
        println!("Soli isolated signer on 127.0.0.1:8788; owner approval required");
        axum::serve(listener, routes)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        return Ok(());
    }
    if !args.is_empty() && args[0] != "serve" {
        return client_command(&token, &args).await;
    }
    if std::env::var_os("VAULT_TOKEN").is_some() || std::env::var_os("SOLI_OWNER_TOKEN").is_some() {
        return Err(
            "engine refuses signing secrets in its environment; use the separate signer account"
                .into(),
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8787").await?;
    let path = std::path::PathBuf::from(std::env::var("SOLI_DB").unwrap_or("data/soli.db".into()));
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let db = Connection::open(&path)?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS ledger(id INTEGER PRIMARY KEY CHECK(id=1),data TEXT NOT NULL);")?;
    let initial = std::env::var("SOLI_INITIAL_USDC")
        .unwrap_or("1000000000".into())
        .parse()?;
    let empty = Ledger::new(initial)?;
    db.execute(
        "INSERT OR IGNORE INTO ledger(id,data) VALUES(1,?1)",
        params![serde_json::to_string(&empty)?],
    )?;
    let saved: String = db.query_row("SELECT data FROM ledger WHERE id=1", [], |r| r.get(0))?;
    let ledger: Ledger = serde_json::from_str(&saved)?;
    let fee = std::env::var("SOLI_PAPER_FEE_USDC")
        .unwrap_or("100000".into())
        .parse::<u64>()?;
    if fee > 1_000_000 {
        return Err("paper fee must be <= 1 USDC".into());
    }
    let app = App {
        smart_wallets: std::env::var("SOLI_SMART_WALLETS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        entry_trigger: std::env::var("SOLI_ENTRY_TRIGGER").unwrap_or("wallet_convergence".into()),
        data: data::Data::from_env(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()?,
        ),
        strategy: research::Strategy::from_env()?,
        watchlist: std::env::var("SOLI_WATCH_MINTS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        auto_paper: std::env::var("SOLI_AUTO_PAPER").as_deref() == Ok("true"),
        live_enabled,
        store: Arc::new(Mutex::new(Store {
            db,
            ledger,
            previews: HashMap::new(),
        })),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        token,
        jupiter_key: std::env::var("JUPITER_API_KEY")
            .ok()
            .filter(|s| !s.is_empty()),
        fee,
    };
    app.strategy.validate()?;
    if app.watchlist.iter().any(|m| !valid_mint(m)) {
        return Err("invalid watchlist mint".into());
    }
    research::init(&app)?;
    notify::init(&app.store.lock().map_err(|_| "store unavailable")?.db)?;
    signals::init(&app)?;
    if !["wallet_convergence", "momentum"].contains(&app.entry_trigger.as_str())
        || app.smart_wallets.iter().any(|w| !valid_mint(w))
    {
        return Err("invalid trigger or smart-wallet address".into());
    }
    let mut routes = Router::new()
        .route("/v1/status", get(status))
        .route("/v1/lifecycle", get(paper_cycle::status))
        .route("/v1/lifecycle/control", post(paper_cycle::control))
        .route("/v1/learning", get(learning::status))
        .route("/v1/wallet", get(wallet::status))
        .route("/v1/wallet/positions/{mint}", get(wallet::position))
        .route("/v1/live/candidates", get(research::live_candidates))
        .route("/v1/market/{mint}", get(market))
        .route("/v1/paper/previews", post(preview))
        .route("/v1/paper/orders", post(order))
        .route("/v1/halt", post(halt))
        .route("/v1/risk/{mint}", get(research::risk))
        .route("/v1/samples/{mint}", get(research::history))
        .route("/v1/samples/import", post(research::import))
        .route("/v1/replay/{mint}", get(research::replay))
        .route("/v1/report", get(research::report))
        .route("/v1/reports/daily", get(research::daily))
        .route("/v1/notifications", get(notify::status))
        .route("/v1/notifications/retry", post(notify::retry))
        .route("/v1/readiness", get(readiness::inspect))
        .route("/v1/performance", get(readiness::performance))
        .route("/v1/fees/calibrate", post(research::calibrate))
        .route_layer(middleware::from_fn_with_state(app.clone(), authorize))
        .route(
            "/health",
            get(move || async move {
                Json(json!({"status":"ok","mode":if live_enabled {"live"}else{"dry_run"}}))
            }),
        )
        .layer(axum::extract::DefaultBodyLimit::max(4096))
        .with_state(app.clone());
    if live_enabled {
        let signer_url = std::env::var("SOLI_SIGNER_URL").unwrap_or("http://127.0.0.1:8788".into());
        let parsed = reqwest::Url::parse(&signer_url)?;
        if parsed.scheme() != "http"
            || ![Some("127.0.0.1"), Some("localhost")].contains(&parsed.host_str())
        {
            return Err("signer URL must be loopback".into());
        }
        let check = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?
            .get(format!(
                "{}/v1/live/status",
                signer_url.trim_end_matches('/')
            ))
            .bearer_auth(std::env::var("SOLI_API_TOKEN")?)
            .send()
            .await
            .map_err(|_| "live startup blocked: isolated signer unavailable")?;
        if !check.status().is_success() {
            return Err("live startup blocked: signer authentication failed".into());
        }
        let signer: Value = check.json().await?;
        if signer["service_identity"].as_str() == Some(access::service_identity()?.as_str())
            || signer["service_identity"].as_str().is_none()
        {
            return Err(
                "live startup blocked: signer must run under a separate OS service account".into(),
            );
        }
        if signer["strategy_hash"].as_str() != Some(app.strategy.fingerprint().as_str()) {
            return Err("live startup blocked: engine/signer strategy mismatch".into());
        }
        if readiness::inspect(State(app.clone()))
            .await
            .map_err(|_| "live startup provider acceptance unavailable")?
            .0["ready"]
            != true
        {
            return Err("live startup blocked: provider acceptance failed; use dry-run doctor --providers to diagnose".into());
        }
        routes = routes.merge(proxy::routes()?);
    }
    if let Some(security) = bridge::Security::from_env(&path)? {
        routes = routes.merge(bridge::routes(
            security,
            std::env::var("SOLI_API_TOKEN")?,
            live_enabled,
        ));
    }
    research::start(app.clone());
    notify::start(app.clone());
    println!(
        "Soli {} API listening on http://127.0.0.1:8787",
        if live_enabled {
            "live (manual owner approval)"
        } else {
            "dry-run"
        }
    );
    axum::serve(listener, routes)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

async fn client_command(token: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()?;
    let base = "http://127.0.0.1:8787";
    let request = match args[0].as_str() {
        "status" if args.len() == 1 => http.get(format!("{base}/v1/status")),
        "lifecycle" if args.len()==1=>http.get(format!("{base}/v1/lifecycle")),
        "learning" if args.len()==1=>http.get(format!("{base}/v1/learning")),
        "wallet"|"positions"|"pnl" if args.len()==1=>http.get(format!("{base}/v1/wallet")),
        "position" if args.len()==2 && valid_mint(&args[1])=>http.get(format!("{base}/v1/wallet/positions/{}",args[1])),
        "live-candidates" if args.len()==1=>http.get(format!("{base}/v1/live/candidates")),
        "live-mode" if args.len()==5=>http.post(format!("{base}/v1/live/automation")).json(&json!({"mode":args[1],"expires_at":args[2].parse::<u64>()?,"entry_usdc":args[3].parse::<u64>()?,"strategy_hash":args[4]})),
        "live-pause"|"live-resume-cycle" if args.len()==2=>http.post(format!("{base}/v1/live/pause")).json(&json!({"scope":args[1],"paused":args[0]=="live-pause"})),
        "live-force-close" if args.len()==3 && args[2]=="CONFIRM"=>http.post(format!("{base}/v1/live/close")).json(&json!({"mint":args[1],"confirm":true})),
        "report" if args.len()==1=>http.get(format!("{base}/v1/report")),
        "daily" if args.len()==1=>http.get(format!("{base}/v1/reports/daily")),
        "calibrate" if args.len()==1=>http.post(format!("{base}/v1/fees/calibrate")),
        "risk"|"history"|"replay" if args.len()==2 && valid_mint(&args[1])=>http.get(format!("{base}/v1/{}/{}",if args[0]=="history"{"samples"}else{&args[0]},args[1])),
        "market" if args.len() == 2 && valid_mint(&args[1]) => http.get(format!("{base}/v1/market/{}", args[1])),
        "preview" if args.len() == 4 => http.post(format!("{base}/v1/paper/previews")).json(&json!({"side":args[1],"mint":args[2],"amount":args[3].parse::<u64>()?})),
        "paper-fill" if args.len() == 2 => http.post(format!("{base}/v1/paper/orders")).json(&json!({"quote_id":args[1]})),
        "halt" | "resume" if args.len() == 1 => http.post(format!("{base}/v1/halt")).json(&json!({"halted":args[0]=="halt"})),
        "live-status" if args.len()==1 => http.get(format!("{base}/v1/live/status")),
        "live-preview" if args.len()==4 => http.post(format!("{base}/v1/live/previews")).json(&json!({"side":args[1],"mint":args[2],"amount":args[3].parse::<u64>()?})),
        "live-execute" if args.len()==4 && args[3]=="CONFIRM" => http.post(format!("{base}/v1/live/execute")).json(&json!({"order_id":args[1],"digest":args[2],"confirm":true})),
        "live-reconcile" if args.len()==1 => http.post(format!("{base}/v1/live/reconcile")),
        "live-cancel" if args.len()==1 => http.post(format!("{base}/v1/live/cancel")),
        "live-exit-arm" if args.len()==4 => http.post(format!("{base}/v1/live/exits")).json(&json!({"mint":args[1],"enabled":true,"expires_at":args[2].parse::<u64>()?,"strategy_hash":args[3]})),
        "live-exit-disarm" if args.len()==2 => http.post(format!("{base}/v1/live/exits")).json(&json!({"mint":args[1],"enabled":false,"expires_at":0,"strategy_hash":""})),
        "live-halt" | "live-resume" if args.len()==1 => http.post(format!("{base}/v1/live/halt")).json(&json!({"halted":args[0]=="live-halt"})),
        _ => return Err("commands: serve | status | market MINT | preview buy|sell MINT ATOMIC_AMOUNT | paper-fill QUOTE_ID | halt | resume".into()),
    };
    let owner_token = if args[0].starts_with("live-")
        && !["live-status", "live-preview", "live-candidates"].contains(&args[0].as_str())
    {
        Some(
            std::env::var("SOLI_OWNER_TOKEN")
                .map_err(|_| "SOLI_OWNER_TOKEN is required for live commands")?,
        )
    } else {
        None
    };
    let response = request
        .bearer_auth(owner_token.as_deref().unwrap_or(token))
        .send()
        .await
        .map_err(|_| "local Soli service unavailable")?;
    let status = response.status();
    let value: Value = response.json().await?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    if !status.is_success() {
        return Err(format!("Soli returned HTTP {status}").into());
    }
    Ok(())
}
