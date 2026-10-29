# Install Soli into any compatible agent

Soli is one Rust binary: engine, native stdio MCP, installer, credential management
and service tools. Python, Node.js and Hermes are not runtime dependencies.

An agent with repository/terminal access can follow this file when asked to install
the repo. A chat-only bot first needs an MCP/HTTP/terminal connector in its host.

## Build and install

Use a dedicated checkout, stable Rust (1.89+) and native C/C++ build tools for SQLite.

```text
cargo build --locked --release
target/release/soli-agent install my-agent
```

On Windows use `target\release\soli-agent.exe`. Later commands use the installed
`.soli/bin/soli-agent` binary (`.exe` on Windows), abbreviated below as `soli-agent`.
`SOLI_HOME` may select a dedicated private state directory. Installation preserves
existing profiles, generates credentials, defaults to dry run and starts the engine.
Fresh installations keep an independent strategy copy in `.soli/strategy.json`;
existing profiles keep their explicitly configured strategy path.

```text
soli-agent doctor
soli-agent agent-add researcher read
soli-agent agent-add trader paper
soli-agent agent-revoke researcher
```

Roles: `read`, `preview`, `paper`. New installs use `preview`; only `paper` can fill
virtual orders. No agent credential authorizes real trades. Revocation applies
without restart. Add `--rotate` to `agent-add ID ROLE` to replace its credential.

## Connect the host

The installer writes `.soli/mcp-my-agent.json`. It launches the native binary with
`args: ["mcp"]` and supplies `SOLI_CREDENTIAL_FILE`, pointing to that agent's private
scoped credential file. Host config contains no token value.

```text
soli-agent connect my-agent /absolute/path/host.json json
soli-agent connect my-agent /absolute/path/config.toml toml
```

Other host settings are preserved; conflicts are rejected for review. The TOML
format uses `[mcp_servers.soli]`, following the
[official Codex MCP documentation](https://learn.chatgpt.com/docs/extend/mcp).
Use the actual host's supported format and reload its MCP connection afterward.
Tools: `soli_status`, `soli_token`, `soli_preview`, `soli_paper_fill`.
Verify discovery and status before claiming that a particular host works.

## Configure locally

Each command prompts for hidden input; no credentials belong in chat or arguments.

```text
soli-agent configure SOLI_RPC_URL
soli-agent configure SOLI_WS_URL
soli-agent configure JUPITER_API_KEY
soli-agent configure GOPLUS_TOKEN
soli-agent configure GOPLUS_APP_KEY
soli-agent configure GOPLUS_APP_SECRET
soli-agent configure SOLI_SMART_WALLETS
soli-agent stop engine
soli-agent start engine
soli-agent doctor --providers
```

Blank input removes a setting. GoPlus token is optional when anonymous access is
permitted. Provider acceptance checks fresh mainnet RPC, WS subscription, Jupiter
routes and GoPlus/on-chain response coverage, saving `.soli/readiness.json`.
Missing credentials produce failed acceptance. Paper evidence is `/v1/performance`.
For GoPlus App Key + App Secret, configure both `GOPLUS_APP_KEY` and
`GOPLUS_APP_SECRET` instead of a manual token. Soli obtains and caches access
tokens in memory, refreshes before expiry and retries authentication once on
HTTP 401/403. Partial app credentials fail closed. No token is logged or saved
to disk. Restart the engine after configuration. The signer must receive its
own provider configuration under its private service account.

For `.env` configuration, copy `.env.example` into the private `SOLI_HOME/.env`,
set mode 0600 on Unix, and fill it locally. Native lifecycle commands prefer
that file over legacy JSON profiles; `configure` updates whichever format is
active. Keep the signer in a separate SOLI_HOME and account. Local wallet mode
uses `SOLI_SIGNING_BACKEND=local`, `SOLI_PRIVATE_KEY` and
`SOLI_EXPECTED_WALLET`; run `soli-agent wallet-check` under the signer account
to verify without starting live trading. See [operations](docs/operations.md).

## Services and delivery

`service` installs a Windows login task or Linux systemd user service;
`service --remove` removes it. Linux boot without login requires operator-enabled
lingering. Unattended Windows boot needs a separately provisioned service manager.
Use `start engine` / `stop engine` for a local background instance.

```text
soli-agent notification-add reports slack CHANNEL_ID
soli-agent notification-add reports telegram CHAT_ID
soli-agent notification-add reports discord
soli-agent notification-add reports webhook https://receiver.example/events
```

The hidden prompt takes a bot token, Discord webhook URL or generic webhook signing
secret (32+ characters). Restart the engine to load notification secrets.
Persistent outbox status is `/v1/notifications`; failed deliveries retry with
backoff, then require operator requeue through `POST /v1/notifications/retry`.
Delivery is at-least-once: uncertain acknowledgements may duplicate. Generic
receivers must deduplicate `x-soli-event-id` and verify HMAC/time headers.

`update` fast-forwards a clean checkout, builds Rust and restarts dry-run service.
Live upgrades require reviewed shutdown/reconciliation. `uninstall` removes
services and registered Soli host entries, preserving journals, credentials and Vault.

## Live and optional adapters

Read [operations](docs/operations.md). The signer needs a different OS account
and private state. Choose local key custody in its private `.env`, or Vault
Transit. Keep model hosts away from both service profiles.
Live startup checks account separation, matching strategy, signing compatibility and
provider acceptance. The owner reviews orders at the signer's loopback `/approve`
page. Optional automatic exits require a separate expiring authorization for an
existing position and saved strategy. They are off by default.

For remote use, configure authenticated SSH tunnels, including port 8788 for owner
review. Do not expose the raw services publicly. Host-specific hooks belong under
`integrations/adapters/`. The optional [Hermes shim](docs/hermes.md) only registers
callbacks; Rust's general `connector` command signs/verifies context. Other hosts
use MCP/HTTP/CLI without loading the shim.

Run `cargo test --all-targets --locked` with ports 8787/8788 free. Tests include a
native install, HTTP scopes, revocation, restart and MCP protocol checks.
Local changes need publishing before a repository link distributes them.
