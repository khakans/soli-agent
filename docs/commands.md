# Universal Soli commands

The command catalogue is in the Rust core (`soli-agent menu`), also exposed as
the read-only MCP tool `soli_menu`. Hermes is one optional adapter. Slack,
Discord, Telegram, WhatsApp and other agent hosts can present the same catalogue.

`/soli-menu` lists commands. `/soli wallet`, `positions`, `position MINT`, `pnl`,
`candidates`, `learning` and `report` read current information. CLI equivalents
are `soli-agent wallet`, `position MINT`, `learning`, `report` and `live-candidates`.
Missing receipt history shows unavailable PnL; paper money is never presented
as the real wallet balance. A wallet can hold tokens not managed by Soli.

Authenticated owner commands in live mode:

```text
/soli mode semi 24 1       # 24h mandate, 1 USDC per entry; human approves entry
/soli mode full 24 1       # same bounded mandate, automatic entries and exits
/soli mode manual 24 1     # stop new automatic entries; existing exit mandates persist
/soli pause entries        # exits/reconciliation continue
/soli pause all            # all automatic entry/exit stops; reconciliation continues
/soli resume-cycle entries
/soli resume-cycle all
/soli force-close MINT CONFIRM
/soli close-all CONFIRM    # tracked positions only; fresh sequential full-position sells
```

The same mode, pause and close commands operate the virtual ledger in dry run.
Paper lifecycle controls persist across restart (`soli-agent lifecycle` or MCP
status view `lifecycle`). Semi mode permits strategy exits after human paper
entry approval; full permits entries and exits. Entry size is also capped by
the current strategy. `close-all` halts new entries while processing exits.

Force close overrides strategy triggers, including during automation pause; it
never bypasses transaction policy, expiry, liquidity/slippage or reconciliation.
It cancels unsubmitted previews, preserves submitted intents, and arms a one-hour
liquidation mandate. An unavailable market can prevent closure. Inspect status;
queued is not sold.

Host adapters derive platform/user/chat/thread from the verified transport, not
model arguments. The Rust connector HMAC-signs it and the service checks owner
allowlists, timestamps and replay IDs. Platform keys are `slack`, `telegram`,
`discord`, `whatsapp`, and `agent` for another trusted agent-host adapter. Omit
unused platforms; adding a label alone does not connect an external account.
Each adapter must authenticate its actual user before signing. Models receive
read/preview/paper MCP credentials, never owner credentials or connector secrets.

Slack requires `/soli` and `/soli-menu` registration in its app manifest and the
`commands` scope. On Hermes Slack, `!soli-menu` / `!soli ...` work through known
command rewriting, including threads. Other platforms route native slash or
message-prefix commands through their adapter. WhatsApp requires a connected
verified WhatsApp adapter; Soli does not provision a phone number or Meta app.
Telegram uses `/soli_menu` (underscore) for its native command-name rules;
`/soli menu` also opens the same catalogue on every adapter.
