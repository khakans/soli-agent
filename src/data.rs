use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use soli_agent::{USDC, valid_mint};
use std::collections::BTreeMap;

pub const TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const TOKEN_2022: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
pub type Result<T> = std::result::Result<T, String>;
pub fn within_slippage(output: u64, minimum: u64) -> bool {
    output > 0
        && minimum > 0
        && minimum <= output
        && minimum as u128 >= output as u128 * 9900 / 10000
}
#[derive(Clone)]
pub struct Data {
    pub jupiter_url: String,
    pub http: reqwest::Client,
    pub rpc: String,
    pub jupiter_key: String,
    pub goplus_token: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Risk {
    pub mint: String,
    pub checked_at: u64,
    pub accepted: bool,
    pub reasons: Vec<String>,
    pub token_program: String,
    pub extensions: Vec<String>,
    pub top_owner_pct: f64,
    pub top_ten_owners_pct: f64,
    pub buy_route: bool,
    pub sell_route: bool,
    pub goplus: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sample {
    pub mint: String,
    pub timestamp: u64,
    pub price_usd: f64,
    pub liquidity_usd: f64,
    pub volume_5m_usd: f64,
    pub buys_5m: u64,
    pub sells_5m: u64,
    pub pair_created_at: u64,
    pub source: String,
}
impl Data {
    pub async fn check_fresh_chain(&self) -> Result<()> {
        if self.rpc("getGenesisHash", json!([])).await?.as_str()
            != Some("5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp")
        {
            return Err("data RPC is not Solana mainnet".into());
        }
        let slot = self
            .rpc("getSlot", json!([{"commitment":"confirmed"}]))
            .await?
            .as_u64()
            .ok_or("missing current slot")?;
        let stamp = self
            .rpc("getBlockTime", json!([slot]))
            .await?
            .as_u64()
            .ok_or("RPC block time unavailable")?;
        if super::now().abs_diff(stamp) > 90 {
            return Err("RPC data is stale; entries paused".into());
        }
        Ok(())
    }
    pub fn from_env(http: reqwest::Client) -> Option<Self> {
        Some(Self {
            jupiter_url: "https://api.jup.ag/swap/v2/order".into(),
            http,
            rpc: std::env::var("SOLI_RPC_URL")
                .ok()
                .filter(|s| !s.is_empty())?,
            jupiter_key: std::env::var("JUPITER_API_KEY").unwrap_or_default(),
            goplus_token: std::env::var("GOPLUS_TOKEN").ok(),
        })
    }
    pub async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let r = self
            .http
            .post(&self.rpc)
            .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
            .send()
            .await
            .map_err(|_| "RPC unavailable")?;
        if !r.status().is_success() {
            return Err("RPC HTTP failure".into());
        }
        let v: Value = r.json().await.map_err(|_| "invalid RPC JSON")?;
        if v.get("error").is_some() {
            return Err(format!("RPC {method} failed"));
        }
        v.get("result").cloned().ok_or("missing RPC result".into())
    }
    pub async fn quote(&self, input: &str, output: &str, amount: u64) -> Result<Value> {
        if self.jupiter_key.is_empty() {
            return Err("Jupiter key not configured".into());
        }
        let r = self
            .http
            .get(&self.jupiter_url)
            .header("x-api-key", &self.jupiter_key)
            .query(&[
                ("inputMint", input),
                ("outputMint", output),
                ("amount", &amount.to_string()),
                ("slippageBps", "100"),
            ])
            .send()
            .await
            .map_err(|_| "quote unavailable")?;
        if !r.status().is_success() {
            return Err("quote request rejected".into());
        }
        let q: Value = r.json().await.map_err(|_| "invalid quote")?;
        if q["inputMint"] != input
            || q["outputMint"] != output
            || amount_field(&q, "inAmount")? != amount
            || amount_field(&q, "outAmount")? == 0
            || !within_slippage(
                amount_field(&q, "outAmount")?,
                amount_field(&q, "otherAmountThreshold")?,
            )
        {
            return Err("quote fields missing or mismatch".into());
        }
        Ok(q)
    }
    pub async fn sample(&self, mint: &str) -> Result<Sample> {
        if !valid_mint(mint) {
            return Err("invalid mint".into());
        }
        let r = self
            .http
            .get(format!(
                "https://api.dexscreener.com/token-pairs/v1/solana/{mint}"
            ))
            .send()
            .await
            .map_err(|_| "market unavailable")?;
        if !r.status().is_success() {
            return Err("market HTTP failure".into());
        }
        let v: Value = r.json().await.map_err(|_| "invalid market response")?;
        let pair = v
            .as_array()
            .ok_or("missing pairs")?
            .iter()
            .filter(|p| p["chainId"] == "solana" && p["baseToken"]["address"] == mint)
            .max_by(|a, b| {
                a["liquidity"]["usd"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .total_cmp(&b["liquidity"]["usd"].as_f64().unwrap_or(0.0))
            })
            .ok_or("no supported base-token pair")?;
        let price = pair["priceUsd"]
            .as_str()
            .and_then(|v| v.parse::<f64>().ok())
            .ok_or("missing price")?;
        if !price.is_finite() || price <= 0.0 {
            return Err("invalid market price".into());
        }
        Ok(Sample {
            mint: mint.into(),
            timestamp: super::now(),
            price_usd: price,
            liquidity_usd: pair["liquidity"]["usd"]
                .as_f64()
                .ok_or("missing liquidity")?,
            volume_5m_usd: pair["volume"]["m5"].as_f64().ok_or("missing volume")?,
            buys_5m: pair["txns"]["m5"]["buys"].as_u64().unwrap_or(0),
            sells_5m: pair["txns"]["m5"]["sells"].as_u64().unwrap_or(0),
            pair_created_at: pair["pairCreatedAt"].as_u64().ok_or("missing pair age")? / 1000,
            source: "dexscreener".into(),
        })
    }
    pub async fn risk(&self, mint: &str) -> Result<Risk> {
        self.check_fresh_chain().await?;
        if !valid_mint(mint) || mint == USDC {
            return Err("invalid target mint".into());
        }
        let account = self
            .rpc(
                "getAccountInfo",
                json!([mint,{"encoding":"jsonParsed","commitment":"confirmed"}]),
            )
            .await?;
        let value = &account["value"];
        let info = &value["data"]["parsed"]["info"];
        let program = value["owner"].as_str().ok_or("mint missing")?.to_string();
        let mut reasons = vec![];
        if program != TOKEN && program != TOKEN_2022 {
            return Err("unsupported mint owner".into());
        }
        if !info.is_object() {
            return Err("RPC did not parse mint; fail closed".into());
        }
        reasons.extend(authority_reasons(info));
        if info["isInitialized"] != true {
            reasons.push("mint is not verified initialized".into());
        }
        let extensions: Vec<String> = info
            .get("extensions")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|v| v["extension"].as_str().unwrap_or("unknown").to_string())
                    .collect()
            })
            .unwrap_or_default();
        // Explicitly conservative: inventory Token-2022 extensions, but no live entries until every extension is supported.
        if program == TOKEN_2022 {
            reasons.push("Token-2022 execution disabled; extension inventory only".into());
        }
        let supply = info["supply"]
            .as_str()
            .and_then(|v| v.parse::<u128>().ok())
            .filter(|n| *n > 0)
            .ok_or("missing supply")?;
        let largest = self
            .rpc(
                "getTokenLargestAccounts",
                json!([mint,{"commitment":"confirmed"}]),
            )
            .await?;
        let accounts = largest["value"]
            .as_array()
            .ok_or("missing largest accounts")?;
        let addresses: Vec<&str> = accounts
            .iter()
            .map(|a| {
                a["address"]
                    .as_str()
                    .ok_or_else(|| String::from("missing holder address"))
            })
            .collect::<Result<_>>()?;
        let owners = self
            .rpc(
                "getMultipleAccounts",
                json!([addresses,{"encoding":"jsonParsed","commitment":"confirmed"}]),
            )
            .await?;
        let owner_accounts = owners["value"]
            .as_array()
            .filter(|v| v.len() == accounts.len())
            .ok_or("holder lookup incomplete")?;
        let mut balances: BTreeMap<String, u128> = BTreeMap::new();
        for (a, o) in accounts.iter().zip(owner_accounts) {
            let owner = o["data"]["parsed"]["info"]["owner"]
                .as_str()
                .ok_or("holder owner unknown")?;
            let amount = a["amount"]
                .as_str()
                .and_then(|a| a.parse::<u128>().ok())
                .ok_or("holder balance unknown")?;
            *balances.entry(owner.into()).or_default() += amount;
        }
        let mut values: Vec<u128> = balances.into_values().collect();
        values.sort_unstable_by(|a, b| b.cmp(a));
        let top = values.first().copied().unwrap_or(0) as f64 / supply as f64 * 100.0;
        let ten = values.iter().take(10).sum::<u128>() as f64 / supply as f64 * 100.0;
        if top > 20.0 {
            reasons.push("largest observed owner above 20% of supply".into());
        }
        if ten > 50.0 {
            reasons.push("top ten observed owners above 50% of supply".into());
        }
        let mut request = self
            .http
            .get("https://api.gopluslabs.io/api/v1/solana/token_security")
            .query(&[("contract_addresses", mint)]);
        if let Some(token) = &self.goplus_token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|_| "GoPlus unavailable; no entry")?;
        if !response.status().is_success() {
            return Err("GoPlus HTTP failure; no entry".into());
        }
        let gp: Value = response.json().await.map_err(|_| "invalid GoPlus data")?;
        let security = gp["result"]
            .get(mint)
            .cloned()
            .ok_or("GoPlus has no token coverage; no entry")?;
        reasons.extend(goplus_reasons(&security));
        let buy = self.quote(USDC, mint, 1_000_000).await;
        let buy_route = buy.is_ok();
        let sell_route = if let Ok(q) = buy {
            self.quote(mint, USDC, amount_field(&q, "otherAmountThreshold")?)
                .await
                .is_ok()
        } else {
            false
        };
        if !buy_route || !sell_route {
            reasons.push("two-way Jupiter route not verified".into());
        }
        Ok(Risk {
            mint: mint.into(),
            checked_at: super::now(),
            accepted: reasons.is_empty(),
            reasons,
            token_program: program,
            extensions,
            top_owner_pct: top,
            top_ten_owners_pct: ten,
            buy_route,
            sell_route,
            goplus: security,
        })
    }
}
pub fn amount_field(q: &Value, key: &str) -> Result<u64> {
    q[key]
        .as_str()
        .and_then(|v| v.parse().ok())
        .ok_or(format!("missing {key}"))
}
pub fn goplus_reasons(s: &Value) -> Vec<String> {
    let mut reasons = vec![];
    for key in [
        "mintable",
        "freezable",
        "balance_mutable",
        "closable",
        "transfer_fee_upgradable",
        "transfer_hook_upgradable",
    ] {
        if let Some(value) = s.get(key) {
            let status = value.get("status").unwrap_or(value);
            if status == "1" || status == 1 || status == true {
                reasons.push(format!("GoPlus flag: {key}"));
            }
        }
    }
    for key in ["mintable", "freezable"] {
        let state = s.get(key).map(|v| v.get("status").unwrap_or(v));
        if state.is_none_or(|v| v != "0" && v != 0 && v != false) {
            reasons.push(format!("GoPlus missing {key}; fail closed"));
        }
    }
    reasons
}
fn authority_reasons(info: &Value) -> Vec<String> {
    ["mintAuthority", "freezeAuthority"]
        .into_iter()
        .filter(|key| info.get(*key) != Some(&Value::Null))
        .map(|key| format!("{key} not verified as revoked"))
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn slippage_floor_is_verified_including_large_atomic_amounts() {
        assert!(super::within_slippage(1_000_000, 990_000));
        assert!(!super::within_slippage(1_000_000, 989_999));
        assert!(!super::within_slippage(1_000_000, 1_000_001));
        assert!(super::within_slippage(u64::MAX, u64::MAX));
        assert!(!super::within_slippage(0, 0));
    }
    use super::*;
    #[test]
    fn flags_and_missing_security_coverage() {
        assert!(
            goplus_reasons(&json!({"mintable":{"status":"0"},"freezable":{"status":"0"}}))
                .is_empty()
        );
        assert!(!goplus_reasons(&json!({"mintable":{"status":"1"}})).is_empty());
        assert!(!goplus_reasons(&json!({})).is_empty());
    }
    #[test]
    fn mint_and_freeze_authority_checks_fail_closed() {
        assert!(
            authority_reasons(&json!({"mintAuthority":null,"freezeAuthority":null})).is_empty()
        );
        assert_eq!(
            authority_reasons(&json!({"mintAuthority":"owner","freezeAuthority":null})).len(),
            1
        );
        assert_eq!(authority_reasons(&json!({})).len(), 2);
        assert!(
            !goplus_reasons(&json!({"mintable":{"status":"unknown"},"freezable":"0"})).is_empty()
        );
    }
}
