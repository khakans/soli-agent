# Validation status — 2026-10-30

Latest local run: **43 Rust checks and 5 optional-adapter checks passed** across ledger, engine/adapter/policy,
deployment helper and native installation acceptance. Formatting and Clippy with
warnings denied passed. The earlier locked dependency audit reported no advisories;
that result is not a fresh audit of later dependency changes.
Native installation and MCP-to-engine status were exercised in the local checkout;
the temporary background engine was stopped after verification.
Acceptance also exercises idempotent application startup, settings authorization,
validated child reload without replacing the supervisor, persisted pause/halt, and
shutdown without an orphan engine. Adapter checks cover DM identity/confirmation,
shared-card refusal, owner rejection and headless fallback using stdlib unittest.

Engine, MCP, installer and acceptance checks are native Rust. Python implementations
were removed; only an optional Hermes registration shim uses Python for its host API.

```text
cargo fmt --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
cargo audit
```

Checks cover accounting, risk/freshness, legacy/v0 wire format, simulation policy,
Ed25519 signatures, owner/agent boundaries, exact-order context, uncertain broadcast
recovery, configured live exits, separately authorized automatic exits, outbox
persistence/signatures, native MCP discovery, host settings and native installation.

`scripts/validate-vault.ps1 -VaultExe PATH` uses real local Vault and native
`check-signer`: generate/import, signatures, actual `dry_run=false` startup, seal
rejection, unseal, journal restart and encrypted-storage restoration. Disposable
keys are unfunded; the fixture RPC cannot broadcast. Live transaction tests use
local RPC/Jupiter/Vault fixtures, not production trades.

Transaction policy consumes complete Borsh frames from the pinned Jupiter ABI,
including PumpSwap route variants. Tests reject appended fake bounds and excessive
slippage; route source/destination accounts must belong to the wallet. Unsupported
root variants and Token-2022 state are rejected before signing.

Windows/Linux CI is defined in `.github/workflows/check.yml`; a workflow file is
not evidence of an actual hosted run. Real Helius RPC/WebSocket, Jupiter/GoPlus
and native Hermes MCP acceptance have passed on the operator's VPS. Real live
signer execution and each channel's owner routing still need separate acceptance.
Forward paper observation requires
market data and elapsed time; no trading record is fabricated from historical signals.

Live startup rejects missing dependencies, same engine/signer account, mismatched
strategy, incompatible/sealed Vault and failed provider checks. Token-2022
execution is explicitly unsupported. Exit authorization does not guarantee liquidity
or execution during infrastructure failure. No funded wallet, mainnet broadcast or
production deployment was used during local validation.
