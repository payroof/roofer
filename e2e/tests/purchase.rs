//! Payroof, Stor and Payroof Wallet as one system, on a local Anvil chain:
//!
//! 1. The Wallet registers a project on a real `payroof` server (its release binary).
//! 2. The Wallet creates the project's store on Stor, which prepares the store's sheet
//!    (an in-memory `Source`: Google Sheets are not reachable from a test).
//! 3. Stor syncs the sheet, connects to Payroof and registers its webhook.
//! 4. A customer checks out on the storefront and pays the order's address on Anvil.
//! 5. Payroof sees and confirms the transfer and signs a webhook; Stor delivers the order
//!    and appends it to History.
//! 6. The Wallet finds the payment address in the project's paths and derives the same
//!    address itself: the money is the merchant's.
//!
//! Needs `anvil` on PATH and the Payroof binary (`PAYROOF_BIN`, default
//! `payroof/target/release/payroof` in the workspace); `roofer e2e` builds it and runs this test.

mod common;

use common::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_purchase_on_a_stor_storefront_pays_the_merchants_own_address() {
    let dir = temp_dir("purchase");
    let anvil = Anvil::start(31337).await;
    let watch = Watch {
        anvil: &anvil,
        name: "Anvil",
        tokens: vec![],
    };
    let (payroof, _payroof) = payroof(&[watch], &dir).await;
    let (stor, app, memory) = stor(&payroof).await;

    // 1–3. The merchant's Wallet: a project on Payroof, its store on Stor with Stor's
    // sample catalog, and a USD rate for Anvil ETH.
    let eth = anvil.native();
    let merchant = Merchant::open(&payroof, &stor, &app, &memory, &[(&eth, "3000")], None).await;
    let origin = stor.clone();
    let info: serde_json::Value = blocking(move || {
        serde_json::from_str(&payroof_wallet_core::ffi::stor_info(origin).unwrap()).unwrap()
    })
    .await;
    assert_eq!(info["share_with"], "stor@example.iam.gserviceaccount.com");
    assert_eq!(
        info["store_url"]
            .as_str()
            .unwrap()
            .replace("{store}", "shop"),
        merchant.storefront
    );
    assert_eq!(
        memory.tab("Payments").rows[1][1],
        format!("{payroof}/v1/projects/{PROJECT}")
    );
    let status = &memory.tab("Payments").rows[3][1];
    assert!(status.starts_with("Live · 32 products"), "{status}");

    // 4. A customer buys the first product payable in Anvil ETH.
    let (customer, catalog) = Customer::visit(&merchant.storefront, "10.0.0.1").await;
    let product = catalog["products"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| {
            p["payment_options"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["asset"] == eth.as_str())
        })
        .expect("a product payable in Anvil ETH");
    let sku = product["sku"].as_str().unwrap();
    customer.add(sku, 1).await.ok();
    let order = customer.checkout(&eth, &[sku]).await.ok();
    assert_eq!(order["status"], "awaiting_payment", "{order}");
    assert_eq!(order["chain"], anvil.chain);
    let address = order["address"].as_str().unwrap().to_string();
    let amount: u128 = order["amount"].as_str().unwrap().parse().unwrap();
    assert!(amount > 0);
    anvil.pay_native(&anvil.accounts[9], &address, amount).await;

    // 5. Payroof confirms it and Stor delivers; History gets the sale.
    let delivered = customer
        .wait_status(order["id"].as_str().unwrap(), "delivered", 90)
        .await;
    assert_eq!(delivered["paid"], amount.to_string());
    assert!(delivered["transaction"].as_str().unwrap().starts_with("0x"));
    assert!(
        !delivered["delivery"].as_array().unwrap().is_empty(),
        "{delivered}"
    );
    let history = eventually("History row", 30, || async {
        let history = memory.tab("History");
        (history.rows.len() > 1).then_some(history)
    })
    .await;
    assert!(
        history.rows[1].contains(&sku.to_string()),
        "{:?}",
        history.rows
    );

    // 6. The merchant's Wallet finds the address and derives it on its own; it is not
    // one of the account's own Receive addresses.
    let found = merchant.discover().await;
    assert!(
        found.values().any(|a| a.eq_ignore_ascii_case(&address)),
        "{found:?}"
    );
    assert!(!found.values().any(|a| *a == merchant.receive));
    let _ = std::fs::remove_dir_all(&dir);
}
