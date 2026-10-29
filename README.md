# Soli Agent

Standalone Rust service for **Solana mainnet paper trading**, separate from Hermes.
Version 0.1 is an executable foundation, not an autonomous or profitable trading strategy.
No signing, private-key import, transaction broadcast, or live-mode switch exists.

## Run locally

Requires stable Rust and the platform C/C++ build tools (SQLite is compiled with the app).

```powershell
$env:SOLI_API_TOKEN = [Convert]::ToHexString([Security.Cryptography.RandomNumberGenerator]::GetBytes(32))
# Optional until requesting a swap preview:
$env:JUPITER_API_KEY = '<your Jupiter API key>'
cargo run -- serve
```

Use the same API token in another terminal, without putting it in shell history or chat:

```powershell
cargo run -- status
cargo run -- market So11111111111111111111111111111111111111112
# Buy wrapped SOL with 10 virtual USDC (USDC has 6 decimals):
cargo run -- preview buy So11111111111111111111111111111111111111112 10000000
cargo run -- paper-fill '<preview.id>'
cargo run -- halt
cargo run -- resume
```

Sell uses the position's entire raw token amount from `status`. Amounts always use
token base units; never infer token decimals from a ticker. Preview expires in 20
seconds. Repeating `paper-fill` for a filled preview returns its original fill,
including after a restart. Unfilled previews are intentionally discarded on restart.

The service binds only `127.0.0.1:8787`. All `/v1` requests require
`Authorization: Bearer <SOLI_API_TOKEN>`. `/health` is public and contains no balances.
No `.env` file is loaded automatically. See `.env.example` for configuration.
Never put production tokens/private keys in this repository.

## What is implemented

- Jupiter Swap API V2 `/order` quotes; no taker/signing or `/execute` requests.
- DEX Screener pair data, tagged as external data rather than trusted instructions.
- Persistent SQLite paper cash, cost basis, positions, fills, halt state and realized P&L.
- Conservative fills at the quote's minimum output, plus configurable estimated fee.
- Initial virtual capital 1,000 USDC. Changing the environment does not reset an existing ledger.
- Maximum 2 positions, each costing at most 5% of initial virtual capital including
  estimated fees; total entry cost capped at 10%; no additional buys into an open position.
- Block new buys after 3% daily realized loss (UTC day). Exits remain allowed when halted.
- CLI bridge suitable for use by a Hermes terminal tool, with server-side validation.

The paper fee defaults to 0.10 virtual USDC per fill. It is an estimate, not fetched
network pricing. Paper execution cannot prove sellability, liquidity availability,
or mainnet fill quality. The realized-loss guard does **not** include unrealized losses.
Status reports cost basis, not portfolio market value. No automatic exits, token
safety checks, market discovery, streaming, or autonomous entries are enabled yet.

## API

| Endpoint | Request / behavior |
|---|---|
| `GET /health` | Liveness; dry-run mode |
| `GET /v1/status` | Ledger and capabilities |
| `GET /v1/market/{mint}` | Solana pair information |
| `POST /v1/paper/previews` | `{ "side":"buy", "mint":"...", "amount":10000000 }` |
| `POST /v1/paper/orders` | `{ "quote_id":"..." }`; virtual fill only |
| `POST /v1/halt` | `{ "halted":true }`; blocks entries, not exits |

## Hermes integration

Use the commands in [the integration guide](docs/hermes.md). This repo does not
change the VPS, create a Slack bot/profile, or configure existing Coki/Irul agents.
Slack messages and approvals must be routed by Hermes to this local service; do not
expose this HTTP port publicly. A local bearer token allows **paper** mutations,
so give read-only bots only a restricted wrapper when deploying.

## Validation

```text
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

Quote calls require a user-supplied Jupiter key. Offline tests do not pretend to
verify live provider availability. Pinned dependency versions are in Cargo.lock.

## Next milestones (not implemented)

1. Helius RPC/WebSocket and GoPlus integrations; test mint authority, freeze
   authority, Token-2022 extensions, holder concentration and route coverage.
2. Versioned strategy, historical/live paper sampling, fee calibration, mark-to-market
   drawdown, exit monitoring, stale-data protection and daily performance reports.
3. Actual Hermes tool adapter and authorized Slack identity/context routing.
4. Isolated signer and Vault Transit integration. Generate/import keys out of chat;
   verify Ed25519 compatibility, transaction decoding, allowlisted instructions,
   spend/slippage caps, recovery and unseal behavior. Encrypted storage alone does
   not enforce transaction policy. No approval to build this stage authorizes live trades.

Official references: [Jupiter](https://developers.jup.ag/docs/api-reference/swap/order),
[DEX Screener](https://docs.dexscreener.com/api/reference),
[Helius](https://www.helius.dev/docs/api-reference/endpoints),
[GoPlus](https://docs.gopluslabs.io/reference/solanatokensecurityusingget),
[Vault Transit](https://developer.hashicorp.com/vault/docs/secrets/transit).

