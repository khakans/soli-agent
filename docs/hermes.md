# Optional Hermes adapter

Install `integrations/adapters/hermes/` through Hermes's directory-plugin install/enable
workflow in a separate **Soli profile**. Run `hermes plugins doctor` against it.
This is native `plugin.yaml` + `register(ctx)` glue; core trading is Rust.

The same Rust catalogue and connector support Slack, Telegram, Discord,
WhatsApp and other trusted agent hosts. See [universal commands](commands.md).
`soli_strategy_update` is a paper-scoped MCP tool for atomic, evidence-backed
entry-filter changes; it cannot edit live strategy, size, freshness limits, exits,
or risk caps and is rejected when the engine is live.
Set `SOLI_BINARY`, the agent scoped `SOLI_CREDENTIAL_FILE`, and matching connector secrets in the host and engine. Configure the isolated signer only when the host's permissions are suitable for live owner commands:

```text
SOLI_BRIDGE_SECRETS_JSON={"slack":"<random32+>","telegram":"<different32+>","discord":"<different32+>","whatsapp":"<different32+>","whatsapp_cloud":"<different32+>","agent":"<different32+>"}
SOLI_OWNERS_JSON={"slack":["U..."],"telegram":["123456789"],"discord":["123456789012345678"],"whatsapp":["verified-user-id"],"whatsapp_cloud":["verified-cloud-user-id"],"agent":["verified-owner-id"]}
```

Omit unused platforms. Missing owners are denied. Do not grant owner privileges
to bot IDs. Hermes must not have `VAULT_TOKEN` or `SOLI_OWNER_TOKEN`.

Commands:

```text
/soli-menu
/soli status
/soli report
/soli market MINT
/soli risk MINT
/soli preview buy MINT USDC_ATOMIC_AMOUNT
/soli preview sell MINT FULL_TOKEN_ATOMIC_AMOUNT
/soli fill QUOTE_ID                       # paper only
/soli approve ORDER_ID TRANSACTION_DIGEST # live only
/soli halt
/soli resume                             # live owner control
/soli cancel                             # unsubmitted live preview only
/soli reconcile
```

The native command handler receives adapter-derived `source`, including stable
user/chat/thread IDs. Identity is never taken from model arguments. The general Rust connector HMAC-signs
the context; Rust verifies timestamp, signature, replay ID and owner allowlist.
Live approval must match the preview's original user/platform/chat/thread and
transaction digest. The model tool `soli_read` only reads status/report.

Returning the command result lets Hermes deliver to the **original** DM/channel/
thread. No code chooses a new destination. Telegram lists registered commands;
Slack/Discord may require their command manifest to be refreshed through Hermes.
Test the actual installed version/platform; mock source tests are not that test.
Hermes Slack also rewrites `!soli-menu` and `!soli ...` through its known-command
handler, without needing a Slack slash registration. Other transports still need
their real account/adapter configured; protocol support does not connect accounts.
Strict mention policy still applies to message-prefix commands in shared channels:
use `@Soli !soli-menu` there, or `!soli-menu` directly in a DM.

The engine persists daily summaries. To deliver them daily, use Hermes cron with
the `soli_read` report tool and your chosen platform/chat target. Transport tokens
remain in Hermes; this repo does not configure or deploy those external bots.

Reference: https://hermes-agent.nousresearch.com/docs/developer-guide/plugins
