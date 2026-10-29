# Meridian reference and Soli design

Reference reviewed: https://github.com/yunus-0x/meridian at
`cb3a2f0d99c4938c8f9b2e2ca164bf98495ad2ef` on 2026-10-29.
No Meridian source files are vendored or copied. The implementation is independent;
the separate Jupiter ABI data is attributed in `config/abi/README.md`.

Meridian main is a deterministic wallet-triggered Meteora DLMM LP bot. Its README
describes WebSocket monitoring, signature backfill, distinct-wallet convergence,
bounded screening, independent management and durable submission reconciliation.
It does not currently require an LLM; its Telegram integration is notification-only.

Soli adopts that lifecycle for **spot memecoin swaps**, not liquidity provision:

| Pattern | Soli implementation |
|---|---|
| Wallet stream + catch-up | Helius/Solana logs subscriptions + bounded RPC signature backfill |
| Baseline existing positions | First wallet observation checkpoints newest signature; no historical entry signal |
| Distinct-wallet confirmation | Two tracked wallets buying same mint within 120 seconds |
| Verify economic event | Signed wallet, direct Jupiter invocation, net target-token acquisition and meaningful USDC/SOL spend |
| Screening deadline | Retry failed screening for at most 120 seconds, then discard |
| Risk / route checks | GoPlus + on-chain authorities + holder concentration + two-way Jupiter coverage |
| Memory / cooldown | SQLite cursors, durable queue, decision events, 30-minute per-mint cooldown |
| Shadow | Default dry run; automatic paper execution additionally requires SOLI_AUTO_PAPER=true |
| Position management | Independent paper exit/mark worker; live exit recommendations require owner approval |
| Restart safety | Persisted submission signature blocks new orders until finalization/reconciliation |
| Control surfaces | Hermes plugin for Slack, Telegram and Discord, signed stable-ID context |

LP bin ranges, fee harvesting, LP TVL yield metrics and DLMM transaction encoders
are deliberately not transplanted into a spot strategy.

Limits: discovery currently follows **operator-supplied smart-wallet addresses**;
Soli does not claim these addresses are smart or profitable. Only direct Jupiter
swaps are recognized; transfers, LP events and unrecognized routes do not count.
SOL spend is a conservative net-balance heuristic after fee/new-account rent
allowance, not an attribution proof for arbitrary multi-action transactions.
Multiple wallets may share a beneficial owner. Holder concentration aggregates
observed largest token accounts by owner; it cannot prove absence of Sybil control.

The optional `momentum` trigger uses persisted five-minute market samples and the
versioned thresholds. The default `wallet_convergence` trigger does not silently
fall back to momentum if wallet monitoring fails.
