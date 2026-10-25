//! The local Anvil chain: JSON-RPC, faucet transfers and the ERC-20 test tokens.

use crate::ctx::{BUYER_SLOTS, CHAIN, Ctx, RPC};
use crate::units::{self, parse_units};
use crate::{http, sys, ui};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::io::Write;
use std::process::Command;

const LOCAL_TOKEN_SOL: &str = include_str!("../contracts/LocalToken.sol");

#[derive(Clone, Debug)]
pub struct Token {
    pub symbol: String,
    pub decimals: u32,
    pub address: String,
    pub name: String,
}

pub fn rpc(method: &str, params: Value) -> Result<Value> {
    http::rpc(RPC, method, params)
}

pub fn chain_ok() -> bool {
    rpc("eth_chainId", json!([])).is_ok_and(|v| v == "0x7a69")
}

pub fn require_chain() -> Result<()> {
    if !chain_ok() {
        bail!("local chain is not running at {RPC} (run roofer up)");
    }
    Ok(())
}

pub fn block_number() -> Result<String> {
    let hex = rpc("eth_blockNumber", json!([]))?;
    hex.as_str()
        .and_then(units::hex_to_decimal)
        .ok_or_else(|| anyhow!("bad block number {hex}"))
}

pub fn accounts() -> Result<Vec<String>> {
    let list = rpc("eth_accounts", json!([]))?;
    Ok(list
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a.as_str().map(str::to_string))
        .collect())
}

/// Anvil account 0: the unlocked faucet and deployer.
pub fn payer() -> Result<String> {
    accounts()?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("Anvil has no unlocked accounts"))
}

pub fn buyer_slots() -> Result<Vec<String>> {
    Ok(accounts()?.into_iter().skip(1).take(BUYER_SLOTS).collect())
}

pub fn evm_address(address: &str) -> Result<()> {
    let hex = address.strip_prefix("0x").unwrap_or("");
    if hex.len() != 40 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid address '{address}' (expected 0x + 40 hex chars)");
    }
    if hex.bytes().all(|b| b == b'0') {
        bail!("refusing the zero address");
    }
    Ok(())
}

fn cast() -> Command {
    let mut cmd = Command::new("cast");
    cmd.env("NO_PROXY", "*");
    cmd
}

/// `cast send --json` output -> transaction hash, failing unless the receipt succeeded.
fn receipt_ok(out: &str) -> Result<Value> {
    let receipt: Value = serde_json::from_str(out).unwrap_or(Value::Null);
    let status = receipt["status"].as_str().unwrap_or("");
    if status != "0x1" && status != "1" {
        bail!(
            "transaction failed: {}",
            out.chars().take(400).collect::<String>()
        );
    }
    Ok(receipt)
}

fn send(from: &str, args: &[&str]) -> Result<Value> {
    let out = sys::capture(
        cast()
            .args([
                "send",
                "--rpc-url",
                RPC,
                "--unlocked",
                "--from",
                from,
                "--json",
            ])
            .args(args),
    )?;
    receipt_ok(&out)
}

fn tx_hash(receipt: &Value) -> String {
    receipt["transactionHash"]
        .as_str()
        .unwrap_or("?")
        .to_string()
}

pub fn send_eth(from: &str, to: &str, units: &str) -> Result<String> {
    send(from, &[to, "--value", units]).map(|r| tx_hash(&r))
}

pub fn send_token(from: &str, token: &str, to: &str, units: &str) -> Result<String> {
    send(from, &[token, "transfer(address,uint256)", to, units]).map(|r| tx_hash(&r))
}

pub fn token_balance(token: &str, owner: &str) -> Result<String> {
    let out = sys::capture(cast().args([
        "call",
        "--rpc-url",
        RPC,
        token,
        "balanceOf(address)(uint256)",
        owner,
    ]))?;
    Ok(out.split_whitespace().next().unwrap_or("0").to_string())
}

pub fn eth_balance(owner: &str) -> Result<String> {
    let wei = rpc("eth_getBalance", json!([owner, "latest"]))?;
    let wei = wei
        .as_str()
        .and_then(units::hex_to_decimal)
        .ok_or_else(|| anyhow!("bad balance {wei}"))?;
    Ok(units::format_units(&wei, 18))
}

pub fn asset_id(address: &str) -> String {
    format!("{CHAIN}/erc20:{}", address.to_lowercase())
}

impl Ctx {
    pub fn token_list(&self) -> Vec<Token> {
        let text = std::fs::read_to_string(&self.tokens).unwrap_or_default();
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| {
                let mut f = l.split('\t');
                Some(Token {
                    symbol: f.next()?.to_string(),
                    decimals: f.next()?.trim().parse().ok()?,
                    address: f.next()?.to_string(),
                    name: f.next().unwrap_or("").to_string(),
                })
            })
            .collect()
    }

    pub fn token_lookup(&self, symbol: &str) -> Option<Token> {
        self.token_list()
            .into_iter()
            .find(|t| t.symbol.eq_ignore_ascii_case(symbol))
    }

    fn token_bytecode(&self) -> Result<String> {
        let contract = self.state.join("contracts/LocalToken.sol");
        std::fs::create_dir_all(contract.parent().unwrap())?;
        std::fs::write(&contract, LOCAL_TOKEN_SOL)?;
        let out = sys::try_capture(
            Command::new("solc")
                .args(["--evm-version", "paris", "--bin"])
                .arg(&contract),
        )
        .unwrap_or_default();
        let bin = out.lines().last().unwrap_or("").trim();
        if bin.is_empty() || !bin.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            bail!("solc failed to compile {}", contract.display());
        }
        Ok(format!("0x{bin}"))
    }

    pub fn deploy_token(
        &self,
        symbol: &str,
        decimals: &str,
        name: &str,
        supply: &str,
    ) -> Result<()> {
        let valid_symbol = (1..=11).contains(&symbol.len())
            && symbol.starts_with(|c: char| c.is_ascii_alphabetic())
            && symbol.chars().all(|c| c.is_ascii_alphanumeric());
        if !valid_symbol {
            bail!("symbol must be 1-11 letters/digits starting with a letter");
        }
        if symbol.eq_ignore_ascii_case("ETH") {
            bail!("ETH is the native asset");
        }
        if self.token_lookup(symbol).is_some() {
            bail!("token {symbol} already exists (roofer tokens)");
        }
        let dec: u32 = decimals
            .parse()
            .ok()
            .filter(|d| *d <= 36)
            .ok_or_else(|| anyhow!("decimals must be 0..36"))?;
        if supply.is_empty()
            || supply.starts_with('0')
            || !supply.bytes().all(|b| b.is_ascii_digit())
        {
            bail!("supply must be a positive whole number of tokens");
        }
        if name.is_empty() || name.chars().count() > 64 {
            bail!("name must be 1-64 characters");
        }
        if name.contains(['\t', '"', '\\', '\n']) {
            bail!("name must not contain tabs, quotes or backslashes");
        }
        require_chain()?;
        let units = parse_units(supply, dec)?;
        let from = payer()?;
        let code = self.token_bytecode()?;
        let receipt = send(
            &from,
            &[
                "--create",
                &code,
                "constructor(string,string,uint8,uint256)",
                name,
                symbol,
                &dec.to_string(),
                &units,
            ],
        )?;
        let address = receipt["contractAddress"]
            .as_str()
            .unwrap_or("")
            .to_lowercase();
        if address.len() != 42 || evm_address(&address).is_err() {
            bail!("deployment returned no contract address");
        }
        let reported =
            sys::capture(cast().args(["call", "--rpc-url", RPC, &address, "decimals()(uint8)"]))?;
        if reported != dec.to_string() {
            bail!("deployed token reports wrong decimals");
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.tokens)?;
        writeln!(file, "{symbol}\t{dec}\t{address}\t{name}").context("record token")?;
        // Keep the standard buyer slots spendable in the new token too.
        let gift = parse_units("10000", dec)?;
        for account in buyer_slots()? {
            send_token(&from, &address, &account, &gift)?;
        }
        ui::ok(format!(
            "deployed {symbol} ({dec} decimals, supply {supply}) at {address}"
        ));
        Ok(())
    }

    /// Tops buyer slots below 100 of a token back up with 10,000.
    pub fn fund_buyer_slots(&self) -> Result<()> {
        let from = payer()?;
        for account in buyer_slots()? {
            for token in self.token_list() {
                let min = parse_units("100", token.decimals)?;
                if units::cmp(&token_balance(&token.address, &account)?, &min).is_lt() {
                    send_token(
                        &from,
                        &token.address,
                        &account,
                        &parse_units("10000", token.decimals)?,
                    )?;
                }
            }
        }
        Ok(())
    }

    pub fn print_tokens(&self) {
        let tokens = self.token_list();
        if tokens.is_empty() {
            println!("  No ERC-20 tokens yet: roofer token add SYMBOL DECIMALS");
            return;
        }
        let row = |a: &str, b: &str, c: &str, d: &str| println!("  {a:<8} {b:<9} {c:<44} {d}");
        row(
            "SYMBOL",
            "DECIMALS",
            "CONTRACT (Wallet → Settings → Networks & tokens → Add token)",
            "NAME",
        );
        for t in &tokens {
            row(&t.symbol, &t.decimals.to_string(), &t.address, &t.name);
        }
        row("ETH", "18", "(native, preconfigured on Anvil)", "");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_addresses() {
        assert!(evm_address("0x70997970C51812dc3A010C7d01b50e0d17dc79C8").is_ok());
        assert!(evm_address("0x0000000000000000000000000000000000000000").is_err());
        assert!(evm_address("70997970C51812dc3A010C7d01b50e0d17dc79C8").is_err());
        assert!(evm_address("0x7099").is_err());
    }

    #[test]
    fn checks_receipts() {
        assert_eq!(
            tx_hash(&receipt_ok(r#"{"status":"0x1","transactionHash":"0xab"}"#).unwrap()),
            "0xab"
        );
        assert!(receipt_ok(r#"{"status":"0x0"}"#).is_err());
        assert!(receipt_ok("not json").is_err());
    }
}
