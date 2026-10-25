//! A store made on Stor's own page, connected to the merchant's project afterwards:
//!
//! 1. The store is created as Stor's page does it: a sheet and an address, no payments.
//!    The Payments tab waits for a URL and KEY.
//! 2. The Wallet can't create that store again (the address and the sheet are taken);
//!    it connects it instead: a key that can only create payment addresses, and the
//!    project's URL, for the merchant to paste into the Payments tab.
//! 3. Pasted in, the next sync connects the store to Payroof and lists its tokens.
//!
//! Needs `anvil` on PATH and the Payroof binary, as `purchase.rs`.

mod common;

use common::*;
use payroof_wallet_core::ffi::Wallet;
use serde_json::Value;

#[tokio::test(flavor = "multi_thread")]
async fn a_store_made_on_stors_page_is_connected_with_a_key_from_the_wallet() {
    let dir = temp_dir("connect");
    let anvil = Anvil::start(31337).await;
    let watch = Watch {
        anvil: &anvil,
        name: "Anvil",
        tokens: vec![],
    };
    let (payroof, _payroof) = payroof(&[watch], &dir).await;
    let (stor, app, memory) = stor(&payroof).await;

    // 1. Stor's page: the store and its tabs, nothing about payments.
    stortop::stores::create(&app, "shop", &format!("mem:{SHEET}"), None)
        .await
        .expect("create on Stor's page");
    let store = stortop::stores::StoreId(
        sqlx::query_scalar("SELECT id FROM stores WHERE slug='shop'")
            .fetch_one(&app.db)
            .await
            .unwrap(),
    );
    stortop::source::sync(&app, &store).await.expect("sync");
    let payments = memory.tab("Payments");
    assert_eq!(
        (payments.rows[1][1].as_str(), payments.rows[2][1].as_str()),
        ("", "")
    );
    assert!(
        payments.rows[3][1].starts_with("Add the URL and KEY"),
        "{:?}",
        payments.rows[3]
    );

    // 2. The Wallet's project; New store is refused, Connect store gives URL and KEY.
    // SAFETY: set before the Wallet core reads the environment, by every test alike.
    unsafe { std::env::set_var("PAYROOF_WALLET_LOCAL_DEVELOPMENT", "1") };
    let (p, s) = (payroof.clone(), stor.clone());
    let (refused, connected, grants) = blocking(move || {
        let wallet = Wallet::create(MERCHANT_MNEMONIC.into(), PIN.into()).expect("wallet");
        wallet.unlock(PIN.into()).expect("unlock");
        assert_eq!(wallet.add_project("Main".into(), Some(0)).unwrap(), 0);
        wallet
            .connect(p.clone(), PROJECT.into(), 0)
            .expect("connect");
        let refused: Value = serde_json::from_str(
            &wallet
                .create_store(
                    p.clone(),
                    PROJECT.into(),
                    s,
                    "shop".into(),
                    format!("mem:{SHEET}"),
                )
                .expect("create store"),
        )
        .unwrap();
        assert!(
            wallet
                .create_store_key(p.clone(), PROJECT.into(), "Not A Store".into())
                .is_err()
        );
        let connected: Value = serde_json::from_str(
            &wallet
                .create_store_key(p.clone(), PROJECT.into(), "shop".into())
                .expect("store key"),
        )
        .unwrap();
        let grants: Value =
            serde_json::from_str(&wallet.grants(p, PROJECT.into()).unwrap()).unwrap();
        (refused, connected, grants)
    })
    .await;
    assert_eq!(refused["error"], "subdomain_taken");
    let url = connected["url"].as_str().unwrap();
    let key = connected["key"].as_str().unwrap();
    assert_eq!(url, format!("{payroof}/v1/projects/{PROJECT}"));
    assert!(key.starts_with("prf_"), "a Payroof key");
    // The refused store took its key back; the connected one has exactly a store's.
    let named: Vec<&Value> = grants
        .as_array()
        .unwrap()
        .iter()
        .filter(|g| g["name"] == "Stor shop")
        .collect();
    assert_eq!(named.len(), 1, "{grants}");
    assert_eq!(
        named[0]["permissions"],
        serde_json::json!(["references", "paths", "webhooks"])
    );

    // 3. Pasted into the Payments tab: Stor connects and lists the project's tokens.
    memory.set("Payments", 1, 1, url);
    memory.set("Payments", 2, 1, key);
    stortop::source::sync(&app, &store).await.expect("sync");
    assert_eq!(
        memory.tab("Payments").rows[3][1],
        "Tokens added; prices follow in a moment"
    );
    assert!(
        memory
            .tab("Tokens")
            .rows
            .iter()
            .any(|r| r.get(2).is_some_and(|a| *a == anvil.native())),
        "{:?}",
        memory.tab("Tokens").rows
    );
    // A USD rate typed in, as the merchant would where CoinGecko has none (Anvil ETH).
    let row = memory
        .tab("Tokens")
        .rows
        .iter()
        .position(|r| r.get(2).is_some_and(|a| *a == anvil.native()))
        .unwrap();
    memory.set("Tokens", row, 4, "3000");
    stortop::source::sync(&app, &store).await.expect("sync");
    let status = &memory.tab("Payments").rows[3][1];
    assert!(status.starts_with("Live · 32 products"), "{status}");
}
