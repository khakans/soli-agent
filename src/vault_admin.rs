//! Operator-only key provisioning; no HTTP endpoint or chat tool exposes these commands.
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};
use std::{io::Write, path::Path};
use zeroize::Zeroizing;
pub async fn command(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() != 3 || !["generate", "import", "verify"].contains(&args[1].as_str()) {
        return Err("usage: soli-agent vault-key generate|import|verify KEY_NAME (import reads SOLI_IMPORT_KEY_FILE)".into());
    }
    let name = &args[2];
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("invalid Vault key name".into());
    }
    let address = std::env::var("VAULT_ADDR")?;
    let url = reqwest::Url::parse(&address)?;
    if url.scheme() != "https"
        && !(url.scheme() == "http"
            && [Some("127.0.0.1"), Some("localhost")].contains(&url.host_str()))
    {
        return Err("Vault requires HTTPS except loopback".into());
    }
    let token = Zeroizing::new(std::env::var("VAULT_TOKEN")?);
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let base = format!("{}/v1/transit/keys/{name}", address.trim_end_matches('/'));
    let existing = http
        .get(&base)
        .header("X-Vault-Token", token.as_str())
        .send()
        .await
        .map_err(|_| "Vault unreachable")?;
    if args[1] != "verify" {
        if existing.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(
                "provisioning requires a new key name; existing keys are never overwritten".into(),
            );
        }
        if args[1] == "generate" {
            let r=http.post(&base).header("X-Vault-Token",token.as_str()).json(&json!({"type":"ed25519","exportable":false,"allow_plaintext_backup":false,"derived":false})).send().await.map_err(|_|"key creation request failed")?;
            if !r.status().is_success() {
                return Err("Vault rejected key generation".into());
            }
        } else {
            let path = std::env::var("SOLI_IMPORT_KEY_FILE")?;
            let file = Path::new(&path);
            let meta = std::fs::symlink_metadata(file)?;
            if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 4096 {
                return Err("import must be a small regular local file, not a symlink".into());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o077 != 0 {
                    return Err("import key file must have mode 0600".into());
                }
            }
            let text = Zeroizing::new(std::fs::read_to_string(file)?);
            let key = decode_solana_key(&text)?;
            let seed: &[u8; 32] = key[..32].try_into().map_err(|_| "invalid Ed25519 seed")?;
            let secret = ed25519_dalek::SigningKey::from_bytes(seed);
            let public = secret.verifying_key();
            if key.len() == 64 && key[32..] != public.to_bytes() {
                return Err("Solana key public half does not match seed".into());
            }
            // RFC 8410 PKCS#8 DER wrapping, then the official Vault CLI performs BYOK RSA/AES wrapping.
            let mut der = Zeroizing::new(hex::decode("302e020100300506032b657004220420")?);
            der.extend_from_slice(&key[..32]);
            let encoded = Zeroizing::new(B64.encode(&*der));
            let mut temp = tempfile::NamedTempFile::new()?;
            temp.write_all(encoded.as_bytes())?;
            temp.flush()?;
            let status = std::process::Command::new("vault")
                .args([
                    "transit",
                    "import",
                    &format!("transit/keys/{name}"),
                    &format!("@{}", temp.path().display()),
                    "type=ed25519",
                    "exportable=false",
                    "allow_plaintext_backup=false",
                    "derived=false",
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()?;
            if !status.success() {
                return Err(
                    "Vault CLI BYOK import failed; check Vault edition/API support and permissions"
                        .into(),
                );
            }
        }
    }
    let r = http
        .get(&base)
        .header("X-Vault-Token", token.as_str())
        .send()
        .await
        .map_err(|_| "key verification unavailable")?;
    if !r.status().is_success() {
        return Err("key verification rejected".into());
    }
    let v: Value = r.json().await?;
    let d = &v["data"];
    if d["type"] != "ed25519"
        || d["exportable"] != false
        || d["allow_plaintext_backup"] != false
        || d["derived"] != false
    {
        return Err("key policy incompatible with Soli".into());
    }
    let version = d["latest_version"].as_u64().ok_or("missing version")?;
    let public = B64.decode(
        d["keys"][version.to_string()]["public_key"]
            .as_str()
            .ok_or("missing public key")?,
    )?;
    let challenge = format!("Soli Ed25519 compatibility test {}", uuid::Uuid::new_v4());
    let r=http.post(format!("{}/v1/transit/sign/{name}",address.trim_end_matches('/'))).header("X-Vault-Token",token.as_str()).json(&json!({"input":B64.encode(challenge.as_bytes()),"prehashed":false,"key_version":version})).send().await.map_err(|_|"signing test unavailable (Vault may be sealed)")?;
    if !r.status().is_success() {
        return Err("Vault rejected compatibility test".into());
    }
    let signed: Value = r.json().await?;
    let prefix = format!("vault:v{version}:");
    let sig = B64.decode(
        signed["data"]["signature"]
            .as_str()
            .and_then(|s| s.strip_prefix(&prefix))
            .ok_or("invalid Vault signature format")?,
    )?;
    let sig =
        solana_signature::Signature::try_from(sig.as_slice()).map_err(|_| "invalid signature")?;
    if public.len() != 32 || !sig.verify(&public, challenge.as_bytes()) {
        return Err("Ed25519 signing compatibility verification failed".into());
    }
    println!(
        "Verified non-exportable wallet {} (Vault key version {version}); no transaction signed",
        bs58::encode(public).into_string()
    );
    Ok(())
}
pub(crate) fn decode_solana_key(
    text: &str,
) -> Result<Zeroizing<Vec<u8>>, Box<dyn std::error::Error>> {
    let key = if text.trim_start().starts_with('[') {
        serde_json::from_str::<Vec<u8>>(text)?
    } else {
        let raw = text.trim();
        let hex_key = raw.strip_prefix("0x").unwrap_or(raw);
        if [64, 128].contains(&hex_key.len()) && hex_key.bytes().all(|c| c.is_ascii_hexdigit()) {
            hex::decode(hex_key)?
        } else {
            bs58::decode(raw).into_vec()?
        }
    };
    if ![32, 64].contains(&key.len()) {
        return Err("expected a 32-byte seed or 64-byte Solana keypair file".into());
    }
    Ok(Zeroizing::new(key))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_local_solana_key_formats_only() {
        assert!(decode_solana_key(&serde_json::to_string(&vec![1u8; 64]).unwrap()).is_ok());
        assert!(decode_solana_key(&bs58::encode([2u8; 32]).into_string()).is_ok());
        assert_eq!(
            &*decode_solana_key(&hex::encode([2u8; 32])).unwrap(),
            &[2u8; 32]
        );
        assert_eq!(
            &*decode_solana_key(&format!("0x{}", hex::encode([2u8; 64]))).unwrap(),
            &[2u8; 64]
        );
        assert!(decode_solana_key("[1,2,3]").is_err());
    }
}
