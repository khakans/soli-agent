# Hermes integration: Slack, Telegram, Discord

Install `integrations/adapters/hermes/` through Hermes's directory-plugin install/enable
workflow in a separate **Soli profile**. Run `hermes plugins doctor` against it.
This is native `plugin.yaml` + `register(ctx)` glue; core trading is Rust.

Set `SOLI_BINARY`, the agent scoped `SOLI_CREDENTIAL_FILE`, and matching connector secrets in Hermes/engine/signer:

```text
SOLI_BRIDGE_SECRETS_JSON={"slack":"<random32+>","telegram":"<different32+>","discord":"<different32+>"}
SOLI_OWNERS_JSON={"slack":["U..."],"telegram":["123456789"],"discord":["123456789012345678"]}
```

Omit unused platforms. Missing owners are denied. Do not grant owner privileges
to bot IDs. Hermes must not have `VAULT_TOKEN` or `SOLI_OWNER_TOKEN`.

Commands:

```text
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

The engine persists daily summaries. To deliver them daily, use Hermes cron with
the `soli_read` report tool and your chosen platform/chat target. Transport tokens
remain in Hermes; this repo does not configure or deploy those external bots.

Reference: https://hermes-agent.nousresearch.com/docs/developer-guide/plugins
