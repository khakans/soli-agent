# Jupiter transaction interface

`jupiter-v6.json` contains pinned interface data for interoperability, extracted
from Jupiter's [aggregator ABI](https://github.com/jup-ag/jupiter-amm-implementation/blob/main/idls/jupiter_aggregator_v6.json)
on 2026-10-29. Original source SHA-256:
`100b41d7caad93818cf440e8ae97bde43e6dad1ca732de45762adf43013cf9d6`.

Only exact-in `route` and `shared_accounts_route` roots are enabled. Their route
plan enum includes the published PumpSwap/Meteora variants. Other root variants,
unknown enum variants, trailing bytes, nonzero platform fees, slippage above 100
bps, wrong input amounts or an insufficient encoded output floor are rejected.
The complete Borsh argument stream is consumed; a plausible trailing suffix is
not accepted as evidence of transaction bounds. Schema updates require code
review and parser/policy tests, not a runtime download.
