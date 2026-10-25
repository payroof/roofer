//! The local stack: Anvil + Payroof + Stor, their generated configs, secrets and processes.

use crate::chain::{self, asset_id};
use crate::ctx::{CHAIN, CHAIN_ID, Ctx, NATIVE_ASSET, PAYROOF, RPC, STOR_LOCAL};
use crate::{http, sys, ui};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Service {
    Anvil,
    Payroof,
    Stor,
}

impl Service {
    pub const ALL: [Service; 3] = [Service::Anvil, Service::Payroof, Service::Stor];

    pub fn name(self) -> &'static str {
        match self {
            Service::Anvil => "anvil",
            Service::Payroof => "payroof",
            Service::Stor => "stor",
        }
    }

    fn port(self) -> u16 {
        match self {
            Service::Anvil => 8545,
            Service::Payroof => 8080,
            Service::Stor => 8081,
        }
    }
}

#[derive(Default)]
pub struct UpOptions {
    pub no_build: bool,
    pub rebuild_wallet: bool,
    pub no_wallet: bool,
    pub web: bool,
    pub ios: bool,
}

impl Ctx {
    fn pid_file(&self, s: Service) -> PathBuf {
        self.state.join(format!("{}.pid", s.name()))
    }

    pub fn log(&self, name: &str) -> PathBuf {
        self.logs.join(format!("{name}.log"))
    }

    /// A substring of the service's command line that identifies this workspace's process.
    fn needle(&self, s: Service) -> String {
        match s {
            Service::Anvil => "anvil".into(),
            Service::Payroof => self.payroof_bin.display().to_string(),
            Service::Stor => self.stor_bin.display().to_string(),
        }
    }

    /// The live, tracked pid of a service; a stale pid file is removed.
    pub fn pid(&self, s: Service) -> Option<i32> {
        let file = self.pid_file(s);
        let text = std::fs::read_to_string(&file).ok()?;
        let pid = text.trim().parse::<i32>().ok().filter(|p| *p > 0);
        let live = pid.filter(|p| sys::command_of(*p).is_some_and(|c| c.contains(&self.needle(s))));
        if live.is_none() {
            let _ = std::fs::remove_file(&file);
        }
        live
    }

    pub fn wallet_pids(&self) -> Vec<i32> {
        sys::try_capture(Command::new("pgrep").arg("-f").arg(&self.wallet_bin))
            .map(|out| out.lines().filter_map(|l| l.trim().parse().ok()).collect())
            .unwrap_or_default()
    }

    fn require_port(&self, port: u16) -> Result<()> {
        if sys::listeners(port).is_empty() {
            return Ok(());
        }
        bail!(
            "port {port} is already in use by another process:\n{}\nStop it (or roofer down) and retry.",
            sys::listener_table(port)
        )
    }

    // ------------------------------------------------------------ secrets and configs

    fn write_secrets(&self) -> Result<()> {
        let secrets = self.state.join("secrets.env");
        if !secrets.is_file() {
            let mut text = String::new();
            for k in [
                "PAYROOF_ALTCHA",
                "PAYROOF_CURSOR",
                "PAYROOF_MASTER",
                "STORTOP_MASTER",
                "STORTOP_ALTCHA",
            ] {
                text.push_str(&format!("{k}_KEY={}\n", sys::random_hex32()?));
            }
            std::fs::write(&secrets, text)?;
        }
        sys::chmod(&secrets, 0o600)?;
        let all = std::fs::read_to_string(&secrets)?;
        for (prefix, file) in [("PAYROOF_", "payroof.env"), ("STORTOP_", "stor.env")] {
            let lines: String = all
                .lines()
                .filter(|l| l.starts_with(prefix))
                .map(|l| format!("{l}\n"))
                .collect();
            let path = self.state.join(file);
            std::fs::write(&path, lines)?;
            sys::chmod(&path, 0o600)?;
        }
        Ok(())
    }

    /// Stores are Google Sheets, reached with a service-account key: STOR_GOOGLE_CREDENTIALS,
    /// else the first service_account *.json in .local/ or at the workspace root. None: no new stores.
    pub fn google_credentials(&self) -> Result<Option<PathBuf>> {
        let file = match std::env::var_os("STOR_GOOGLE_CREDENTIALS").filter(|f| !f.is_empty()) {
            Some(file) => {
                let file = PathBuf::from(file);
                if !file.is_file() {
                    bail!("STOR_GOOGLE_CREDENTIALS: no such file: {}", file.display());
                }
                Some(file)
            }
            None => [self.root.join(".local"), self.root.clone()]
                .iter()
                .find_map(|dir| {
                    let mut candidates: Vec<PathBuf> = std::fs::read_dir(dir)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|e| e == "json") && p.is_file())
                        .collect();
                    candidates.sort();
                    candidates.into_iter().find(|p| {
                        std::fs::read_to_string(p)
                            .ok()
                            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                            .is_some_and(|v| {
                                v["type"] == "service_account" && v["client_email"].is_string()
                            })
                    })
                }),
        };
        if let Some(f) = &file
            && f.to_string_lossy().contains(['"', '\\'])
        {
            bail!("Google key path must not contain quotes or backslashes");
        }
        Ok(file)
    }

    fn write_configs(&self) -> Result<()> {
        let state = self.state.display().to_string();
        if state.contains(['"', '\\']) {
            bail!("workspace path must not contain quotes or backslashes");
        }
        let google = self.google_credentials()?;
        let quoted = |s: &str| format!("\"{s}\"");
        let alias = |public: &str, local: &str| {
            if public == local {
                String::new()
            } else {
                quoted(local)
            }
        };

        let mut payroof = format!(
            r#"# Generated by roofer on every start — edit roofer, not this file.
bind = "{bind}:8080"
origin = "{public}"
# The same Payroof server on this Mac's loopback: Wallet may connect, and recover, through either.
origin_aliases = [{aliases}]
database = "sqlite://{state}/payroof.sqlite"
development = true
trusted_proxies = []
[[chains]]
id = "{CHAIN}"
name = "Anvil"
rpc = "{RPC}"
confirmations = 2
start_height = 0
[[chains.assets]]
id = "{NATIVE_ASSET}"
kind = "native"
symbol = "ETH"
decimals = 18
"#,
            bind = self.bind_host,
            public = self.payroof_public,
            aliases = alias(&self.payroof_public, PAYROOF),
        );
        for t in self.token_list() {
            payroof.push_str(&format!(
                "[[chains.assets]]\nid = \"{}\"\nkind = \"erc20\"\nsymbol = \"{}\"\ndecimals = {}\n",
                asset_id(&t.address),
                t.symbol,
                t.decimals
            ));
        }
        // Payroof's real networks and assets, exactly as in its example configuration.
        let example = std::fs::read_to_string(&self.real_networks)
            .with_context(|| format!("read {}", self.real_networks.display()))?;
        if let Some(start) = example.lines().position(|l| l.starts_with("[[chains]]")) {
            for line in example.lines().skip(start) {
                payroof.push_str(line);
                payroof.push('\n');
            }
        }
        std::fs::write(self.state.join("payroof.toml"), payroof)?;

        let payroof_origins = if self.payroof_public == PAYROOF {
            quoted(PAYROOF)
        } else {
            format!("{}, {}", quoted(&self.payroof_public), quoted(PAYROOF))
        };
        let google_line = google
            .map(|g| format!("google_credentials = \"{}\"", g.display()))
            .unwrap_or_default();
        let stor = format!(
            r#"# Generated by roofer on every start — edit roofer, not this file.
bind = "{bind}:8081"
# Loopback is Stor's own address (Payroof's webhooks), so it survives a Wi-Fi address change;
# devices on the network create stores at this Mac's LAN address.
origin = "{STOR_LOCAL}"
origin_aliases = [{aliases}]
database = "sqlite://{state}/stor.sqlite"
# Stores are <store>.stor.localhost: this Mac resolves it without any DNS.
store_suffix = "{suffix}"
payroof_origins = [{payroof_origins}]
development = true
trusted_proxies = []
sync_seconds = 30
{google_line}
[limits]
request_timeout_seconds = 120
"#,
            bind = self.bind_host,
            aliases = alias(STOR_LOCAL, &self.stor),
            suffix = self.store_suffix,
        );
        std::fs::write(self.state.join("stor.toml"), stor)?;
        Ok(())
    }

    // ------------------------------------------------------------ processes

    fn start_anvil(&self) -> Result<()> {
        if self.pid(Service::Anvil).is_some() && chain::chain_ok() {
            ui::ok("anvil already running");
            return Ok(());
        }
        self.require_port(Service::Anvil.port())?;
        let chain_json = self.state.join("chain.json");
        if self.state.join("payroof.sqlite").is_file() && !chain_json.is_file() {
            bail!(
                "Payroof database exists but the chain snapshot .local/manual/chain.json is missing; refusing to start a mismatched chain. Use roofer reset --yes"
            );
        }
        let log = self.log("anvil");
        // Keep only recent states in memory: --preserve-historical-states grew RSS without bound
        // (~8 GB after hours of 2 s blocks). Nothing here queries state older than "latest".
        let mut child = sys::spawn_daemon(
            Command::new("anvil")
                .args([
                    "--host",
                    &self.bind_host,
                    "--port",
                    "8545",
                    "--chain-id",
                    &CHAIN_ID.to_string(),
                ])
                .args(["--block-time", "2", "--state"])
                .arg(&chain_json)
                .args(["--state-interval", "10", "--prune-history", "64"]),
            &log,
        )?;
        std::fs::write(self.pid_file(Service::Anvil), format!("{}\n", child.id()))?;
        let deadline = Instant::now() + Duration::from_secs(30);
        while !chain::chain_ok() {
            if Instant::now() > deadline || child.try_wait()?.is_some() {
                eprintln!("{}", sys::tail(&log, 20));
                bail!("anvil did not start (log: {})", log.display());
            }
            sys::sleep_ms(500);
        }
        ui::ok(format!("anvil on {} (pid {})", self.rpc_public, child.id()));
        Ok(())
    }

    fn wait_ready(&self, s: Service, child: &mut Child, url: &str, log: &Path) -> Result<()> {
        let mut stable = 0;
        for _ in 0..120 {
            if http::ready(url) {
                stable += 1;
                if stable >= 3 {
                    return Ok(());
                }
            } else {
                stable = 0;
            }
            if child.try_wait()?.is_some() {
                eprintln!("{}", sys::tail(log, 30));
                bail!(
                    "{} exited during startup (log: {})",
                    s.name(),
                    log.display()
                );
            }
            sys::sleep_ms(1000);
        }
        eprintln!("{}", sys::tail(log, 30));
        bail!(
            "{} did not become ready at {url}/readyz (log: {})",
            s.name(),
            log.display()
        )
    }

    fn start_service(&self, s: Service) -> Result<()> {
        let (bin, url, public) = match s {
            Service::Payroof => (&self.payroof_bin, PAYROOF, &self.payroof_public),
            _ => (&self.stor_bin, STOR_LOCAL, &self.stor),
        };
        if let Some(pid) = self.pid(s) {
            ui::ok(format!("{} already running (pid {pid})", s.name()));
            return Ok(());
        }
        if !sys::is_executable(bin) {
            bail!("{} missing; run without --no-build", bin.display());
        }
        self.require_port(s.port())?;
        let mut cmd = Command::new(bin);
        cmd.arg(self.state.join(format!("{}.toml", s.name())));
        for line in std::fs::read_to_string(self.state.join(format!("{}.env", s.name())))?.lines() {
            if let Some((k, v)) = line.split_once('=') {
                cmd.env(k.trim(), v.trim());
            }
        }
        // Warnings too (rejected requests, Google errors), so `roofer logs` shows why.
        if std::env::var_os("RUST_LOG").is_none() {
            cmd.env("RUST_LOG", "warn");
        }
        let log = self.log(s.name());
        let mut child = sys::spawn_daemon(&mut cmd, &log)?;
        std::fs::write(self.pid_file(s), format!("{}\n", child.id()))?;
        self.wait_ready(s, &mut child, url, &log)?;
        ui::ok(format!(
            "{} ready on {public} (pid {})",
            s.name(),
            child.id()
        ));
        Ok(())
    }

    fn stop_pid(&self, s: Service, pid: i32, seconds: u64) {
        if !sys::signal(pid, libc::SIGTERM) {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while sys::alive(pid) && Instant::now() < deadline {
            sys::sleep_ms(500);
        }
        if sys::alive(pid) {
            if s == Service::Anvil {
                ui::warn(format!(
                    "anvil did not exit in {seconds} s; killing it (the last ~10 s of chain state may be lost)"
                ));
            }
            sys::signal(pid, libc::SIGKILL);
        }
        let _ = std::fs::remove_file(self.pid_file(s));
        ui::ok(format!("stopped {}", s.name()));
    }

    fn stop_service(&self, s: Service) {
        if let Some(pid) = self.pid(s) {
            self.stop_pid(s, pid, if s == Service::Anvil { 30 } else { 20 });
        }
    }

    /// A service of ours can outlive its pid file (e.g. `rm -rf .local` while running).
    /// It is recognized by its listening port plus a command line that names this
    /// workspace's binary or chain-state path, so unrelated processes are never touched.
    fn stop_orphan(&self, s: Service) {
        let needle = match s {
            Service::Anvil => format!("--state {}", self.state.join("chain.json").display()),
            _ => self.needle(s),
        };
        let Some(pid) = sys::listeners(s.port())
            .into_iter()
            .find(|p| sys::command_of(*p).is_some_and(|c| c.contains(&needle)))
        else {
            return;
        };
        if self.pid(s) == Some(pid) {
            return; // tracked: the normal stop handles it
        }
        if s == Service::Anvil && !self.state.join("initialized").is_file() {
            // Its state directory was deleted. A graceful stop would write the old chain
            // into the new state directory, so discard it instead.
            ui::warn(format!(
                "leftover anvil from a deleted .local (pid {pid}); discarding its chain"
            ));
            sys::signal(pid, libc::SIGKILL);
            for _ in 0..20 {
                if !sys::alive(pid) {
                    break;
                }
                sys::sleep_ms(250);
            }
            let _ = std::fs::remove_file(self.state.join("chain.json"));
            let _ = std::fs::remove_file(self.pid_file(s));
            ui::ok("stopped leftover anvil");
        } else {
            ui::warn(format!(
                "leftover {} without a pid file (pid {pid}); stopping it",
                s.name()
            ));
            self.stop_pid(s, pid, if s == Service::Anvil { 30 } else { 20 });
        }
    }

    fn stop_orphans(&self) {
        for s in [Service::Stor, Service::Payroof, Service::Anvil] {
            self.stop_orphan(s);
        }
    }

    fn build_all(&self) -> Result<()> {
        sys::need(&["cargo"])?;
        for repo in ["payroof", "stor"] {
            ui::say(format!("cargo build --release --offline ({repo})"));
            let log = self.log(&format!("build-{repo}"));
            let built = sys::logged(
                Command::new("cargo")
                    .args(["build", "--release", "--locked", "--offline"])
                    .current_dir(self.root.join(repo)),
                &log,
                false,
            )?;
            if !built {
                let text = std::fs::read_to_string(&log).unwrap_or_default();
                let lines: Vec<&str> = text.lines().collect();
                let mut shown: Vec<&str> = Vec::new();
                for (i, l) in lines.iter().enumerate() {
                    if l.starts_with("error") || l.starts_with("warning: unused") {
                        shown.extend(lines.iter().skip(i).take(6).copied());
                    }
                }
                eprintln!(
                    "{}",
                    shown
                        .iter()
                        .take(40)
                        .copied()
                        .collect::<Vec<&str>>()
                        .join("\n")
                );
                bail!("{repo} build failed (log: {})", log.display());
            }
            ui::ok(format!("{repo} built"));
        }
        Ok(())
    }

    // ------------------------------------------------------------ LAN checks

    /// Devices on the Wi-Fi reach this Mac at the LAN IP: say early what would stop them.
    fn check_lan(&self) -> Result<()> {
        let Some(ip) = &self.lan_ip else {
            return Ok(());
        };
        let marker = self.state.join("lan-ip");
        let previous = std::fs::read_to_string(&marker)
            .unwrap_or_default()
            .trim()
            .to_string();
        if !previous.is_empty() && &previous != ip {
            ui::warn(format!(
                "this Mac's Wi-Fi address changed: {previous} → {ip}. On the iPhone:"
            ));
            ui::warn(
                "  reinstall the test Wallet (roofer ios --local) and connect the project again (it is recovered);",
            );
            ui::warn(format!(
                "  stores created from the iPhone keep http://{previous}:8080 as their sheet's Payments URL: change it."
            ));
            ui::warn(
                "  A DHCP reservation for this Mac on the router (or PAYROOF_LAN_IP) keeps the address fixed.",
            );
        }
        std::fs::write(&marker, format!("{ip}\n"))?;
        let firewall = Path::new("/usr/libexec/ApplicationFirewall/socketfilterfw");
        let query = |flag: &str| {
            sys::is_executable(firewall)
                && sys::try_capture(Command::new(firewall).arg(flag))
                    .is_some_and(|o| o.contains("enabled"))
        };
        if query("--getblockall") {
            ui::warn(
                "the macOS firewall blocks all incoming connections: devices cannot reach this Mac",
            );
            ui::warn(
                "  (System Settings → Network → Firewall → Options: turn off \"Block all incoming connections\")",
            );
        } else if query("--getglobalstate") {
            ui::ok(
                "macOS firewall on: allow incoming connections for anvil, payroof and stortop when asked",
            );
        }
        Ok(())
    }

    fn lan_reachable(&self) -> bool {
        http::ready(&self.payroof_public) && http::ready(&self.stor)
    }

    fn check_lan_reachable(&self) {
        let Some(ip) = &self.lan_ip else { return };
        if self.lan_reachable() {
            ui::ok(format!(
                "reachable on the Wi-Fi at {ip} (Payroof :8080, Stor :8081, Anvil :8545)"
            ));
        } else {
            ui::warn(format!(
                "Payroof or Stor does not answer at {ip}; check PAYROOF_LAN_IP and PAYROOF_BIND_HOST"
            ));
        }
    }

    // ------------------------------------------------------------ commands

    pub fn up(&self, o: &UpOptions) -> Result<()> {
        sys::need(&["anvil", "cast", "solc", "lsof"])?;
        if o.ios && self.lan_ip.is_none() {
            bail!(
                "--ios needs this Mac's Wi-Fi address (not found; set PAYROOF_LAN_IP, and don't set PAYROOF_BIND_HOST=127.0.0.1)"
            );
        }
        for dir in [&self.state, &self.logs, &self.wallet_dir] {
            std::fs::create_dir_all(dir)?;
        }
        sys::chmod(&self.state, 0o700)?;
        sys::chmod(&self.wallet_dir, 0o700)?;
        self.check_lan()?;
        if !o.no_build {
            self.build_all()?;
            if !o.no_wallet {
                self.build_wallet(o.rebuild_wallet)?;
            }
        }
        ui::say("starting local chain");
        self.stop_orphans();
        self.start_anvil()?;
        let initialized = self.state.join("initialized");
        if !initialized.is_file() {
            let specs = self.default_tokens.trim();
            ui::say(format!(
                "fresh chain: deploying default tokens ({})",
                if specs.is_empty() { "none" } else { specs }
            ));
            std::fs::write(&self.tokens, "")?;
            for spec in specs.split_whitespace() {
                let (symbol, decimals) = spec.split_once(':').unwrap_or((spec, spec));
                self.deploy_token(
                    symbol,
                    decimals,
                    &format!("Payroof Test {symbol}"),
                    "1000000000",
                )?;
            }
            std::fs::write(&initialized, format!("{}\n", utc_now()))?;
        } else {
            let tokens = self.token_list();
            for t in &tokens {
                let code = chain::rpc("eth_getCode", json!([t.address, "latest"]))?;
                if code == "0x" {
                    bail!(
                        "token {} has no code at {}: the chain snapshot does not match .local/manual. Use roofer reset --yes",
                        t.symbol,
                        t.address
                    );
                }
            }
            ui::ok(format!(
                "chain state matches {} registered token(s)",
                tokens.len()
            ));
        }
        self.write_secrets()?;
        self.write_configs()?;
        ui::say("starting Payroof and Stor");
        self.start_service(Service::Payroof)?;
        self.start_service(Service::Stor)?;
        self.check_lan_reachable();
        self.fund_buyer_slots()?;
        if !o.no_wallet {
            self.open_wallet()?;
        }
        if o.web {
            let _ = sys::quiet(Command::new("open").arg(&self.stor));
        }
        self.summary()?;
        if o.ios {
            self.ios(None, true, !o.no_build)?;
        }
        Ok(())
    }

    fn summary(&self) -> Result<()> {
        println!();
        ui::say("Payroof local stack is running");
        let suffix = &self.store_suffix;
        println!("  Chain   {} (chain id {CHAIN_ID})", self.rpc_public);
        println!("  Payroof {}", self.payroof_public);
        println!("  Stor    {}", self.stor);
        println!("  Store   http://<slug>.{suffix}  (e.g. http://shop.{suffix}; on this Mac)");
        println!("  Faucet  {} (Anvil account 0)", chain::payer()?);
        if let Some(ip) = &self.lan_ip {
            println!(
                "  iPhone  same Wi-Fi as this Mac ({ip}): roofer ios --local installs the test Wallet"
            );
        }
        println!();
        self.print_tokens();
        println!(
            "\n  Next: create a wallet, add the tokens above (Settings → Networks & tokens → Add token), fund a buyer:"
        );
        println!(
            "        roofer fund 0xBUYER   — full walkthrough: roofer guide   stop: roofer down"
        );
        Ok(())
    }

    pub fn down(&self) -> Result<()> {
        ui::say("stopping");
        self.stop_wallet();
        self.stop_service(Service::Stor);
        self.stop_service(Service::Payroof);
        self.stop_service(Service::Anvil);
        self.stop_orphans();
        ui::ok("state kept in .local/manual (resume with roofer up)");
        Ok(())
    }

    pub fn status(&self) -> Result<()> {
        for s in Service::ALL {
            match self.pid(s) {
                Some(pid) => ui::ok(format!("{} running (pid {pid})", s.name())),
                None => ui::warn(format!("{} not running", s.name())),
            }
        }
        if chain::chain_ok() {
            ui::ok(format!("chain {CHAIN} at block {}", chain::block_number()?));
        }
        if http::ready(PAYROOF) {
            ui::ok(format!("Payroof ready ({PAYROOF})"))
        } else {
            ui::warn("Payroof not ready")
        }
        if http::ready(STOR_LOCAL) {
            ui::ok(format!("Stor ready ({})", self.stor))
        } else {
            ui::warn("Stor not ready")
        }
        if let Some(ip) = &self.lan_ip {
            if self.lan_reachable() {
                ui::ok(format!("reachable on the Wi-Fi at {ip}"));
            } else {
                ui::warn(format!("not reachable at {ip}"));
            }
        }
        if self.wallet_pids().is_empty() {
            ui::warn("local Wallet app not open (roofer wallet)");
        } else {
            ui::ok("local Wallet app open");
        }
        let db = self.state.join("stor.sqlite");
        if sys::on_path("sqlite3") && db.is_file() {
            // Only SELECTs. A read-only open fails on a WAL database whose -shm file
            // is gone (Stor stopped), so the database is opened normally.
            let count = |sql: &str| {
                sys::try_capture(
                    Command::new("sqlite3")
                        .args(["-cmd", ".timeout 2000"])
                        .arg(&db)
                        .arg(sql),
                )
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "?".into())
            };
            let stores = count("SELECT count(*) FROM stores");
            let paired = count("SELECT count(*) FROM connections WHERE status='active'");
            ui::ok(format!(
                "Stor: {stores} store(s), {paired} paired with Payroof"
            ));
        }
        println!();
        self.print_tokens();
        Ok(())
    }

    pub fn add_token(
        &self,
        symbol: &str,
        decimals: &str,
        name: Option<&str>,
        supply: &str,
    ) -> Result<()> {
        sys::need(&["cast", "solc"])?;
        if !self.state.join("initialized").is_file() {
            bail!("start the stack first: roofer up");
        }
        let name = name
            .map(str::to_string)
            .unwrap_or_else(|| format!("Local {symbol}"));
        self.deploy_token(symbol, decimals, &name, supply)?;
        self.write_configs()?;
        if let Some(pid) = self.pid(Service::Payroof) {
            ui::say("restarting Payroof with the new asset list");
            self.stop_pid(Service::Payroof, pid, 20);
            self.start_service(Service::Payroof)?;
        }
        let token = self
            .token_lookup(symbol)
            .context("token was not recorded")?;
        println!(
            "\n  Wallet → Settings → Networks & tokens → Add token: network Anvil, contract {}, symbol {}, decimals {}",
            token.address, token.symbol, token.decimals
        );
        Ok(())
    }

    pub fn reset(&self, wipe_wallet: bool) -> Result<()> {
        self.down()?;
        if !self.state.is_dir() && !wipe_wallet {
            ui::ok("nothing to reset");
            return Ok(());
        }
        for entry in std::fs::read_dir(&self.state)
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = entry.path();
            if path == self.wallet_dir && !wipe_wallet {
                continue;
            }
            let removed = if path.is_dir() && !path.is_symlink() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            removed.with_context(|| format!("remove {}", path.display()))?;
        }
        if wipe_wallet && self.wallet_container_dir.exists() {
            std::fs::remove_dir_all(&self.wallet_container_dir)?;
        }
        ui::ok(format!(
            "reset .local/manual{}",
            if wipe_wallet {
                " including Wallet data"
            } else {
                " (Wallet data kept)"
            }
        ));
        Ok(())
    }

    pub fn logs(&self, which: Option<Service>) -> Result<()> {
        use std::os::unix::process::CommandExt;
        let mut cmd = Command::new("tail");
        match which {
            Some(s) => cmd.args(["-n", "50", "-F"]).arg(self.log(s.name())),
            None => cmd
                .args(["-n", "20", "-F"])
                .args(Service::ALL.map(|s| self.log(s.name()))),
        };
        Err(cmd.exec()).context("run tail")
    }

    pub fn mine(&self, blocks: u32) -> Result<()> {
        chain::require_chain()?;
        chain::rpc("anvil_mine", json!([format!("0x{blocks:x}")]))?;
        ui::ok(format!(
            "mined {blocks} block(s); height {}",
            chain::block_number()?
        ));
        Ok(())
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` without a date library.
fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil from days (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats_utc_timestamps() {
        let now = super::utc_now();
        assert_eq!(now.len(), 20);
        assert!(now.ends_with('Z') && now.as_bytes()[10] == b'T');
    }
}
