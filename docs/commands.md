# Commands

`roofer <command> --help` documents every option. State, secrets and logs live in
`.local/manual/` at the workspace root.

| Command | Description |
| --- | --- |
| `roofer up [--no-build] [--rebuild-wallet] [--no-wallet] [--web] [--ios]` | Build (offline), start Anvil + Payroof + Stor, open the Wallet (alias `start`) |
| `roofer down` | Stop everything; chain state kept (alias `stop`) |
| `roofer restart [up options]` | `down` + `up` |
| `roofer status` | Processes, readiness, block height, stores, tokens (alias `st`) |
| `roofer logs [all\|anvil\|payroof\|stor]` | Follow logs |
| `roofer tokens` | Anvil tokens with the exact values for Wallet → Add token |
| `roofer token add SYMBOL [DECIMALS] [NAME] [SUPPLY]` | Deploy an ERC-20 and register it with Payroof |
| `roofer fund ADDRESS [ETH] [TOKENS]` | Faucet: ETH (10) + every token (1000) |
| `roofer send ASSET AMOUNT ADDRESS` | Send ETH or a token from the faucet (e.g. pay a checkout) |
| `roofer balance ADDRESS` | ETH + token balances (alias `bal`) |
| `roofer mine [N]` | Mine N blocks now (3) |
| `roofer accounts` | Standard Anvil dev accounts |
| `roofer wallet` | (Re)open the local Wallet app |
| `roofer ios [--device NAME] [--local] [--no-build]` | Build, install and launch the Wallet on a paired iPhone (see [iOS](ios.md)) |
| `roofer reset --yes [--wallet]` | Stop and delete chain, databases and tokens |
| `roofer e2e [CARGO TEST ARGS]` | End-to-end tests (alias `test`; see [e2e](e2e.md)) |
| `roofer docker` | Hardened container checks of Payroof and Stor |
| `roofer guide` | Walkthrough: tokens → store → wallet → purchase → balances → transfer |

## Notes

- `up` builds offline (`cargo --offline`); the Wallet app is rebuilt only when its sources
  changed, or with `--rebuild-wallet`.
- `reset` keeps Wallet data unless `--wallet` is given. After a reset, connect the Wallet to
  Payroof again with a new project name, since the old Payroof database is gone.
- `roofer docker` builds the Payroof and Stor images and runs them together, hardened as in
  their `compose.yaml` files (read-only, no capabilities, no privilege escalation, UID
  10001), against an Anvil chain on this host: image size and user, compose validity, the
  built-in healthcheck, readiness, public routes, chain scanning from the container, the
  network alias, no shell, and a clean stop on SIGTERM.
- Without installing, run from a checkout with
  `cargo run --manifest-path roofer/Cargo.toml -- <command>`.
