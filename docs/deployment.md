# Autonomous engine deployment on the managed VPS

`soli-deploy` is a fixed-path Linux operator helper. Install its release binary
as root at `/usr/local/sbin/soli-deploy` (root:root, 0755). Give the gateway user
sudo permission for exactly `soli-deploy status` and `soli-deploy deploy`.
No shell, wildcard arguments, arbitrary paths or signer restart permissions.

The helper reads the ubuntu-owned release artifact from
`/home/ubuntu/.agents/skills/soli-agent/target/release/soli-agent` and deploys
only `/var/lib/soli-engine/bin/soli-agent`. The engine systemd unit must execute
that private binary as `soli-engine`, with ProtectHome and NoNewPrivileges.
It never executes a candidate as root or changes `/opt/soli`, signer binaries,
wallet files or live settings. Explicit dry run and an x86_64 ELF are required.

```sh
git pull --ff-only origin master
CARGO_BUILD_JOBS=2 cargo build --locked --release --bin soli-agent
sudo /usr/local/sbin/soli-deploy deploy
sudo /usr/local/sbin/soli-deploy status
```

The engine restarts and must report healthy dry run. On failed restart/health,
the previous binary is restored and checked. A private receipt records hashes.
The helper itself and signer updates remain operator-controlled. Status verifies
the private engine hash without exposing its credentials.
