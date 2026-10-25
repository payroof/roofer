//! The local system the tests run against: Anvil chains with test tokens, the real
//! `payroof` binary watching them, Stor in this process with an in-memory sheet, the
//! merchant's Payroof Wallet and customers' browsers.
#![allow(dead_code)]

use payroof_wallet_core::ffi::Wallet;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use stortop::source::{
    Change, Failure, Fut, Source,
    sheet::{Cell, Tab},
};
use zeroize::Zeroizing;

/// The merchant's recovery words. Anvil's accounts come from other words, so no
/// merchant address starts out with coins.
pub const MERCHANT_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";
/// Anvil's own (funded, unlocked) accounts: the customers and the token deployer.
pub const BUYER_MNEMONIC: &str = "test test test test test test test test test test test junk";
pub const PIN: &str = "123456";
pub const PROJECT: &str = "shop";
pub const SHEET: &str = "sheet";
pub const ETHER: u128 = 1_000_000_000_000_000_000;

/// A child process that is killed when the test ends, pass or fail.
pub struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub async fn eventually<T, F>(what: &str, seconds: u64, mut probe: impl FnMut() -> F) -> T
where
    F: Future<Output = Option<T>>,
{
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        if let Some(value) = probe().await {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Runs a blocking Wallet core call (it has its own runtime) off the async workers.
pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

pub fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("payroof-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ------------------------------------------------------------------ sheets

/// Spreadsheets in memory, keyed by ID; `mem:<id>` names one.
#[derive(Default)]
pub struct Memory {
    pub sheets: Mutex<HashMap<String, Vec<Tab>>>,
}
impl Memory {
    pub fn tab(&self, title: &str) -> Tab {
        self.sheets.lock().unwrap()[SHEET]
            .iter()
            .find(|t| t.title == title)
            .cloned()
            .unwrap_or_else(|| panic!("no tab {title}"))
    }
    pub fn set(&self, title: &str, row: usize, column: usize, value: &str) {
        let mut sheets = self.sheets.lock().unwrap();
        let tab = sheets
            .get_mut(SHEET)
            .unwrap()
            .iter_mut()
            .find(|t| t.title == title)
            .unwrap();
        write(&mut tab.rows, row, column, value.into());
    }
    /// Replaces every row below the header.
    pub fn replace_rows(&self, title: &str, rows: Vec<Vec<String>>) {
        let mut sheets = self.sheets.lock().unwrap();
        let tab = sheets
            .get_mut(SHEET)
            .unwrap()
            .iter_mut()
            .find(|t| t.title == title)
            .unwrap();
        tab.rows.truncate(1);
        tab.rows.extend(rows);
    }
}
fn write(rows: &mut Vec<Vec<String>>, row: usize, column: usize, value: String) {
    if rows.len() <= row {
        rows.resize(row + 1, Vec::new());
    }
    if rows[row].len() <= column {
        rows[row].resize(column + 1, String::new());
    }
    rows[row][column] = value;
}
fn render(cell: &Cell) -> String {
    match cell {
        Cell::Header(s) | Cell::Text(s) | Cell::Formula(s) => s.clone(),
        Cell::Number(n) => n.to_string(),
        Cell::Date(d) => d.to_string(),
    }
}
impl Source for Memory {
    fn kind(&self) -> &'static str {
        "google"
    }
    fn id(&self, input: &str) -> Option<String> {
        input.strip_prefix("mem:").map(str::to_string)
    }
    fn account(&self) -> &str {
        "stor@example.iam.gserviceaccount.com"
    }
    fn read<'a>(&'a self, id: &'a str) -> Fut<'a, Vec<Tab>> {
        Box::pin(async move {
            self.sheets
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .ok_or_else(|| Failure::Unreachable.into())
        })
    }
    fn apply<'a>(&'a self, id: &'a str, changes: &'a [Change]) -> Fut<'a, ()> {
        Box::pin(async move {
            let mut sheets = self.sheets.lock().unwrap();
            let tabs = sheets.get_mut(id).ok_or(Failure::Unreachable)?;
            for change in changes {
                match change {
                    Change::Add { tab, title } => tabs.push(Tab {
                        id: *tab,
                        title: title.clone(),
                        rows: Vec::new(),
                    }),
                    Change::Rename { tab, title } => {
                        tabs.iter_mut().find(|t| t.id == *tab).unwrap().title = title.clone()
                    }
                    Change::Write {
                        tab,
                        row,
                        column,
                        rows,
                    } => {
                        let t = tabs.iter_mut().find(|t| t.id == *tab).unwrap();
                        for (r, cells) in rows.iter().enumerate() {
                            for (c, cell) in cells.iter().enumerate() {
                                write(&mut t.rows, row + r, column + c, render(cell));
                            }
                        }
                    }
                    Change::Append { tab, rows } => {
                        let t = tabs.iter_mut().find(|t| t.id == *tab).unwrap();
                        t.rows
                            .extend(rows.iter().map(|r| r.iter().map(render).collect()));
                    }
                    Change::Stats { .. } | Change::Mark { .. } => {}
                }
            }
            Ok(())
        })
    }
}

// ------------------------------------------------------------------ chains

pub struct Anvil {
    pub id: u64,
    pub chain: String,
    pub url: String,
    pub http: reqwest::Client,
    pub accounts: Vec<String>,
    _process: Process,
}
impl Anvil {
    /// A chain mining a block every second, with 20 funded accounts.
    pub async fn start(id: u64) -> Self {
        let port = free_port();
        let process = Command::new("anvil")
            .args(["--port", &port.to_string(), "--chain-id", &id.to_string()])
            .args(["--block-time", "1", "--accounts", "20"])
            .args(["--mnemonic", BUYER_MNEMONIC])
            .stdout(Stdio::null())
            .spawn()
            .expect("anvil on PATH (Foundry)");
        let mut anvil = Anvil {
            id,
            chain: format!("eip155:{id}"),
            url: format!("http://127.0.0.1:{port}"),
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            accounts: Vec::new(),
            _process: Process(process),
        };
        eventually("anvil", 30, || async {
            anvil.try_rpc("eth_chainId", json!([])).await
        })
        .await;
        anvil.accounts = anvil
            .rpc("eth_accounts", json!([]))
            .await
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_ascii_lowercase())
            .collect();
        anvil
    }
    pub fn native(&self) -> String {
        format!("{}/slip44:60", self.chain)
    }
    pub async fn try_rpc(&self, method: &str, params: Value) -> Option<Value> {
        let body: Value = self
            .http
            .post(&self.url)
            .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        assert!(body.get("error").is_none(), "{method}: {body}");
        Some(body["result"].clone())
    }
    pub async fn rpc(&self, method: &str, params: Value) -> Value {
        self.try_rpc(method, params).await.expect("anvil answers")
    }
    /// Sends from an unlocked account and waits until it is mined successfully.
    pub async fn send(&self, from: &str, to: Option<&str>, value: u128, data: &str) -> Value {
        let mut tx =
            json!({"from": from, "value": format!("0x{value:x}"), "data": format!("0x{data}")});
        if let Some(to) = to {
            tx["to"] = json!(to);
        }
        let hash = self.rpc("eth_sendTransaction", json!([tx])).await;
        let receipt = self.receipt(hash.as_str().unwrap()).await;
        assert_eq!(receipt["status"], "0x1", "transaction failed: {receipt}");
        receipt
    }
    /// Submits a token transfer without waiting for it to be mined; returns its hash.
    pub async fn submit_token(&self, token: &str, from: &str, to: &str, amount: u128) -> String {
        let data = format!("0xa9059cbb{}{}", pad(to), word(amount));
        let hash = self
            .rpc(
                "eth_sendTransaction",
                json!([{"from": from, "to": token, "data": data}]),
            )
            .await;
        hash.as_str().unwrap().to_string()
    }
    pub async fn receipt(&self, hash: &str) -> Value {
        eventually("receipt", 60, || async {
            let r = self.rpc("eth_getTransactionReceipt", json!([hash])).await;
            (!r.is_null()).then_some(r)
        })
        .await
    }
    pub async fn pay_native(&self, from: &str, to: &str, wei: u128) -> String {
        let r = self.send(from, Some(to), wei, "").await;
        r["transactionHash"].as_str().unwrap().into()
    }
    pub async fn pay_token(&self, token: &str, from: &str, to: &str, amount: u128) -> String {
        let data = format!("a9059cbb{}{}", pad(to), word(amount));
        let r = self.send(from, Some(token), 0, &data).await;
        r["transactionHash"].as_str().unwrap().into()
    }
    pub async fn balance(&self, address: &str) -> u128 {
        quantity(&self.rpc("eth_getBalance", json!([address, "latest"])).await)
    }
    pub async fn token_balance(&self, token: &str, address: &str) -> u128 {
        let data = format!("0x70a08231{}", pad(address));
        quantity(
            &self
                .rpc("eth_call", json!([{"to": token, "data": data}, "latest"]))
                .await,
        )
    }
    pub async fn set_balance(&self, address: &str, wei: u128) {
        self.rpc("anvil_setBalance", json!([address, format!("0x{wei:x}")]))
            .await;
    }
    /// Turns block production off (`false`) or back on, one block a second.
    pub async fn mining(&self, on: bool) {
        self.rpc("evm_setIntervalMining", json!([if on { 1 } else { 0 }]))
            .await;
    }
    pub async fn deploy(&self, from: &str, code: &str) -> String {
        let r = self.send(from, None, 0, code).await;
        r["contractAddress"].as_str().unwrap().to_ascii_lowercase()
    }
}

pub fn quantity(v: &Value) -> u128 {
    u128::from_str_radix(v.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}
pub fn pad(address: &str) -> String {
    format!(
        "{:0>64}",
        address.trim_start_matches("0x").to_ascii_lowercase()
    )
}
pub fn word(n: u128) -> String {
    format!("{n:064x}")
}

// ------------------------------------------------------------------ tokens

/// An ERC-20 on one chain.
#[derive(Clone, Debug)]
pub struct Token {
    pub symbol: &'static str,
    pub decimals: u32,
    pub address: String,
    pub asset: String,
}
impl Token {
    /// `amount` whole tokens written as a decimal ("1.5"), in the smallest unit.
    pub fn units(&self, amount: &str) -> u128 {
        units(amount, self.decimals)
    }
}
pub fn units(amount: &str, decimals: u32) -> u128 {
    let (whole, fraction) = amount.split_once('.').unwrap_or((amount, ""));
    assert!(fraction.len() <= decimals as usize, "{amount} too precise");
    format!("{whole}{fraction:0<width$}", width = decimals as usize)
        .parse()
        .unwrap()
}

#[derive(Clone, Copy)]
pub enum Kind {
    /// EIP-2612 permit and EIP-3009 transferWithAuthorization, like USDC.
    Authorizing,
    /// EIP-2612 permit only.
    Permitting,
    /// Neither.
    Plain,
    /// Like Tether's USDT on Ethereum: no return values.
    Tether,
}

/// Creation code of each test token contract, compiled with `solc`.
pub struct Contracts {
    token: String,
    tether: String,
}
impl Contracts {
    pub fn compile() -> Self {
        let source = concat!(env!("CARGO_MANIFEST_DIR"), "/contracts/Tokens.sol");
        let out = Command::new("solc")
            .args([
                "--evm-version",
                "paris",
                "--optimize",
                "--combined-json",
                "bin",
                source,
            ])
            .output()
            .expect("solc on PATH");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let json: Value = serde_json::from_slice(&out.stdout).unwrap();
        let bin = |name: &str| {
            json["contracts"]
                .as_object()
                .unwrap()
                .iter()
                .find(|(k, _)| k.ends_with(&format!(":{name}")))
                .map(|(_, v)| v["bin"].as_str().unwrap().to_string())
                .unwrap()
        };
        Contracts {
            token: bin("Token"),
            tether: bin("TetherToken"),
        }
    }
    /// Deploys a token, its whole supply to `from`.
    pub async fn deploy(
        &self,
        anvil: &Anvil,
        from: &str,
        symbol: &'static str,
        decimals: u32,
        kind: Kind,
    ) -> Token {
        let supply = 10u128.pow(12) * 10u128.pow(decimals);
        let name = format!("Test {symbol}");
        let code = match kind {
            Kind::Tether => format!(
                "{}{}",
                self.tether,
                abi(&[
                    Arg::Str(&name),
                    Arg::Str(symbol),
                    Arg::Uint(decimals.into()),
                    Arg::Uint(supply)
                ])
            ),
            _ => format!(
                "{}{}",
                self.token,
                abi(&[
                    Arg::Str(&name),
                    Arg::Str(symbol),
                    Arg::Uint(decimals.into()),
                    Arg::Uint(supply),
                    Arg::Bool(matches!(kind, Kind::Authorizing | Kind::Permitting)),
                    Arg::Bool(matches!(kind, Kind::Authorizing)),
                ])
            ),
        };
        let address = anvil.deploy(from, &code).await;
        Token {
            symbol,
            decimals,
            asset: format!("{}/erc20:{address}", anvil.chain),
            address,
        }
    }
}

pub enum Arg<'a> {
    Str(&'a str),
    Uint(u128),
    Bool(bool),
}
/// ABI-encodes constructor arguments (hex, no `0x`).
pub fn abi(args: &[Arg]) -> String {
    let mut head = String::new();
    let mut tail = String::new();
    let head_size = 32 * args.len();
    for arg in args {
        match arg {
            Arg::Uint(n) => head += &word(*n),
            Arg::Bool(b) => head += &word(u128::from(*b)),
            Arg::Str(s) => {
                head += &word((head_size + tail.len() / 2) as u128);
                tail += &word(s.len() as u128);
                let mut bytes = hex(s.as_bytes());
                while !bytes.len().is_multiple_of(64) {
                    bytes.push('0');
                }
                tail += &bytes;
            }
        }
    }
    head + &tail
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ------------------------------------------------------------------ Payroof

/// What one Payroof chain watches.
pub struct Watch<'a> {
    pub anvil: &'a Anvil,
    pub name: &'a str,
    pub tokens: Vec<&'a Token>,
}

/// The real `payroof` binary, watching each chain's coin and the given tokens.
pub async fn payroof(chains: &[Watch<'_>], dir: &std::path::Path) -> (String, Process) {
    let binary = std::env::var("PAYROOF_BIN").unwrap_or_else(|_| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../payroof/target/release/payroof"
        )
        .into()
    });
    assert!(
        std::path::Path::new(&binary).is_file(),
        "{binary} missing: build Payroof (cargo build --release in payroof) or set PAYROOF_BIN"
    );
    let port = free_port();
    let origin = format!("http://127.0.0.1:{port}");
    let mut config = format!(
        "bind = \"127.0.0.1:{port}\"\norigin = \"{origin}\"\ndatabase = \"sqlite://{}\"\ndevelopment = true\n",
        dir.join("payroof.sqlite").display()
    );
    for watch in chains {
        config += &format!(
            "[[chains]]\nid = \"{}\"\nname = \"{}\"\nrpc = \"{}\"\nconfirmations = 2\nstart_height = 0\n",
            watch.anvil.chain, watch.name, watch.anvil.url
        );
        config += &format!(
            "[[chains.assets]]\nid = \"{}\"\nkind = \"native\"\nsymbol = \"ETH\"\ndecimals = 18\n",
            watch.anvil.native()
        );
        for token in &watch.tokens {
            config += &format!(
                "[[chains.assets]]\nid = \"{}\"\nkind = \"erc20\"\nsymbol = \"{}\"\ndecimals = {}\n",
                token.asset, token.symbol, token.decimals
            );
        }
    }
    let path = dir.join("payroof.toml");
    std::fs::write(&path, config).unwrap();
    let key = |byte: &str| byte.repeat(32);
    let process = Command::new(&binary)
        .arg(&path)
        .env("PAYROOF_MASTER_KEY", key("01"))
        .env("PAYROOF_CURSOR_KEY", key("02"))
        .env("PAYROOF_ALTCHA_KEY", key("03"))
        .env("RUST_LOG", "info")
        .stdout(std::fs::File::create(dir.join("payroof.log")).unwrap())
        .spawn()
        .expect("start payroof");
    let process = Process(process);
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    eventually("Payroof readiness", 60, || async {
        let r = http.get(format!("{origin}/readyz")).send().await.ok()?;
        r.status().is_success().then_some(())
    })
    .await;
    (origin, process)
}

// ------------------------------------------------------------------ Stor

pub const STOR_MASTER: [u8; 32] = [9; 32];

/// Stor in this process, with the in-memory sheets as its only source. Customers are
/// told apart by the client IP a trusted proxy (loopback) forwards.
pub async fn stor(payroof: &str) -> (String, stortop::App, Arc<Memory>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let origin = format!("http://127.0.0.1:{port}");
    let config: stortop::config::Config = toml::from_str(&format!(
        r#"bind = "127.0.0.1:{port}"
origin = "{origin}"
store_suffix = "stor.localhost:{port}"
database = "sqlite::memory:"
payroof_origins = ["{payroof}"]
development = true
trusted_proxies = ["127.0.0.1/32"]
"#
    ))
    .unwrap();
    let memory = Arc::new(Memory::default());
    memory
        .sheets
        .lock()
        .unwrap()
        .insert(SHEET.into(), Vec::new());
    let app = stortop::State::new(
        config,
        stortop::db::connect("sqlite::memory:").await.unwrap(),
        Zeroizing::new(STOR_MASTER),
        Zeroizing::new("system-test-altcha".into()),
        reqwest::Client::builder().no_proxy().build().unwrap(),
        vec![memory.clone()],
    );
    stortop::workers::start(app.clone());
    stortop::workers::start_sync(app.clone());
    let router = stortop::http::router(app.clone());
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    (origin, app, memory)
}

/// A response that may be an error, for requests a test expects to be refused.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
}
impl Reply {
    pub fn ok(self) -> Value {
        assert!(
            (200..300).contains(&self.status),
            "{} {}",
            self.status,
            self.body
        );
        self.body
    }
    pub fn error(&self) -> &str {
        self.body["error"].as_str().unwrap_or_default()
    }
}

/// A customer's browser on one storefront: its host, Origin, cookie and client IP.
pub struct Customer {
    pub http: reqwest::Client,
    pub origin: String,
    pub cookie: String,
    pub ip: String,
}
impl Customer {
    pub async fn visit(storefront: &str, ip: &str) -> (Self, Value) {
        let http = storefront_client(storefront);
        let catalog = http
            .get(format!("{storefront}/v1/catalog"))
            .header("x-forwarded-for", ip)
            .send()
            .await
            .unwrap();
        assert!(
            catalog.status().is_success(),
            "catalog {}",
            catalog.status()
        );
        let cookie = catalog.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        assert!(cookie.starts_with("stor_customer="), "{cookie}");
        let catalog = catalog.json().await.unwrap();
        let customer = Customer {
            http,
            origin: storefront.into(),
            cookie,
            ip: ip.into(),
        };
        (customer, catalog)
    }
    pub async fn request(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Reply {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.origin))
            .header("cookie", &self.cookie)
            .header("origin", &self.origin)
            .header("x-forwarded-for", &self.ip);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        Reply { status, body }
    }
    pub async fn catalog(&self) -> Value {
        self.request(reqwest::Method::GET, "/v1/catalog", None)
            .await
            .ok()
    }
    pub async fn cart(&self) -> Reply {
        self.request(reqwest::Method::GET, "/v1/cart", None).await
    }
    pub async fn add(&self, sku: &str, quantity: u32) -> Reply {
        self.request(
            reqwest::Method::PUT,
            &format!("/v1/cart/{sku}"),
            Some(json!({"quantity": quantity})),
        )
        .await
    }
    pub async fn remove(&self, sku: &str) -> Reply {
        self.request(reqwest::Method::DELETE, &format!("/v1/cart/{sku}"), None)
            .await
    }
    /// A solved proof of work, then the checkout of these lines in this asset.
    pub async fn checkout(&self, asset: &str, skus: &[&str]) -> Reply {
        let proof = self.proof().await;
        self.checkout_with(proof, asset, skus).await
    }
    pub async fn checkout_with(&self, proof: Value, asset: &str, skus: &[&str]) -> Reply {
        self.request(
            reqwest::Method::POST,
            "/v1/checkout",
            Some(json!({"asset": asset, "skus": skus, "altcha": proof})),
        )
        .await
    }
    /// A solved checkout proof of work, so racing checkouts can be sent at once.
    pub async fn proof(&self) -> Value {
        let challenge: altcha::Challenge = self
            .http
            .get(format!("{}/v1/altcha?purpose=checkout", self.origin))
            .header("cookie", &self.cookie)
            .header("x-forwarded-for", &self.ip)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let solution = tokio::task::spawn_blocking(move || {
            let solution = altcha::solve_challenge(altcha::SolveChallengeOptions::new(&challenge))
                .unwrap()
                .expect("checkout proof");
            (challenge, solution)
        })
        .await
        .unwrap();
        json!({"challenge": solution.0, "solution": solution.1})
    }
    pub async fn order(&self, id: &str) -> Value {
        self.request(reqwest::Method::GET, &format!("/v1/orders/{id}"), None)
            .await
            .ok()
    }
    /// Polls once a second, as the storefront page does (well within the limits).
    pub async fn wait_status(&self, id: &str, status: &str, seconds: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            let order = self.order(id).await;
            if order["status"] == status {
                return order;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for order {id} {status}: {order}"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}

/// A client that dials a storefront's host (`<store>.stor.localhost:<port>`) on
/// loopback, so requests carry the storefront's own Host, as a browser's would.
pub fn storefront_client(storefront: &str) -> reqwest::Client {
    let url = reqwest::Url::parse(storefront).unwrap();
    let host = url.host_str().unwrap().to_string();
    let port = url.port().unwrap();
    reqwest::Client::builder()
        .no_proxy()
        .resolve(&host, SocketAddr::from(([127, 0, 0, 1], port)))
        .build()
        .unwrap()
}

// ------------------------------------------------------------------ the merchant

/// The merchant's Wallet: account 0 connected to the Payroof project, and its store.
pub struct Merchant {
    pub wallet: Arc<Wallet>,
    /// The account's first Receive address, the one the app shows the merchant.
    pub receive: String,
    pub payroof: String,
    pub storefront: String,
    pub store: stortop::stores::StoreId,
}
impl Merchant {
    /// Wallet → Payroof project → Stor store (`shop`), then the first sync with these
    /// USD rates on the Tokens tab and `catalog` (else the sample products Stor wrote).
    pub async fn open(
        payroof: &str,
        stor: &str,
        app: &stortop::App,
        memory: &Memory,
        rates: &[(&str, &str)],
        catalog: Option<Vec<Vec<String>>>,
    ) -> Merchant {
        // SAFETY: set before the Wallet core reads the environment, by every test alike.
        unsafe { std::env::set_var("PAYROOF_WALLET_LOCAL_DEVELOPMENT", "1") };
        assert!(payroof_wallet_core::ffi::local_development_enabled());
        let (p, s) = (payroof.to_string(), stor.to_string());
        let (wallet, created, receive) = blocking(move || {
            let wallet = Wallet::create(MERCHANT_MNEMONIC.into(), PIN.into()).expect("wallet");
            wallet.unlock(PIN.into()).expect("unlock");
            assert_eq!(wallet.add_project("Main".into(), Some(0)).unwrap(), 0);
            // As the app does for a new account: its first Receive address.
            let receive: Value = serde_json::from_str(&wallet.address(0, None).unwrap()).unwrap();
            let connection: Value =
                serde_json::from_str(&wallet.connect(p.clone(), PROJECT.into(), 0).unwrap())
                    .unwrap();
            assert_eq!(connection["project"], PROJECT);
            let created: Value = serde_json::from_str(
                &wallet
                    .create_store(p, PROJECT.into(), s, "shop".into(), format!("mem:{SHEET}"))
                    .expect("create store"),
            )
            .unwrap();
            (wallet, created, receive)
        })
        .await;
        let storefront = created["url"].as_str().unwrap().to_string();
        let store: String = sqlx::query_scalar("SELECT id FROM stores WHERE slug='shop'")
            .fetch_one(&app.db)
            .await
            .unwrap();
        let tokens = memory.tab("Tokens");
        for (asset, rate) in rates {
            let row = tokens
                .rows
                .iter()
                .position(|r| r.get(2).is_some_and(|a| a == asset))
                .unwrap_or_else(|| panic!("{asset} on the Tokens tab: {:?}", tokens.rows));
            memory.set("Tokens", row, 4, rate);
        }
        if let Some(catalog) = catalog {
            memory.replace_rows("Catalog", catalog);
        }
        let merchant = Merchant {
            wallet,
            receive: receive["address"].as_str().unwrap().to_ascii_lowercase(),
            payroof: payroof.into(),
            storefront,
            store: stortop::stores::StoreId(store),
        };
        merchant.sync(app).await;
        merchant
    }
    pub async fn sync(&self, app: &stortop::App) {
        stortop::source::sync(app, &self.store).await.expect("sync");
    }
    /// Finds every address the project handed out (the Wallet adds them to account 0
    /// and derives each itself); returns path → address.
    pub async fn discover(&self) -> HashMap<String, String> {
        let (wallet, payroof) = (self.wallet.clone(), self.payroof.clone());
        blocking(move || {
            let page: Value = serde_json::from_str(
                &wallet
                    .sync(payroof, PROJECT.into(), 0, String::new(), "all".into())
                    .expect("sync paths"),
            )
            .unwrap();
            let projects: Value = serde_json::from_str(&wallet.public_json().unwrap()).unwrap();
            let account = projects["visible"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["slot"] == 0)
                .unwrap()
                .clone();
            let mut found = HashMap::new();
            for (path, address) in page["addresses"].as_object().unwrap() {
                assert!(
                    path.starts_with("m/44'/60'/0'/1/"),
                    "payment branch: {path}"
                );
                let derived: Value =
                    serde_json::from_str(&wallet.exact_path(path.clone()).unwrap()).unwrap();
                assert!(
                    derived["address"]
                        .as_str()
                        .unwrap()
                        .eq_ignore_ascii_case(address.as_str().unwrap()),
                    "the Wallet derives {path} itself"
                );
                assert!(
                    account["paths"].as_array().unwrap().contains(&json!(path)),
                    "{path} added to the account: {account}"
                );
                found.insert(path.clone(), address.as_str().unwrap().to_ascii_lowercase());
            }
            found
        })
        .await
    }
}

/// A Stor webhook as Payroof would sign it, for the tests that forge or replay one.
pub struct Hook {
    pub url: String,
    pub secret: [u8; 32],
    pub project: String,
}
impl Hook {
    pub async fn of(app: &stortop::App, stor: &str, store: &stortop::stores::StoreId) -> Hook {
        use sqlx::Row;
        let r = sqlx::query(
            "SELECT id,hook_id,project,webhook_ciphertext FROM connections
             WHERE store_id=? AND status='active'",
        )
        .bind(&store.0)
        .fetch_one(&app.db)
        .await
        .unwrap();
        let connection: String = r.get("id");
        let secret = stortop::crypto::open(
            &STOR_MASTER,
            stortop::payroof::client::aad(store, &connection, "webhook").as_bytes(),
            r.get("webhook_ciphertext"),
        )
        .unwrap();
        let secret = stortop::payroof::signature::decode_webhook_secret(
            std::str::from_utf8(&secret).unwrap(),
        )
        .unwrap();
        Hook {
            url: format!("{stor}/v1/webhooks/{}", r.get::<String, _>("hook_id")),
            secret,
            project: r.get("project"),
        }
    }
    /// Posts `body` with Payroof's headers; `timestamp` defaults to now.
    pub async fn post(
        &self,
        id: &str,
        body: &str,
        timestamp: Option<i64>,
        signature: Option<String>,
    ) -> u16 {
        let timestamp = timestamp.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
        });
        let signature = signature.unwrap_or_else(|| {
            stortop::payroof::signature::sign_webhook(&self.secret, timestamp, body.as_bytes())
        });
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(&self.url)
            .header("content-type", "application/json")
            .header("payroof-id", id)
            .header("payroof-timestamp", timestamp.to_string())
            .header("payroof-signature", signature)
            .body(body.to_string())
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }
}
