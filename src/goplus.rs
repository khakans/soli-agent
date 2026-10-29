//! GoPlus app credentials stay in the service; expiring tokens stay in memory.
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

type TokenCache = Arc<Mutex<Option<(Zeroizing<String>, Instant)>>>;

#[derive(Clone)]
pub struct Auth {
    key: Option<Zeroizing<String>>,
    secret: Option<Zeroizing<String>>,
    legacy_token: Option<Zeroizing<String>>,
    cached: TokenCache,
}
impl Auth {
    pub fn from_env() -> Self {
        let read = |name| {
            std::env::var(name)
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(Zeroizing::new)
        };
        Self {
            key: read("GOPLUS_APP_KEY"),
            secret: read("GOPLUS_APP_SECRET"),
            legacy_token: read("GOPLUS_TOKEN"),
            cached: Arc::new(Mutex::new(None)),
        }
    }
    async fn token(
        &self,
        http: &reqwest::Client,
        base: &str,
    ) -> Result<Option<Zeroizing<String>>, String> {
        let (key, secret) = match (&self.key, &self.secret) {
            (Some(k), Some(s)) => (k, s),
            (None, None) => return Ok(self.legacy_token.clone()),
            _ => return Err("configure both GOPLUS_APP_KEY and GOPLUS_APP_SECRET".into()),
        };
        let mut cached = self.cached.lock().await;
        if let Some((token, expires)) = &*cached
            && Instant::now() < *expires
        {
            return Ok(Some(token.clone()));
        }
        let stamp = super::now();
        let mut digest = Sha1::new();
        digest.update(key.as_bytes());
        digest.update(stamp.to_string());
        digest.update(secret.as_bytes());
        let response = http
            .post(format!("{base}/api/v1/token"))
            .json(
                &json!({"app_key":key.as_str(),"time":stamp,"sign":hex::encode(digest.finalize())}),
            )
            .send()
            .await
            .map_err(|_| "GoPlus authentication unavailable")?;
        if !response.status().is_success() {
            return Err("GoPlus authentication rejected".into());
        }
        let value: Value = response
            .json()
            .await
            .map_err(|_| "invalid GoPlus authentication response")?;
        if value["code"] != 1 {
            return Err("GoPlus authentication rejected".into());
        }
        let token = value["result"]["access_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("GoPlus access token missing")?;
        let ttl = value["result"]["expires_in"]
            .as_u64()
            .or_else(|| value["result"]["expires_in"].as_str()?.parse().ok())
            .filter(|v| *v > 0 && *v <= 31_536_000)
            .ok_or("invalid GoPlus token lifetime")?;
        let token = Zeroizing::new(token.to_owned());
        // Refresh early, even for short-lived tokens. No token is written to disk.
        let usable = ttl.saturating_sub((ttl / 10).clamp(1, 60));
        *cached = Some((token.clone(), Instant::now() + Duration::from_secs(usable)));
        Ok(Some(token))
    }
    pub async fn security(&self, http: &reqwest::Client, mint: &str) -> Result<Value, String> {
        self.security_at(http, mint, "https://api.gopluslabs.io")
            .await
    }
    async fn security_at(
        &self,
        http: &reqwest::Client,
        mint: &str,
        base: &str,
    ) -> Result<Value, String> {
        for attempt in 0..2 {
            let token = self.token(http, base).await?;
            let mut request = http
                .get(format!("{base}/api/v1/solana/token_security"))
                .query(&[("contract_addresses", mint)]);
            if let Some(t) = &token {
                request = request.header("Authorization", t.as_str());
            }
            let response = request
                .send()
                .await
                .map_err(|_| "GoPlus unavailable; no entry")?;
            if matches!(response.status().as_u16(), 401 | 403) && attempt == 0 && self.key.is_some()
            {
                let mut cached = self.cached.lock().await;
                if cached.as_ref().map(|(t, _)| t.as_str()) == token.as_ref().map(|t| t.as_str()) {
                    *cached = None;
                }
                continue;
            }
            if !response.status().is_success() {
                return Err("GoPlus request rejected; no entry".into());
            }
            let value: Value = response.json().await.map_err(|_| "invalid GoPlus data")?;
            if value["code"] != 1 {
                return Err("GoPlus API rejected request; no entry".into());
            }
            return Ok(value);
        }
        Err("GoPlus authentication failed; no entry".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::{get, post},
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[tokio::test]
    async fn auth_refresh_cache_expiry_and_fail_closed() {
        async fn issue(State(n): State<Arc<AtomicUsize>>, Json(v): Json<Value>) -> Json<Value> {
            let expected = format!("{:x}", Sha1::digest(format!("app{}secret", v["time"])));
            assert_eq!(v["sign"], expected);
            assert_eq!(v["app_key"], "app");
            let count = n.fetch_add(1, Ordering::SeqCst) + 1;
            Json(
                json!({"code":1,"result":{"access_token":format!("token{count}"),"expires_in":3600}}),
            )
        }
        async fn security(headers: HeaderMap) -> (StatusCode, Json<Value>) {
            let t = headers.get("authorization").unwrap().to_str().unwrap();
            assert!(!t.starts_with("Bearer "));
            if t == "token1" {
                return (StatusCode::UNAUTHORIZED, Json(json!({"code":401})));
            }
            (StatusCode::OK, Json(json!({"code":1,"result":{}})))
        }
        let count = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/api/v1/token", post(issue))
            .route("/api/v1/solana/token_security", get(security))
            .with_state(count.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let auth = Auth {
            key: Some(Zeroizing::new("app".into())),
            secret: Some(Zeroizing::new("secret".into())),
            legacy_token: None,
            cached: Arc::new(Mutex::new(None)),
        };
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        assert!(auth.security_at(&http, "mint", &base).await.is_ok());
        assert_eq!(count.load(Ordering::SeqCst), 2);
        let calls = (0..8).map(|_| auth.security_at(&http, "mint", &base));
        assert!(
            futures_util::future::join_all(calls)
                .await
                .into_iter()
                .all(|r| r.is_ok())
        );
        assert_eq!(count.load(Ordering::SeqCst), 2);
        auth.cached.lock().await.as_mut().unwrap().1 = Instant::now();
        assert!(auth.security_at(&http, "mint", &base).await.is_ok());
        assert_eq!(count.load(Ordering::SeqCst), 3);
        let partial = Auth {
            secret: None,
            ..auth.clone()
        };
        assert!(partial.security_at(&http, "mint", &base).await.is_err());
        assert_eq!(count.load(Ordering::SeqCst), 3);
        task.abort();
    }
}
