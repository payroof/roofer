//! A busy store, end to end: two Anvil chains, four ERC-20 tokens plus the native coin,
//! the real Payroof binary, Stor and the merchant's Payroof Wallet.
//!
//! 1. Setup: tokens BNB, POL, USDC (EIP-2612 + EIP-3009), USDT (Tether-like) on chain A,
//!    USDC on chain B; Payroof watches all of them; the Wallet creates the store and a
//!    20-product catalog with mixed prices (token, USD, several tokens, both).
//! 2. Races: customers check out the last units of a product at the same instant.
//! 3. Payments by 10 customers: exact, in parts, overpaid, the wrong token, the wrong
//!    network, a cart split into payments, a four-product order.
//! 4. Deadlines: no payment, a late payment with stock, a late payment without stock, a
//!    partial payment, a payment still unconfirmed when the window ends.
//! 5. Webhooks: forged, stale, replayed and mismatched events.
//! 6. Books: stock counters, allocations, payments and History agree.
//! 7. Wallet: payment addresses found and derived, balances where customers paid, and
//!    sends from many addresses that hold the token but not the coin for gas.
//!
//! Correctness checks are collected, so one run reports every defect; the test fails at
//! the end if there is any. Needs `anvil`, `solc` and the Payroof release binary
//! (`roofer e2e`).

mod common;

use common::*;
use serde_json::{Value, json};
use sqlx::Row;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Defects found; reported together at the end.
#[derive(Default, Clone)]
struct Findings(Arc<Mutex<Vec<String>>>);
impl Findings {
    fn check(&self, ok: bool, what: impl Into<String>) {
        if !ok {
            let what = what.into();
            eprintln!("FINDING: {what}");
            self.0.lock().unwrap().push(what);
        }
    }
}

struct Tokens {
    usdc: Token,
    usdt: Token,
    bnb: Token,
    pol: Token,
    usdc_b: Token,
}

struct World {
    a: Anvil,
    b: Anvil,
    t: Tokens,
    stor: String,
    app: stortop::App,
    memory: Arc<Memory>,
    merchant: Merchant,
    found: Findings,
    customers: Mutex<HashMap<usize, Arc<Customer>>>,
    _payroof: Process,
}

impl World {
    /// Pays `amount` of `asset` to `to` from Anvil account `from` on its chain.
    async fn pay(&self, from: usize, asset: &str, to: &str, amount: u128) -> String {
        let t = &self.t;
        if asset == self.a.native() {
            self.a.pay_native(&self.a.accounts[from], to, amount).await
        } else if asset == self.b.native() {
            self.b.pay_native(&self.b.accounts[from], to, amount).await
        } else if asset == t.usdc_b.asset {
            self.b
                .pay_token(&t.usdc_b.address, &self.b.accounts[from], to, amount)
                .await
        } else {
            let token = [&t.usdc, &t.usdt, &t.bnb, &t.pol]
                .into_iter()
                .find(|k| k.asset == asset)
                .unwrap_or_else(|| panic!("unknown asset {asset}"));
            self.a
                .pay_token(&token.address, &self.a.accounts[from], to, amount)
                .await
        }
    }
    /// Customer `n`'s browser (one cookie, one client IP), the same one every time.
    async fn customer(&self, n: usize) -> Arc<Customer> {
        if let Some(c) = self.customers.lock().unwrap().get(&n) {
            return c.clone();
        }
        let c = Arc::new(
            Customer::visit(&self.merchant.storefront, &format!("10.0.0.{}", n + 1))
                .await
                .0,
        );
        self.customers.lock().unwrap().insert(n, c.clone());
        c
    }
    /// Ends an order's payment window now (instead of waiting 30 minutes); Stor's
    /// worker expires it within a second.
    async fn end_window(&self, order: &str) {
        sqlx::query("UPDATE orders SET expires_at=? WHERE id=?")
            .bind(stortop::time::now() - 1)
            .bind(order)
            .execute(&self.app.db)
            .await
            .unwrap();
    }
    async fn available(&self, sku: &str) -> i64 {
        sqlx::query_scalar("SELECT capacity-reserved-sold FROM products WHERE sku=?")
            .bind(sku)
            .fetch_one(&self.app.db)
            .await
            .unwrap()
    }
}

fn amount(order: &Value) -> u128 {
    order["amount"].as_str().unwrap().parse().unwrap()
}
fn paid(order: &Value) -> u128 {
    order["paid"].as_str().unwrap().parse().unwrap()
}
fn id(order: &Value) -> String {
    order["id"].as_str().unwrap().to_string()
}
fn address(order: &Value) -> String {
    order["address"].as_str().unwrap().to_string()
}

/// SKU, price, stock. Delivery is `code-<sku>`.
const CATALOG: &[(&str, &str, u32)] = &[
    ("tee", "20 USD", 5),
    ("mug", "12 USDC", 6),
    ("cap", "15 USDC\n15 USDT", 3),
    ("ebook", "0.002 ETH", 100),
    ("key", "9 USDT", 1),
    ("poster", "25 POL", 2),
    ("sticker", "0.01 BNB", 10),
    ("hoodie", "0.02 ETH, 45 USDC", 2),
    ("course", "99 USD", 1),
    ("ticket", "30 USDT, 0.05 BNB", 2),
    ("art", "150 POL, 80 USD", 1),
    ("pin", "1.5 USDC", 6),
    ("bag", "35 USDT", 3),
    ("game", "0.005 ETH\n10 USDC\n10 USDT", 4),
    ("font", "7.25 USDC", 50),
    ("vip", "500 USDT", 1),
    ("socks", "5 USD", 8),
    ("card", "0.000001 ETH", 10),
    ("lamp", "60 POL, 0.1 BNB", 2),
    ("book", "18 USDC, 0.006 ETH", 3),
];

async fn setup() -> World {
    let dir = temp_dir("marketplace");
    let (a, b) = tokio::join!(Anvil::start(31337), Anvil::start(31338));
    let contracts = Contracts::compile();
    let deployer = a.accounts[19].clone();
    let t = Tokens {
        usdc: contracts
            .deploy(&a, &deployer, "USDC", 6, Kind::Authorizing)
            .await,
        usdt: contracts
            .deploy(&a, &deployer, "USDT", 6, Kind::Tether)
            .await,
        bnb: contracts
            .deploy(&a, &deployer, "BNB", 18, Kind::Permitting)
            .await,
        pol: contracts
            .deploy(&a, &deployer, "POL", 18, Kind::Plain)
            .await,
        usdc_b: contracts
            .deploy(&b, &b.accounts[19], "USDC", 6, Kind::Authorizing)
            .await,
    };
    // Customers 0–9 hold every token.
    for buyer in 0..10 {
        for token in [&t.usdc, &t.usdt, &t.bnb, &t.pol] {
            a.pay_token(
                &token.address,
                &deployer,
                &a.accounts[buyer],
                token.units("100000"),
            )
            .await;
        }
        b.pay_token(
            &t.usdc_b.address,
            &b.accounts[19],
            &b.accounts[buyer],
            t.usdc_b.units("100000"),
        )
        .await;
    }
    let (payroof, process) = payroof(
        &[
            Watch {
                anvil: &a,
                name: "Anvil",
                tokens: vec![&t.usdc, &t.usdt, &t.bnb, &t.pol],
            },
            Watch {
                anvil: &b,
                name: "Anvil B",
                tokens: vec![&t.usdc_b],
            },
        ],
        &dir,
    )
    .await;
    let (stor, app, memory) = stor(&payroof).await;
    let catalog = CATALOG
        .iter()
        .map(|(sku, price, stock)| {
            vec![
                sku.to_string(),
                format!("The {sku}"),
                format!("A test {sku}"),
                price.to_string(),
                stock.to_string(),
                String::new(),
                format!("code-{sku}"),
            ]
        })
        .collect();
    let rates = [
        (a.native(), "3000"),
        (b.native(), "3000"),
        (t.usdc.asset.clone(), "1"),
        (t.usdc_b.asset.clone(), "1"),
        (t.usdt.asset.clone(), "1"),
        (t.bnb.asset.clone(), "600"),
        (t.pol.asset.clone(), "0.5"),
    ];
    let rates: Vec<(&str, &str)> = rates.iter().map(|(a, r)| (a.as_str(), *r)).collect();
    let merchant = Merchant::open(&payroof, &stor, &app, &memory, &rates, Some(catalog)).await;
    let status = &memory.tab("Payments").rows[3][1];
    assert!(status.starts_with("Live · 20 products"), "{status}");
    World {
        a,
        b,
        t,
        stor,
        app,
        memory,
        merchant,
        found: Findings::default(),
        customers: Mutex::default(),
        _payroof: process,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_busy_store_sells_exactly_its_stock_and_the_wallet_spends_what_it_received() {
    let w = setup().await;
    let catalog = w.customer(99).await.catalog().await;
    check_catalog(&w, &catalog);
    let orders = races(&w).await;
    let paid = payments(&w, orders).await;
    deadlines(&w).await;
    webhooks(&w, &paid).await;
    books(&w).await;
    wallet(&w, &paid).await;
    let found = w.found.0.lock().unwrap().clone();
    assert!(
        found.is_empty(),
        "{} findings:\n{}",
        found.len(),
        found.join("\n")
    );
}

/// Every price became exact options on every network the token is on.
fn check_catalog(w: &World, catalog: &Value) {
    let products = catalog["products"].as_array().unwrap();
    assert_eq!(products.len(), 20);
    let options = |sku: &str| -> HashMap<String, String> {
        products.iter().find(|p| p["sku"] == sku).unwrap()["payment_options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| {
                (
                    o["asset"].as_str().unwrap().to_string(),
                    o["amount"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    let t = &w.t;
    // 12 USDC: on both chains, exactly.
    let mug = options("mug");
    assert_eq!(mug.len(), 2, "{mug:?}");
    assert_eq!(mug[&t.usdc.asset], "12000000");
    assert_eq!(mug[&t.usdc_b.asset], "12000000");
    // $20 in every rated token: 0.00666667 ETH (rounded up), 40 POL, 20 USDT.
    let tee = options("tee");
    assert_eq!(tee.len(), 7, "{tee:?}");
    assert_eq!(tee[&w.a.native()], "6666670000000000");
    assert_eq!(tee[&t.pol.asset], "40000000000000000000");
    assert_eq!(tee[&t.usdt.asset], "20000000");
    // A token price and a USD price: POL stays 150, the rest is $80.
    let art = options("art");
    assert_eq!(art[&t.pol.asset], "150000000000000000000");
    assert_eq!(art[&t.bnb.asset], "133334000000000000");
    // The smallest ETH price stays exact.
    assert_eq!(options("card")[&w.a.native()], "1000000000000");
}

/// Orders the races left awaiting payment, by customer.
type Open = Vec<(usize, Value)>;

async fn races(w: &World) -> Open {
    let t = &w.t;
    let mut open: Open = Vec::new();

    // Five customers, one `key` left, checkouts at the same instant. Each bag also
    // holds socks.
    let mut racers = Vec::new();
    for n in 0..5 {
        let c = w.customer(n).await;
        c.add("key", 1).await.ok();
        c.add("socks", 1).await.ok();
        let proof = c.proof().await;
        racers.push((n, c, proof));
    }
    let mut set = tokio::task::JoinSet::new();
    for (n, c, proof) in racers {
        let asset = t.usdt.asset.clone();
        set.spawn(async move { (n, c.checkout_with(proof, &asset, &["key"]).await) });
    }
    let mut won = Vec::new();
    let mut lost = Vec::new();
    while let Some(r) = set.join_next().await {
        let (n, reply) = r.unwrap();
        if reply.status == 200 {
            won.push((n, reply.body));
        } else {
            w.found.check(
                reply.status == 409 && reply.error() == "inventory_unavailable",
                format!("race loser {n} got {} {}", reply.status, reply.body),
            );
            lost.push(n);
        }
    }
    assert_eq!(
        won.len(),
        1,
        "exactly one customer gets the last key: {won:?}"
    );
    assert_eq!(w.available("key").await, 0);
    let (winner, order) = won.pop().unwrap();
    assert_eq!(order["status"], "awaiting_payment");
    open.push((winner, order));
    // The key left the winner's bag with the order; the socks stayed.
    let bag = w.customer(winner).await.cart().await.ok();
    assert_eq!(bag["items"].as_array().unwrap().len(), 1, "{bag}");
    assert_eq!(bag["items"][0]["sku"], "socks");

    // A loser: the bag, with its sold-out key, stays readable, and the socks in it can
    // still be bought.
    let loser = lost[0];
    let c = w.customer(loser).await;
    let cart = c.cart().await;
    w.found.check(
        cart.status == 200,
        format!(
            "a bag with a sold-out line can't be read: GET /v1/cart {} {}",
            cart.status, cart.body
        ),
    );
    let socks = c.checkout(&t.usdt.asset, &["socks"]).await;
    w.found.check(
        socks.status == 200,
        format!(
            "a sold-out line blocks the checkout of the rest of the bag: {} {}",
            socks.status, socks.body
        ),
    );
    if socks.status == 200 {
        open.push((loser, socks.body));
    }
    // Removing the sold-out line always works and unblocks the bag.
    for n in 0..5 {
        let c = w.customer(n).await;
        c.remove("key").await.ok();
        c.remove("socks").await.ok();
        assert_eq!(c.cart().await.status, 200);
    }

    // `cap`: three left, five customers want 1, 1, 1, 2 and 2.
    let mut racers = Vec::new();
    for (n, q) in [(5usize, 1u32), (6, 1), (7, 1), (8, 2), (9, 2)] {
        let c = w.customer(n).await;
        c.add("cap", q).await.ok();
        let proof = c.proof().await;
        racers.push((n, q, c, proof));
    }
    let mut set = tokio::task::JoinSet::new();
    for (n, q, c, proof) in racers {
        let asset = t.usdt.asset.clone();
        set.spawn(async move { (n, q, c.checkout_with(proof, &asset, &["cap"]).await) });
    }
    let mut taken = 0;
    while let Some(r) = set.join_next().await {
        let (n, q, reply) = r.unwrap();
        if reply.status == 200 {
            taken += q;
            open.push((n, reply.body));
        } else {
            assert_eq!(reply.error(), "inventory_unavailable", "{}", reply.body);
            w.customer(n).await.remove("cap").await.ok();
        }
    }
    assert!((2..=3).contains(&taken), "cap reserved {taken} of 3");
    assert_eq!(w.available("cap").await, 3 - i64::from(taken));
    open
}

/// Orders that got paid: (customer, order view once settled).
type Paid = Vec<(usize, Value)>;

async fn payments(w: &World, open: Open) -> Paid {
    let t = &w.t;
    let eth = w.a.native();
    let mut paid: Paid = Vec::new();

    // The race winners pay exactly; cap buyers overpay by 1.5 USDT.
    for (n, order) in &open {
        let extra = if order["items"][0]["sku"] == "cap" {
            t.usdt.units("1.5")
        } else {
            0
        };
        w.pay(
            *n,
            order["asset"].as_str().unwrap(),
            &address(order),
            amount(order) + extra,
        )
        .await;
    }
    for (n, order) in &open {
        let c = w.customer(*n).await;
        let done = c.wait_status(&id(order), "delivered", 60).await;
        let extra = if order["items"][0]["sku"] == "cap" {
            t.usdt.units("1.5")
        } else {
            0
        };
        assert_eq!(paid_of(&done), amount(order) + extra, "{done}");
        assert_eq!(done["remaining"], "0");
        let sku = order["items"][0]["sku"].as_str().unwrap();
        let delivery = done["delivery"].as_array().unwrap();
        let quantity = order["items"][0]["quantity"].as_u64().unwrap() as usize;
        assert_eq!(delivery.len(), quantity, "{done}");
        assert!(delivery.iter().all(|d| d == &json!(format!("code-{sku}"))));
        paid.push((*n, done));
    }

    // Customer 0: an ebook for ETH, exactly.
    let c0 = w.customer(0).await;
    c0.add("ebook", 1).await.ok();
    let o = c0.checkout(&eth, &["ebook"]).await.ok();
    assert_eq!(o["amount"], "2000000000000000");
    w.pay(0, &eth, &address(&o), amount(&o)).await;
    paid.push((0, c0.wait_status(&id(&o), "delivered", 60).await));

    // Customer 2: two mugs and a pin in USDC, paid in two parts.
    let c2 = w.customer(2).await;
    c2.add("mug", 2).await.ok();
    c2.add("pin", 1).await.ok();
    let o = c2.checkout(&t.usdc.asset, &["mug", "pin"]).await.ok();
    assert_eq!(o["amount"], "25500000");
    w.pay(2, &t.usdc.asset, &address(&o), t.usdc.units("10"))
        .await;
    let partial = c2.wait_status(&id(&o), "partial", 60).await;
    assert_eq!(partial["remaining"], "15500000");
    w.pay(2, &t.usdc.asset, &address(&o), t.usdc.units("15.5"))
        .await;
    paid.push((2, c2.wait_status(&id(&o), "delivered", 60).await));

    // Customer 3: a mug in USDC on chain B, the same address on another network.
    let c3 = w.customer(3).await;
    c3.add("mug", 1).await.ok();
    let o = c3.checkout(&t.usdc_b.asset, &["mug"]).await.ok();
    assert_eq!(o["chain"], w.b.chain);
    w.pay(3, &t.usdc_b.asset, &address(&o), amount(&o)).await;
    paid.push((3, c3.wait_status(&id(&o), "delivered", 60).await));

    // Customer 4: a poster in POL, first paid in BNB by mistake (the same number of
    // units), then in POL. The BNB is not counted.
    let c4 = w.customer(4).await;
    c4.add("poster", 1).await.ok();
    let o = c4.checkout(&t.pol.asset, &["poster"]).await.ok();
    w.pay(4, &t.bnb.asset, &address(&o), amount(&o)).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let still = c4.order(&id(&o)).await;
    assert_eq!(still["status"], "awaiting_payment", "{still}");
    assert_eq!(still["paid"], "0");
    w.pay(4, &t.pol.asset, &address(&o), amount(&o)).await;
    paid.push((4, c4.wait_status(&id(&o), "delivered", 60).await));

    // Customer 5: a bag with no token in common (lamp: POL or BNB; font: USDC) is two
    // payments.
    let c5 = w.customer(5).await;
    c5.add("lamp", 1).await.ok();
    c5.add("font", 3).await.ok();
    let cart = c5.cart().await.ok();
    let groups = cart["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2, "{cart}");
    let lamp = c5.checkout(&t.bnb.asset, &["lamp"]).await.ok();
    let font = c5.checkout(&t.usdc.asset, &["font"]).await;
    // Three checkouts a minute per client: this is the customer's third.
    let font = font.ok();
    assert_eq!(font["amount"], "21750000");
    w.pay(5, &t.bnb.asset, &address(&lamp), amount(&lamp)).await;
    w.pay(5, &t.usdc.asset, &address(&font), amount(&font))
        .await;
    paid.push((5, c5.wait_status(&id(&lamp), "delivered", 60).await));
    paid.push((5, c5.wait_status(&id(&font), "delivered", 60).await));

    // Customer 6: four products in one order, paid in the one token they share.
    let c6 = w.customer(6).await;
    for (sku, q) in [("game", 2), ("book", 1), ("socks", 1), ("tee", 1)] {
        c6.add(sku, q).await.ok();
    }
    let cart = c6.cart().await.ok();
    let group = &cart["groups"][0];
    assert_eq!(cart["groups"].as_array().unwrap().len(), 1, "{cart}");
    let assets: Vec<&str> = group["payment_options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["asset"].as_str().unwrap())
        .collect();
    assert!(assets.contains(&t.usdc.asset.as_str()) && assets.contains(&eth.as_str()));
    let o = c6
        .checkout(&t.usdc.asset, &["book", "game", "socks", "tee"])
        .await
        .ok();
    // 2×10 + 18 + 5 + 20 USDC.
    assert_eq!(o["amount"], "63000000");
    w.pay(6, &t.usdc.asset, &address(&o), amount(&o)).await;
    let done = c6.wait_status(&id(&o), "delivered", 60).await;
    assert_eq!(done["delivery"].as_array().unwrap().len(), 5);
    paid.push((6, done));

    // Customer 7: a mug in USDC on chain A, paid on chain B first (not counted), then
    // on chain A.
    let c7 = w.customer(7).await;
    c7.add("mug", 1).await.ok();
    let o = c7.checkout(&t.usdc.asset, &["mug"]).await.ok();
    w.pay(7, &t.usdc_b.asset, &address(&o), amount(&o)).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(c7.order(&id(&o)).await["paid"], "0");
    w.pay(7, &t.usdc.asset, &address(&o), amount(&o)).await;
    paid.push((7, c7.wait_status(&id(&o), "delivered", 60).await));

    // Customer 8: a sticker in BNB in three transfers, sent at once (one block).
    let c8 = w.customer(8).await;
    c8.add("sticker", 1).await.ok();
    let o = c8.checkout(&t.bnb.asset, &["sticker"]).await.ok();
    let third = amount(&o) / 3;
    let (to, from) = (address(&o), w.a.accounts[8].clone());
    let mut hashes = Vec::new();
    for part in [third, third, amount(&o) - 2 * third] {
        hashes.push(w.a.submit_token(&t.bnb.address, &from, &to, part).await);
    }
    for hash in hashes {
        assert_eq!(w.a.receipt(&hash).await["status"], "0x1");
    }
    paid.push((8, c8.wait_status(&id(&o), "delivered", 60).await));

    // Customer 9: the tiniest price, one micro-ether.
    let c9 = w.customer(9).await;
    c9.add("card", 1).await.ok();
    let o = c9.checkout(&eth, &["card"]).await.ok();
    w.pay(9, &eth, &address(&o), amount(&o)).await;
    paid.push((9, c9.wait_status(&id(&o), "delivered", 60).await));
    paid
}

fn paid_of(order: &Value) -> u128 {
    paid(order)
}

async fn deadlines(w: &World) {
    let t = &w.t;
    let eth = w.a.native();

    // No payment: the window ends, the order expires, its stock is back.
    let c = w.customer(10).await;
    c.add("hoodie", 2).await.ok();
    let o = c.checkout(&eth, &["hoodie"]).await.ok();
    assert_eq!(w.available("hoodie").await, 0);
    w.end_window(&id(&o)).await;
    c.wait_status(&id(&o), "expired", 30).await;
    assert_eq!(w.available("hoodie").await, 2);

    // Paid in full after the window, the stock still there: delivered.
    let c = w.customer(11).await;
    c.add("art", 1).await.ok();
    let o = c.checkout(&t.pol.asset, &["art"]).await.ok();
    w.end_window(&id(&o)).await;
    c.wait_status(&id(&o), "expired", 30).await;
    assert_eq!(w.available("art").await, 1);
    w.pay(1, &t.pol.asset, &address(&o), amount(&o)).await;
    let late = c.wait_status(&id(&o), "delivered", 60).await;
    assert_eq!(late["delivery"][0], "code-art");
    assert_eq!(w.available("art").await, 0);

    // Paid in full after the window, but someone else bought the last one meanwhile:
    // `late_payment`, never sold twice.
    let slow = w.customer(12).await;
    slow.add("vip", 1).await.ok();
    let late = slow.checkout(&t.usdt.asset, &["vip"]).await.ok();
    w.end_window(&id(&late)).await;
    slow.wait_status(&id(&late), "expired", 30).await;
    let quick = w.customer(13).await;
    quick.add("vip", 1).await.ok();
    let first = quick.checkout(&t.usdt.asset, &["vip"]).await.ok();
    w.pay(3, &t.usdt.asset, &address(&first), amount(&first))
        .await;
    quick.wait_status(&id(&first), "delivered", 60).await;
    w.pay(2, &t.usdt.asset, &address(&late), amount(&late))
        .await;
    let refused = slow.wait_status(&id(&late), "late_payment", 60).await;
    assert!(refused.get("delivery").is_none(), "{refused}");
    let sold: i64 = sqlx::query_scalar("SELECT sold FROM products WHERE sku='vip'")
        .fetch_one(&w.app.db)
        .await
        .unwrap();
    assert_eq!(sold, 1, "vip sold once");

    // Part paid, then the window ends: manual review, the stock released; the rest
    // arrives later and the order is delivered (the stock is still there).
    let c = w.customer(14).await;
    c.add("bag", 1).await.ok();
    let o = c.checkout(&t.usdt.asset, &["bag"]).await.ok();
    w.pay(4, &t.usdt.asset, &address(&o), t.usdt.units("10"))
        .await;
    c.wait_status(&id(&o), "partial", 60).await;
    w.end_window(&id(&o)).await;
    c.wait_status(&id(&o), "manual_review", 30).await;
    assert_eq!(w.available("bag").await, 3);
    w.pay(4, &t.usdt.asset, &address(&o), t.usdt.units("25"))
        .await;
    c.wait_status(&id(&o), "delivered", 60).await;
    assert_eq!(w.available("bag").await, 2);

    // Paid in full, seen but not yet confirmed when the window ends (block production
    // paused): manual review; the confirmation then delivers it.
    let c = w.customer(15).await;
    c.add("ticket", 1).await.ok();
    let o = c.checkout(&t.usdt.asset, &["ticket"]).await.ok();
    w.a.mining(false).await;
    let data = format!("a9059cbb{}{}", pad(&address(&o)), word(amount(&o)));
    w.a.rpc(
        "eth_sendTransaction",
        json!([{"from": w.a.accounts[5], "to": t.usdt.address, "data": format!("0x{data}")}]),
    )
    .await;
    w.a.rpc("evm_mine", json!([])).await;
    c.wait_status(&id(&o), "seen", 30).await;
    w.end_window(&id(&o)).await;
    let review = c.wait_status(&id(&o), "manual_review", 30).await;
    w.found.check(
        review["remaining"] == "0",
        format!("a fully paid, unconfirmed order shows something remaining: {review}"),
    );
    w.a.mining(true).await;
    c.wait_status(&id(&o), "delivered", 60).await;
}

async fn webhooks(w: &World, settled: &Paid) {
    let t = &w.t;
    let hook = Hook::of(&w.app, &w.stor, &w.merchant.store).await;
    // An order waiting for payment, the target of the forgeries.
    let c = w.customer(16).await;
    c.add("pin", 1).await.ok();
    let o = c.checkout(&t.usdc.asset, &["pin"]).await.ok();
    let event = |id: &str, address: &str, asset: &str, amount: u128, project: &str| {
        json!({
            "id": id, "event": "confirmed", "transfer": format!("tr_{id}"),
            "project": project, "reference": o["id"], "chain": w.a.chain,
            "asset": asset, "address": address,
            "transaction": format!("0x{}", "ab".repeat(32)), "amount": amount.to_string(),
            "block": {"height": "1", "hash": format!("0x{}", "cd".repeat(32))}
        })
        .to_string()
    };
    let good = event(
        "evt_forged1",
        &address(&o),
        &t.usdc.asset,
        amount(&o),
        &hook.project,
    );
    let now = stortop::time::now();
    // Unsigned, wrongly signed, signed with another key, stale: all refused.
    assert_eq!(
        hook.post("evt_forged1", &good, None, Some("v1=00".into()))
            .await,
        401
    );
    assert_eq!(
        hook.post(
            "evt_forged1",
            &good,
            None,
            Some(format!("v1={}", "0".repeat(64)))
        )
        .await,
        401
    );
    let other = stortop::payroof::signature::sign_webhook(&[7; 32], now, good.as_bytes());
    assert_eq!(
        hook.post("evt_forged1", &good, Some(now), Some(other))
            .await,
        401
    );
    assert_eq!(
        hook.post("evt_forged1", &good, Some(now - 3600), None)
            .await,
        401
    );
    // The header ID must be the body's.
    assert_eq!(hook.post("evt_other", &good, None, None).await, 400);
    // Another project's event.
    let foreign = event(
        "evt_forged2",
        &address(&o),
        &t.usdc.asset,
        amount(&o),
        "elsewhere",
    );
    assert_eq!(hook.post("evt_forged2", &foreign, None, None).await, 400);
    // Signed, but to another address or in another token: recorded, not counted.
    let elsewhere = event(
        "evt_forged3",
        "0x1111111111111111111111111111111111111111",
        &t.usdc.asset,
        amount(&o),
        &hook.project,
    );
    assert_eq!(hook.post("evt_forged3", &elsewhere, None, None).await, 204);
    let token = event(
        "evt_forged4",
        &address(&o),
        &t.usdt.asset,
        amount(&o),
        &hook.project,
    );
    assert_eq!(hook.post("evt_forged4", &token, None, None).await, 204);
    let view = c.order(&id(&o)).await;
    assert_eq!(
        (view["status"].as_str(), view["paid"].as_str()),
        (Some("awaiting_payment"), Some("0"))
    );

    // A paid order's real event, replayed: accepted once, counted once.
    let (n, done) = &settled[0];
    let before = w.customer(*n).await.order(&id(done)).await;
    let half = event(
        "evt_replay",
        &address(&o),
        &t.usdc.asset,
        amount(&o) / 2,
        &hook.project,
    );
    assert_eq!(hook.post("evt_replay", &half, None, None).await, 204);
    assert_eq!(hook.post("evt_replay", &half, None, None).await, 204);
    let view = c.order(&id(&o)).await;
    assert_eq!(
        paid(&view),
        amount(&o) / 2,
        "a replay is counted once: {view}"
    );
    // The same event ID with another body is refused.
    let changed = event(
        "evt_replay",
        &address(&o),
        &t.usdc.asset,
        amount(&o),
        &hook.project,
    );
    assert_eq!(hook.post("evt_replay", &changed, None, None).await, 409);
    assert_eq!(paid(&c.order(&id(&o)).await), amount(&o) / 2);
    let after = w.customer(*n).await.order(&id(done)).await;
    assert_eq!(before["paid"], after["paid"]);
    w.end_window(&id(&o)).await;
    c.wait_status(&id(&o), "manual_review", 30).await;
}

/// Stock counters, allocations, payments and History agree.
async fn books(w: &World) {
    let db = &w.app.db;
    // Nothing left in a payment window by now.
    eventually("settled orders", 30, || async {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM orders WHERE status IN ('creating','awaiting_payment','partial','seen')",
        )
        .fetch_one(db)
        .await
        .unwrap();
        (n == 0).then_some(())
    })
    .await;
    for p in sqlx::query("SELECT sku,capacity,reserved,sold FROM products")
        .fetch_all(db)
        .await
        .unwrap()
    {
        let sku: String = p.get("sku");
        let sum = |state: &'static str| {
            let sku = sku.clone();
            async move {
                sqlx::query_scalar::<_, i64>(
                    "SELECT COALESCE(SUM(quantity),0) FROM allocations WHERE sku=? AND state=?",
                )
                .bind(sku)
                .bind(state)
                .fetch_one(db)
                .await
                .unwrap()
            }
        };
        let (reserved, sold) = (p.get::<i64, _>("reserved"), p.get::<i64, _>("sold"));
        assert_eq!(reserved, sum("reserved").await, "{sku} reserved");
        assert_eq!(sold, sum("sold").await, "{sku} sold");
        assert_eq!(reserved, 0, "{sku}: nothing held after every window ended");
        assert!(p.get::<i64, _>("capacity") >= sold, "{sku} oversold");
    }
    // Each order's allocations match its status.
    for o in sqlx::query("SELECT id,status FROM orders")
        .fetch_all(db)
        .await
        .unwrap()
    {
        let (order, status): (String, String) = (o.get("id"), o.get("status"));
        let states: Vec<String> =
            sqlx::query_scalar("SELECT DISTINCT state FROM allocations WHERE order_id=?")
                .bind(&order)
                .fetch_all(db)
                .await
                .unwrap();
        let expected = match status.as_str() {
            "delivered" => "sold",
            _ => "released",
        };
        assert_eq!(states, [expected], "{order} {status}");
        let confirmed: Vec<String> =
            sqlx::query_scalar("SELECT amount FROM payments WHERE order_id=? AND confirmed=1")
                .bind(&order)
                .fetch_all(db)
                .await
                .unwrap();
        let confirmed: u128 = confirmed.iter().map(|a| a.parse::<u128>().unwrap()).sum();
        let expected: String = sqlx::query_scalar("SELECT expected_amount FROM orders WHERE id=?")
            .bind(&order)
            .fetch_one(db)
            .await
            .unwrap();
        if matches!(status.as_str(), "delivered" | "late_payment") {
            assert!(
                confirmed >= expected.parse().unwrap(),
                "{order} paid in full"
            );
        }
    }
    // History: one row per line of every paid order, each once.
    let lines: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(json_array_length(items_json)),0) FROM orders
         WHERE status IN ('delivered','late_payment')",
    )
    .fetch_one(db)
    .await
    .unwrap();
    let history = eventually("History rows", 30, || async {
        let rows = w.memory.tab("History").rows;
        (rows.len() as i64 > lines).then_some(rows)
    })
    .await;
    assert_eq!(history.len() as i64 - 1, lines, "{history:?}");
    let mut seen = std::collections::HashSet::new();
    for row in &history[1..] {
        assert!(
            seen.insert((row[1].clone(), row[2].clone())),
            "duplicate {row:?}"
        );
    }
    assert!(
        history.iter().any(|r| r[8] == "late payment"),
        "a late payment is in History"
    );
    // History records what an order cost (15 USDT a cap); an overpayment is visible
    // only on-chain.
    for row in history.iter().filter(|r| r[2] == "cap") {
        let quantity: f64 = row[4].parse().unwrap();
        assert_eq!(row[5], (15.0 * quantity).to_string(), "{row:?}");
    }
}

async fn wallet(w: &World, paid: &Paid) {
    let t = &w.t;
    let m = &w.merchant;
    let found = m.discover().await;
    let paths: HashMap<String, String> =
        found.iter().map(|(p, a)| (a.clone(), p.clone())).collect();

    // The merchant's own Receive addresses are never a customer's payment address:
    // money the merchant moves there would count as that customer's payment. Not the
    // first one, and not a new one made now, just before the next order.
    w.found.check(
        !paths.contains_key(&m.receive),
        format!(
            "the Wallet's Receive address {} is also payment address {} of an order",
            m.receive,
            paths.get(&m.receive).cloned().unwrap_or_default()
        ),
    );
    let wallet = m.wallet.clone();
    let fresh: Value =
        blocking(move || serde_json::from_str(&wallet.address(0, None).unwrap()).unwrap()).await;
    let c = w.customer(17).await;
    c.add("font", 1).await.ok();
    let next = c.checkout(&t.usdc.asset, &["font"]).await.ok();
    w.found.check(
        !fresh["address"]
            .as_str()
            .unwrap()
            .eq_ignore_ascii_case(&address(&next)),
        format!("a new Receive address {fresh} is the next order's payment address"),
    );
    // Money the merchant receives on a Receive address pays no order.
    w.pay(
        0,
        &t.usdc.asset,
        fresh["address"].as_str().unwrap(),
        amount(&next),
    )
    .await;
    w.pay(0, &t.usdc.asset, &m.receive, amount(&next)).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(c.order(&id(&next)).await["status"], "awaiting_payment");
    w.end_window(&id(&next)).await;
    c.wait_status(&id(&next), "expired", 30).await;

    // Each paid order's address holds what its customer paid.
    for (_, order) in paid {
        let address = address(order);
        let path = paths
            .get(&address)
            .unwrap_or_else(|| panic!("{address} found by the Wallet"))
            .clone();
        let (wallet, chain, asset) = (
            m.wallet.clone(),
            order["chain"].as_str().unwrap().to_string(),
            order["asset"].as_str().unwrap().to_string(),
        );
        let rpc = if chain == w.a.chain {
            w.a.url.clone()
        } else {
            w.b.url.clone()
        };
        let balances = blocking(move || {
            wallet
                .query_address_balances(rpc, chain, asset, vec![path])
                .unwrap()
        })
        .await;
        assert_eq!(balances[0], order["paid"].as_str().unwrap(), "{order}");
    }

    // Sweep all USDT to an outside address. No payment address that holds USDT has
    // ETH for gas; the addresses paid in ETH do and pay for the rest.
    let usdt_total: u128 = {
        let mut total = 0;
        for address in found.values() {
            total += w.a.token_balance(&t.usdt.address, address).await;
        }
        total
    };
    let treasury = "0x2222222222222222222222222222222222222222";
    let plan = plan_send(w, 0, &t.usdt.asset, treasury, usdt_total, None).await;
    let plan = plan.get("plan").unwrap_or_else(|| panic!("{plan}")).clone();
    let kinds: Vec<&str> = plan["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"top_up"),
        "USDT has neither permit nor authorization: {kinds:?}"
    );
    execute(w, &w.a, &plan).await;
    assert_eq!(
        w.a.token_balance(&t.usdt.address, treasury).await,
        usdt_total
    );

    treasury_sends(w).await;
}

async fn plan_send(
    w: &World,
    slot: u32,
    asset: &str,
    to: &str,
    amount: u128,
    prefer: Option<&str>,
) -> Value {
    let (wallet, rpc) = (w.merchant.wallet.clone(), w.a.url.clone());
    let request = json!({"chain": w.a.chain, "asset": asset, "to": to,
                         "amount": amount.to_string(), "slot": slot, "prefer": prefer})
    .to_string();
    blocking(move || {
        serde_json::from_str(&wallet.plan_send(rpc, request, false).expect("plan")).unwrap()
    })
    .await
}

/// Confirms the reviewed plan and runs its steps in order, each once mined.
async fn execute(w: &World, anvil: &Anvil, plan: &Value) {
    let wallet = w.merchant.wallet.clone();
    blocking(move || wallet.confirm_plan(true).unwrap()).await;
    for index in 0..plan["steps"].as_array().unwrap().len() {
        let wallet = w.merchant.wallet.clone();
        let hash = blocking(move || wallet.execute_step(index as u32))
            .await
            .unwrap_or_else(|e| panic!("step {index}: {e:?} in {plan}"));
        let receipt = anvil.receipt(&hash).await;
        assert_eq!(receipt["status"], "0x1", "step {index}: {receipt}");
    }
}

/// The scenario from the brief: 9 tokens from addresses holding 1.3, 3.5, 5.8, 2.1 and
/// 6.4, only the last with 0.1 ETH, for each way a token lets gas be paid.
async fn treasury_sends(w: &World) {
    let t = &w.t;
    let m = &w.merchant;
    let funder = w.a.accounts[19].clone();
    let recipient = "0x3333333333333333333333333333333333333333";
    for (slot, token) in [(1u32, &t.usdt), (2, &t.usdc), (3, &t.bnb), (4, &t.pol)] {
        let wallet = m.wallet.clone();
        let addresses = blocking(move || {
            assert_eq!(
                wallet
                    .add_project(format!("Treasury {slot}"), Some(slot))
                    .unwrap(),
                slot
            );
            (0..5)
                .map(|_| {
                    let a: Value =
                        serde_json::from_str(&wallet.address(slot, None).unwrap()).unwrap();
                    (
                        a["path"].as_str().unwrap().to_string(),
                        a["address"].as_str().unwrap().to_ascii_lowercase(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .await;
        let holdings = ["1.3", "3.5", "5.8", "2.1", "6.4"];
        for ((_, address), amount) in addresses.iter().zip(holdings) {
            w.a.pay_token(&token.address, &funder, address, token.units(amount))
                .await;
        }
        w.a.set_balance(&addresses[4].1, ETHER / 10).await;

        // Too much: the problem says how much there is.
        let problem = plan_send(w, slot, &token.asset, recipient, token.units("19.2"), None).await;
        assert_eq!(
            problem["problem"]["code"], "insufficient_funds",
            "{problem}"
        );
        assert_eq!(problem["problem"]["held"], token.units("19.1").to_string());

        // Preferring the 1.3 address puts it first.
        let preferred = plan_send(
            w,
            slot,
            &token.asset,
            recipient,
            token.units("9"),
            Some(&addresses[0].0),
        )
        .await;
        let sources: Vec<String> = preferred["plan"]["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["amount"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(sources[0], token.units("1.3").to_string(), "{preferred}");

        // The plan: the fewest sources (6.4, then 2.6 of the 5.8); the 6.4 address pays
        // its own gas and sponsors the other.
        let plan = plan_send(w, slot, &token.asset, recipient, token.units("9"), None).await;
        let plan = plan.get("plan").unwrap_or_else(|| panic!("{plan}")).clone();
        let sources: Vec<(String, String)> = plan["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                (
                    s["address"].as_str().unwrap().to_string(),
                    s["amount"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(
            sources,
            [
                (addresses[4].1.clone(), token.units("6.4").to_string()),
                (addresses[2].1.clone(), token.units("2.6").to_string()),
            ],
            "{} {plan}",
            token.symbol
        );
        assert_eq!(plan["sponsor"]["address"], addresses[4].1);
        let kinds: Vec<&str> = plan["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["kind"].as_str().unwrap())
            .collect();
        let expected: &[&str] = match token.symbol {
            "USDC" => &["transfer", "authorization"],
            "BNB" => &["transfer", "permit", "transfer_from"],
            _ => &["transfer", "top_up", "transfer"],
        };
        assert_eq!(kinds, expected, "{} {plan}", token.symbol);
        let before = w.a.token_balance(&token.address, recipient).await;
        let eth_before = w.a.balance(&addresses[4].1).await;
        execute(w, &w.a, &plan).await;
        assert_eq!(
            w.a.token_balance(&token.address, recipient).await - before,
            token.units("9")
        );
        assert_eq!(
            w.a.token_balance(&token.address, &addresses[2].1).await,
            token.units("3.2")
        );
        assert_eq!(w.a.token_balance(&token.address, &addresses[4].1).await, 0);
        let spent = eth_before - w.a.balance(&addresses[4].1).await;
        let ceiling: u128 = plan["maximum_fee"].as_str().unwrap().parse().unwrap();
        assert!(
            spent <= ceiling,
            "{} gas {spent} above the reviewed {ceiling}",
            token.symbol
        );

        // Nobody can pay gas any more: the problem says who has the most coin.
        w.a.set_balance(&addresses[4].1, 0).await;
        for (_, a) in &addresses {
            w.a.set_balance(a, 0).await;
        }
        let stuck = plan_send(w, slot, &token.asset, recipient, token.units("3"), None).await;
        let code = stuck["problem"]["code"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(code, "no_sponsor", "{}: {stuck}", token.symbol);
    }
}
