# Soli Agent

Agent-independent Rust engine for **Solana spot memecoins**. Default: dry run.
Connect an AI agent through native Rust MCP, HTTP tools or CLI; Hermes is an optional native
adapter for **Slack, Telegram and Discord**. Vault-backed signing runs as a
separate process. No wallets or credentials are included.

**Installing from an agent? Read [INSTALL.md](INSTALL.md), then [SKILL.md](SKILL.md).**
The repository contains installation instructions for terminal-capable agents and
a stdio MCP adapter. Host-specific settings still follow the host's own format.

Architecture inspired by [Meridian](https://github.com/yunus-0x/meridian), adapted
from LP management to spot swaps. See [the comparison](docs/meridian-comparison.md).
See [validation results and remaining acceptance checks](docs/validation.md)
before considering real funds.

## Implemented

- Helius-compatible RPC/WebSocket, logs/account subscriptions, reconnect, durable
  signature backfill and first-observation wallet baseline.
- Default entry trigger: two distinct tracked wallets buying one mint in 120
  seconds; bounded screening retries and persistent 30-minute mint cooldown.
- GoPlus, on-chain authorities, Token-2022 extension inventory, observed-holder
  concentration, mainnet freshness and two-way Jupiter route checks.
- Versioned/hashed strategy, persistent market samples, historical import and
  no-look-ahead signal replay. Replay does not invent executable historical P&L.
- SQLite paper ledger, idempotent quote-bound fills, independent exit/mark worker,
  liquidation-quote equity, drawdown halt, fee calibration and daily reports.
- Live transaction decoding, program allowlist, wallet balance/authority simulation,
  exact-order approval, Vault Ed25519 signature verification, preflight submission,
  durable submission journal, finalization polling and restart reconciliation.
- Live liquidation marks and exit recommendations. Live exits, like entries,
  require owner approval. Optional automatic exits use a separate expiring owner mandate.
- Native Hermes plugin, read-only model tool, signed transport context, stable-ID
  owner authorization and replay protection across Slack, Telegram and Discord.
- Agent-neutral MCP tools for status/reports, token research, paper/live previews
  and virtual paper fills. MCP does not expose real-order execution or key access.
- Operator-only key generation/import and Vault Ed25519 compatibility probe.

## Run locally

Requires stable Rust plus C/C++ tools for bundled SQLite.

```powershell
$env:SOLI_API_TOKEN = [Convert]::ToHexString([Security.Cryptography.RandomNumberGenerator]::GetBytes(32))
$env:SOLI_DRY_RUN = 'true'
$env:JUPITER_API_KEY = '<key>'
$env:SOLI_RPC_URL = '<Helius HTTPS URL>'
$env:SOLI_WS_URL = '<Helius WebSocket URL>'
$env:SOLI_STRATEGY_FILE = 'config/strategy.json'
$env:SOLI_SMART_WALLETS = '<wallet-one>,<wallet-two>'
cargo run -- serve
```

No `.env` is loaded automatically. See `.env.example`. Without provider keys,
local ledger/status work but screening/quotes report unavailable. Automatic paper
entries/exits require explicit `SOLI_AUTO_PAPER=true`. No private key is required.

CLI, using the same token in another terminal:

```text
cargo run -- status
cargo run -- market MINT
cargo run -- preview buy MINT USDC_ATOMIC_AMOUNT
cargo run -- paper-fill QUOTE_ID
cargo run -- preview sell MINT FULL_TOKEN_ATOMIC_AMOUNT
cargo run -- halt
cargo run -- resume
```

1 USDC = 1,000,000 atomic units. Sell uses the token's raw units. Paper quotes
expire after 20 seconds. Filled IDs replay safely across restart; unfilled paper
previews are discarded on restart. Default virtual capital: 1,000 USDC, applied
only when initializing a new database.

## Risk limits and scope

Paper: two positions, 5% initial capital per position including estimated fees,
10% entry-cost exposure, no averaging, 3% daily realized-loss entry halt and 3%
complete marked-equity daily drawdown entry halt. Exits remain allowed. UTC day.

Live: exact owner approval each order, configurable gross daily spend/per-position
caps plus 5%/10% starting marked-equity limits, 3% daily marked-equity entry halt,
maximum simulated SOL cost 0.005 SOL and remaining reserve 0.01 SOL. Live equity
covers USDC and tracked positions, excluding SOL reserve and unrelated holdings.
Paper fees are calibrated estimates; live receipts retain actual native fees.

Token-2022 extensions are inventoried but **execution is rejected** until supported.
Unknown security fields, unsupported programs/routes, stale RPC, changed token
authority or unexpected asset movement fail closed. Observed holder concentration
cannot identify every related wallet; pooling/custody addresses are conservatively
counted rather than guessed away. No profitability or guaranteed exit claim.

## API (127.0.0.1:8787)

`/v1` engine routes require bearer `SOLI_API_TOKEN`, except HMAC-signed bridge events.

| Endpoint | Function |
|---|---|
| `GET /health` | Liveness/mode |
| `GET /v1/status`, `/v1/report`, `/v1/reports/daily` | State/current/daily summaries |
| `GET /v1/market/{mint}`, `/v1/risk/{mint}` | Market and screening |
| `POST /v1/paper/previews` | `{ "side":"buy", "mint":"...", "amount":10000000 }` |
| `POST /v1/paper/orders` | `{ "quote_id":"..." }` |
| `POST /v1/halt` | `{ "halted":true }` (paper only) |
| `GET /v1/samples/{mint}`, `/v1/replay/{mint}` | Persisted samples/signal replay |
| `POST /v1/samples/import` | Up to 100 historical samples |
| `POST /v1/fees/calibrate` | RPC fee/rent estimate in USDC |
| `GET /v1/live/status`, `POST /v1/live/previews` | Live status/preview |
| `POST /v1/live/execute` | Owner-only ID/digest/confirmation |
| `POST /v1/live/reconcile`, `/cancel`, `/halt` | Owner-only recovery/control |
| `POST /v1/bridge/events` | Signed native transport command source |

Paper mutation endpoints are disabled in live mode. Read [operations](docs/operations.md)
before live setup, and [Hermes integration](docs/hermes.md) before channel setup.

## Validate

```text
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

Tests use mocked RPC/Jupiter/Vault HTTP with real Ed25519 signatures; no mainnet
transactions are submitted. External provider credentials, the actual Vault
deployment and installed Hermes transports require separate operator acceptance
tests. Building live support does not authorize deployment, funding or live trades.
