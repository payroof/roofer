//! Send's Max for a network's coin, on a local Anvil chain: the fee of each address's
//! transfer comes out of the same balance, so the most an account can send is less than
//! it holds. The Wallet core says exactly how much, Review accepts exactly that much,
//! and one wei more is refused with the same figure.
//!
//! Needs `anvil` on PATH.

mod common;

use common::*;
use payroof_wallet_core::ffi::Wallet;
use serde_json::{Value, json};

#[tokio::test(flavor = "multi_thread")]
async fn max_for_the_networks_coin_is_what_review_accepts() {
    let anvil = Anvil::start(31337).await;
    // SAFETY: set before the Wallet core reads the environment, by every test alike.
    unsafe { std::env::set_var("PAYROOF_WALLET_LOCAL_DEVELOPMENT", "1") };
    let (wallet, addresses) = blocking(|| {
        let wallet = Wallet::create(MERCHANT_MNEMONIC.into(), PIN.into()).unwrap();
        wallet.unlock(PIN.into()).unwrap();
        assert_eq!(wallet.add_project("Main".into(), Some(0)).unwrap(), 0);
        let addresses: Vec<String> = (0..2)
            .map(|_| {
                let a: Value = serde_json::from_str(&wallet.address(0, None).unwrap()).unwrap();
                a["address"].as_str().unwrap().to_ascii_lowercase()
            })
            .collect();
        (wallet, addresses)
    })
    .await;
    // The figure from the report: 0.041164 ETH shown, and a second, small address.
    let held = [41_164_000_000_000_000u128, 1_000_000_000_000_000];
    for (address, wei) in addresses.iter().zip(held) {
        anvil.set_balance(address, wei).await;
    }
    let total: u128 = held.iter().sum();
    // No new blocks meanwhile, so every quote below sees the same base fee.
    anvil.mining(false).await;
    let recipient = "0x3333333333333333333333333333333333333333";
    let request = |to: &str, amount: u128| {
        json!({"chain": anvil.chain, "asset": anvil.native(), "to": to,
               "amount": amount.to_string(), "slot": 0, "prefer": null})
        .to_string()
    };

    // Max needs no recipient yet.
    let (w, rpc, r) = (wallet.clone(), anvil.url.clone(), request("", 1));
    let maximum: Value =
        serde_json::from_str(&blocking(move || w.send_maximum(rpc, r, false).unwrap()).await)
            .unwrap();
    let maximum: u128 = maximum["maximum"].as_str().unwrap().parse().unwrap();
    assert!(0 < maximum && maximum < total, "{maximum} of {total}");

    // Review takes exactly that much, and what is left over is exactly its fees.
    let (w, rpc, r) = (
        wallet.clone(),
        anvil.url.clone(),
        request(recipient, maximum),
    );
    let plan: Value =
        serde_json::from_str(&blocking(move || w.plan_send(rpc, r, false).unwrap()).await).unwrap();
    let plan = plan.get("plan").unwrap_or_else(|| panic!("{plan}"));
    assert_eq!(plan["amount"], maximum.to_string());
    assert_eq!(
        plan["steps"].as_array().unwrap().len(),
        2,
        "both addresses send"
    );
    assert_eq!(
        plan["maximum_fee"]
            .as_str()
            .unwrap()
            .parse::<u128>()
            .unwrap(),
        total - maximum,
        "nothing left but the fees"
    );
    let w = wallet.clone();
    blocking(move || w.confirm_plan(false).unwrap()).await;

    // One wei more is refused, naming the same maximum.
    let (w, rpc, r) = (
        wallet.clone(),
        anvil.url.clone(),
        request(recipient, maximum + 1),
    );
    let refused: Value =
        serde_json::from_str(&blocking(move || w.plan_send(rpc, r, false).unwrap()).await).unwrap();
    assert_eq!(
        refused["problem"]["code"], "insufficient_funds",
        "{refused}"
    );
    assert_eq!(refused["problem"]["held"], maximum.to_string());
}
