# Universal Soli commands

The command catalogue is in the Rust core (`soli-agent menu`), also exposed as
the read-only MCP tool `soli_menu`. Hermes is one optional adapter. Slack,
Discord, Telegram, WhatsApp and other agent hosts can present the same catalogue.

`/soli-menu` lists commands. `/soli wallet`, `positions`, `position MINT`, `pnl`,
`candidates`, `learning` and `report` read current information. CLI equivalents
are `soli-agent wallet`, `position MINT`, `learning`, `report` and `live-candidates`.
Missing receipt history shows unavailable PnL; paper money is never presented
as the real wallet balance. A wallet can hold tokens not managed by Soli.

`/soli-menu` opens grouped native choices: buttons on Slack, Telegram and Discord;
numbered choices on WhatsApp and other text surfaces. Rust `menu-json` and MCP
`soli_menu` structured content expose the same catalogue to other hosts. Headless
callers retain the plain command list. Mutations require the configured owner,
an individual DM and confirmation. Shared-channel menus show the native owner
command to send; another member tapping a card cannot borrow the initiating
owner's authority. These native controls need no LLM.

`/soli settings` reads public strategy settings. In paper the authenticated owner
may use `/soli settings set entry 1.25USDC`, `stop-loss 15`, `take-profit 30`, or
`max-hold-hours 6`. The native supervisor reloads validated settings automatically;
unsupervised `serve` saves them and requires manual restart. Live edits remain
maintenance operations so existing mandates cannot silently change. Actual paper
entry remains bounded by both the mode allowance and strategy ceiling, plus fixed
ledger risk limits. Status, wallet, settings and reports have concise native text.

Owner command `/soli settings set report-minutes 60` enables hourly lifecycle
reports to the operator-configured notification destinations. Use any whole
number from 5 to 1440 minutes, or `off` (also `0`) to stop periodic reports.
Native menu → Pengaturan includes 30, 60, 120 minutes and off. This setting
applies immediately in both paper and live, without restarting or changing
trading mandates; `/soli settings` shows the interval and next due timestamp.
The interval and next deadline survive restart. A frequency change sends one
current report; recovery after downtime sends one current snapshot, not backlog.
Important entry/exit and fault notifications remain enabled independently.
The durable outbox checks every 10 seconds, retries delivery failures and may
duplicate delivery if a platform acknowledgement is lost. Configure private
destinations and credentials through `SOLI_NOTIFICATIONS_FILE`; no destination
means no delivery. Reports use the Rust engine directly and consume no LLM tokens.
Chat notifications contain concise Indonesian summaries with readable USDC
amounts. Provider payloads, audit logs and transaction receipts stay in local
storage; HTTPS webhook receivers still receive the original structured event.

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
`discord`, `whatsapp`, `whatsapp_cloud`, and `agent` for another trusted agent-host adapter. WhatsApp Cloud has its own signing key and owner IDs; its authority is not merged with the regular WhatsApp connection. Omit
unused platforms; adding a label alone does not connect an external account.
Each adapter must authenticate its actual user before signing. Models receive
read/preview/paper MCP credentials, never owner credentials or connector secrets.

Slack requires `/soli` and `/soli-menu` registration in its app manifest and the
`commands` scope. On Hermes Slack, `!soli-menu` / `!soli ...` work through known
command rewriting, including threads. Shared-channel mention rules still apply:
with strict mention enabled, use `@Soli !soli-menu` or `@Soli !soli ...` there;
DMs need no mention. Other platforms route native slash or
message-prefix commands through their adapter. WhatsApp requires a connected
verified WhatsApp adapter; Soli does not provision a phone number or Meta app.
Telegram uses `/soli_menu` (underscore) for its native command-name rules;
`/soli menu` also opens the same catalogue on every adapter.
