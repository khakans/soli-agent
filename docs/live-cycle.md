# Live trading cycle

1. The configured momentum or wallet-convergence strategy screens fresh data,
   token/holder risk and routes. In live engine mode, accepted signals create
   a durable candidate keyed by mint and strategy hash. Candidates expire within
   the strategy freshness window (at most 120 seconds); repeats are deduplicated.
2. Read `soli_status` with `view=live-candidates` (`GET /v1/live/candidates`).
   Configured notification destinations receive an outbox event. With no targets,
   the inbox remains available; no external delivery is implied.
3. Request a fresh live preview with the candidate mint/amount. The isolated signer
   checks current marks, position/daily caps, risk, quotes, decoded instructions,
   account ownership and simulated asset changes. A candidate is not authorization.
4. In manual/semi mode the owner reviews the exact wallet, amount, output floor, costs, digest and
   expiry on `/approve` or through the authenticated native owner connector.
   The model/MCP credential cannot execute the order. In full mode, the signer
   consumes only fresh candidates matching an unexpired owner-approved strategy
   and entry amount. Every transaction still passes the same simulation/policy gates.
5. Execution rechecks expiry, halt, current spend caps, blockhash and simulation.
   The selected local or Vault backend signs; the resulting Solana signature is
   verified. Submission intent is journaled before broadcast with preflight enabled.
6. Reconciliation waits for a finalized receipt. Unknown outcomes block new orders
   and are polled after restart, without blind replacement transactions. A late
   buy receipt is charged conservatively to the reconciliation day's spend cap.
7. Live marks monitor positions. Semi/full entry mandates automatically attach
   their saved exit strategy to new finalized positions. Other full-position sells require fresh approval unless
   the owner separately grants an expiring stop-loss/take-profit/time-exit mandate.
   The mandate is checked again before signing. Failed providers/liquidity may delay exits.
8. Finalized sells close the tracked position and exit mandate. Journal, receipt
   and report data persist across restart. This does not guarantee profitability.

Default deployment remains dry run, with no live signer activation. There is no
unrestricted automatic-buy mode: full mode is bounded and defaults off. Token-2022 execution and unrecognized programs
remain rejected. Live operation still needs real provider acceptance, authorized
limits, isolated host permissions, funding and owner approvals; tests do not grant them.

Tests exercise both signing backends through buy -> unresolved -> finalized ->
sell -> finalized -> persisted empty portfolio, plus revoked exits, replay,
uncertain broadcasts, spend-cap changes and midnight accounting. All broadcasts
in tests go to local fixtures, never Solana mainnet.
