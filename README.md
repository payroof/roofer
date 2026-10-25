# roofer

**One command to run the whole local Payroof stack, and to test it end to end.**

`roofer` starts an Anvil chain, Payroof, Stor and the isolated Payroof Wallet on your
machine, acts as the faucet, and runs the end-to-end tests of the three projects together.

- One-command local stack: `roofer up` builds and starts everything, `roofer down` stops it.
- Built-in faucet: fund addresses, send ETH or tokens, deploy new ERC-20s, mine blocks.
- Real-device ready: build and launch the Wallet on a paired iPhone.
- End-to-end tests and hardened container checks across Payroof, Stor and the Wallet.
- A guided walkthrough: `roofer guide`.

## Requirements

- Rust toolchain (see [`rust-toolchain.toml`](rust-toolchain.toml)) and `lsof`
- [Foundry](https://getfoundry.sh) (`anvil`, `cast`) and `solc`
- The Payroof workspace: sibling checkouts `payroof/`, `stor/` and `wallet/` next to `roofer/`
- macOS with Xcode and `xcodegen` for the Wallet apps (skip with `--no-wallet`)
- Docker, only for `roofer docker`

## Install

```sh
brew install payroof/tap/roofer
```

```sh
cargo install roofer
```

Or download a prebuilt binary from
[GitHub Releases](https://github.com/payroof/roofer/releases).

## Quick start

From anywhere inside your workspace (or with `ROOFER_ROOT=/path/to/workspace`):

```sh
roofer up        # build, start Anvil + Payroof + Stor, open the Wallet
roofer guide     # walkthrough: tokens, store, wallet, purchase, balances, transfer
roofer status    # what is running
roofer down      # stop everything (chain state is kept)
```

`roofer --help` and `roofer <command> --help` document every option.

## Documentation

- [Commands](docs/commands.md)
- [Environment variables](docs/environment.md)
- [iOS and Apple signing](docs/ios.md)
- [End-to-end tests](docs/e2e.md)
- [Workspace layout](docs/workspace.md)
- [Troubleshooting](docs/troubleshooting.md)

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT)
at your option.
