//! `roofer`: one command line for the Payroof workspace (payroof, stor, wallet, roofer).

mod apple;
mod chain;
mod checks;
mod ctx;
mod faucet;
mod http;
mod stack;
mod sys;
mod ui;
mod units;

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use ctx::{CHAIN_ID, Ctx, RPC};
use stack::{Service, UpOptions};

const ENV_HELP: &str = "\
Environment:
  PAYROOF_DEFAULT_TOKENS=\"TEST:6 USDC:6 DAI:18\"  tokens deployed on a fresh chain (\"\" = none)
  PAYROOF_BIND_HOST=127.0.0.1    Anvil, Payroof and Stor only on this Mac (default 0.0.0.0: LAN access)
  PAYROOF_LAN_IP=192.168.1.20    the Mac's LAN address (default: Wi-Fi/Ethernet IP)
  STOR_GOOGLE_CREDENTIALS=FILE   Google service-account key: stores are Google Sheets (default: a
                                 service_account *.json in .local/ or the workspace root; none = no new stores)
  PAYROOF_APPLE_TEAM, PAYROOF_IOS_DEVICE, PAYROOF_IOS_BUNDLE_ID   Wallet signing and iPhone selection
  ROOFER_ROOT=DIR                the workspace (default: found from the current directory)

Walkthrough (tokens → store → wallet → purchase → balances → transfer): roofer guide";

#[derive(Parser)]
#[command(
    name = "roofer",
    version,
    about = "Run the Payroof workspace: the local stack (Anvil + Payroof + Stor + Wallet), its faucet and the end-to-end tests",
    after_help = ENV_HELP
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Args, Default)]
struct UpArgs {
    /// Do not run cargo (use existing release binaries)
    #[arg(long)]
    no_build: bool,
    /// Force a rebuild of the local Wallet app (otherwise rebuilt only when sources changed).
    /// Signed with your Xcode team when one is signed in, so Touch ID unlock works; otherwise PIN only
    #[arg(long)]
    rebuild_wallet: bool,
    /// Do not build/open the Wallet app
    #[arg(long)]
    no_wallet: bool,
    /// Also open Stor's page in the browser (stores are created from the Wallet)
    #[arg(long)]
    web: bool,
    /// Then build, install and launch the local iPhone Wallet (= roofer ios --local)
    #[arg(long)]
    ios: bool,
}

impl From<&UpArgs> for UpOptions {
    fn from(a: &UpArgs) -> Self {
        UpOptions {
            no_build: a.no_build,
            rebuild_wallet: a.rebuild_wallet,
            no_wallet: a.no_wallet,
            web: a.web,
            ios: a.ios,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum LogTarget {
    All,
    Anvil,
    Payroof,
    Stor,
}

#[derive(Subcommand)]
enum TokenCmd {
    /// Deploy a new ERC-20 on Anvil and register it with Payroof (restarts it)
    Add {
        /// 1-11 letters/digits, starting with a letter
        symbol: String,
        #[arg(default_value = "18")]
        decimals: String,
        /// Default: "Local SYMBOL"
        name: Option<String>,
        /// Whole tokens minted to the faucet
        #[arg(default_value = "1000000000")]
        supply: String,
    },
    /// List Anvil tokens with the exact values for Wallet → Add token
    List,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build (offline), start Anvil + Payroof + Stor and open the Wallet
    #[command(visible_alias = "start", after_help = ENV_HELP)]
    Up(UpArgs),
    /// Stop Stor, Payroof, Anvil (chain state kept) and the local Wallet app
    #[command(visible_alias = "stop")]
    Down,
    /// down + up
    Restart(UpArgs),
    /// Processes, readiness, block height, stores, tokens
    #[command(visible_alias = "st")]
    Status,
    /// Follow logs (Ctrl-C to quit)
    Logs {
        #[arg(value_enum, default_value = "all")]
        service: LogTarget,
    },
    /// Anvil ERC-20 tokens: `token add SYMBOL [DECIMALS] [NAME] [SUPPLY]`; lists them by default
    #[command(visible_alias = "tokens")]
    Token {
        #[command(subcommand)]
        action: Option<TokenCmd>,
    },
    /// Faucet: ETH + the same amount of every Anvil token
    Fund {
        address: String,
        #[arg(default_value = "10")]
        eth: String,
        #[arg(default_value = "1000")]
        tokens: String,
    },
    /// Send one asset from the faucet (ETH or a token symbol), e.g. to pay a checkout address
    Send {
        asset: String,
        amount: String,
        address: String,
    },
    /// ETH and every Anvil token balance of an address
    #[command(visible_alias = "bal")]
    Balance { address: String },
    /// Mine blocks immediately
    Mine {
        #[arg(default_value_t = 3, value_parser = clap::value_parser!(u32).range(1..=9999))]
        blocks: u32,
    },
    /// Standard Anvil dev accounts (public test keys)
    Accounts,
    /// (Re)open the isolated local Wallet app
    Wallet,
    /// Build, install and launch the iOS Wallet on a paired iPhone (signed with your Xcode team)
    Ios {
        /// iPhone name (default: the only one paired with this Mac, or PAYROOF_IOS_DEVICE)
        #[arg(long)]
        device: Option<String>,
        /// Separate test app (bundle ID <id>.local) talking plain HTTP to this Mac's Wi-Fi IP;
        /// without it the app is HTTPS-only (real networks only)
        #[arg(long)]
        local: bool,
        /// Install the existing build
        #[arg(long)]
        no_build: bool,
    },
    /// Stop and DELETE the chain, databases, stores, orders and tokens
    Reset {
        /// Confirm the deletion
        #[arg(long)]
        yes: bool,
        /// Also delete the isolated Wallet data
        #[arg(long)]
        wallet: bool,
    },
    /// End-to-end tests of Payroof, Stor and Wallet together (builds Payroof, then cargo test)
    #[command(visible_alias = "test")]
    E2e {
        /// Passed to cargo test, e.g. `--test purchase -- --nocapture`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Build the Payroof and Stor images and check them running together, hardened
    Docker,
    /// Walkthrough: tokens → store → wallet → purchase → balances → transfer
    Guide,
}

fn main() {
    // Exit quietly when the reader goes away (`roofer accounts | head`), like other Unix tools.
    // SAFETY: restores the default disposition before any thread exists.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let cli = Cli::parse();
    if let Err(e) = run(cli.command) {
        if let Some(ui::Exit(code)) = e.downcast_ref::<ui::Exit>() {
            std::process::exit(*code);
        }
        ui::error(format!("{e:#}"));
        std::process::exit(1);
    }
}

fn run(command: Cmd) -> Result<()> {
    let root = ctx::find_root()?;
    match &command {
        Cmd::E2e { args } => return checks::e2e(&root, args),
        Cmd::Docker => return checks::docker_checks(&root),
        _ => {}
    }
    // Generated configs, secrets and logs are private to this user.
    // SAFETY: umask only changes this process's file creation mask.
    unsafe { libc::umask(0o077) };
    let ctx = Ctx::new(root)?;
    // Offline builds; never route local/LAN traffic through a proxy.
    // SAFETY: set before any thread is spawned; children inherit them.
    unsafe {
        std::env::set_var("CARGO_NET_OFFLINE", "true");
        if std::env::var_os("CARGO_BUILD_JOBS").is_none() {
            std::env::set_var("CARGO_BUILD_JOBS", "2");
        }
        let mut no_proxy = "127.0.0.1,localhost,::1".to_string();
        if let Some(ip) = &ctx.lan_ip {
            no_proxy.push_str(&format!(",{ip}"));
        }
        if let Ok(existing) = std::env::var("NO_PROXY").or_else(|_| std::env::var("no_proxy"))
            && !existing.is_empty()
        {
            no_proxy.push_str(&format!(",{existing}"));
        }
        std::env::set_var("NO_PROXY", &no_proxy);
        std::env::set_var("no_proxy", &no_proxy);
    }
    match command {
        Cmd::Up(a) => ctx.up(&(&a).into()),
        Cmd::Down => ctx.down(),
        Cmd::Restart(a) => {
            ctx.down()?;
            ctx.up(&(&a).into())
        }
        Cmd::Status => ctx.status(),
        Cmd::Logs { service } => ctx.logs(match service {
            LogTarget::All => None,
            LogTarget::Anvil => Some(Service::Anvil),
            LogTarget::Payroof => Some(Service::Payroof),
            LogTarget::Stor => Some(Service::Stor),
        }),
        Cmd::Token {
            action: None | Some(TokenCmd::List),
        } => {
            ctx.print_tokens();
            Ok(())
        }
        Cmd::Token {
            action:
                Some(TokenCmd::Add {
                    symbol,
                    decimals,
                    name,
                    supply,
                }),
        } => ctx.add_token(&symbol, &decimals, name.as_deref(), &supply),
        Cmd::Fund {
            address,
            eth,
            tokens,
        } => ctx.fund(&address, &eth, &tokens),
        Cmd::Send {
            asset,
            amount,
            address,
        } => ctx.send(&asset, &amount, &address),
        Cmd::Balance { address } => ctx.balance(&address),
        Cmd::Mine { blocks } => ctx.mine(blocks),
        Cmd::Accounts => ctx.accounts(),
        Cmd::Wallet => ctx.open_wallet(),
        Cmd::Ios {
            device,
            local,
            no_build,
        } => ctx.ios(device, local, !no_build),
        Cmd::Reset { yes, wallet } => {
            if !yes {
                bail!(
                    "reset deletes the chain, Payroof and Stor databases, stores, orders and tokens. Re-run: roofer reset --yes [--wallet]"
                );
            }
            ctx.reset(wallet)
        }
        Cmd::Guide => {
            guide(&ctx);
            Ok(())
        }
        Cmd::E2e { .. } | Cmd::Docker => unreachable!("handled above"),
    }
}

fn guide(c: &Ctx) {
    let b = ui::bold;
    let (rpc, payroof, stor, suffix) = (&c.rpc_public, &c.payroof_public, &c.stor, &c.store_suffix);
    println!(
        "{networks}
  Anvil (local)      {rpc}   chain id {CHAIN_ID}, 2 s blocks, state persisted; test funds
  Real networks      Ethereum, Base, Polygon, Arbitrum One, BNB Smart Chain with their real
                     tokens, from payroof/config/payroof.example.toml. Payroof watches them through
                     public RPCs (needs internet); store prices resolve on them and Anvil. Paying
                     on them moves real funds: test purchases on Anvil.

{services}
  Payroof            {payroof}
  Stor               {stor}
  Storefront         http://<slug>.{suffix}   e.g. http://shop.{suffix}
                     on this Mac only (resolved locally: no DNS, no VPN/proxy)
  State / logs       .local/manual/  (secrets, chain.json, sqlite DBs, tokens.tsv, logs/)

{walk}
 1. {s1}:  roofer up
    First run deploys ERC-20s TEST(6) USDC(6) DAI(18) on Anvil and opens the isolated Wallet
    (own data dir and Keychain item, never your real vault).
 2. {s2} (best done before step 6, see note):
      roofer token add EURC 6 \"Euro Coin\"
      roofer token add GOLD 18 \"Gold Token\" 5000000
    Each one is deployed on Anvil and added to Payroof's asset list (Payroof restarts automatically).
    roofer tokens prints contract + decimals.
 3. {s3} (merchant):
    - Create a new wallet: choose a 6-digit PIN, write down the 24 TEST words (never real ones).
    - Settings → Networks & tokens: \"Anvil\" (Local Anvil, {RPC}) is preconfigured next to the
      real networks and tokens.
    - Settings → Networks & tokens → Add token: network Anvil, contract, symbol and decimals exactly
      as shown by roofer tokens. Repeat for every Anvil token.
    - Settings → Project → Connect: account Main, server {payroof}, a fresh project
      name (e.g. shop) → Connect.
 4. {s4}: Wallet → New account (e.g. \"Buyer\"), open Receive, copy its address, then
      roofer fund 0xBUYER_ADDRESS            # 10 ETH + 1000 of every Anvil token
    (Optional: importing the public Anvil mnemonic gives accounts 1-3, which are always pre-funded.)
 5. {s5}: create an empty Google Sheet and share it as Editor with the account shown in
    Wallet → Settings → Project → Stor (the service account in STOR_GOOGLE_CREDENTIALS).
    Paste the sheet's link, keep or change the address (e.g. shop) → Create. Stor adds the
    tabs Catalog (samples), Branding, Payments (URL and KEY filled in by the Wallet), History
    and Stats.
 6. {s6}: Catalog rows are products (32 samples to start). Price is
    one per line: \"25 USD\" (converted with the Tokens tab's USD rates) or \"25 USDC\" (exactly
    that, on every network the Tokens tab lists USDC for, Anvil included). Tokens lists each
    accepted token by its exact asset, with a USD rate (CoinGecko formula, or typed in).
    Stor reads the sheet every 30 s here and writes the outcome to Payments → STATUS
    (\"Live · 32 products\", or what to fix).
 7. {s7}: open http://shop.{suffix} → a product (own page, e.g.
    http://shop.{suffix}/p/guide) → add to the bag → pay with a token and network; for a
    test purchase pick Anvil (e.g. USDC · Anvil) → checkout shows amount, network, address and a
    QR code. Wallet → Buyer account → Send → the same asset on Anvil, paste the checkout address
    and the exact amount → Review → Confirm & sign locally. With 2 s blocks and 2 confirmations
    the order turns Delivered within ~10 s (roofer mine 3 speeds it up).
    No wallet at hand? roofer send USDC 5 0xCHECKOUT_ADDRESS pays from the faucet.
    Paying after the payment window still delivers while the product is in stock.
 8. {s8}: Wallet → select Main → Refresh (nothing refreshes on its own; the
    line under the total shows each network and source as it is read): new payment addresses
    are added, ETH and every token added in step 3 show their balances, and Activity lists
    every incoming and outgoing transfer of those assets, read by the Wallet itself from the
    chain (no Payroof involved). Cross-check: roofer balance 0xADDRESS
 9. {s9}: Send → source = a payment address → asset → any recipient (e.g. the Buyer
    or roofer accounts) → Review → Confirm & sign locally → Activity.
    ETH: leave room for the shown maximum fee. ERC-20: the source address needs a little ETH
    for gas first:  roofer send ETH 0.01 0xPAYMENT_ADDRESS
10. {s10}: roofer down, later roofer up — chain, stores, orders and the Wallet resume.

{iphone}
  roofer up --ios    starts the stack on this Mac's Wi-Fi address and installs a test Wallet on
                     the iPhone that is preset to it (plain HTTP, private addresses only).
                     Once per iPhone: pair it with Xcode (cable, Trust), turn on Developer Mode, and
                     after the first install trust the developer (Settings → General → VPN & Device
                     Management). Allow \"Local Network\" when the app asks. Anvil starts off in
                     the Wallet: turn it on in Networks & tokens.
                     Keep the Mac's address fixed (a DHCP reservation on the router): it is built
                     into the iPhone app and kept in projects created from it.

{notes}
 - Stor reads Payroof's tokens on every sync, so tokens added with roofer token add can be
   used in the sheet's prices right away. A product resolves to at most 32 payment options.
 - Paid orders are appended to the sheet's History tab; its Stats tab charts them.
 - Phone: on the same Wi-Fi, the Wallet reaches Payroof and Stor ({stor}) by address.
   Storefronts are http://<slug>.{suffix}, which only this Mac resolves: open them here.
   macOS may ask to allow incoming connections for payroof and stortop. Anyone on the network can reach these test services (including the
   Anvil chain with its unlocked test accounts) while they run;
   PAYROOF_BIND_HOST=127.0.0.1 keeps them on this Mac.
 - The Wallet locks after 5 minutes without activity (or when the Mac sleeps); switching to
   the browser and back is fine.
 - Independence: Payroof watches only the assets in its config; the Wallet uses Payroof only for
   project addresses, pairing and access, and tracks balances and activity on its own (Anvil by
   scanning the local chain; real networks through indexers, RPCs and optional provider keys).
 - Builds are offline (cargo --offline; Xcode signing may contact Apple once for a profile).
   Requires anvil + cast (Foundry), solc, lsof; cargo for builds; Xcode + xcodegen for the
   Wallet app; Docker for roofer docker. Never send real funds to these test addresses or
   reuse these mnemonics.
 - Clean slate: roofer reset --yes  (keeps Wallet data; in the Wallet connect Payroof again
   with a new project name, since the old Payroof database is gone). Add --wallet to wipe it too.",
        networks = b("Networks"),
        services = b("Services"),
        walk = b("Walkthrough: tokens → store → wallet → purchase → balances → transfer"),
        s1 = b("Start"),
        s2 = b("More tokens"),
        s3 = b("Wallet setup"),
        s4 = b("Buyer account"),
        s5 = b("Store"),
        s6 = b("Edit the store in the sheet"),
        s7 = b("Purchase"),
        s8 = b("See merchant funds"),
        s9 = b("Transfer out"),
        s10 = b("Stop / resume"),
        iphone = b("iPhone and Mac on the same Wi-Fi"),
        notes = b("Notes"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_commands_and_aliases() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(std::iter::once("roofer").chain(args.iter().copied()))
        };
        assert!(matches!(
            parse(&["start", "--no-build"]).unwrap().command,
            Cmd::Up(UpArgs { no_build: true, .. })
        ));
        assert!(matches!(parse(&["stop"]).unwrap().command, Cmd::Down));
        assert!(matches!(
            parse(&["tokens"]).unwrap().command,
            Cmd::Token { action: None }
        ));
        assert!(matches!(
            parse(&["token", "add", "EURC", "6", "Euro Coin"])
                .unwrap()
                .command,
            Cmd::Token {
                action: Some(TokenCmd::Add { .. })
            }
        ));
        assert!(matches!(
            parse(&["mine"]).unwrap().command,
            Cmd::Mine { blocks: 3 }
        ));
        assert!(parse(&["mine", "0"]).is_err());
        assert!(parse(&["mine", "10000"]).is_err());
        match parse(&["e2e", "--test", "purchase", "--", "--nocapture"])
            .unwrap()
            .command
        {
            Cmd::E2e { args } => assert_eq!(args, ["--test", "purchase", "--", "--nocapture"]),
            _ => panic!("e2e"),
        }
        assert!(parse(&["logs", "nope"]).is_err());
    }
}
