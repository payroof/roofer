//! Faucet commands on the local chain: fund, send, balance, accounts.

use crate::chain::{self, evm_address, require_chain};
use crate::ctx::{ANVIL_MNEMONIC, BUYER_SLOTS, Ctx};
use crate::units::{format_units, parse_units};
use crate::{sys, ui};
use anyhow::{Result, anyhow};

fn is_zero(amount: &str) -> bool {
    amount == "0"
}

impl Ctx {
    pub fn fund(&self, to: &str, eth: &str, tokens: &str) -> Result<()> {
        sys::need(&["cast"])?;
        evm_address(to)?;
        require_chain()?;
        let from = chain::payer()?;
        if !is_zero(eth) {
            chain::send_eth(&from, to, &parse_units(eth, 18)?)?;
            ui::ok(format!("{eth} ETH → {to}"));
        }
        if !is_zero(tokens) {
            for t in self.token_list() {
                chain::send_token(&from, &t.address, to, &parse_units(tokens, t.decimals)?)?;
                ui::ok(format!("{tokens} {} → {to}", t.symbol));
            }
        }
        Ok(())
    }

    pub fn send(&self, asset: &str, amount: &str, to: &str) -> Result<()> {
        sys::need(&["cast"])?;
        evm_address(to)?;
        require_chain()?;
        let from = chain::payer()?;
        let (symbol, tx) = if asset.eq_ignore_ascii_case("ETH") {
            (
                "ETH".to_string(),
                chain::send_eth(&from, to, &parse_units(amount, 18)?)?,
            )
        } else {
            let t = self
                .token_lookup(asset)
                .ok_or_else(|| anyhow!("unknown token '{asset}' (roofer tokens)"))?;
            let tx = chain::send_token(&from, &t.address, to, &parse_units(amount, t.decimals)?)?;
            (t.symbol.to_uppercase(), tx)
        };
        ui::ok(format!("{amount} {symbol} → {to}  tx {tx}"));
        Ok(())
    }

    pub fn balance(&self, owner: &str) -> Result<()> {
        sys::need(&["cast"])?;
        evm_address(owner)?;
        require_chain()?;
        println!("  {:<8} {}", "ETH", chain::eth_balance(owner)?);
        for t in self.token_list() {
            println!(
                "  {:<8} {}",
                t.symbol,
                format_units(&chain::token_balance(&t.address, owner)?, t.decimals)
            );
        }
        Ok(())
    }

    pub fn accounts(&self) -> Result<()> {
        require_chain()?;
        println!("  Standard Anvil dev accounts — PUBLIC test keys, local chain only.");
        println!("  Mnemonic: {ANVIL_MNEMONIC}");
        println!(
            "  0 = faucet/deployer used by roofer; 1-{BUYER_SLOTS} are kept funded with every token."
        );
        for (i, a) in chain::accounts()?.iter().enumerate() {
            println!("  {i:>2}  {a}  {} ETH", chain::eth_balance(a)?);
        }
        Ok(())
    }
}
