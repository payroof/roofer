# End-to-end tests

`roofer e2e` builds `payroof` in release mode, then runs `cargo test -p roofer-e2e`
(extra arguments go to cargo, e.g. `roofer e2e --test purchase -- --nocapture`). The
[`e2e`](../e2e) crate depends on Stor and the Wallet core by path, so the projects stay
independent; it is part of the repository but not of the published crate. It needs
`anvil` (Foundry) and `solc` on `PATH`. Every service binds a random loopback port: the
tests do not conflict with a running `roofer up`.

## `purchase`

`e2e/tests/purchase.rs` runs one purchase through the whole system on a local Anvil chain:

1. The Wallet core registers a project on the real `payroof` binary.
2. The Wallet creates the project's store on Stor (`stortop`, embedded with an in-memory
   sheet source, since Google Sheets is not reachable from a test).
3. Stor syncs the sheet, connects to Payroof and registers its webhook.
4. A customer checks out on the storefront host and pays the order address on Anvil.
5. Payroof confirms the transfer and signs a webhook; Stor delivers the order and
   appends it to History.
6. The Wallet finds the payment address in the project's paths and derives the same
   address from its own keys.

## `marketplace`

`e2e/tests/marketplace.rs` runs a busy store end to end, on two Anvil chains with test
tokens compiled from [`e2e/contracts/Tokens.sol`](../e2e/contracts/Tokens.sol) (USDC with
EIP-2612 and EIP-3009, BNB with EIP-2612, a plain POL, a Tether-like USDT without return
values):

1. A 20-product catalog with token, USD and mixed prices, resolved to exact options on
   every network.
2. Races: five customers check out the last unit at the same instant; five more race for
   three units. Exactly the stock is reserved; a loser's bag stays usable.
3. Ten customers pay exactly, in parts, too much, in the wrong token, on the wrong
   network, a bag split into two payments, a four-product order, three transfers in one
   block.
4. Deadlines: no payment, a late payment with stock left (delivered) and without
   (`late_payment`, never sold twice), a partial payment (`manual_review`), a payment
   still unconfirmed when the window ends.
5. Forged, stale, replayed and mismatched webhooks.
6. Stock counters, allocations, payments and History agree.
7. The Wallet finds and derives the payment addresses (never its own Receive
   addresses), reads the balances customers paid, sweeps USDT from addresses without
   gas, and sends 9 tokens from addresses holding 1.3, 3.5, 5.8, 2.1 and 6.4 (only the
   last with ETH) for each kind of token: a top-up, an EIP-3009 authorization or a
   permit pays the gas.

Correctness checks are collected and reported together, so one run lists every defect.
Payment windows are ended early by moving `expires_at` instead of waiting 30 minutes.

## Other tests

`e2e/tests/connect.rs` and `e2e/tests/send_max.rs` cover connecting the Wallet core to
Payroof and sending the maximum amount.

## Development checks

```sh
cargo test -p roofer                       # CLI unit tests (fast, no services)
cargo clippy -p roofer --all-targets -- -D warnings
```
