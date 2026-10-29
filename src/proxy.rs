use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Method, StatusCode},
    response::Response,
    routing::any,
};
use serde_json::json;
#[derive(Clone)]
struct Proxy {
    http: reqwest::Client,
    url: String,
    access: crate::access::Access,
    token: String,
}
async fn forward(
    State(p): State<Proxy>,
    method: Method,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut request = p
        .http
        .request(method.clone(), format!("{}{}", p.url, uri.path()))
        .body(body)
        .header("content-type", "application/json");
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if ["/v1/live/status", "/v1/live/previews"].contains(&uri.path()) {
        match p.access.identify(bearer, method.as_str(), uri.path()) {
            Ok(id) => request = request.bearer_auth(&p.token).header("x-soli-agent-id", id),
            Err(_) => {
                return Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .body(Body::empty())
                    .unwrap();
            }
        }
    } else if let Some(token) = headers.get("authorization") {
        request = request.header("authorization", token);
    }
    match request.send().await {
        Ok(r) => {
            let status = r.status();
            match r.bytes().await {
                Ok(b) => Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(Body::from(b))
                    .unwrap(),
                Err(_) => failure(),
            }
        }
        Err(_) => failure(),
    }
}
fn failure() -> Response {
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"error":"isolated signer unavailable; no execution attempted by engine"})
                .to_string(),
        ))
        .unwrap()
}
pub fn routes() -> Result<Router, String> {
    let token = std::env::var("SOLI_API_TOKEN").map_err(|_| "engine credential missing")?;
    let url = std::env::var("SOLI_SIGNER_URL").unwrap_or("http://127.0.0.1:8788".into());
    let parsed = reqwest::Url::parse(&url).map_err(|_| "invalid signer URL")?;
    if parsed.scheme() != "http"
        || ![Some("127.0.0.1"), Some("localhost")].contains(&parsed.host_str())
    {
        return Err("v1 signer proxy must use loopback HTTP behind OS user isolation".into());
    }
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "client initialization failed")?;
    Ok(Router::new()
        .route("/v1/live/{*path}", any(forward))
        .layer(axum::extract::DefaultBodyLimit::max(4096))
        .with_state(Proxy {
            http,
            url: url.trim_end_matches('/').into(),
            access: crate::access::Access::from_env(&token),
            token,
        }))
}
