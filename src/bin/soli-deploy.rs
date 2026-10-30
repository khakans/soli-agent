//! Fixed-path Linux operator helper. Install root-owned; sudo permits only status/deploy.
#[cfg(any(unix, test))]
use sha2::{Digest, Sha256};

#[cfg(any(unix, test))]
fn elf(bytes: &[u8]) -> bool {
    bytes.len() >= 64
        && &bytes[..6] == b"\x7fELF\x02\x01"
        && [2, 3].contains(&u16::from_le_bytes([bytes[16], bytes[17]]))
        && u16::from_le_bytes([bytes[18], bytes[19]]) == 62
}
#[cfg(any(unix, test))]
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(unix)]
mod linux {
    use super::*;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::{
        fs,
        io::{Read, Write},
        path::Path,
        process::Command,
    };
    const SOURCE: &str = "/home/ubuntu/.agents/skills/soli-agent/target/release/soli-agent";
    const HOME: &str = "/var/lib/soli-engine";
    const TARGET: &str = "/var/lib/soli-engine/bin/soli-agent";
    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
    fn systemctl(args: &[&str]) -> Result<String> {
        let r = Command::new("/usr/bin/systemctl")
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .args(args)
            .output()?;
        if !r.status.success() {
            return Err("systemd operation failed; inspect operator journal".into());
        }
        Ok(String::from_utf8(r.stdout)?.trim().into())
    }
    fn dry() -> Result<()> {
        let env = Path::new(HOME).join(".env");
        let mode = if env.exists() {
            let entries = dotenvy::from_read_iter(fs::File::open(env)?)
                .collect::<std::result::Result<std::collections::BTreeMap<_, _>, _>>()
                .map_err(|_| "invalid engine profile")?;
            entries.get("SOLI_DRY_RUN").cloned()
        } else {
            let v: serde_json::Value =
                serde_json::from_slice(&fs::read(Path::new(HOME).join("engine.json"))?)?;
            v["SOLI_DRY_RUN"].as_str().map(String::from)
        };
        if mode.as_deref() != Some("true") {
            return Err("automatic deployment requires explicit dry run".into());
        }
        Ok(())
    }
    fn artifact() -> Result<Vec<u8>> {
        let f = fs::OpenOptions::new()
            .read(true)
            .custom_flags(0x20000)
            .open(SOURCE)?; // Linux O_NOFOLLOW.
        let m = f.metadata()?;
        if !m.is_file() || m.uid() != 1000 || m.len() > 100 * 1024 * 1024 || m.len() < 64 {
            return Err(
                "artifact must be an ubuntu-owned regular ELF file, at most 100 MiB".into(),
            );
        }
        let mut b = Vec::new();
        f.take(100 * 1024 * 1024 + 1).read_to_end(&mut b)?;
        if b.len() > 100 * 1024 * 1024 || !elf(&b) {
            return Err("invalid Linux x86_64 artifact".into());
        }
        Ok(b)
    }
    fn health() -> Result<()> {
        let http = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(2))
            .build()?;
        for _ in 0..20 {
            if let Ok(r) = http.get("http://127.0.0.1:8787/health").send()
                && r.status().is_success()
                && let Ok(v) = r.json::<serde_json::Value>()
                && v["mode"] == "dry_run"
            {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        Err("dry-run health verification failed".into())
    }
    fn install(bytes: &[u8]) -> Result<()> {
        let mut tmp = tempfile::NamedTempFile::new_in(HOME)?;
        tmp.write_all(bytes)?;
        tmp.as_file().sync_all()?;
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o755))?;
        let id = Command::new("/usr/bin/id")
            .args(["-u", "soli-engine"])
            .output()?;
        if !id.status.success() {
            return Err("engine account missing".into());
        }
        let uid = String::from_utf8(id.stdout)?.trim().parse::<u32>()?;
        std::os::unix::fs::chown(tmp.path(), Some(uid), None)?;
        // Destination directories belong to the engine. Rename with that account,
        // so redirected paths cannot turn this helper into a privileged file writer.
        let r = Command::new("/usr/sbin/runuser")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .args(["-u", "soli-engine", "--", "/usr/bin/mv", "-f", "--"])
            .arg(tmp.path())
            .arg(TARGET)
            .output()?;
        if !r.status.success() {
            return Err("unprivileged atomic engine install failed".into());
        }
        Ok(())
    }
    pub fn run() -> Result<()> {
        let uid = fs::read_to_string("/proc/self/status")?;
        if !uid
            .lines()
            .any(|l| l.starts_with("Uid:") && l.split_whitespace().nth(2) == Some("0"))
        {
            return Err("run the installed helper through its restricted sudo rule".into());
        }
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        if args.len() != 1 || !["status", "deploy"].contains(&args[0].as_str()) {
            return Err("usage: soli-deploy status|deploy".into());
        }
        dry()?;
        let candidate = artifact()?;
        let mut old = Vec::new();
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(0x20000)
            .open(TARGET)?
            .take(100 * 1024 * 1024 + 1)
            .read_to_end(&mut old)?;
        if !elf(&old) || old.len() > 100 * 1024 * 1024 {
            return Err("invalid existing engine binary".into());
        }
        if args[0] == "deploy" {
            let unit = systemctl(&[
                "show",
                "soli-engine.service",
                "-p",
                "User",
                "-p",
                "ExecStart",
                "-p",
                "ProtectHome",
                "-p",
                "NoNewPrivileges",
            ])?;
            if !unit.lines().any(|l| l == "User=soli-engine")
                || !unit.contains(TARGET)
                || !unit.contains("ProtectHome=yes")
                || !unit.contains("NoNewPrivileges=yes")
            {
                return Err("engine service does not enforce the required boundary".into());
            }
            install(&candidate)?;
            if systemctl(&["restart", "soli-engine.service"])
                .and_then(|_| health())
                .is_err()
            {
                install(&old)?;
                systemctl(&["restart", "soli-engine.service"])?;
                health()?;
                return Err("deployment failed; previous engine restored and healthy".into());
            }
            let receipt = serde_json::json!({"artifact_sha256":hash(&candidate),"target":TARGET,"mode":"dry_run","health_verified":true,"previous_sha256":hash(&old)});
            let mut file = tempfile::NamedTempFile::new_in(HOME)?;
            file.write_all(&serde_json::to_vec_pretty(&receipt)?)?;
            file.as_file().sync_all()?;
            file.persist(Path::new(HOME).join("deployment.json"))
                .map_err(|_| "receipt replacement failed")?;
        }
        health()?;
        println!(
            "{}",
            serde_json::json!({"mode":"dry_run","service":systemctl(&["is-active","soli-engine.service"])?,"artifact_sha256":hash(&candidate),"deployed_sha256":hash(&fs::read(TARGET)?),"matches":candidate==fs::read(TARGET)?,"scope":"engine only; signer, wallet and /opt binary untouched"})
        );
        Ok(())
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        linux::run()
    }
    #[cfg(not(unix))]
    {
        Err("soli-deploy is Linux-only".into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn artifact_validation_rejects_scripts_wrong_arch_and_short_headers() {
        let mut b = vec![0; 64];
        b[..6].copy_from_slice(b"\x7fELF\x02\x01");
        b[16] = 3;
        b[18] = 62;
        assert!(elf(&b));
        b[18] = 183;
        assert!(!elf(&b));
        assert!(!elf(b"#!/bin/sh"));
        assert_ne!(hash(b"one"), hash(b"two"));
    }
}
