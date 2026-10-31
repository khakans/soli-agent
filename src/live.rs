//! Owner-authorized live executor. The model never receives a Vault token or a raw key.
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use soli_agent::{USDC, valid_mint};
use std::{
    collections::{BTreeMap, HashMap},
    str::FromStr,
    sync::Arc,
    time::Duration,
};
use tokio::sync::Mutex;

const TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const ROUTER: &str = "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4";
type Result<T> = std::result::Result<T, String>;
type Reply = std::result::Result<Json<Value>, (StatusCode, Json<Value>)>;
fn err(s: impl ToString) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":s.to_string()})),
    )
}
fn env(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{name} required for live mode"))
}
fn units(v: &Value) -> Result<u64> {
    v.as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| v.as_u64())
        .ok_or("missing integer amount".into())
}

#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    #[serde(default)]
    entry_mandate: Option<String>,
    #[serde(default)]
    exit_hash: Option<String>,
    #[serde(default)]
    requested_by: Option<String>,
    #[serde(default)]
    origin: Option<Origin>,
    id: String,
    digest: String,
    side: String,
    mint: String,
    input: u64,
    minimum: u64,
    quote: Value,
    expires: u64,
    signature: Option<String>,
    state: String,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
struct Origin {
    platform: String,
    user_id: String,
    conversation_id: String,
    thread_id: Option<String>,
}
impl From<&super::bridge::Event> for Origin {
    fn from(e: &super::bridge::Event) -> Self {
        Self {
            platform: e.platform.clone(),
            user_id: e.user_id.clone(),
            conversation_id: e.conversation_id.clone(),
            thread_id: e.thread_id.clone(),
        }
    }
}
#[derive(Default, Serialize, Deserialize)]
struct Journal {
    #[serde(default)]
    automation_paused: bool,
    #[serde(default)]
    automation: Option<Automation>,
    #[serde(default)]
    entry_attempts: BTreeMap<String, u64>,
    #[serde(default)]
    exit_mandates: BTreeMap<String, ExitMandate>,
    #[serde(default)]
    marks: Value,
    #[serde(default)]
    day_equity: Option<(u64, u64)>,
    #[serde(default)]
    peak_equity: u64,
    #[serde(default)]
    opened: BTreeMap<String, u64>,
    #[serde(default)]
    wallet: String,
    halted: bool,
    day: u64,
    spent: u64,
    positions: BTreeMap<String, u64>,
    pending: Option<Pending>,
    completed: HashMap<String, Value>,
}
struct Db {
    connection: Connection,
    journal: Journal,
}
#[derive(Clone, Serialize, Deserialize)]
struct ExitMandate {
    #[serde(default)]
    forced: bool,
    #[serde(default)]
    retry_at: u64,
    strategy: super::research::Strategy,
    expires_at: u64,
}
#[derive(Clone, Serialize, Deserialize)]
struct Automation {
    id: String,
    mode: String,
    expires_at: u64,
    entry_usdc: u64,
    strategy: super::research::Strategy,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AutomationRequest {
    mode: String,
    expires_at: u64,
    entry_usdc: u64,
    strategy_hash: String,
}
async fn authorize_automation(
    State(l): State<Arc<Live>>,
    Json(r): Json<AutomationRequest>,
) -> Reply {
    if !["manual", "semi", "full"].contains(&r.mode.as_str()) {
        return Err(err("mode must be manual, semi or full"));
    }
    let mut db = l.db.lock().await;
    if r.mode == "manual" {
        db.journal.automation = None;
    } else {
        if r.expires_at <= super::now()
            || r.expires_at > super::now() + 7 * 86400
            || r.entry_usdc == 0
            || r.entry_usdc > l.max_position
            || r.strategy_hash != l.strategy.fingerprint()
        {
            return Err(err(
                "automation needs current strategy, bounded entry amount and expiry within seven days",
            ));
        }
        db.journal.automation = Some(Automation {
            id: uuid::Uuid::new_v4().to_string(),
            mode: r.mode,
            expires_at: r.expires_at,
            entry_usdc: r.entry_usdc,
            strategy: l.strategy.clone(),
        });
    }
    db.save().map_err(err)?;
    Ok(Json(
        json!({"automation":db.journal.automation,"existing_exit_mandates_preserved":true}),
    ))
}
async fn automatic_entry(l: Arc<Live>, candidates: &Value) -> Result<()> {
    let authorization = {
        let db = l.db.lock().await;
        if db.journal.halted
            || db.journal.automation_paused
            || db
                .journal
                .pending
                .as_ref()
                .is_some_and(|p| p.state != "preview" || p.expires > super::now())
        {
            return Ok(());
        }
        db.journal.automation.clone().filter(|a| {
            a.mode == "full"
                && a.expires_at > super::now()
                && a.strategy.fingerprint() == l.strategy.fingerprint()
        })
    };
    let Some(a) = authorization else {
        return Ok(());
    };
    for c in candidates["candidates"]
        .as_array()
        .ok_or("invalid live candidate response")?
    {
        let mint = c["mint"].as_str().ok_or("candidate mint missing")?;
        let stamp = c["detected_at"].as_u64().ok_or("candidate time missing")?;
        let expiry = c["expires_at"].as_u64().ok_or("candidate expiry missing")?;
        if !valid_mint(mint)
            || stamp > super::now()
            || super::now().saturating_sub(stamp) > a.strategy.max_data_age_secs
            || expiry <= super::now()
            || expiry > stamp + 120
            || c["strategy_hash"] != a.strategy.fingerprint()
        {
            continue;
        }
        {
            let db = l.db.lock().await;
            if db.journal.positions.contains_key(mint)
                || db
                    .journal
                    .entry_attempts
                    .get(mint)
                    .is_some_and(|v| *v >= stamp)
            {
                continue;
            }
        }
        let p = preview(
            State(l.clone()),
            Json(PreviewRequest {
                side: "buy".into(),
                mint: mint.into(),
                amount: a.entry_usdc,
            }),
        )
        .await
        .map_err(|(_, Json(v))| v.to_string())?
        .0;
        {
            let mut db = l.db.lock().await;
            if !db
                .journal
                .automation
                .as_ref()
                .is_some_and(|x| x.id == a.id && x.expires_at > super::now())
            {
                return Err("entry automation revoked".into());
            }
            let pending = db.journal.pending.as_mut().ok_or("entry preview missing")?;
            if Some(pending.id.as_str()) != p["order_id"].as_str() {
                return Err("entry preview changed".into());
            }
            pending.entry_mandate = Some(a.id.clone());
            db.journal.entry_attempts.insert(mint.into(), stamp);
            db.save()?;
        }
        let _ = execute(
            State(l.clone()),
            Json(Approval {
                order_id: p["order_id"].as_str().unwrap().into(),
                digest: p["digest"].as_str().unwrap().into(),
                confirm: true,
            }),
        )
        .await
        .map_err(|(_, Json(v))| v.to_string())?;
        break;
    }
    Ok(())
}
impl Db {
    fn save(&self) -> Result<()> {
        self.connection
            .execute(
                "UPDATE live_journal SET data=?1 WHERE id=1",
                params![
                    serde_json::to_string(&self.journal)
                        .map_err(|_| "journal serialization error")?
                ],
            )
            .map_err(|_| "live journal write failed")?;
        Ok(())
    }
}
pub struct Live {
    service_identity: String,
    strategy: super::research::Strategy,
    strict_external_checks: bool,
    http: reqwest::Client,
    rpc: String,
    jupiter: String,
    jupiter_url: String,
    goplus: super::goplus::Auth,
    vault: String,
    vault_token: String,
    key: String,
    key_version: u64,
    local_key: Option<super::local_key::LocalKey>,
    wallet: String,
    owner_token: String,
    agent_token: String,
    max_position: u64,
    max_daily: u64,
    max_sol_fee: u64,
    db: Mutex<Db>,
}

impl Live {
    pub async fn connect(path: &std::path::Path, general_token: &str) -> Result<Arc<Self>> {
        let rpc = env("SOLI_RPC_URL")?;
        let backend = std::env::var("SOLI_SIGNING_BACKEND").unwrap_or_else(|_| "vault".into());
        let local_key = match backend.as_str() {
            "local" => Some(super::local_key::LocalKey::from_env()?),
            "vault" => {
                if std::env::var("SOLI_PRIVATE_KEY").is_ok_and(|s| !s.is_empty()) {
                    return Err(
                        "local private key must not be supplied to the Vault backend".into(),
                    );
                }
                None
            }
            _ => return Err("SOLI_SIGNING_BACKEND must be local or vault".into()),
        };
        let vault = if local_key.is_none() {
            env("VAULT_ADDR")?
        } else {
            String::new()
        };
        for url in [&rpc, &vault].into_iter().filter(|s| !s.is_empty()) {
            let u = reqwest::Url::parse(url).map_err(|_| "invalid service URL")?;
            if u.scheme() != "https"
                && !(u.scheme() == "http"
                    && [Some("127.0.0.1"), Some("localhost")].contains(&u.host_str()))
            {
                return Err("service URLs require HTTPS except loopback".into());
            }
        }
        let owner_token = env("SOLI_OWNER_TOKEN")?;
        if owner_token.len() < 32 || owner_token == general_token {
            return Err(
                "owner approval token must be >=32 characters and different from agent token"
                    .into(),
            );
        }
        let key = if local_key.is_none() {
            env("VAULT_TRANSIT_KEY")?
        } else {
            String::new()
        };
        if local_key.is_none()
            && !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err("invalid Vault key name".into());
        }
        let max_position = env("SOLI_LIVE_MAX_POSITION_USDC")?
            .parse()
            .map_err(|_| "invalid position cap")?;
        let max_daily = env("SOLI_LIVE_DAILY_SPEND_USDC")?
            .parse()
            .map_err(|_| "invalid daily spend cap")?;
        if !(1_000_000..=100_000_000).contains(&max_position)
            || max_daily < max_position
            || max_daily > 500_000_000
        {
            return Err("v1 live caps: position 1-100 USDC, daily spend <=500 USDC".into());
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "HTTP client initialization failed")?;
        let (vault_token, key_version, wallet) = if let Some(local) = &local_key {
            local.verify()?;
            (String::new(), 0, local.wallet())
        } else {
            let vault_token = env("VAULT_TOKEN")?;
            let response = http
                .get(format!(
                    "{}/v1/transit/keys/{key}",
                    vault.trim_end_matches('/')
                ))
                .header("X-Vault-Token", &vault_token)
                .send()
                .await
                .map_err(|_| "Vault unavailable")?;
            if !response.status().is_success() {
                return Err("Vault key lookup rejected".into());
            }
            let data: Value = response
                .json()
                .await
                .map_err(|_| "invalid Vault response")?;
            let d = &data["data"];
            if d["type"] != "ed25519"
                || d["exportable"] != false
                || d["allow_plaintext_backup"] != false
                || d["derived"] != false
            {
                return Err("Vault requires non-derived, non-exportable Ed25519 key with plaintext backup disabled".into());
            }
            let key_version = d["latest_version"]
                .as_u64()
                .ok_or("missing Vault key version")?;
            let public = d["keys"][key_version.to_string()]["public_key"]
                .as_str()
                .ok_or("missing Vault public key")?;
            let bytes = B64.decode(public).map_err(|_| "invalid Vault public key")?;
            if bytes.len() != 32 {
                return Err("invalid Ed25519 public key length".into());
            }
            let wallet = bs58::encode(bytes).into_string();
            (vault_token, key_version, wallet)
        };
        let connection = Connection::open(path).map_err(|_| "cannot open live journal")?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS live_journal(id INTEGER PRIMARY KEY CHECK(id=1),data TEXT NOT NULL);").map_err(|_|"cannot create live journal")?;
        connection
            .execute(
                "INSERT OR IGNORE INTO live_journal VALUES(1,?1)",
                params![serde_json::to_string(&Journal::default()).unwrap()],
            )
            .map_err(|_| "cannot initialize live journal")?;
        let saved: String = connection
            .query_row("SELECT data FROM live_journal WHERE id=1", [], |r| r.get(0))
            .map_err(|_| "cannot read live journal")?;
        let mut journal: Journal =
            serde_json::from_str(&saved).map_err(|_| "corrupt live journal; refusing startup")?;
        if !journal.wallet.is_empty() && journal.wallet != wallet {
            return Err("journal belongs to a different wallet".into());
        }
        journal.wallet = wallet.clone();
        let live = Arc::new(Self {
            service_identity: super::access::service_identity()?,
            strategy: super::research::Strategy::from_env()?,
            strict_external_checks: true,
            http,
            rpc,
            jupiter: env("JUPITER_API_KEY")?,
            jupiter_url: "https://api.jup.ag/swap/v2/order".into(),
            goplus: super::goplus::Auth::from_env(),
            vault: vault.trim_end_matches('/').into(),
            vault_token,
            key,
            key_version,
            local_key,
            wallet,
            owner_token,
            agent_token: general_token.into(),
            max_position,
            max_daily,
            max_sol_fee: 5_000_000,
            db: Mutex::new(Db {
                connection,
                journal,
            }),
        });
        if live.rpc("getGenesisHash", json!([])).await?.as_str()
            != Some(super::data::MAINNET_GENESIS)
        {
            return Err("RPC is not Solana mainnet-beta".into());
        }
        live.db.lock().await.save()?;
        live.data().check_fresh_chain().await?;
        // Sign a domain-separated compatibility challenge, never a transaction, at startup.
        if live.local_key.is_none() {
            super::vault_admin::command(&["vault-key".into(), "verify".into(), live.key.clone()])
                .await
                .map_err(|_| "live startup blocked: Vault Ed25519 signing probe failed")?;
        }
        Ok(live)
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let r = self
            .http
            .post(&self.rpc)
            .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
            .send()
            .await
            .map_err(|_| "RPC request failed")?;
        if !r.status().is_success() {
            return Err("RPC HTTP error".into());
        }
        let v: Value = r.json().await.map_err(|_| "invalid RPC JSON")?;
        if v.get("error").is_some() {
            return Err(format!("RPC {method} rejected request"));
        }
        v.get("result").cloned().ok_or("missing RPC result".into())
    }

    async fn accounts(&self, keys: &[String]) -> Result<Vec<Value>> {
        let v = self
            .rpc(
                "getMultipleAccounts",
                json!([keys,{"encoding":"base64","commitment":"confirmed"}]),
            )
            .await?;
        let a = v["value"].as_array().ok_or("missing accounts")?;
        if a.len() != keys.len() {
            return Err("account count mismatch".into());
        }
        Ok(a.clone())
    }

    async fn inspect(&self, p: &Pending) -> Result<VersionedTransaction> {
        let raw = B64
            .decode(
                p.quote["transaction"]
                    .as_str()
                    .ok_or("missing assembled transaction")?,
            )
            .map_err(|_| "invalid transaction base64")?;
        if raw.len() > 1232 {
            return Err("oversized transaction".into());
        }
        let tx: VersionedTransaction =
            wincode::deserialize(&raw).map_err(|_| "invalid Solana transaction")?;
        tx.sanitize().map_err(|_| "transaction sanitation failed")?;
        let static_keys = tx.message.static_account_keys();
        if tx.message.header().num_required_signatures != 1
            || tx.signatures.len() != 1
            || static_keys.first().map(ToString::to_string).as_deref() != Some(&self.wallet)
        {
            return Err("transaction requires unexpected signer or fee payer".into());
        }
        let mut keys: Vec<String> = static_keys.iter().map(ToString::to_string).collect();
        let mut writable = vec![];
        let mut readonly = vec![];
        for lookup in tx.message.address_table_lookups().unwrap_or(&[]) {
            let v=self.rpc("getAccountInfo",json!([lookup.account_key.to_string(),{"encoding":"base64","commitment":"confirmed"}])).await?;
            let a = &v["value"];
            if a["owner"] != "AddressLookupTab1e1111111111111111111111111" {
                return Err("invalid lookup table owner".into());
            }
            let data = account_data(a)?;
            if data.len() < 56 || (data.len() - 56) % 32 != 0 {
                return Err("invalid lookup table data".into());
            }
            for (indices, target) in [
                (&lookup.writable_indexes, &mut writable),
                (&lookup.readonly_indexes, &mut readonly),
            ] {
                for i in indices {
                    let start = 56 + usize::from(*i) * 32;
                    let bytes = data
                        .get(start..start + 32)
                        .ok_or("lookup index out of range")?;
                    target.push(bs58::encode(bytes).into_string());
                }
            }
        }
        keys.extend(writable);
        keys.extend(readonly);
        validate_programs(&tx, &keys, p)?;
        // Observe every existing wallet token account too, so unrelated holdings cannot silently change.
        let owned=self.rpc("getTokenAccountsByOwner",json!([self.wallet,{"programId":TOKEN},{"encoding":"base64","commitment":"confirmed"}])).await?;
        for a in owned["value"]
            .as_array()
            .ok_or("missing owned token accounts")?
        {
            let key = a["pubkey"]
                .as_str()
                .ok_or("missing account address")?
                .to_string();
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        if keys.len() > 100 {
            return Err(
                "transaction observation exceeds 100 accounts; refusing partial inspection".into(),
            );
        }
        let before = self.accounts(&keys).await?;
        if before.iter().any(|a| a["owner"] == super::data::TOKEN_2022) {
            return Err("live route touches unsupported Token-2022 accounts".into());
        }
        let mint = self
            .rpc(
                "getAccountInfo",
                json!([p.mint,{"encoding":"base64","commitment":"confirmed"}]),
            )
            .await?;
        if mint["value"]["owner"] != TOKEN {
            return Err("v1 live supports classic SPL tokens only, not Token-2022".into());
        }
        let md = account_data(&mint["value"])?;
        if p.side == "buy"
            && (md.len() != 82
                || md[45] != 1
                || md[0..4] != [0, 0, 0, 0]
                || md[46..50] != [0, 0, 0, 0])
        {
            return Err("entry requires revoked mint and freeze authorities".into());
        }
        let sim=self.rpc("simulateTransaction",json!([B64.encode(raw),{"encoding":"base64","sigVerify":false,"replaceRecentBlockhash":true,"commitment":"confirmed","accounts":{"encoding":"base64","addresses":keys}}])).await?;
        let result = &sim["value"];
        if !result["err"].is_null() {
            return Err("transaction simulation failed".into());
        }
        let after = result["accounts"]
            .as_array()
            .ok_or("simulation returned no accounts")?;
        if after.len() != before.len() {
            return Err("simulation account count mismatch".into());
        }
        if after.iter().any(|a| a["owner"] == super::data::TOKEN_2022) {
            return Err("simulation creates or touches unsupported Token-2022 state".into());
        }
        validate_deltas(p, &self.wallet, &before, after, self.max_sol_fee)?;
        for ix in tx.message.instructions().iter().filter(|ix| {
            keys.get(ix.program_id_index as usize)
                .is_some_and(|k| k == ROUTER)
        }) {
            let route = super::jupiter_wire::decode(&ix.data)?;
            let slot = |role: usize| {
                ix.accounts
                    .get(role)
                    .map(|i| *i as usize)
                    .filter(|i| *i < keys.len())
                    .ok_or("missing Jupiter account role")
            };
            if keys[slot(route.authority)?] != self.wallet {
                return Err("route authority is not this wallet".into());
            }
            let source = token_balance(&before[slot(route.source)?], &self.wallet)?
                .ok_or("route source is not wallet owned")?;
            let destination = token_balance(&after[slot(route.destination)?], &self.wallet)?
                .ok_or("route output is not wallet owned")?;
            let (input, output) = if p.side == "buy" {
                (USDC, p.mint.as_str())
            } else {
                (p.mint.as_str(), USDC)
            };
            if source.0 != input || destination.0 != output {
                return Err("route source/destination mint mismatch".into());
            }
            if let Some(extra) = route.destination_extra {
                let i = slot(extra)?;
                if keys[i] != ROUTER
                    && !token_balance(&after[i], &self.wallet)?.is_some_and(|b| b.0 == output)
                {
                    return Err("optional route receiver is not wallet owned".into());
                }
            }
        }
        Ok(tx)
    }

    async fn sign(&self, tx: &mut VersionedTransaction) -> Result<String> {
        let message = tx.message.serialize();
        let sig = if let Some(local) = &self.local_key {
            local.sign(&message)
        } else {
            let r=self.http.post(format!("{}/v1/transit/sign/{}",self.vault,self.key)).header("X-Vault-Token",&self.vault_token)
            .json(&json!({"input":B64.encode(&message),"key_version":self.key_version,"prehashed":false})).send().await.map_err(|_|"Vault signing unavailable")?;
            if !r.status().is_success() {
                return Err("Vault signing rejected".into());
            }
            let v: Value = r.json().await.map_err(|_| "invalid signing response")?;
            let encoded = v["data"]["signature"]
                .as_str()
                .ok_or("missing Vault signature")?;
            let prefix = format!("vault:v{}:", self.key_version);
            B64.decode(
                encoded
                    .strip_prefix(&prefix)
                    .ok_or("unexpected signing key version")?,
            )
            .map_err(|_| "invalid Vault signature encoding")?
        };
        let signature =
            Signature::try_from(sig.as_slice()).map_err(|_| "invalid signature size")?;
        let pk = Pubkey::from_str(&self.wallet).map_err(|_| "invalid wallet")?;
        if !signature.verify(pk.as_ref(), &message) {
            return Err("signer signature verification failed".into());
        }
        tx.signatures[0] = signature;
        tx.verify_and_hash_message()
            .map_err(|_| "signed transaction verification failed")?;
        Ok(signature.to_string())
    }
}

impl Live {
    fn data(&self) -> super::data::Data {
        super::data::Data {
            jupiter_url: self.jupiter_url.clone(),
            http: self.http.clone(),
            rpc: self.rpc.clone(),
            jupiter_key: self.jupiter.clone(),
            goplus: self.goplus.clone(),
        }
    }
    async fn refresh_marks(&self) -> Result<()> {
        let mut db = self.db.lock().await;
        if db
            .journal
            .pending
            .as_ref()
            .is_some_and(|p| p.state == "submitted")
        {
            return Ok(());
        }
        let mut holdings: BTreeMap<String, u64> = BTreeMap::new();
        let owned=self.rpc("getTokenAccountsByOwner",json!([self.wallet,{"programId":TOKEN},{"encoding":"jsonParsed","commitment":"confirmed"}])).await?;
        for a in owned["value"].as_array().ok_or("missing holdings")? {
            let info = &a["account"]["data"]["parsed"]["info"];
            let mint = info["mint"].as_str().ok_or("missing holding mint")?;
            let amount = units(&info["tokenAmount"]["amount"])?;
            let current = holdings.entry(mint.into()).or_default();
            *current = current.checked_add(amount).ok_or("holding overflow")?;
        }
        let mut equity = holdings.get(USDC).copied().unwrap_or(0);
        let data = self.data();
        let mut positions = vec![];
        let mut complete = true;
        for (mint, cost) in &db.journal.positions {
            let amount = holdings.get(mint).copied().unwrap_or(0);
            let quote = if amount > 0 {
                data.quote(mint, USDC, amount).await
            } else {
                Err("tracked holding missing; manual reconciliation required".into())
            };
            let liquidation = match quote
                .and_then(|q| super::data::amount_field(&q, "otherAmountThreshold"))
            {
                Ok(v) => v.saturating_sub(100_000),
                Err(reason) => {
                    complete = false;
                    positions.push(json!({"mint":mint,"cost_usdc_atomic":cost,"error":reason,"liquidation_usdc_atomic":null}));
                    continue;
                }
            };
            equity = equity.checked_add(liquidation).ok_or("equity overflow")?;
            let age = super::now()
                .saturating_sub(db.journal.opened.get(mint).copied().unwrap_or(super::now()));
            let mandate = db
                .journal
                .exit_mandates
                .get(mint)
                .filter(|m| m.expires_at > super::now());
            let strategy = mandate.map_or(&self.strategy, |m| &m.strategy);
            positions.push(json!({"mint":mint,"amount":amount,"cost_usdc_atomic":cost,"liquidation_usdc_atomic":liquidation,"exit_recommendation":strategy.exit(*cost,liquidation,age),"strategy_hash":strategy.fingerprint(),"automatic_exits_authorized":mandate.is_some(),"execution_requires_owner_approval":mandate.is_none()}));
        }
        let day = super::now() / 86400;
        if complete
            && db
                .journal
                .day_equity
                .is_none_or(|(d, b)| d != day || (b == 0 && equity > 0))
        {
            db.journal.day_equity = Some((day, equity));
        }
        let baseline = db.journal.day_equity.map_or(0, |(_, v)| v);
        if complete {
            db.journal.peak_equity = db.journal.peak_equity.max(equity);
        }
        if complete && equity.saturating_mul(100) < baseline.saturating_mul(97) {
            db.journal.halted = true;
        }
        let peak = db.journal.peak_equity;
        db.journal.marks = json!({"timestamp":super::now(),"complete":complete,"equity_usdc_atomic":complete.then_some(equity),"scope":"USDC and tracked positions; excludes native SOL and unrelated tokens","positions":positions,"day_start_equity_usdc_atomic":baseline,"drawdown_pct":complete.then(||if peak>0{100.0*(1.0-equity as f64/peak as f64).max(0.0)}else{0.0})});
        db.save()
    }
}

fn account_data(v: &Value) -> Result<Vec<u8>> {
    B64.decode(v["data"][0].as_str().ok_or("missing base64 account data")?)
        .map_err(|_| "invalid account data".into())
}
fn validate_programs(tx: &VersionedTransaction, keys: &[String], p: &Pending) -> Result<()> {
    let allowed = [
        ROUTER,
        TOKEN,
        "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
        "ComputeBudget111111111111111111111111111111",
        "11111111111111111111111111111111",
    ];
    let mut route = 0;
    for ix in tx.message.instructions() {
        let program = keys
            .get(usize::from(ix.program_id_index))
            .ok_or("invalid program index")?;
        if !allowed.contains(&program.as_str()) {
            return Err("transaction contains unsupported program".into());
        }
        if program == ROUTER {
            route += 1;
            let bounds = super::jupiter_wire::decode(&ix.data)?;
            if bounds.input != p.input || bounds.minimum < p.minimum {
                return Err(
                    "encoded Jupiter input/minimum output does not enforce this order".into(),
                );
            }
        }
        if program == TOKEN && ![3, 12, 17].contains(ix.data.first().unwrap_or(&255)) {
            return Err("token authority/approval/close instruction is forbidden".into());
        }
        if program == "11111111111111111111111111111111" {
            return Err("top-level system instructions are forbidden in v1 live swaps".into());
        }
    }
    if route != 1 {
        return Err("missing allowlisted Jupiter route instruction".into());
    }
    Ok(())
}

fn token_balance(v: &Value, wallet: &str) -> Result<Option<(String, u64, Vec<u8>)>> {
    if v.is_null() || v["owner"] != TOKEN {
        return Ok(None);
    }
    let data = account_data(v)?;
    if data.len() != 165 {
        return Err("unsupported SPL account layout".into());
    }
    if bs58::encode(&data[32..64]).into_string() != wallet {
        return Ok(None);
    }
    if data[72..76] != [0, 0, 0, 0] || data[129..133] != [0, 0, 0, 0] || data[108] != 1 {
        return Err("delegated, frozen, or custom-close-authority wallet account rejected".into());
    }
    let amount = u64::from_le_bytes(data[64..72].try_into().unwrap());
    let mint = bs58::encode(&data[0..32]).into_string();
    let mut immutable = data;
    immutable[64..72].fill(0);
    Ok(Some((mint, amount, immutable)))
}

fn validate_deltas(
    p: &Pending,
    wallet: &str,
    before: &[Value],
    after: &[Value],
    max_fee: u64,
) -> Result<()> {
    let mut deltas: BTreeMap<String, i128> = BTreeMap::new();
    for (b, a) in before.iter().zip(after) {
        let old = token_balance(b, wallet)?;
        let new = token_balance(a, wallet)?;
        if let Some((_, _, state)) = &old
            && new.as_ref().map(|v| &v.2) != Some(state)
        {
            return Err("wallet token account authority/state changed or account closed".into());
        }
        if let Some((m, n, _)) = old {
            *deltas.entry(m).or_default() -= i128::from(n);
        }
        if let Some((m, n, _)) = new {
            *deltas.entry(m).or_default() += i128::from(n);
        }
    }
    let (input, output) = if p.side == "buy" {
        (USDC, p.mint.as_str())
    } else {
        (p.mint.as_str(), USDC)
    };
    if deltas.get(input).copied().unwrap_or(0) != -i128::from(p.input)
        || deltas.get(output).copied().unwrap_or(0) < i128::from(p.minimum)
        || deltas
            .iter()
            .any(|(mint, amount)| mint != input && mint != output && *amount != 0)
    {
        return Err("simulated token deltas do not match approved swap".into());
    }
    let b = before.first().ok_or("missing fee payer")?;
    let a = after.first().ok_or("missing simulated fee payer")?;
    if b["owner"] != "11111111111111111111111111111111"
        || a["owner"] != b["owner"]
        || account_data(b)? != account_data(a)?
    {
        return Err("fee payer state changed".into());
    }
    let old = units(&b["lamports"])?;
    let new = units(&a["lamports"])?;
    if old.saturating_sub(new) > max_fee || new < 10_000_000 {
        return Err("SOL costs exceed cap or SOL reserve below 0.01".into());
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewRequest {
    side: String,
    mint: String,
    amount: u64,
}
async fn preview(State(live): State<Arc<Live>>, Json(r): Json<PreviewRequest>) -> Reply {
    if !valid_mint(&r.mint)
        || r.mint == USDC
        || r.amount == 0
        || !["buy", "sell"].contains(&r.side.as_str())
    {
        return Err(err("invalid order"));
    }
    if live.strict_external_checks {
        live.refresh_marks().await.map_err(err)?;
        if r.side == "buy" {
            let risk = live.data().risk(&r.mint).await.map_err(err)?;
            if !risk.accepted {
                return Err(err(format!(
                    "token safety gate rejected: {}",
                    risk.reasons.join("; ")
                )));
            }
        }
    }
    let mut db = live.db.lock().await;
    if r.side == "buy" && db.journal.marks["complete"] == false {
        return Err(err("incomplete portfolio marks block new entries"));
    }
    if db
        .journal
        .pending
        .as_ref()
        .is_some_and(|p| p.state != "preview" || p.expires > super::now())
    {
        return Err(err(
            "another order is pending; reconcile or cancel it first",
        ));
    }
    let day = super::now() / 86400;
    if db.journal.day != day {
        db.journal.day = day;
        db.journal.spent = 0;
    }
    if r.side == "buy" {
        if live.strict_external_checks {
            let capital = db
                .journal
                .day_equity
                .map(|(_, v)| v)
                .ok_or_else(|| err("no marked equity baseline"))?;
            let existing = db.journal.positions.values().copied().sum::<u64>();
            if r.amount.saturating_add(100_000).saturating_mul(100) > capital.saturating_mul(5)
                || existing.saturating_add(r.amount).saturating_mul(100)
                    > capital.saturating_mul(10)
            {
                return Err(err(
                    "live position/exposure exceeds 5%/10% of marked starting equity",
                ));
            }
        }
        if db.journal.halted
            || db.journal.automation_paused
            || r.amount > live.max_position
            || db.journal.spent.saturating_add(r.amount) > live.max_daily
            || db.journal.positions.len() >= 2
            || db.journal.positions.contains_key(&r.mint)
        {
            return Err(err(
                "entry blocked by halt, position, or daily spend limits",
            ));
        }
    } else if !db.journal.positions.contains_key(&r.mint) {
        return Err(err("sell requires a tracked live position"));
    }
    let (input, output) = if r.side == "buy" {
        (USDC, r.mint.as_str())
    } else {
        (r.mint.as_str(), USDC)
    };
    let quote = live
        .http
        .get(&live.jupiter_url)
        .header("x-api-key", &live.jupiter)
        .query(&[
            ("inputMint", input),
            ("outputMint", output),
            ("amount", &r.amount.to_string()),
            ("taker", &live.wallet),
            ("slippageBps", "100"),
            ("mode", "manual"),
        ])
        .send()
        .await
        .map_err(|_| err("Jupiter unavailable"))?;
    if !quote.status().is_success() {
        return Err(err("Jupiter quote rejected"));
    }
    let q: Value = quote
        .json()
        .await
        .map_err(|_| err("invalid Jupiter response"))?;
    if q["inputMint"] != input
        || q["outputMint"] != output
        || units(&q["inAmount"]).map_err(err)? != r.amount
    {
        return Err(err("quote input mismatch"));
    }
    let minimum = units(&q["otherAmountThreshold"]).map_err(err)?;
    let quoted_output = units(&q["outAmount"]).map_err(err)?;
    if minimum == 0
        || minimum > quoted_output
        || !super::data::within_slippage(quoted_output, minimum)
    {
        return Err(err("invalid minimum output"));
    }
    let impact = q["priceImpactPct"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .or(q["priceImpactPct"].as_f64())
        .ok_or_else(|| err("missing price impact"))?;
    if !impact.is_finite() || impact.abs() > 1.0 {
        return Err(err("price impact exceeds 1%"));
    }
    if r.side == "sell" {
        let owned=live.rpc("getTokenAccountsByOwner",json!([live.wallet,{"mint":r.mint},{"encoding":"jsonParsed","commitment":"confirmed"}])).await.map_err(err)?;
        let amount = owned["value"]
            .as_array()
            .ok_or_else(|| err("missing wallet holdings"))?
            .iter()
            .try_fold(0u64, |sum, a| {
                sum.checked_add(units(
                    &a["account"]["data"]["parsed"]["info"]["tokenAmount"]["amount"],
                )?)
                .ok_or_else(|| String::from("amount overflow"))
            })
            .map_err(err)?;
        if amount != r.amount {
            return Err(err(
                "live v1 requires full-position sell amount from current on-chain balance",
            ));
        }
    }
    let raw = q["transaction"]
        .as_str()
        .ok_or_else(|| err("no executable transaction"))?;
    let digest = format!(
        "{:x}",
        Sha256::digest(B64.decode(raw).map_err(|_| err("invalid transaction"))?)
    );
    let p = Pending {
        exit_hash: None,
        entry_mandate: None,
        requested_by: None,
        origin: None,
        id: uuid::Uuid::new_v4().to_string(),
        digest,
        side: r.side,
        mint: r.mint,
        input: r.amount,
        minimum,
        quote: q,
        expires: super::now() + 60,
        signature: None,
        state: "preview".into(),
    };
    live.inspect(&p).await.map_err(err)?;
    db.journal.pending = Some(p.clone());
    db.save().map_err(err)?;
    Ok(Json(
        json!({"mode":"live","wallet":live.wallet,"order_id":p.id,"digest":p.digest,"side":p.side,"mint":p.mint,"input_amount":p.input,"minimum_output":p.minimum,"expires_at":p.expires,"max_sol_cost_lamports":live.max_sol_fee,"approval_required":true}),
    ))
}

async fn agent_preview(
    State(live): State<Arc<Live>>,
    headers: HeaderMap,
    input: Json<PreviewRequest>,
) -> Reply {
    let mut result = preview(State(live.clone()), input).await?.0;
    let actor = headers
        .get("x-soli-agent-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("operator");
    let mut db = live.db.lock().await;
    let p = db
        .journal
        .pending
        .as_mut()
        .ok_or_else(|| err("preview disappeared"))?;
    if result["order_id"].as_str() != Some(p.id.as_str()) {
        return Err(err("preview changed"));
    }
    p.requested_by = Some(actor.into());
    db.save().map_err(err)?;
    result["requested_by"] = json!(actor);
    result["owner_review_url"] = json!("http://127.0.0.1:8788/approve");
    Ok(Json(result))
}

async fn owner_pending(State(l): State<Arc<Live>>) -> Reply {
    let db = l.db.lock().await;
    Ok(Json(
        json!({"wallet":l.wallet,"strategy_hash":l.strategy.fingerprint(),"strategy":l.strategy,"tracked_positions":db.journal.positions,"exit_mandates":db.journal.exit_mandates,"pending":db.journal.pending.as_ref().map(|p|json!({
            "order_id":p.id,"digest":p.digest,"state":p.state,"expires_at":p.expires,"expired":p.expires<=super::now(),
            "side":p.side,"mint":p.mint,"input_amount":p.input,"minimum_output":p.minimum,"requested_by":p.requested_by,
            "origin":p.origin,"max_sol_cost_lamports":l.max_sol_fee
        }))}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExitAuthorization {
    mint: String,
    enabled: bool,
    expires_at: u64,
    strategy_hash: String,
}
async fn authorize_exit(State(l): State<Arc<Live>>, Json(r): Json<ExitAuthorization>) -> Reply {
    if !valid_mint(&r.mint) {
        return Err(err("invalid mint"));
    }
    let mut db = l.db.lock().await;
    if r.enabled {
        if r.strategy_hash != l.strategy.fingerprint()
            || r.expires_at <= super::now()
            || r.expires_at > super::now() + 7 * 86400
            || !db.journal.positions.contains_key(&r.mint)
        {
            return Err(err(
                "authorize a tracked position, current strategy hash and expiry within seven days",
            ));
        }
        db.journal.exit_mandates.insert(
            r.mint.clone(),
            ExitMandate {
                forced: false,
                retry_at: 0,
                strategy: l.strategy.clone(),
                expires_at: r.expires_at,
            },
        );
    } else {
        db.journal.exit_mandates.remove(&r.mint);
    }
    db.save().map_err(err)?;
    Ok(Json(
        json!({"mint":r.mint,"automatic_exits_enabled":r.enabled,"expires_at":r.expires_at,"strategy_hash":l.strategy.fingerprint()}),
    ))
}
async fn automatic_exit(l: Arc<Live>) -> Result<()> {
    let candidate = {
        let db = l.db.lock().await;
        if db
            .journal
            .pending
            .as_ref()
            .is_some_and(|p| p.state != "preview" || p.expires > super::now())
        {
            return Ok(());
        }
        db.journal.marks["positions"]
            .as_array()
            .and_then(|positions| {
                positions.iter().find_map(|p| {
                    if !p["amount"].is_u64() || !p["liquidation_usdc_atomic"].is_u64() {
                        return None;
                    }
                    let mint = p["mint"].as_str()?;
                    let mandate = db.journal.exit_mandates.get(mint)?;
                    if db.journal.automation_paused && !mandate.forced {
                        return None;
                    }
                    if mandate.retry_at > super::now()
                        || mandate.expires_at <= super::now()
                        || db.journal.marks["timestamp"]
                            .as_u64()?
                            .abs_diff(super::now())
                            > mandate.strategy.max_data_age_secs
                    {
                        return None;
                    }
                    let age = super::now().saturating_sub(*db.journal.opened.get(mint)?);
                    if !mandate.forced {
                        mandate.strategy.exit(
                            p["cost_usdc_atomic"].as_u64()?,
                            p["liquidation_usdc_atomic"].as_u64()?,
                            age,
                        )?;
                    }
                    Some((
                        mint.to_owned(),
                        p["amount"].as_u64()?,
                        mandate.strategy.fingerprint(),
                    ))
                })
            })
    };
    if let Some((mint, amount, hash)) = candidate {
        let result = match preview(
            State(l.clone()),
            Json(PreviewRequest {
                side: "sell".into(),
                mint: mint.clone(),
                amount,
            }),
        )
        .await
        {
            Ok(result) => result.0,
            Err((_, Json(reason))) => {
                let mut db = l.db.lock().await;
                if let Some(m) = db.journal.exit_mandates.get_mut(&mint) {
                    m.retry_at = super::now() + 30;
                    db.save()?;
                }
                return Err(reason.to_string());
            }
        };
        let mut db = l.db.lock().await;
        let still_triggered = db.journal.exit_mandates.get(&mint).is_some_and(|m| {
            let Some(p) = db.journal.pending.as_ref() else {
                return false;
            };
            m.expires_at > super::now()
                && m.strategy.fingerprint() == hash
                && (m.forced
                    || m.strategy
                        .exit(
                            db.journal.positions.get(&mint).copied().unwrap_or(0),
                            p.minimum.saturating_sub(100_000),
                            super::now().saturating_sub(
                                db.journal
                                    .opened
                                    .get(&mint)
                                    .copied()
                                    .unwrap_or(super::now()),
                            ),
                        )
                        .is_some())
        });
        if !still_triggered {
            if db
                .journal
                .pending
                .as_ref()
                .is_some_and(|p| result["order_id"].as_str() == Some(p.id.as_str()))
            {
                db.journal.pending = None;
                db.save()?;
            }
            return Ok(());
        }
        if let Some(p) = db.journal.pending.as_mut() {
            if result["order_id"].as_str() != Some(p.id.as_str()) {
                return Err("exit preview changed".into());
            }
            p.exit_hash = Some(hash.clone());
            p.requested_by = Some(format!("owner-authorized-exit:{hash}"));
            db.save()?;
        }
        drop(db);
        let _ = execute(
            State(l),
            Json(Approval {
                order_id: result["order_id"]
                    .as_str()
                    .ok_or("missing order ID")?
                    .into(),
                digest: result["digest"].as_str().ok_or("missing digest")?.into(),
                confirm: true,
            }),
        )
        .await
        .map_err(|(_, Json(v))| v.to_string())?;
    }
    Ok(())
}

async fn owner_auth(State(l): State<Arc<Live>>, r: Request, next: Next) -> Response {
    if r.headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        != Some(format!("Bearer {}", l.owner_token).as_str())
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"owner approval credential required"})),
        )
            .into_response();
    }
    next.run(r).await
}

async fn reader_auth(State(l): State<Arc<Live>>, r: Request, next: Next) -> Response {
    let actual = r
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if actual != format!("Bearer {}", l.agent_token)
        && actual != format!("Bearer {}", l.owner_token)
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"invalid API token"})),
        )
            .into_response();
    }
    next.run(r).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    order_id: String,
    digest: String,
    confirm: bool,
}
async fn execute(State(l): State<Arc<Live>>, Json(a): Json<Approval>) -> Reply {
    let mut db = l.db.lock().await;
    if let Some(done) = db.journal.completed.get(&a.order_id) {
        return Ok(Json(done.clone()));
    }
    let mut p = db
        .journal
        .pending
        .clone()
        .ok_or_else(|| err("no pending order"))?;
    if !a.confirm
        || a.order_id != p.id
        || a.digest != p.digest
        || p.state != "preview"
        || super::now() > p.expires
    {
        return Err(err("approval mismatch, already submitted, or expired"));
    }
    if (db.journal.halted || db.journal.automation_paused) && p.side == "buy" {
        return Err(err("entries halted"));
    }
    let today = super::now() / 86400;
    if db.journal.day != today {
        db.journal.day = today;
        db.journal.spent = 0;
    }
    if p.side == "buy"
        && (p.input > l.max_position || db.journal.spent.saturating_add(p.input) > l.max_daily)
    {
        return Err(err("approved order exceeds current spend caps"));
    }
    let mut tx = l.inspect(&p).await.map_err(err)?;
    // Recheck actual blockhash validity before signing; no silent rebuilding after approval.
    let valid = l
        .rpc(
            "isBlockhashValid",
            json!([tx.message.recent_blockhash().to_string(),{"commitment":"confirmed"}]),
        )
        .await
        .map_err(err)?;
    if valid["value"] != true || super::now() > p.expires {
        return Err(err("approved transaction expired"));
    }
    if let Some(hash) = &p.exit_hash
        && !db.journal.exit_mandates.get(&p.mint).is_some_and(|m| {
            p.side == "sell" && m.expires_at > super::now() && m.strategy.fingerprint() == *hash
        })
    {
        return Err(err("automatic exit authorization revoked or expired"));
    }
    if db.journal.automation_paused
        && p.exit_hash.is_some()
        && !db
            .journal
            .exit_mandates
            .get(&p.mint)
            .is_some_and(|m| m.forced)
    {
        return Err(err("automated exits paused"));
    }
    if let Some(id) = &p.entry_mandate
        && !db.journal.automation.as_ref().is_some_and(|a| {
            a.mode == "full"
                && a.id == *id
                && a.expires_at > super::now()
                && a.strategy.fingerprint() == l.strategy.fingerprint()
                && p.side == "buy"
                && p.input <= a.entry_usdc
        })
    {
        return Err(err("automatic entry authorization revoked or expired"));
    }
    let signature = l.sign(&mut tx).await.map_err(err)?;
    p.signature = Some(signature.clone());
    p.state = "submitted".into();
    db.journal.pending = Some(p.clone());
    db.save().map_err(err)?;
    // Durable intent is written before broadcast. A timeout NEVER triggers a second transaction.
    let wire = B64.encode(wincode::serialize(&tx).map_err(|_| err("transaction encoding failed"))?);
    let send=l.rpc("sendTransaction",json!([wire,{"encoding":"base64","skipPreflight":false,"preflightCommitment":"confirmed","maxRetries":2}])).await;
    match send {
        Ok(v) if v.as_str() == Some(&signature) => Ok(Json(
            json!({"order_id":p.id,"state":"submitted","signature":signature,"next":"call reconcile; submission is not confirmation"}),
        )),
        _ => Ok(Json(
            json!({"order_id":p.id,"state":"unknown","signature":signature,"next":"reconcile required; all new orders blocked"}),
        )),
    }
}

async fn reconcile(State(l): State<Arc<Live>>) -> Reply {
    let mut db = l.db.lock().await;
    let p = db
        .journal
        .pending
        .clone()
        .ok_or_else(|| err("no pending order"))?;
    let signature = p
        .signature
        .as_ref()
        .ok_or_else(|| err("preview has not been submitted"))?;
    let receipt=l.rpc("getTransaction",json!([signature,{"encoding":"jsonParsed","commitment":"finalized","maxSupportedTransactionVersion":0}])).await.map_err(err)?;
    if receipt.is_null() {
        return Ok(Json(
            json!({"order_id":p.id,"state":"unresolved","signature":signature,"new_orders_blocked":true}),
        ));
    }
    let meta = &receipt["meta"];
    if meta.is_null() {
        return Err(err("receipt has no metadata; unresolved"));
    }
    let succeeded = meta["err"].is_null();
    if succeeded {
        if p.side == "buy" {
            // A preview/submission can straddle UTC midnight. Charge the day of
            // reconciliation conservatively, never reset away an overnight buy.
            let day = super::now() / 86400;
            if db.journal.day != day {
                db.journal.day = day;
                db.journal.spent = 0;
            }
            db.journal.positions.insert(p.mint.clone(), p.input);
            db.journal.opened.insert(p.mint.clone(), super::now());
            if let Some(a) = db
                .journal
                .automation
                .clone()
                .filter(|a| a.expires_at > super::now())
            {
                db.journal.exit_mandates.insert(
                    p.mint.clone(),
                    ExitMandate {
                        forced: false,
                        retry_at: 0,
                        strategy: a.strategy,
                        expires_at: a.expires_at,
                    },
                );
            }
            db.journal.spent = db
                .journal
                .spent
                .checked_add(p.input)
                .ok_or_else(|| err("spend counter overflow"))?;
        } else {
            db.journal.positions.remove(&p.mint);
            db.journal.opened.remove(&p.mint);
            db.journal.exit_mandates.remove(&p.mint);
        }
    }
    let result = json!({"order_id":p.id,"side":p.side,"mint":p.mint,"input_amount":p.input,"sequence":db.journal.completed.len(),"finalized_at":super::now(),"usdc_delta_atomic":receipt_usdc(&receipt,&l.wallet).map(|v|v.to_string()),"state":if succeeded{"finalized"}else{"failed"},"signature":signature,"receipt":receipt});
    db.journal.completed.insert(p.id, result.clone());
    db.journal.pending = None;
    db.save().map_err(err)?;
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    halted: bool,
}
async fn halt(State(l): State<Arc<Live>>, Json(c): Json<Control>) -> Reply {
    let mut db = l.db.lock().await;
    db.journal.halted = c.halted;
    db.save().map_err(err)?;
    Ok(Json(json!({"halted":c.halted,"exits_allowed":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PauseRequest {
    scope: String,
    paused: bool,
}
async fn pause_cycle(State(l): State<Arc<Live>>, Json(r): Json<PauseRequest>) -> Reply {
    let mut db = l.db.lock().await;
    match r.scope.as_str() {
        "entries" => db.journal.halted = r.paused,
        "all" => db.journal.automation_paused = r.paused,
        _ => return Err(err("pause scope must be entries or all")),
    };
    db.save().map_err(err)?;
    Ok(Json(
        json!({"entries_paused":db.journal.halted,"automation_paused":db.journal.automation_paused,"reconciliation_continues":true,"owner_force_close_available":true}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseRequest {
    mint: String,
    confirm: bool,
}
async fn force_close(State(l): State<Arc<Live>>, Json(r): Json<CloseRequest>) -> Reply {
    if !r.confirm {
        return Err(err("explicit owner confirmation required"));
    }
    let mut db = l.db.lock().await;
    if r.mint == "all" {
        db.journal.halted = true;
    }
    let mints = if r.mint == "all" {
        db.journal.positions.keys().cloned().collect::<Vec<_>>()
    } else if db.journal.positions.contains_key(&r.mint) {
        vec![r.mint]
    } else {
        return Err(err("position is not tracked"));
    };
    for mint in &mints {
        db.journal.exit_mandates.insert(
            mint.clone(),
            ExitMandate {
                forced: true,
                retry_at: 0,
                strategy: l.strategy.clone(),
                expires_at: super::now() + 3600,
            },
        );
    }
    if db
        .journal
        .pending
        .as_ref()
        .is_some_and(|p| p.state == "preview")
    {
        db.journal.pending = None;
    }
    db.save().map_err(err)?;
    Ok(Json(
        json!({"queued":mints,"scope":"tracked positions only","execution":"sequential fresh full-position sells; unresolved broadcasts block the queue","expires_in_seconds":3600}),
    ))
}
async fn cancel(State(l): State<Arc<Live>>) -> Reply {
    let mut db = l.db.lock().await;
    if db
        .journal
        .pending
        .as_ref()
        .is_some_and(|p| p.state != "preview")
    {
        return Err(err("submitted orders cannot be cleared; reconcile first"));
    }
    db.journal.pending = None;
    db.save().map_err(err)?;
    Ok(Json(json!({"cancelled":true})))
}
async fn base_status(State(l): State<Arc<Live>>) -> Reply {
    let db = l.db.lock().await;
    Ok(Json(
        json!({"mode":"live","wallet":l.wallet,"service_identity":l.service_identity,"strategy_hash":l.strategy.fingerprint(),"halted":db.journal.halted,"spent_today_usdc_atomic":if db.journal.day==super::now()/86400{db.journal.spent}else{0},"max_daily_spend_usdc_atomic":l.max_daily,"tracked_positions":db.journal.positions,"marks":db.journal.marks,"completed_order_sample":db.journal.completed.values().take(10).collect::<Vec<_>>(),"pending":db.journal.pending.as_ref().map(|p|json!({"id":p.id,"state":p.state,"signature":p.signature,"expires_at":p.expires})),"owner_approval_required":true}),
    ))
}
fn receipt_usdc(receipt: &Value, wallet: &str) -> Option<i128> {
    let pre = receipt["meta"]["preTokenBalances"].as_array()?;
    let post = receipt["meta"]["postTokenBalances"].as_array()?;
    let indexes = pre
        .iter()
        .chain(post)
        .filter(|v| v["owner"] == wallet && v["mint"] == USDC)
        .filter_map(|v| v["accountIndex"].as_u64())
        .collect::<std::collections::BTreeSet<_>>();
    if indexes.is_empty() {
        return None;
    }
    let sum = |rows: &Vec<Value>| -> Option<i128> {
        rows.iter()
            .filter(|v| {
                v["mint"] == USDC
                    && v["accountIndex"]
                        .as_u64()
                        .is_some_and(|i| indexes.contains(&i))
            })
            .try_fold(0i128, |a, v| {
                a.checked_add(
                    v["uiTokenAmount"]["amount"]
                        .as_str()?
                        .parse::<i128>()
                        .ok()?,
                )
            })
    };
    sum(post)?.checked_sub(sum(pre)?)
}
async fn status(State(l): State<Arc<Live>>) -> Reply {
    let mut value = base_status(State(l.clone())).await?.0;
    let db = l.db.lock().await;
    value["automation"] = json!(db.journal.automation);
    value["trading_mode"] = json!(
        db.journal
            .automation
            .as_ref()
            .filter(|a| a.expires_at > super::now())
            .map_or("manual", |a| a.mode.as_str())
    );
    value["owner_approval_required"] = json!(value["trading_mode"] != "full");
    value["owner_authorization_required"] = json!(true);
    value["automation_paused"] = json!(db.journal.automation_paused);
    value["exit_mandates"] = json!(db.journal.exit_mandates);
    let mut history = db
        .journal
        .completed
        .values()
        .filter(|v| v["state"] == "finalized")
        .collect::<Vec<_>>();
    history.sort_by_key(|v| v["sequence"].as_u64().unwrap_or(0));
    let mut e = crate::learning::Evidence {
        source: "live_finalized_receipts".into(),
        complete: true,
        ..Default::default()
    };
    let mut costs = BTreeMap::new();
    for v in history {
        let Some(delta) = v["usdc_delta_atomic"]
            .as_str()
            .and_then(|s| s.parse::<i128>().ok())
        else {
            e.complete = false;
            continue;
        };
        let ts = v["finalized_at"].as_u64().unwrap_or(0);
        if e.first == 0 {
            e.first = ts;
        }
        e.last = ts;
        let mint = v["mint"].as_str().unwrap_or("");
        if v["side"] == "buy" {
            costs.insert(mint, -delta);
        } else if let Some(cost) = costs.remove(mint) {
            let pnl = delta - cost;
            e.closed += 1;
            e.wins += u64::from(pnl > 0);
            e.net_usdc_atomic += pnl;
        } else {
            e.complete = false;
        }
    }
    value["learning_evidence"] = json!(e);
    let trades = db
        .journal
        .completed
        .values()
        .filter(|v| v["state"] == "finalized")
        .collect::<Vec<_>>();
    let delta = trades.iter().try_fold(0i128, |a, v| {
        a.checked_add(v["usdc_delta_atomic"].as_str()?.parse::<i128>().ok()?)
    });
    let open_cost = db
        .journal
        .positions
        .values()
        .map(|v| *v as i128)
        .sum::<i128>();
    value["pnl"] = json!({"realized_usdc_atomic":delta.and_then(|v|v.checked_add(open_cost)).map(|v|v.to_string()),"receipt_coverage_complete":delta.is_some(),"closed_trades":trades.iter().filter(|v|v["side"]=="sell").count(),"native_fees_lamports":db.journal.completed.values().filter_map(|v|v["receipt"]["meta"]["fee"].as_u64()).map(u128::from).sum::<u128>().to_string(),"note":"USDC PnL from finalized receipt deltas; native SOL fees shown separately, no invented FX conversion"});
    if let Some(positions) = value["marks"]["positions"].as_array_mut() {
        for p in positions {
            p["unrealized_pnl_usdc_atomic"] = json!(
                p["liquidation_usdc_atomic"]
                    .as_u64()
                    .zip(p["cost_usdc_atomic"].as_u64())
                    .map(|(v, c)| (v as i128 - c as i128).to_string())
            );
        }
    }
    Ok(Json(value))
}

pub async fn from_event(live: Arc<Live>, e: &super::bridge::Event) -> Result<Value> {
    let origin = Origin::from(e);
    let response = match (e.command.as_str(), e.arguments.as_slice()) {
        ("preview", [side, mint, amount]) => {
            let r = preview(
                State(live.clone()),
                Json(PreviewRequest {
                    side: side.clone(),
                    mint: mint.clone(),
                    amount: amount.parse().map_err(|_| "invalid atomic amount")?,
                }),
            )
            .await
            .map_err(|(_, Json(v))| v.to_string())?
            .0;
            let mut db = live.db.lock().await;
            if let Some(p) = db.journal.pending.as_mut()
                && Some(p.id.as_str()) == r["order_id"].as_str()
            {
                p.origin = Some(origin);
                db.save()?;
            } else {
                return Err("preview changed before context binding".into());
            }
            return Ok(r);
        }
        ("approve", [id, digest]) => {
            {
                let db = live.db.lock().await;
                let p = db.journal.pending.as_ref().ok_or("no pending order")?;
                if p.origin.as_ref() != Some(&origin) {
                    return Err(
                        "approval must come from the original owner, platform, chat and thread"
                            .into(),
                    );
                }
            }
            execute(
                State(live),
                Json(Approval {
                    order_id: id.clone(),
                    digest: digest.clone(),
                    confirm: true,
                }),
            )
            .await
        }
        ("halt", []) => halt(State(live), Json(Control { halted: true })).await,
        ("resume", []) => halt(State(live), Json(Control { halted: false })).await,
        ("cancel", []) => cancel(State(live)).await,
        ("reconcile", []) => reconcile(State(live)).await,
        ("force-close", [mint, confirm]) if confirm == "CONFIRM" => {
            force_close(
                State(live),
                Json(CloseRequest {
                    mint: mint.clone(),
                    confirm: true,
                }),
            )
            .await
        }
        ("close-all", [confirm]) if confirm == "CONFIRM" => {
            force_close(
                State(live),
                Json(CloseRequest {
                    mint: "all".into(),
                    confirm: true,
                }),
            )
            .await
        }
        ("pause", [scope]) => {
            pause_cycle(
                State(live),
                Json(PauseRequest {
                    scope: scope.clone(),
                    paused: true,
                }),
            )
            .await
        }
        ("resume-cycle", [scope]) => {
            pause_cycle(
                State(live),
                Json(PauseRequest {
                    scope: scope.clone(),
                    paused: false,
                }),
            )
            .await
        }
        ("mode", [mode, hours, amount]) => {
            let hours = hours.parse::<u64>().map_err(|_| "hours must be integer")?;
            if !(1..=168).contains(&hours) {
                return Err("hours must be 1..168".into());
            }
            let amount = amount
                .parse::<u64>()
                .ok()
                .and_then(|v| v.checked_mul(1_000_000))
                .ok_or("invalid USDC amount")?;
            let hash = live.strategy.fingerprint();
            authorize_automation(
                State(live),
                Json(AutomationRequest {
                    mode: mode.clone(),
                    expires_at: super::now() + hours * 3600,
                    entry_usdc: amount,
                    strategy_hash: hash,
                }),
            )
            .await
        }
        _ => return Err("unsupported owner event".into()),
    };
    response
        .map(|Json(v)| v)
        .map_err(|(_, Json(v))| v.to_string())
}

pub fn routes(live: Arc<Live>) -> Router {
    let worker = live.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            let submitted = {
                let db = worker.db.lock().await;
                db.journal
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.state == "submitted")
            };
            if submitted && reconcile(State(worker.clone())).await.is_err() {
                eprintln!("Live reconciliation pending; new orders remain blocked");
            }
            if worker.strict_external_checks && worker.refresh_marks().await.is_err() {
                eprintln!(
                    "Live marks unavailable; entry preview will fail closed until data recovers"
                );
            }
            if let Err(reason) = automatic_exit(worker.clone()).await {
                eprintln!("Owner-authorized automatic exit deferred: {reason}");
            }
            let enabled = {
                let db = worker.db.lock().await;
                db.journal
                    .automation
                    .as_ref()
                    .is_some_and(|a| a.mode == "full" && a.expires_at > super::now())
            };
            if enabled {
                let response = worker
                    .http
                    .get("http://127.0.0.1:8787/v1/live/candidates")
                    .bearer_auth(&worker.agent_token)
                    .send()
                    .await;
                if let Ok(r) = response
                    && r.status().is_success()
                    && let Ok(candidates) = r.json::<Value>().await
                    && automatic_entry(worker.clone(), &candidates).await.is_err()
                {
                    eprintln!("Automatic entry deferred; authorization and risk gates retained");
                }
            }
        }
    });
    let read = Router::new()
        .route("/v1/live/previews", post(agent_preview))
        .route("/v1/live/status", get(status))
        .route_layer(middleware::from_fn_with_state(live.clone(), reader_auth));
    Router::new()
        .route("/v1/live/approval", get(owner_pending))
        .route("/v1/live/exits", post(authorize_exit))
        .route("/v1/live/automation", post(authorize_automation))
        .route("/v1/live/pause", post(pause_cycle))
        .route("/v1/live/close", post(force_close))
        .route("/v1/live/execute", post(execute))
        .route("/v1/live/reconcile", post(reconcile))
        .route("/v1/live/cancel", post(cancel))
        .route("/v1/live/halt", post(halt))
        .route_layer(middleware::from_fn_with_state(live.clone(), owner_auth))
        .merge(read)
        .route("/approve", get(|| async { axum::response::Html(include_str!("../assets/approve.html")) }))
        .route("/approve.js", get(|| async { ([("content-type", "text/javascript; charset=utf-8"), ("cache-control", "no-store")], include_str!("../assets/approve.js")) }))
        .layer(middleware::from_fn(|r:Request,next:Next|async move {
            let mut response=next.run(r).await;
            for (name,value) in [("x-frame-options","DENY"),("cache-control","no-store"),("x-content-type-options","nosniff"),("referrer-policy","no-referrer"),("content-security-policy","default-src 'none'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'")] {
                response.headers_mut().insert(axum::http::HeaderName::from_static(name),axum::http::HeaderValue::from_static(value));
            }
            response
        }))
        .layer(axum::extract::DefaultBodyLimit::max(4096))
        .with_state(live)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;
    use solana_instruction::{AccountMeta, Instruction};
    use solana_message::{Message, VersionedMessage};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    #[tokio::test]
    async fn failed_close_does_not_starve_other_positions() {
        let (l, m, server) = fixture_backend(false, true).await;
        let bad = Pubkey::new_from_array([22; 32]).to_string();
        m.selling.store(true, Ordering::SeqCst);
        {
            let mut db = l.db.lock().await;
            for mint in [&bad, &m.mint] {
                db.journal.positions.insert(mint.clone(), 1_000_000);
                db.journal.opened.insert(mint.clone(), super::super::now());
            }
            db.journal.marks = json!({"timestamp":super::super::now(),"positions":[
                {"mint":bad,"amount":0,"liquidation_usdc_atomic":0},
                {"mint":m.mint,"amount":500,"liquidation_usdc_atomic":800000}]});
        }
        let _ = force_close(
            State(l.clone()),
            Json(CloseRequest {
                mint: "all".into(),
                confirm: true,
            }),
        )
        .await
        .unwrap();
        assert!(automatic_exit(l.clone()).await.is_err());
        assert!(l.db.lock().await.journal.exit_mandates[&bad].retry_at > super::super::now());
        automatic_exit(l.clone()).await.unwrap();
        assert_eq!(m.sends.load(Ordering::SeqCst), 1);
        assert!(l.db.lock().await.journal.halted);
        server.abort();
    }
    #[tokio::test]
    async fn semi_full_pause_and_forced_exit_complete_without_bypassing_policy() {
        let (l, m, server) = fixture_backend(false, true).await;
        let candidates = json!({"candidates":[{"mint":m.mint,"strategy_hash":l.strategy.fingerprint(),"detected_at":super::super::now(),"expires_at":super::super::now()+30}]});
        automatic_entry(l.clone(), &candidates).await.unwrap();
        assert_eq!(m.sends.load(Ordering::SeqCst), 0);
        let authorization = |mode: &str| AutomationRequest {
            mode: mode.into(),
            expires_at: super::super::now() + 3600,
            entry_usdc: 1_000_000,
            strategy_hash: l.strategy.fingerprint(),
        };
        let _ = authorize_automation(State(l.clone()), Json(authorization("semi")))
            .await
            .unwrap();
        automatic_entry(l.clone(), &candidates).await.unwrap();
        assert_eq!(m.sends.load(Ordering::SeqCst), 0);
        let _ = authorize_automation(State(l.clone()), Json(authorization("full")))
            .await
            .unwrap();
        let mut stale = candidates.clone();
        stale["candidates"][0]["expires_at"] = json!(0);
        automatic_entry(l.clone(), &stale).await.unwrap();
        assert_eq!(m.sends.load(Ordering::SeqCst), 0);
        automatic_entry(l.clone(), &candidates).await.unwrap();
        assert_eq!(m.sends.load(Ordering::SeqCst), 1);
        m.finalized.store(true, Ordering::SeqCst);
        let _ = reconcile(State(l.clone())).await.unwrap();
        assert!(
            l.db.lock()
                .await
                .journal
                .exit_mandates
                .contains_key(&m.mint)
        );
        m.selling.store(true, Ordering::SeqCst);
        {
            let mut db = l.db.lock().await;
            db.journal.marks = json!({"timestamp":super::super::now(),"positions":[{"mint":m.mint,"amount":500,"cost_usdc_atomic":1000000,"liquidation_usdc_atomic":800000}]});
        }
        let _ = pause_cycle(
            State(l.clone()),
            Json(PauseRequest {
                scope: "all".into(),
                paused: true,
            }),
        )
        .await
        .unwrap();
        automatic_exit(l.clone()).await.unwrap();
        assert_eq!(m.sends.load(Ordering::SeqCst), 1);
        let _ = force_close(
            State(l.clone()),
            Json(CloseRequest {
                mint: "all".into(),
                confirm: true,
            }),
        )
        .await
        .unwrap();
        automatic_exit(l.clone()).await.unwrap();
        assert_eq!(m.sends.load(Ordering::SeqCst), 2);
        let _ = reconcile(State(l.clone())).await.unwrap();
        assert!(l.db.lock().await.journal.positions.is_empty());
        let mut denied = authorization("full");
        denied.entry_usdc = l.max_position + 1;
        assert!(
            authorize_automation(State(l.clone()), Json(denied))
                .await
                .is_err()
        );
        server.abort();
    }
    #[test]
    fn receipt_pnl_is_wallet_scoped_and_missing_data_is_not_zero() {
        let row = |owner: &str, amount: &str| json!({"accountIndex":1,"owner":owner,"mint":USDC,"uiTokenAmount":{"amount":amount}});
        let receipt = json!({"meta":{"preTokenBalances":[row("wallet","1000")],"postTokenBalances":[row("wallet","1400")]}});
        assert_eq!(receipt_usdc(&receipt, "wallet"), Some(400));
        assert_eq!(receipt_usdc(&receipt, "other"), None);
        assert_eq!(receipt_usdc(&Value::Null, "wallet"), None);
    }
    #[tokio::test]
    async fn execute_rechecks_caps_and_overnight_receipts_keep_daily_spend() {
        let (l, m, server) = fixture(false).await;
        let p = preview(
            State(l.clone()),
            Json(PreviewRequest {
                side: "buy".into(),
                mint: m.mint.clone(),
                amount: 1_000_000,
            }),
        )
        .await
        .unwrap()
        .0;
        let approval = || Approval {
            order_id: p["order_id"].as_str().unwrap().into(),
            digest: p["digest"].as_str().unwrap().into(),
            confirm: true,
        };
        l.db.lock().await.journal.spent = l.max_daily;
        assert!(execute(State(l.clone()), Json(approval())).await.is_err());
        assert_eq!(m.sends.load(Ordering::SeqCst), 0);
        l.db.lock().await.journal.spent = 0;
        let _ = execute(State(l.clone()), Json(approval())).await.unwrap();
        {
            let mut db = l.db.lock().await;
            db.journal.day = super::super::now() / 86400 - 1;
            db.journal.spent = 777;
        }
        m.finalized.store(true, Ordering::SeqCst);
        let _ = reconcile(State(l.clone())).await.unwrap();
        let db = l.db.lock().await;
        assert_eq!(db.journal.day, super::super::now() / 86400);
        assert_eq!(db.journal.spent, 1_000_000);
        server.abort();
    }

    #[tokio::test]
    async fn automatic_exits_require_a_current_owner_mandate_and_finalize_once() {
        let (live, mock, server) = fixture(false).await;
        mock.selling.store(true, Ordering::SeqCst);
        {
            let mut db = live.db.lock().await;
            db.journal.positions.insert(mock.mint.clone(), 1_000_000);
            db.journal
                .opened
                .insert(mock.mint.clone(), super::super::now() - 100);
            db.journal.marks = json!({"timestamp":super::super::now(),"positions":[{"mint":mock.mint,"amount":500,"cost_usdc_atomic":1_000_000,"liquidation_usdc_atomic":800_000}]});
        }
        automatic_exit(live.clone()).await.unwrap();
        assert_eq!(mock.sends.load(Ordering::SeqCst), 0);
        let authorization = || ExitAuthorization {
            mint: mock.mint.clone(),
            enabled: true,
            expires_at: super::super::now() + 3600,
            strategy_hash: live.strategy.fingerprint(),
        };
        let mut wrong = authorization();
        wrong.strategy_hash = "other-config".into();
        assert!(
            authorize_exit(State(live.clone()), Json(wrong))
                .await
                .is_err()
        );
        let _ = authorize_exit(State(live.clone()), Json(authorization()))
            .await
            .unwrap();
        {
            let mut db = live.db.lock().await;
            db.journal
                .exit_mandates
                .get_mut(&mock.mint)
                .unwrap()
                .expires_at = 0;
        }
        automatic_exit(live.clone()).await.unwrap();
        assert_eq!(mock.sends.load(Ordering::SeqCst), 0);
        let _ = authorize_exit(State(live.clone()), Json(authorization()))
            .await
            .unwrap();
        mock.sell_minimum.store(1_000_000, Ordering::SeqCst);
        automatic_exit(live.clone()).await.unwrap();
        assert_eq!(
            mock.sends.load(Ordering::SeqCst),
            0,
            "a recovered fresh quote must cancel the old exit signal"
        );
        mock.sell_minimum.store(900_000, Ordering::SeqCst);
        automatic_exit(live.clone()).await.unwrap();
        assert_eq!(mock.sends.load(Ordering::SeqCst), 1);
        automatic_exit(live.clone()).await.unwrap();
        assert_eq!(mock.sends.load(Ordering::SeqCst), 1);
        mock.finalized.store(true, Ordering::SeqCst);
        let _ = reconcile(State(live.clone())).await.unwrap();
        let db = live.db.lock().await;
        assert!(!db.journal.positions.contains_key(&mock.mint));
        assert!(!db.journal.exit_mandates.contains_key(&mock.mint));
        server.abort();
    }

    #[tokio::test]
    async fn configured_live_exit_and_universal_owner_review_work_over_http() {
        let (mut live, mock, fixture_task) = fixture(false).await;
        Arc::get_mut(&mut live).unwrap().strategy.take_profit_pct = 5.0;
        let mint = mock.mint.clone();
        let holdings = json!({"jsonrpc":"2.0","id":1,"result":{"value":[
            {"account":{"data":{"parsed":{"info":{"mint":USDC,"tokenAmount":{"amount":"10000000"}}}}}},
            {"account":{"data":{"parsed":{"info":{"mint":mint,"tokenAmount":{"amount":"500"}}}}}}
        ]}});
        let quote = json!({"inputMint":mint,"outputMint":USDC,"inAmount":"500","outAmount":"1205000","otherAmountThreshold":"1200000"});
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let provider = Router::new()
            .route(
                "/rpc",
                post(move || {
                    let v = holdings.clone();
                    async { Json(v) }
                }),
            )
            .route(
                "/quote",
                get(move || {
                    let v = quote.clone();
                    async { Json(v) }
                }),
            );
        let task = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
        {
            let inner = Arc::get_mut(&mut live).unwrap();
            inner.rpc = format!("http://{address}/rpc");
            inner.jupiter_url = format!("http://{address}/quote");
            let mut db = inner.db.lock().await;
            db.journal.positions.insert(mint.clone(), 1_000_000);
        }
        live.refresh_marks().await.unwrap();
        let state = status(State(live.clone())).await.unwrap().0;
        assert_eq!(
            state["marks"]["positions"][0]["exit_recommendation"],
            "take_profit"
        );
        assert!(
            super::super::research::Strategy::default()
                .exit(1_000_000, 1_100_000, 0)
                .is_none()
        );
        // An unpriceable/missing holding blocks new buys, while other marks remain usable.
        live.db
            .lock()
            .await
            .journal
            .positions
            .insert(Pubkey::new_from_array([23; 32]).to_string(), 1_000_000);
        live.refresh_marks().await.unwrap();
        let state = status(State(live.clone())).await.unwrap().0;
        assert_eq!(state["marks"]["complete"], false);
        assert!(state["marks"]["equity_usdc_atomic"].is_null());
        assert_eq!(
            state["marks"]["positions"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|p| p["liquidation_usdc_atomic"].is_u64())
                .count(),
            1
        );
        task.abort();
        fixture_task.abort();

        let (live, mock, fixture_task) = fixture(false).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = routes(live.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let http = reqwest::Client::new();
        let page = http
            .get(format!("http://{address}/approve"))
            .send()
            .await
            .unwrap();
        assert_eq!(page.headers()["x-frame-options"], "DENY");
        assert!(page.text().await.unwrap().contains("Review live order"));
        assert_eq!(
            http.get(format!("http://{address}/v1/live/approval"))
                .bearer_auth(&live.agent_token)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let preview: Value = http
            .post(format!("http://{address}/v1/live/previews"))
            .bearer_auth(&live.agent_token)
            .header("x-soli-agent-id", "mcp-agent-a")
            .json(&json!({"side":"buy","mint":mock.mint,"amount":1_000_000}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(preview["requested_by"], "mcp-agent-a");
        let review: Value = http
            .get(format!("http://{address}/v1/live/approval"))
            .bearer_auth(&live.owner_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(review["pending"]["digest"], preview["digest"]);
        let approval: Value = http
            .post(format!("http://{address}/v1/live/execute"))
            .bearer_auth(&live.owner_token)
            .json(
                &json!({"order_id":preview["order_id"],"digest":preview["digest"],"confirm":true}),
            )
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(approval["state"], "submitted");
        assert_eq!(mock.sends.load(Ordering::SeqCst), 1);
        task.abort();
        fixture_task.abort();
    }

    #[test]
    fn legacy_and_v0_wire_match_solana_shortvec_layout() {
        let legacy = Message::new(&[], Some(&Pubkey::new_from_array([7; 32])));
        let v0 = solana_message::v0::Message {
            header: legacy.header,
            account_keys: legacy.account_keys.clone(),
            recent_blockhash: legacy.recent_blockhash.clone(),
            instructions: vec![],
            address_table_lookups: vec![],
        };
        for (message, versioned) in [
            (VersionedMessage::Legacy(legacy), false),
            (VersionedMessage::V0(v0), true),
        ] {
            // Independent byte layout, not a codec-to-itself round trip.
            let mut expected = vec![1]; // compact signature count
            expected.extend([0; 64]);
            if versioned {
                expected.push(0x80);
            }
            expected.extend([1, 0, 0, 1]); // header and compact account count
            expected.extend([7; 32]);
            expected.extend([0; 32]); // blockhash
            expected.push(0); // instructions
            if versioned {
                expected.push(0); // lookup tables
            }
            let tx = VersionedTransaction {
                signatures: vec![Signature::default()],
                message,
            };
            assert_eq!(wincode::serialize(&tx).unwrap(), expected);
            let decoded: VersionedTransaction = wincode::deserialize(&expected).unwrap();
            assert_eq!(decoded, tx);
        }
    }

    fn system_account(lamports: u64) -> Value {
        json!({"owner":"11111111111111111111111111111111","lamports":lamports,"data":["","base64"]})
    }
    fn token_account(mint: &str, wallet: &str, amount: u64) -> Value {
        let mut data = vec![0u8; 165];
        data[..32].copy_from_slice(&bs58::decode(mint).into_vec().unwrap());
        data[32..64].copy_from_slice(&bs58::decode(wallet).into_vec().unwrap());
        data[64..72].copy_from_slice(&amount.to_le_bytes());
        data[108] = 1;
        json!({"owner":TOKEN,"lamports":2039280,"data":[B64.encode(data),"base64"]})
    }
    fn pending(mint: &str) -> Pending {
        Pending {
            exit_hash: None,
            entry_mandate: None,
            requested_by: None,
            origin: None,
            id: "test-order".into(),
            digest: "approved-digest".into(),
            side: "buy".into(),
            mint: mint.into(),
            input: 1_000_000,
            minimum: 500,
            quote: Value::Null,
            expires: super::super::now() + 60,
            signature: None,
            state: "preview".into(),
        }
    }

    #[test]
    fn simulation_policy_rejects_drain_authority_changes_and_extra_assets() {
        let wallet = Pubkey::new_from_array([7; 32]).to_string();
        let mint = Pubkey::new_from_array([8; 32]).to_string();
        let p = pending(&mint);
        let before = vec![
            system_account(100_000_000),
            token_account(USDC, &wallet, 10_000_000),
            token_account(&mint, &wallet, 0),
        ];
        let after = vec![
            system_account(99_990_000),
            token_account(USDC, &wallet, 9_000_000),
            token_account(&mint, &wallet, 500),
        ];
        assert!(validate_deltas(&p, &wallet, &before, &after, 5_000_000).is_ok());
        let mut drain = after.clone();
        drain[1] = token_account(USDC, &wallet, 8_000_000);
        assert!(validate_deltas(&p, &wallet, &before, &drain, 5_000_000).is_err());
        let mut authority = after.clone();
        let mut bytes = account_data(&authority[2]).unwrap();
        bytes[72] = 1;
        authority[2]["data"][0] = json!(B64.encode(bytes));
        assert!(validate_deltas(&p, &wallet, &before, &authority, 5_000_000).is_err());
        let mut fee = after.clone();
        fee[0] = system_account(90_000_000);
        assert!(validate_deltas(&p, &wallet, &before, &fee, 5_000_000).is_err());
    }

    #[derive(Clone)]
    struct Mock {
        sell_minimum: Arc<AtomicUsize>,
        selling: Arc<AtomicBool>,
        signer: Arc<ed25519_dalek::SigningKey>,
        mint: String,
        before: HashMap<String, Value>,
        after: HashMap<String, Value>,
        tx: String,
        sends: Arc<AtomicUsize>,
        finalized: Arc<AtomicBool>,
        timeout: bool,
    }
    async fn mock_rpc(State(m): State<Mock>, Json(r): Json<Value>) -> Json<Value> {
        let result = match r["method"].as_str().unwrap() {
            "getMultipleAccounts" => {
                let before = if m.selling.load(Ordering::SeqCst) {
                    &m.after
                } else {
                    &m.before
                };
                json!({"value":r["params"][0].as_array().unwrap().iter().map(|key|before.get(key.as_str().unwrap()).cloned().unwrap_or(Value::Null)).collect::<Vec<_>>() })
            }
            "getTokenAccountsByOwner" => {
                if m.selling.load(Ordering::SeqCst) && r["params"][1]["mint"].is_string() {
                    json!({"value":[{"account":{"data":{"parsed":{"info":{"tokenAmount":{"amount":"500"}}}}}}]})
                } else {
                    json!({"value":[]})
                }
            }
            "getAccountInfo" => {
                let mut mint = [0u8; 82];
                mint[45] = 1;
                json!({"value":{"owner":TOKEN,"data":[B64.encode(mint),"base64"]}})
            }
            "simulateTransaction" => {
                let after = if m.selling.load(Ordering::SeqCst) {
                    &m.before
                } else {
                    &m.after
                };
                json!({"value":{"err":null,"accounts":r["params"][1]["accounts"]["addresses"].as_array().unwrap().iter().map(|key|after.get(key.as_str().unwrap()).cloned().unwrap_or(Value::Null)).collect::<Vec<_>>()}})
            }
            "isBlockhashValid" => json!({"value":true}),
            "sendTransaction" => {
                m.sends.fetch_add(1, Ordering::SeqCst);
                if m.timeout {
                    return Json(json!({"jsonrpc":"2.0","id":1,"error":{"code":-32000}}));
                }
                let tx: VersionedTransaction =
                    wincode::deserialize(&B64.decode(r["params"][0].as_str().unwrap()).unwrap())
                        .unwrap();
                assert!(tx.verify_and_hash_message().is_ok());
                json!(tx.signatures[0].to_string())
            }
            "getTransaction" => {
                if m.finalized.load(Ordering::SeqCst) {
                    json!({"meta":{"err":null,"fee":10000},"slot":123})
                } else {
                    Value::Null
                }
            }
            _ => panic!("unexpected mock RPC method"),
        };
        Json(json!({"jsonrpc":"2.0","id":1,"result":result}))
    }
    async fn mock_order(State(m): State<Mock>) -> Json<Value> {
        if m.selling.load(Ordering::SeqCst) {
            let mut tx: VersionedTransaction =
                wincode::deserialize(&B64.decode(&m.tx).unwrap()).unwrap();
            if let VersionedMessage::Legacy(message) = &mut tx.message {
                let minimum = m.sell_minimum.load(Ordering::SeqCst) as u64;
                message.instructions[0].data = super::super::jupiter_wire::fixture(500, minimum);
                message.instructions[0].accounts.swap(2, 3);
                message.instructions[0].accounts[4] = message.instructions[0].accounts[3];
                message.instructions[0].accounts[5] = message
                    .account_keys
                    .iter()
                    .position(|k| k.to_string() == USDC)
                    .unwrap() as u8;
            }
            return Json(
                json!({"inputMint":m.mint,"outputMint":USDC,"inAmount":"500","outAmount":(m.sell_minimum.load(Ordering::SeqCst)*201/200).to_string(),"otherAmountThreshold":m.sell_minimum.load(Ordering::SeqCst).to_string(),"priceImpactPct":"0.1","transaction":B64.encode(wincode::serialize(&tx).unwrap())}),
            );
        }
        Json(
            json!({"inputMint":USDC,"outputMint":m.mint,"inAmount":"1000000","outAmount":"500","otherAmountThreshold":"500","priceImpactPct":"0.1","transaction":m.tx}),
        )
    }
    async fn mock_sign(State(m): State<Mock>, Json(r): Json<Value>) -> Json<Value> {
        let input = B64.decode(r["input"].as_str().unwrap()).unwrap();
        Json(
            json!({"data":{"signature":format!("vault:v1:{}",B64.encode(m.signer.sign(&input).to_bytes()))}}),
        )
    }
    async fn fixture(timeout: bool) -> (Arc<Live>, Mock, tokio::task::JoinHandle<()>) {
        fixture_backend(timeout, false).await
    }
    async fn fixture_backend(
        timeout: bool,
        local: bool,
    ) -> (Arc<Live>, Mock, tokio::task::JoinHandle<()>) {
        let signer = Arc::new(ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]));
        let wallet = bs58::encode(signer.verifying_key().to_bytes()).into_string();
        let mint = Pubkey::new_from_array([8; 32]).to_string();
        let input = Pubkey::new_from_array([9; 32]);
        let output = Pubkey::new_from_array([10; 32]);
        let message = Message::new(
            &[Instruction::new_with_bytes(
                Pubkey::from_str(ROUTER).unwrap(),
                &super::super::jupiter_wire::fixture(1_000_000, 500),
                vec![
                    AccountMeta::new_readonly(Pubkey::from_str(TOKEN).unwrap(), false),
                    AccountMeta::new(Pubkey::from_str(&wallet).unwrap(), true),
                    AccountMeta::new(input, false),
                    AccountMeta::new(output, false),
                    AccountMeta::new(output, false),
                    AccountMeta::new_readonly(Pubkey::from_str(&mint).unwrap(), false),
                    AccountMeta::new_readonly(Pubkey::from_str(ROUTER).unwrap(), false),
                    AccountMeta::new_readonly(Pubkey::from_str(ROUTER).unwrap(), false),
                    AccountMeta::new_readonly(Pubkey::from_str(ROUTER).unwrap(), false),
                    AccountMeta::new_readonly(Pubkey::from_str(USDC).unwrap(), false),
                    AccountMeta::new_readonly(
                        Pubkey::from_str(super::super::data::TOKEN_2022).unwrap(),
                        false,
                    ),
                ],
            )],
            Some(&Pubkey::from_str(&wallet).unwrap()),
        );
        let tx = VersionedTransaction {
            signatures: vec![Signature::default()],
            message: VersionedMessage::Legacy(message),
        };
        let before = HashMap::from([
            (wallet.clone(), system_account(100_000_000)),
            (input.to_string(), token_account(USDC, &wallet, 10_000_000)),
            (output.to_string(), token_account(&mint, &wallet, 0)),
        ]);
        let after = HashMap::from([
            (wallet.clone(), system_account(99_990_000)),
            (input.to_string(), token_account(USDC, &wallet, 9_000_000)),
            (output.to_string(), token_account(&mint, &wallet, 500)),
        ]);
        let m = Mock {
            sell_minimum: Arc::new(AtomicUsize::new(900_000)),
            selling: Arc::new(AtomicBool::new(false)),
            signer,
            mint,
            before,
            after,
            tx: B64.encode(wincode::serialize(&tx).unwrap()),
            sends: Arc::new(AtomicUsize::new(0)),
            finalized: Arc::new(AtomicBool::new(false)),
            timeout,
        };
        let router = Router::new()
            .route("/rpc", post(mock_rpc))
            .route("/order", get(mock_order))
            .route("/v1/transit/sign/soli", post(mock_sign))
            .with_state(m.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE live_journal(id INTEGER PRIMARY KEY,data TEXT NOT NULL); INSERT INTO live_journal VALUES(1,'{}');").unwrap();
        let l = Arc::new(Live {
            service_identity: "isolated-test-signer".into(),
            strategy: super::super::research::Strategy::default(),
            strict_external_checks: false,
            http: reqwest::Client::new(),
            rpc: format!("{url}/rpc"),
            jupiter: "test-key".into(),
            jupiter_url: format!("{url}/order"),
            goplus: super::super::goplus::Auth::from_env(),
            vault: url,
            vault_token: "test-vault".into(),
            key: "soli".into(),
            key_version: 1,
            local_key: if local {
                Some(
                    super::super::local_key::LocalKey::decode(
                        &bs58::encode([42u8; 32]).into_string(),
                        &wallet,
                    )
                    .unwrap(),
                )
            } else {
                None
            },
            wallet,
            owner_token: "owner-only-test-token-012345678901".into(),
            agent_token: "agent-test-token-01234567890123456".into(),
            max_position: 5_000_000,
            max_daily: 10_000_000,
            max_sol_fee: 5_000_000,
            db: Mutex::new(Db {
                connection,
                journal: Journal::default(),
            }),
        });
        (l, m, handle)
    }

    #[tokio::test]
    async fn live_preview_both_signers_broadcast_finalize_and_replay() {
        for local in [false, true] {
            let (l, m, handle) = fixture_backend(false, local).await;
            let p = preview(
                State(l.clone()),
                Json(PreviewRequest {
                    side: "buy".into(),
                    mint: m.mint.clone(),
                    amount: 1_000_000,
                }),
            )
            .await
            .unwrap()
            .0;
            let approval = || Approval {
                order_id: p["order_id"].as_str().unwrap().into(),
                digest: p["digest"].as_str().unwrap().into(),
                confirm: true,
            };
            let mut wrong = approval();
            wrong.digest = "wrong".into();
            assert!(execute(State(l.clone()), Json(wrong)).await.is_err());
            assert_eq!(m.sends.load(Ordering::SeqCst), 0);
            let response = execute(State(l.clone()), Json(approval())).await.unwrap().0;
            assert_eq!(response["state"], "submitted");
            assert!(execute(State(l.clone()), Json(approval())).await.is_err());
            assert_eq!(
                reconcile(State(l.clone())).await.unwrap().0["state"],
                "unresolved"
            );
            m.finalized.store(true, Ordering::SeqCst);
            assert_eq!(
                reconcile(State(l.clone())).await.unwrap().0["state"],
                "finalized"
            );
            assert_eq!(
                execute(State(l.clone()), Json(approval())).await.unwrap().0["state"],
                "finalized"
            );
            assert_eq!(m.sends.load(Ordering::SeqCst), 1);
            let db = l.db.lock().await;
            assert_eq!(db.journal.spent, 1_000_000);
            assert!(db.journal.positions.contains_key(&m.mint));
            let saved: String = db
                .connection
                .query_row("SELECT data FROM live_journal WHERE id=1", [], |r| r.get(0))
                .unwrap();
            let restored: Journal = serde_json::from_str(&saved).unwrap();
            assert!(restored.pending.is_none());
            assert_eq!(restored.completed.len(), 1);
            drop(db);
            m.selling.store(true, Ordering::SeqCst);
            let sold = preview(
                State(l.clone()),
                Json(PreviewRequest {
                    side: "sell".into(),
                    mint: m.mint.clone(),
                    amount: 500,
                }),
            )
            .await
            .unwrap()
            .0;
            let approval = Approval {
                order_id: sold["order_id"].as_str().unwrap().into(),
                digest: sold["digest"].as_str().unwrap().into(),
                confirm: true,
            };
            assert_eq!(
                execute(State(l.clone()), Json(approval)).await.unwrap().0["state"],
                "submitted"
            );
            assert_eq!(
                reconcile(State(l.clone())).await.unwrap().0["state"],
                "finalized"
            );
            let db = l.db.lock().await;
            assert!(db.journal.positions.is_empty());
            assert!(db.journal.pending.is_none());
            assert_eq!(db.journal.completed.len(), 2);
            let saved: String = db
                .connection
                .query_row("SELECT data FROM live_journal WHERE id=1", [], |r| r.get(0))
                .unwrap();
            let restored: Journal = serde_json::from_str(&saved).unwrap();
            assert!(restored.positions.is_empty());
            assert_eq!(restored.completed.len(), 2);
            assert_eq!(m.sends.load(Ordering::SeqCst), 2);
            handle.abort();
        }
    }
    #[tokio::test]
    async fn uncertain_broadcast_is_journaled_and_never_blindly_retried() {
        let (l, m, handle) = fixture(true).await;
        let p = preview(
            State(l.clone()),
            Json(PreviewRequest {
                side: "buy".into(),
                mint: m.mint.clone(),
                amount: 1_000_000,
            }),
        )
        .await
        .unwrap()
        .0;
        let a = || Approval {
            order_id: p["order_id"].as_str().unwrap().into(),
            digest: p["digest"].as_str().unwrap().into(),
            confirm: true,
        };
        assert_eq!(
            execute(State(l.clone()), Json(a())).await.unwrap().0["state"],
            "unknown"
        );
        assert!(execute(State(l.clone()), Json(a())).await.is_err());
        assert!(cancel(State(l.clone())).await.is_err());
        assert_eq!(m.sends.load(Ordering::SeqCst), 1);
        let db = l.db.lock().await;
        let saved: String = db
            .connection
            .query_row("SELECT data FROM live_journal WHERE id=1", [], |r| r.get(0))
            .unwrap();
        let restored: Journal = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.pending.unwrap().state, "submitted");
        handle.abort();
    }
    #[tokio::test]
    async fn channel_approval_is_bound_to_the_original_thread_and_owner() {
        let (l, m, handle) = fixture(false).await;
        let mut e = super::super::bridge::Event {
            event_id: "one".into(),
            platform: "discord".into(),
            user_id: "owner".into(),
            conversation_id: "channel".into(),
            thread_id: Some("thread-one".into()),
            is_private: false,
            command: "preview".into(),
            arguments: vec!["buy".into(), m.mint.clone(), "1000000".into()],
        };
        let p = from_event(l.clone(), &e).await.unwrap();
        e.command = "approve".into();
        e.arguments = vec![
            p["order_id"].as_str().unwrap().into(),
            p["digest"].as_str().unwrap().into(),
        ];
        e.thread_id = Some("other-thread".into());
        assert!(from_event(l.clone(), &e).await.is_err());
        e.thread_id = Some("thread-one".into());
        e.user_id = "other-owner".into();
        assert!(from_event(l.clone(), &e).await.is_err());
        assert_eq!(m.sends.load(Ordering::SeqCst), 0);
        e.user_id = "owner".into();
        assert_eq!(
            from_event(l.clone(), &e).await.unwrap()["state"],
            "submitted"
        );
        handle.abort();
    }
    #[tokio::test]
    async fn agent_credentials_cannot_execute_or_unhalt_live() {
        let (l, _, mock) = fixture(false).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let router = routes(l.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let http = reqwest::Client::new();
        assert_eq!(
            http.get(format!("{url}/v1/live/status"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert!(
            http.get(format!("{url}/v1/live/status"))
                .bearer_auth(&l.agent_token)
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
        for path in ["execute", "halt", "cancel", "reconcile"] {
            assert_eq!(
                http.post(format!("{url}/v1/live/{path}"))
                    .bearer_auth(&l.agent_token)
                    .json(&json!({}))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
        }
        server.abort();
        mock.abort();
    }
    #[test]
    fn rejects_signature_or_account_data_corruption() {
        assert!(account_data(&json!({"data":["!!!","base64"]})).is_err());
        assert!(units(&json!("-1")).is_err());
        assert!(units(&json!("18446744073709551616")).is_err());
    }
}
