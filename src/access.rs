//! Agent credentials contain only hashes on disk and are reloaded for revocation.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

#[derive(Clone)]
pub struct Access {
    master: String,
    file: Option<PathBuf>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    pub id: String,
    pub sha256: String,
    pub role: Role,
    #[serde(default)]
    pub revoked: bool,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Read,
    Preview,
    Paper,
}
impl Role {
    fn permits(self, method: &str, path: &str) -> bool {
        if method == "GET" {
            return true;
        }
        match self {
            Self::Read => false,
            Self::Preview => {
                method == "POST" && ["/v1/paper/previews", "/v1/live/previews"].contains(&path)
            }
            Self::Paper => {
                method == "POST"
                    && [
                        "/v1/paper/previews",
                        "/v1/live/previews",
                        "/v1/paper/orders",
                        "/v1/paper/strategy",
                    ]
                    .contains(&path)
            }
        }
    }
}
impl Access {
    pub fn from_env(master: &str) -> Self {
        Self {
            master: master.into(),
            file: std::env::var_os("SOLI_AGENT_KEYS_FILE")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
        }
    }
    pub fn identify(&self, bearer: &str, method: &str, path: &str) -> Result<String, &'static str> {
        let token = bearer.strip_prefix("Bearer ").ok_or("missing credential")?;
        if token == self.master {
            return Ok("operator".into());
        }
        let file = self.file.as_ref().ok_or("invalid credential")?;
        let credentials: Vec<Credential> = serde_json::from_slice(
            &std::fs::read(file).map_err(|_| "credential registry unavailable")?,
        )
        .map_err(|_| "credential registry invalid")?;
        let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
        let c = credentials
            .iter()
            .find(|c| c.sha256 == hash && !c.revoked)
            .ok_or("invalid or revoked credential")?;
        if c.id.is_empty()
            || c.id.len() > 64
            || !c
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("invalid agent identity");
        }
        if !c.role.permits(method, path) {
            return Err("credential scope does not permit operation");
        }
        Ok(c.id.clone())
    }
}
pub fn service_identity() -> Result<String, String> {
    let identity = std::process::Command::new("whoami")
        .output()
        .map_err(|_| "cannot verify service account identity")?;
    if !identity.status.success() || identity.stdout.is_empty() {
        return Err("service identity check failed".into());
    }
    Ok(format!("{:x}", Sha256::digest(&identity.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scopes_and_revocation_are_enforced_without_restart() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let auth = Access {
            master: "operator".into(),
            file: Some(file.path().into()),
        };
        let mut c = Credential {
            id: "agent-a".into(),
            sha256: format!("{:x}", Sha256::digest(b"secret")),
            role: Role::Read,
            revoked: false,
        };
        let write = |c: &Credential| {
            std::fs::write(file.path(), serde_json::to_vec(&[c]).unwrap()).unwrap()
        };
        write(&c);
        assert!(auth.identify("Bearer secret", "GET", "/v1/report").is_ok());
        assert!(
            auth.identify("Bearer secret", "POST", "/v1/paper/orders")
                .is_err()
        );
        c.role = Role::Preview;
        write(&c);
        assert!(
            auth.identify("Bearer secret", "POST", "/v1/live/previews")
                .is_ok()
        );
        assert!(
            auth.identify("Bearer secret", "POST", "/v1/live/execute")
                .is_err()
        );
        c.role = Role::Paper;
        write(&c);
        assert!(
            auth.identify("Bearer secret", "POST", "/v1/paper/orders")
                .is_ok()
        );
        assert!(
            auth.identify("Bearer secret", "POST", "/v1/paper/strategy")
                .is_ok()
        );
        assert!(
            auth.identify("Bearer secret", "POST", "/v1/settings")
                .is_err()
        );
        assert!(
            auth.identify("Bearer secret", "POST", "/v1/live/strategy")
                .is_err()
        );
        c.revoked = true;
        write(&c);
        assert!(auth.identify("Bearer secret", "GET", "/v1/status").is_err());
    }
}
