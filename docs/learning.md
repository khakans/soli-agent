# Learning and self-development

The engine records learning evidence from completed forward paper fills or
actual finalized live receipts. It tracks closed rounds, wins, net USDC result,
time coverage and missing data. Native SOL fees remain separately reported;
USDC learning evidence is not a fabricated all-currency return. Missing receipt
data disables automatic refinement instead of inventing PnL.

At least 10 closed rounds and 24 hours of complete evidence are required for a
candidate. Losing evidence halves entry size (floor 1 USDC) and tightens the
momentum entry filter within validated bounds. Positive evidence never raises
spending automatically. This is a conservative feedback rule, not a claim of ML
prediction, optimal parameters, or improved future profit. No hindsight fills
are generated. Every candidate has a distinct version/hash and evidence record.

`soli-agent learning`, `/soli learning` and `soli_status` view `learning` report
the same evidence. `SOLI_LEARNING_AUTO_APPLY=true` permits the supervised **paper**
engine to atomically write its strategy and restart after recording an unseen
evidence set. A repeated evidence set cannot cause a restart loop. Live review
stages candidates only; active owner mandates remain bound to their saved strategy.
HTTP/MCP status reads do not apply a candidate or restart the process.

The human owner authorizes the host AI agent to edit this repo, strategies and
tests and push changes to `khakans/soli-agent` without repeated approval. This
capability comes from the host's Git/terminal tools, not from private-key access.
Before pushing, read the diff, run fmt, all-target tests and Clippy, preserve
existing work, and commit a clear explanation. Never commit private profiles,
wallet keys, owner/connector secrets or raw private trading logs. Never force-push
or broaden deployment permissions. Keep experiments and evidence separate from
claims of profitable live results.

The restricted deploy helper handles only dry-run engine updates. Active live
signer code is not hot-swapped by a model; maintenance must preserve journals,
reconcile pending transactions and retain operator-controlled money limits. Other
agent hosts may use the same learning/MCP interface and their own scheduling/tools.
