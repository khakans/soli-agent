# Local setup and live acceptance

## Service boundary

Run `soli-agent serve` as an engine user on 127.0.0.1:8787.
Run `soli-agent signer` as a DIFFERENT OS user on 127.0.0.1:8788, with a separate
private `SOLI_SIGNER_DB` and `config/signer.env.example` settings.
Only the signer receives `VAULT_TOKEN` and the independent `SOLI_OWNER_TOKEN`.
Do not put either in Hermes, the engine account, prompts, Git or command arguments.
Loopback bearer/HMAC does not protect against root or a compromised connector.
Run the Soli Hermes gateway as its own OS user, separate from Coki/Irul. Give
Soli's model only `soli_read`, with terminal, code execution and filesystem tools
disabled. Otherwise model-controlled host tools could read connector secrets and
forge transport context. The native `/soli` handler runs outside those model tools.
Do not deploy the connector into a shared unrestricted-shell agent profile.

`SOLI_DRY_RUN=false` is required explicitly for the signer and engine live proxy.
Engine live startup checks signer availability/authentication. Starting with
missing Vault/RPC/Jupiter settings, wrong network, exportable/derived key, or sealed
Vault fails closed. Do not fund a wallet merely because `/health` responds.

## Vault

Native installation is described in [INSTALL.md](../INSTALL.md). Engine live
startup additionally verifies that the signer account differs from the engine
account, both use the same strategy hash, and provider acceptance succeeds. The
signer verifies Vault signing with a random non-transaction challenge at startup.
Run `doctor --providers` in dry run to inspect missing provider requirements.

## Owner review and bounded automatic exits

Any agent may create a preview using its `preview`/`paper` scoped credential.
The response points to `http://127.0.0.1:8788/approve`. Open that trusted local page
(or an authenticated SSH tunnel), enter the owner credential locally and review
the exact wallet, mint, amount, output floor, costs, digest and expiry. The page
does not store credentials in browser storage or expose them to the agent.

Automatic exits are **off by default**. After an entry finalizes, the owner may
authorize them on that page for a tracked mint and a period of at most seven days.
The mandate saves the exact strategy snapshot/hash, survives restart and permits
only full-position sells on its stop-loss, take-profit or holding-time conditions.
It never authorizes automatic entries. Fresh quote conditions and authorization
are rechecked before signing; stale or recovered signals do not trigger a sale.

Equivalent owner CLI commands (epoch seconds, no secret in arguments):

```text
soli-agent live-exit-arm MINT EXPIRES_AT STRATEGY_HASH
soli-agent live-exit-disarm MINT
```

They require the independent owner credential in the operator environment.
`live-halt` stops buys; it does not revoke exit mandates. Revoke the mint's mandate
to stop its automatic exits. Expired mandates stop automatically; final sells
remove them. Provider/Vault/liquidity failure can delay exits, and unresolved
broadcasts block new transactions until reconciliation.

## Vault provisioning

Use a supported Vault deployment with durable encrypted storage, audit logging,
restricted TLS/listener and an operator-managed unseal/recovery procedure. Do not
use dev mode for funded wallets. Provision `transit` with an operator account.

Generate a fresh non-exportable wallet without exposing private material:

```text
soli-agent vault-key generate soli
soli-agent vault-key verify soli
```

For an existing isolated trading wallet, put its Solana JSON keypair or base58
seed/keypair in a private local file (0600 on Unix), set `SOLI_IMPORT_KEY_FILE` to
that path, and run `soli-agent vault-key import NEW_KEY_NAME`. The operator also
needs the official Vault CLI. The Rust helper validates the keypair, creates a
temporary private PKCS#8 payload, and invokes the CLI's wrapped BYOK import.
It never puts the private key in argv/chat/logs, and removes the temporary file.
Import depends on Vault edition/API BYOK support; unsupported servers reject it.
The original key file remains the operator's responsibility. Prefer generation.

Provisioning commands never overwrite an existing key. Their verification signs
a random compatibility challenge, not a Solana transaction, then verifies with
the Solana signature implementation. A public key printed is safe to record.

Runtime Vault policy (adapt key name):

```hcl
path "transit/keys/soli" { capabilities = ["read"] }
path "transit/sign/soli" { capabilities = ["update"] }
```

No export, delete, rotation, key creation/import or root permissions in runtime.
Pin the current key version; rotating an Ed25519 key changes the wallet address.
Never rotate a funded wallet automatically. Journal wallet binding rejects switching
wallets on an existing database.

## Live order flow

1. Obtain preview through `/soli preview ...` or `live-preview` CLI. All on-chain,
   GoPlus, route and simulation gates must pass. Full-position sells only.
2. Review wallet, mint, amount, minimum output, cost cap, digest and expiry.
3. Approve exact ID/digest in the same verified platform/chat/thread, or use
   owner-only CLI `live-execute ORDER_ID DIGEST CONFIRM`.
4. Re-simulate exact transaction, check blockhash, sign in Vault, verify signature.
5. Persist signature and submitted intent BEFORE broadcast. Preflight remains on.
6. Poll finalized receipt. Unknown outcomes block all new orders; do not erase
   journal data to force retry. `live-reconcile` checks the same signature.

Live preview expiry is 60 seconds and also subject to on-chain blockhash validity;
paper previews expire after 20 seconds. Expired previews must be recreated and approved again.
`live-cancel` clears only an unsubmitted preview. `live-halt` prevents new buys;
it does not liquidate positions. Live exits require fresh approval unless the owner granted an expiring exit mandate for that position and saved strategy.

## Seal, restart and recovery

- Vault sealed/unavailable: signing fails, no fallback local private key is used.
- Signer restart: pending signature survives and finalization polling resumes.
- Engine/Hermes restart: signing authority does not move into those processes.
- Missing finalized receipt: remain unresolved; investigate with another trusted
  RPC/explorer. No automatic assumption of failure or automatic replacement trade.
- Storage restore: restore the matching Vault storage/unseal material AND signer
  journal. Reconcile balances/history before accepting entries. Verify restored
  public address matches the intended wallet. Test recovery before funding.
- A stop trigger cannot sell while Vault/RPC/liquidity is unavailable. Maintain an
  operator recovery path; do not claim guaranteed loss caps.

## Acceptance before real funds

Run Rust tests, then use your real provider accounts to check quote schemas,
permissions, rate limits, mint/holder risk data and two-way routes. Validate Vault
generation/import, challenge signatures, seal rejection, unseal and storage restore.
Check authorized/unauthorized users on each selected Hermes transport, and verify
thread/context changes cannot approve another preview. Test dry-run restart,
exit monitoring and fee estimates. Review dependency advisories and deployed OS
permissions. Finally present the validation results to the owner; do not activate
or fund live trading based solely on code tests or a successful build.

References: [Vault Transit](https://developer.hashicorp.com/vault/api-docs/secret/transit),
[Vault BYOK CLI](https://developer.hashicorp.com/vault/docs/commands/transit/import),
[Jupiter order](https://developers.jup.ag/docs/api-reference/swap/order),
[Helius](https://www.helius.dev/docs/api-reference/endpoints).
