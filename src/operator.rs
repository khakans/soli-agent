//! Native installation, host configuration and service lifecycle.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub fn handles(cmd: &str) -> bool {
    [
        "install",
        "setup",
        "configure",
        "agent-add",
        "agent-revoke",
        "connect",
        "start",
        "stop",
        "run",
        "doctor",
        "service",
        "update",
        "uninstall",
        "notification-add",
        "wallet-check",
    ]
    .contains(&cmd)
}
pub fn home() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("SOLI_HOME") {
        let path = PathBuf::from(p);
        return Ok(if path.is_absolute() {
            path
        } else {
            std::env::current_dir()?.join(path)
        });
    }
    let exe = std::env::current_exe()?;
    if let Some(p) = exe.parent().and_then(Path::parent)
        && (p.join("engine.json").exists() || p.join(".env").exists())
    {
        return Ok(p.into());
    }
    Ok(std::env::current_dir()?.join(".soli"))
}
fn load(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}
fn run(c: &mut Command) -> Result<()> {
    if !c.status()?.success() {
        return Err("operator command failed; inspect the local output".into());
    }
    Ok(())
}
fn protect(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(if path.is_dir() { 0o700 } else { 0o600 }),
        )?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let sid = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "[Security.Principal.WindowsIdentity]::GetCurrent().User.Value",
            ])
            .creation_flags(0x08000000)
            .output()?;
        if !sid.status.success() {
            return Err("cannot protect local credentials".into());
        }
        let grant = format!(
            "*{}:{}",
            String::from_utf8(sid.stdout)?.trim(),
            if path.is_dir() { "(OI)(CI)F" } else { "F" }
        );
        run(Command::new("icacls")
            .arg(path)
            .args(["/inheritance:r", "/grant:r", &grant])
            .stdout(Stdio::null())
            .creation_flags(0x08000000))?;
    }
    Ok(())
}
fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or("missing parent directory")?;
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    protect(temp.path())?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|_| "atomic configuration replacement failed")?;
    Ok(())
}
fn save(path: &Path, value: &Value) -> Result<()> {
    write(path, &serde_json::to_vec_pretty(value)?)
}
fn token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
fn binary(home: &Path) -> PathBuf {
    home.join("bin").join(if cfg!(windows) {
        "soli-agent.exe"
    } else {
        "soli-agent"
    })
}
fn id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err("ID must contain 1-64 ASCII letters, numbers, hyphens or underscores".into());
    }
    Ok(())
}
fn add_agent(home: &Path, name: &str, role: &str, rotate: bool) -> Result<()> {
    id(name)?;
    if !["read", "preview", "paper"].contains(&role) {
        return Err("role must be read, preview or paper".into());
    }
    let file = home.join("credentials").join(format!("{name}.json"));
    std::fs::create_dir_all(file.parent().unwrap())?;
    protect(file.parent().unwrap())?;
    let credential = if file.exists() && !rotate {
        load(&file)?
    } else {
        json!({"token":token()})
    };
    save(&file, &credential)?;
    let mut registry = load(&home.join("agents.json"))?
        .as_array()
        .ok_or("invalid registry")?
        .clone();
    registry.retain(|r| r["id"] != name);
    registry.push(json!({"id":name,"role":role,"revoked":false,"sha256":format!("{:x}",Sha256::digest(credential["token"].as_str().ok_or("missing credential")?))}));
    save(&home.join("agents.json"), &json!(registry))?;
    save(
        &home.join(format!("mcp-{name}.json")),
        &json!({"mcpServers":{"soli":{"command":binary(home),"args":["mcp"],"env":{"SOLI_CREDENTIAL_FILE":file}}}}),
    )?;
    println!("Agent {name}: {role}; credentials and MCP configuration saved privately");
    Ok(())
}
fn profile(home: &Path, role: &str) -> Result<BTreeMap<String, String>> {
    let path = home.join(".env");
    if path.exists() || path.is_symlink() {
        let meta = std::fs::symlink_metadata(&path)?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(".env must be a regular private file".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o077 != 0 {
                return Err(".env must have mode 0600".into());
            }
        }
        let mut settings = BTreeMap::new();
        for entry in dotenvy::from_read_iter(std::fs::File::open(path)?) {
            let (key, value) =
                entry.map_err(|_| "invalid .env syntax; credential values suppressed")?;
            if settings.insert(key, value).is_some() {
                return Err("duplicate setting in .env".into());
            }
        }
        return Ok(settings);
    }
    Ok(serde_json::from_value(load(
        &home.join(format!("{role}.json")),
    )?)?)
}
fn save_profile(home: &Path, role: &str, settings: &BTreeMap<String, String>) -> Result<()> {
    if home.join(".env").exists() {
        let mut text = String::from("# Private Soli service settings. Never commit this file.\n");
        for (key, value) in settings {
            if !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                || value.chars().any(char::is_control)
            {
                return Err("unsupported .env setting name or control character".into());
            }
            text.push_str(&format!(
                "{key}={}\n",
                serde_json::to_string(value)?.replace('$', "\\$")
            ));
        }
        write(&home.join(".env"), text.as_bytes())
    } else {
        save(
            &home.join(format!("{role}.json")),
            &serde_json::to_value(settings)?,
        )
    }
}
pub fn service_environment(home: &Path) -> BTreeMap<std::ffi::OsString, std::ffi::OsString> {
    let mut env: BTreeMap<_, _> = std::env::vars_os()
        .filter(|(k, _)| {
            let k = k.to_string_lossy().to_ascii_uppercase();
            !k.starts_with("SOLI_")
                && !k.starts_with("VAULT_")
                && k != "JUPITER_API_KEY"
                && !k.starts_with("GOPLUS_")
        })
        .collect();
    env.insert("SOLI_HOME".into(), home.as_os_str().to_owned());
    env
}
fn spawned(home: &Path, role: &str) -> Result<Command> {
    spawned_command(
        home,
        role,
        if role == "engine" { "serve" } else { "signer" },
    )
}
fn spawned_command(home: &Path, role: &str, entrypoint: &str) -> Result<Command> {
    if !["engine", "signer"].contains(&role) {
        return Err("role must be engine or signer".into());
    }
    let mut c = Command::new(binary(home));
    c.arg(entrypoint);
    for (name, _) in std::env::vars().filter(|(n, _)| {
        n.starts_with("SOLI_")
            || n.starts_with("VAULT_")
            || n == "JUPITER_API_KEY"
            || n.starts_with("GOPLUS_")
    }) {
        c.env_remove(name);
    }
    let settings = profile(home, role)?;
    if role == "engine"
        && settings.iter().any(|(k, v)| {
            !v.is_empty()
                && (k.starts_with("VAULT_") || k == "SOLI_OWNER_TOKEN" || k == "SOLI_PRIVATE_KEY")
        })
    {
        return Err("signer secrets must not be stored in the engine profile".into());
    }
    c.envs(settings);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x08000000);
    }
    Ok(c)
}
fn process(pid: u32) -> Option<(u64, PathBuf)> {
    let sys = sysinfo::System::new_all();
    let p = sys.process(sysinfo::Pid::from_u32(pid))?;
    Some((p.start_time(), p.exe()?.to_path_buf()))
}
fn stamp(pid: u32) -> Result<Value> {
    let (start, exe) = process(pid).ok_or("service exited")?;
    Ok(json!({"pid":pid,"start":start,"exe":exe}))
}
fn terminate(record: &Value) -> Result<()> {
    let pid = record["pid"].as_u64().ok_or("invalid PID record")? as u32;
    if let Some((start, exe)) = process(pid) {
        if Some(start) != record["start"].as_u64()
            || exe != Path::new(record["exe"].as_str().ok_or("invalid executable record")?)
        {
            return Err("PID identity changed; refusing to stop unrelated process".into());
        }
        let sys = sysinfo::System::new_all();
        if let Some(p) = sys.process(sysinfo::Pid::from_u32(pid)) {
            p.kill_with(sysinfo::Signal::Term)
                .unwrap_or_else(|| p.kill());
        }
    }
    Ok(())
}
fn stop(home: &Path, role: &str) -> Result<()> {
    if !["engine", "signer"].contains(&role) {
        return Err("invalid service role".into());
    }
    let file = home.join(format!("{role}.pid.json"));
    if file.exists() {
        let r = load(&file)?;
        terminate(&r["child"])?;
        std::thread::sleep(Duration::from_millis(300));
        terminate(&r["wrapper"])?;
        let _ = std::fs::remove_file(&file);
    }
    Ok(())
}
async fn api(home: &Path, path: &str, body: Option<Value>) -> Result<Value> {
    let env = profile(home, "engine")?;
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(180))
        .build()?;
    let url = format!("http://127.0.0.1:8787{path}");
    let request = if let Some(body) = body {
        http.post(url).json(&body)
    } else {
        http.get(url)
    };
    let response = request
        .bearer_auth(
            env.get("SOLI_API_TOKEN")
                .ok_or("missing engine credential")?,
        )
        .send()
        .await
        .map_err(|_| "engine unavailable")?;
    if !response.status().is_success() {
        return Err("engine rejected operator request".into());
    }
    Ok(response.json().await?)
}
async fn start(home: &Path, role: &str) -> Result<()> {
    if !["engine", "signer"].contains(&role) {
        return Err("invalid service role".into());
    }
    let file = home.join(format!("{role}.pid.json"));
    if file.exists()
        && process(load(&file)?["wrapper"]["pid"].as_u64().unwrap_or(0) as u32).is_some()
    {
        return Err("service is already running".into());
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join(format!("{role}.log")))?;
    protect(&home.join(format!("{role}.log")))?;
    let mut cmd = Command::new(binary(home));
    cmd.args(["run", role])
        .env_clear()
        .envs(service_environment(home))
        .env("SOLI_HOME", home)
        .stdin(Stdio::null())
        .stderr(log.try_clone()?)
        .stdout(log);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    let pid = detached_windows(&binary(home), role, home)?;
    #[cfg(not(windows))]
    let pid = cmd.spawn()?.id();
    for _ in 0..300 {
        if process(pid).is_none() {
            return Err("service startup failed; inspect its private log".into());
        }
        let registered = file.exists()
            && load(&file).is_ok_and(|r| r["wrapper"]["pid"].as_u64() == Some(pid as u64));
        if registered && role == "engine" && api(home, "/v1/status", None).await.is_ok() {
            println!("Engine started");
            return Ok(());
        }
        if role == "signer" && registered {
            println!("Signer launched; verify its health and private log");
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err("service readiness timed out".into())
}
#[cfg(windows)]
fn detached_windows(exe: &Path, role: &str, home: &Path) -> Result<u32> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, PROCESS_INFORMATION,
            STARTUPINFOW,
        },
    };
    // The std launcher inherits unrelated inheritable handles on stable Windows Rust.
    // A native launch with inheritance disabled prevents CLI capture pipes staying open.
    let application: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut command: Vec<u16> = format!("\"{}\" run {role}", exe.display())
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut pairs = service_environment(home).into_iter().collect::<Vec<_>>();
    pairs.sort_by_key(|(k, _)| k.to_string_lossy().to_uppercase());
    let mut environment = Vec::<u16>::new();
    for (key, value) in pairs {
        environment.extend(key.encode_wide());
        environment.push(b'=' as u16);
        environment.extend(value.encode_wide());
        environment.push(0);
    }
    environment.push(0);
    // SAFETY: initialized Win32 structures and NUL-terminated buffers remain valid for
    // the synchronous CreateProcessW call. No credential material enters the command.
    unsafe {
        let mut startup: STARTUPINFOW = std::mem::zeroed();
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut info: PROCESS_INFORMATION = std::mem::zeroed();
        if CreateProcessW(
            application.as_ptr(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
            std::ptr::null(),
            &startup,
            &mut info,
        ) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        CloseHandle(info.hThread);
        CloseHandle(info.hProcess);
        Ok(info.dwProcessId)
    }
}
fn connect(home: &Path, name: &str, path: &Path, format: &str) -> Result<()> {
    id(name)?;
    let config = load(&home.join(format!("mcp-{name}.json")))?["mcpServers"]["soli"].clone();
    if config.is_null() {
        return Err("agent configuration missing".into());
    }
    let current = std::fs::read_to_string(path).unwrap_or_default();
    if format == "json" {
        let mut doc: Value = if current.trim().is_empty() {
            json!({"mcpServers":{}})
        } else {
            serde_json::from_str(&current)?
        };
        if doc["mcpServers"].is_null() {
            doc["mcpServers"] = json!({});
        }
        if !doc["mcpServers"]["soli"].is_null() && doc["mcpServers"]["soli"] != config {
            return Err("existing Soli entry differs; review before replacing".into());
        }
        doc["mcpServers"]["soli"] = config.clone();
        save(path, &doc)?;
    } else if format == "toml" {
        let mut doc = current.parse::<toml_edit::DocumentMut>()?;
        if doc.get("mcp_servers").and_then(|v| v.get("soli")).is_some() {
            return Err("Soli TOML entry already exists; keep or review it".into());
        }
        let mut table = toml_edit::Table::new();
        table["command"] = toml_edit::value(config["command"].as_str().ok_or("missing binary")?);
        let mut args = toml_edit::Array::new();
        args.push("mcp");
        table["args"] = toml_edit::value(args);
        table["startup_timeout_sec"] = toml_edit::value(30);
        table["tool_timeout_sec"] = toml_edit::value(240);
        let mut env = toml_edit::Table::new();
        env["SOLI_CREDENTIAL_FILE"] = toml_edit::value(
            config["env"]["SOLI_CREDENTIAL_FILE"]
                .as_str()
                .ok_or("missing credential file")?,
        );
        table["env"] = toml_edit::Item::Table(env);
        doc["mcp_servers"]["soli"] = toml_edit::Item::Table(table);
        write(path, doc.to_string().as_bytes())?;
    } else {
        return Err("format must be json or toml".into());
    }
    let records = home.join("connections.json");
    let mut all = if records.exists() {
        load(&records)?
            .as_array()
            .ok_or("invalid connection records")?
            .clone()
    } else {
        vec![]
    };
    all.push(json!({"path":path,"format":format,"command":config["command"],"credential_file":config["env"]["SOLI_CREDENTIAL_FILE"]}));
    save(&records, &json!(all))?;
    println!("Soli entry saved; reload the host's MCP connection");
    Ok(())
}
fn service(home: &Path, remove: bool) -> Result<()> {
    let name = format!(
        "soli-{}",
        &format!("{:x}", Sha256::digest(home.to_string_lossy().as_bytes()))[..10]
    );
    if cfg!(windows) {
        if remove {
            let _ = Command::new("schtasks")
                .args(["/Delete", "/TN", &name, "/F"])
                .status();
        } else {
            let task = format!("\"{}\" run engine", binary(home).display());
            run(Command::new("schtasks").args([
                "/Create", "/TN", &name, "/TR", &task, "/SC", "ONLOGON", "/F",
            ]))?;
        }
    } else {
        let dir = PathBuf::from(std::env::var("HOME")?).join(".config/systemd/user");
        std::fs::create_dir_all(&dir)?;
        let unit = format!("{name}.service");
        if remove {
            let _ = Command::new("systemctl")
                .args(["--user", "disable", "--now", &unit])
                .status();
            let _ = std::fs::remove_file(dir.join(&unit));
        } else {
            let exe = binary(home)
                .to_string_lossy()
                .replace('%', "%%")
                .replace('"', "\\\"");
            write(&dir.join(&unit),format!("[Unit]\nDescription=Soli engine\nAfter=network-online.target\n[Service]\nExecStart=\"{exe}\" run engine\nRestart=on-failure\nRestartSec=10\nUMask=0077\n[Install]\nWantedBy=default.target\n").as_bytes())?;
            stop(home, "engine")?;
        }
        run(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
        if !remove {
            run(Command::new("systemctl").args(["--user", "enable", "--now", &unit]))?;
        }
    }
    Ok(())
}
fn disconnect_all(home: &Path) -> Result<()> {
    let records = home.join("connections.json");
    if !records.exists() {
        return Ok(());
    }
    for r in load(&records)?
        .as_array()
        .ok_or("invalid connection records")?
    {
        let path = Path::new(r["path"].as_str().ok_or("invalid host path")?);
        if !path.exists() {
            continue;
        }
        let text = std::fs::read_to_string(path)?;
        if r["format"] == "json" {
            let mut doc: Value = serde_json::from_str(&text)?;
            let entry = &doc["mcpServers"]["soli"];
            if entry["command"] == r["command"]
                && entry["env"]["SOLI_CREDENTIAL_FILE"] == r["credential_file"]
                && entry.as_object().is_some_and(|v| v.len() == 3)
            {
                doc["mcpServers"]
                    .as_object_mut()
                    .ok_or("invalid MCP map")?
                    .remove("soli");
                save(path, &doc)?;
            } else {
                println!("Preserved changed host entry: {}", path.display());
            }
        } else {
            let mut doc = text.parse::<toml_edit::DocumentMut>()?;
            let entry = &doc["mcp_servers"]["soli"];
            if entry["command"].as_str() == r["command"].as_str()
                && entry["env"]["SOLI_CREDENTIAL_FILE"].as_str() == r["credential_file"].as_str()
                && entry.as_table().is_some_and(|v| v.len() == 5)
            {
                doc["mcp_servers"]
                    .as_table_mut()
                    .ok_or("invalid MCP table")?
                    .remove("soli");
                write(path, doc.to_string().as_bytes())?;
            } else {
                println!("Preserved changed host entry: {}", path.display());
            }
        }
    }
    Ok(())
}
pub async fn command(args: &[String]) -> Result<()> {
    let home = home()?;
    let arg = |n: usize| {
        args.get(n)
            .map(String::as_str)
            .ok_or("missing command argument")
    };
    match args[0].as_str() {
        "setup" => {
            let agent = args.get(1).map(String::as_str).unwrap_or("default");
            if !home.join("engine.json").exists() && !home.join(".env").exists() {
                Box::pin(command(&["install".into(), agent.into()])).await?;
            }
            println!("Soli setup: hidden local input; Enter preserves existing settings.");
            let mut settings = profile(&home, "engine")?;
            for name in [
                "SOLI_RPC_URL",
                "SOLI_WS_URL",
                "JUPITER_API_KEY",
                "GOPLUS_APP_KEY",
                "GOPLUS_APP_SECRET",
                "SOLI_WALLET_ADDRESS",
                "SOLI_WATCH_MINTS",
                "SOLI_SMART_WALLETS",
            ] {
                let value = zeroize::Zeroizing::new(rpassword::prompt_password(format!(
                    "{name} (Enter keeps current): "
                ))?);
                if !value.is_empty() {
                    settings.insert(name.into(), value.to_string());
                }
            }
            save_profile(&home, "engine", &settings)?;
            stop(&home, "engine")?;
            start(&home, "engine").await?;
            println!(
                "Engine running. MCP configuration: {}",
                home.join(format!("mcp-{agent}.json")).display()
            );
            Box::pin(command(&["doctor".into(), "--providers".into()])).await?;
        }
        "wallet-check" => run(&mut spawned_command(
            &home,
            "signer",
            "verify-local-wallet",
        )?)?,
        "install" => {
            std::fs::create_dir_all(&home)?;
            let resolved = home.canonicalize()?;
            let user_home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(PathBuf::from)
                .and_then(|p| p.canonicalize().ok());
            if resolved.parent().is_none()
                || resolved == std::env::current_dir()?.canonicalize()?
                || user_home.as_ref() == Some(&resolved)
            {
                return Err("SOLI_HOME must be a dedicated private application directory".into());
            }
            protect(&home)?;
            if !home.join("strategy.json").exists() {
                let source = std::env::current_dir()?.join("config/strategy.json");
                let strategy = std::fs::read(source)
                    .unwrap_or_else(|_| include_bytes!("../config/strategy.json").to_vec());
                let parsed: crate::research::Strategy = serde_json::from_slice(&strategy)?;
                parsed.validate()?;
                write(&home.join("strategy.json"), &strategy)?;
            }
            if !home.join("engine.json").exists() {
                save(
                    &home.join("engine.json"),
                    &json!({"SOLI_API_TOKEN":token(),"SOLI_DRY_RUN":"true","SOLI_AUTO_PAPER":"false","SOLI_DB":home.join("engine.db"),"SOLI_AGENT_KEYS_FILE":home.join("agents.json"),"SOLI_STRATEGY_FILE":home.join("strategy.json"),"SOLI_NOTIFICATIONS_FILE":home.join("notifications.json")}),
                )?;
                save(&home.join("agents.json"), &json!([]))?;
                save(&home.join("notifications.json"), &json!([]))?;
            }
            if profile(&home, "engine")?
                .get("SOLI_DRY_RUN")
                .is_some_and(|v| v == "false")
            {
                return Err("live installation requires reviewed maintenance".into());
            }
            stop(&home, "engine")?;
            std::fs::create_dir_all(home.join("bin"))?;
            if std::env::current_exe()?.canonicalize()?
                != binary(&home).canonicalize().unwrap_or_default()
            {
                std::fs::copy(std::env::current_exe()?, binary(&home))?;
            }
            add_agent(
                &home,
                args.get(1).map(String::as_str).unwrap_or("default"),
                "preview",
                false,
            )?;
            start(&home, "engine").await?;
            println!(
                "Installed natively; no Python required. Configure provider credentials locally before provider acceptance."
            );
        }
        "agent-add" => add_agent(
            &home,
            arg(1)?,
            arg(2)?,
            args.iter().any(|a| a == "--rotate"),
        )?,
        "agent-revoke" => {
            let mut all = load(&home.join("agents.json"))?;
            let mut found = false;
            for r in all.as_array_mut().ok_or("invalid registry")? {
                if r["id"] == arg(1)? {
                    r["revoked"] = json!(true);
                    found = true;
                }
            }
            if !found {
                return Err("unknown agent".into());
            }
            save(&home.join("agents.json"), &all)?;
            println!("Revoked without service restart");
        }
        "configure" => {
            let name = arg(1)?;
            if !(name.starts_with("SOLI_")
                || name.starts_with("VAULT_")
                || [
                    "JUPITER_API_KEY",
                    "GOPLUS_TOKEN",
                    "GOPLUS_APP_KEY",
                    "GOPLUS_APP_SECRET",
                ]
                .contains(&name))
            {
                return Err("unsupported configuration name".into());
            }
            let role = args.get(2).map(String::as_str).unwrap_or("engine");
            if role == "engine"
                && (name.starts_with("VAULT_")
                    || name == "SOLI_OWNER_TOKEN"
                    || name == "SOLI_PRIVATE_KEY")
            {
                return Err("use the separate signer profile for signing credentials".into());
            }
            if !["engine", "signer"].contains(&role) {
                return Err("role must be engine or signer".into());
            }
            let mut settings =
                if home.join(".env").exists() || home.join(format!("{role}.json")).exists() {
                    profile(&home, role)?
                } else {
                    BTreeMap::new()
                };
            let secret = zeroize::Zeroizing::new(rpassword::prompt_password(format!(
                "{name} (local input, hidden): "
            ))?);
            if secret.is_empty() {
                settings.remove(name);
            } else {
                settings.insert(name.into(), secret.to_string());
            }
            save_profile(&home, role, &settings)?;
            println!("Saved privately; restart affected service to apply");
        }
        "notification-add" => {
            let name = arg(1)?;
            id(name)?;
            let kind = arg(2)?;
            if !["slack", "telegram", "discord", "webhook"].contains(&kind) {
                return Err("unsupported transport".into());
            }
            let destination = args.get(3).cloned().unwrap_or_default();
            let secret = zeroize::Zeroizing::new(rpassword::prompt_password(
                "Bot token, webhook URL or signing secret (hidden): ",
            )?);
            if secret.is_empty() || (kind == "webhook" && secret.len() < 32) {
                return Err("valid notification secret required".into());
            }
            let secret_env = format!(
                "SOLI_NOTIFY_{}",
                name.replace('-', "_").to_ascii_uppercase()
            );
            let mut settings = profile(&home, "engine")?;
            settings.insert(secret_env.clone(), secret.to_string());
            save_profile(&home, "engine", &settings)?;
            let file = home.join("notifications.json");
            let mut all = load(&file)?
                .as_array()
                .ok_or("invalid notifications")?
                .clone();
            all.retain(|r| r["id"] != name);
            all.push(
                json!({"id":name,"kind":kind,"destination":destination,"secret_env":secret_env}),
            );
            save(&file, &json!(all))?;
            println!("Notification configured; restart engine to load its secret");
        }
        "connect" => connect(&home, arg(1)?, Path::new(arg(2)?), arg(3)?)?,
        "start" => start(&home, args.get(1).map(String::as_str).unwrap_or("engine")).await?,
        "stop" => stop(&home, args.get(1).map(String::as_str).unwrap_or("engine"))?,
        "run" => {
            let role = arg(1)?;
            if !["engine", "signer"].contains(&role) {
                return Err("invalid service role".into());
            }
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(home.join(format!("{role}.lock")))?;
            lock.try_lock()
                .map_err(|_| "another service instance owns the lifecycle lock")?;
            let file = home.join(format!("{role}.pid.json"));
            if file.exists()
                && process(load(&file)?["wrapper"]["pid"].as_u64().unwrap_or(0) as u32).is_some()
            {
                return Err("duplicate service refused".into());
            }
            let logfile = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(home.join(format!("{role}.log")))?;
            let mut child = spawned(&home, role)?
                .stdout(logfile.try_clone()?)
                .stderr(logfile)
                .spawn()?;
            std::thread::sleep(Duration::from_millis(50));
            save(
                &file,
                &json!({"wrapper":stamp(std::process::id())?,"child":stamp(child.id())?}),
            )?;
            let status = child.wait()?;
            let _ = std::fs::remove_file(file);
            if !status.success() {
                return Err("service exited unsuccessfully; inspect private log".into());
            }
        }
        "doctor" => {
            let status = api(&home, "/v1/status", None).await?;
            println!("Engine mode: {}", status["mode"]);
            if args.iter().any(|a| a == "--providers") {
                let r = api(&home, "/v1/readiness", None).await?;
                save(&home.join("readiness.json"), &r)?;
                if r["ready"] != true {
                    return Err(
                        "provider acceptance incomplete; inspect private readiness.json".into(),
                    );
                }
            }
            println!("Local engine authentication passed");
            if let Some(index) = args.iter().position(|a| a == "--agent") {
                let name = args.get(index + 1).ok_or("agent ID required")?;
                id(name)?;
                crate::mcp::verify(&home.join(format!("mcp-{name}.json"))).await?;
            }
        }
        "service" => service(&home, args.iter().any(|a| a == "--remove"))?,
        "update" => {
            if std::env::current_exe()?.canonicalize()? != binary(&home).canonicalize()? {
                return Err("run update using the installed Soli binary".into());
            }
            let signer_record = home.join("signer.pid.json");
            if signer_record.exists()
                && process(
                    load(&signer_record)?["wrapper"]["pid"]
                        .as_u64()
                        .unwrap_or(0) as u32,
                )
                .is_some()
            {
                return Err("stop and reconcile the signer before a binary upgrade".into());
            }
            if profile(&home, "engine")?
                .get("SOLI_DRY_RUN")
                .is_some_and(|v| v == "false")
            {
                return Err(
                    "live upgrades require owner-reviewed shutdown and reconciliation".into(),
                );
            }
            let dirty = Command::new("git")
                .args(["status", "--porcelain"])
                .output()?;
            if !dirty.stdout.is_empty() {
                return Err("resolve working-tree changes before update".into());
            }
            run(Command::new("git").args(["pull", "--ff-only"]))?;
            run(Command::new("cargo").args(["build", "--locked", "--release"]))?;
            stop(&home, "engine")?;
            self_replace::self_replace(
                PathBuf::from("target/release").join(binary(&home).file_name().unwrap()),
            )?;
            start(&home, "engine").await?;
        }
        "uninstall" => {
            service(&home, true)?;
            stop(&home, "engine")?;
            stop(&home, "signer")?;
            disconnect_all(&home)?;
            println!(
                "Services removed; private profiles and journals preserved. Remove the Soli host entry through its MCP settings. Vault remains intact."
            );
        }
        _ => return Err("unknown operator command".into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_env_roundtrip_precedence_and_engine_secret_rejection() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        save(&home.join("engine.json"), &json!({"SOLI_DRY_RUN":"false"})).unwrap();
        write(
            &home.join(".env"),
            b"# explicit .env overrides legacy JSON\nSOLI_DRY_RUN=true\n",
        )
        .unwrap();
        let mut values = profile(home, "engine").unwrap();
        assert_eq!(values["SOLI_DRY_RUN"], "true");
        values.insert(
            "SOLI_OWNER_TOKEN".into(),
            "a$literal # quote\" and slash\\".into(),
        );
        save_profile(home, "signer", &values).unwrap();
        assert_eq!(profile(home, "signer").unwrap(), values);
        assert!(spawned(home, "engine").is_err());
        write(&home.join(".env"), b"BROKEN=\"never-print-this-secret\n").unwrap();
        let e = profile(home, "signer").unwrap_err().to_string();
        assert!(!e.contains("never-print"));
    }
    #[test]
    fn scoped_native_configuration_preserves_other_host_settings() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        save(&home.join("agents.json"), &json!([])).unwrap();
        add_agent(home, "test", "read", false).unwrap();
        let cred = load(&home.join("credentials/test.json")).unwrap();
        assert_ne!(
            load(&home.join("agents.json")).unwrap()[0]["sha256"],
            cred["token"]
        );
        let host = home.join("host.json");
        save(
            &host,
            &json!({"theme":"dark","mcpServers":{"other":{"command":"other"}}}),
        )
        .unwrap();
        connect(home, "test", &host, "json").unwrap();
        assert_eq!(load(&host).unwrap()["theme"], "dark");
        assert_eq!(
            load(&host).unwrap()["mcpServers"]["other"]["command"],
            "other"
        );
        let toml = home.join("host.toml");
        write(
            &toml,
            b"theme = 'dark'\n[mcp_servers.other]\ncommand='other'\n",
        )
        .unwrap();
        connect(home, "test", &toml, "toml").unwrap();
        let doc = std::fs::read_to_string(&toml)
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(doc["theme"].as_str(), Some("dark"));
        assert_eq!(
            doc["mcp_servers"]["other"]["command"].as_str(),
            Some("other")
        );
        disconnect_all(home).unwrap();
        assert!(load(&host).unwrap()["mcpServers"]["soli"].is_null());
        let doc = std::fs::read_to_string(&toml)
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(doc["theme"].as_str(), Some("dark"));
        assert!(doc["mcp_servers"].get("soli").is_none());
    }
}
