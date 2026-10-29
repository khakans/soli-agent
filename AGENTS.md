# Soli repository

If the user asks to install/connect this repository, read [INSTALL.md](INSTALL.md).
For operating Soli through an agent, read [SKILL.md](SKILL.md).

The Rust engine is independent of any agent host. Put host-specific glue under
`integrations/`; do not import an agent framework into trading or signer policy.
Use the official MCP SDK for protocol handling. Hermes is an optional connector.

Preserve default dry-run mode and exact-owner authorization for live orders.
Do not put credentials in source, chat, process arguments or test output.
Do not discard submission journals to unblock an uncertain transaction.

Validate changed Rust with `cargo test --all-targets` and
`cargo clippy --all-targets -- -D warnings`. Validate adapters with their tests.
Tests do not authorize deployment, funding or mainnet transactions.
