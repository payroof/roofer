//! The workspace (payroof, stor, wallet, roofer side by side) and the local stack's addresses.

use crate::sys;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const CHAIN_ID: u64 = 31337;
pub const CHAIN: &str = "eip155:31337";
pub const NATIVE_ASSET: &str = "eip155:31337/slip44:60";
/// roofer's own calls (loopback).
pub const RPC: &str = "http://127.0.0.1:8545";
pub const PAYROOF: &str = "http://127.0.0.1:8080";
/// Stor health checks and webhooks from this Mac.
pub const STOR_LOCAL: &str = "http://127.0.0.1:8081";
pub const LOCAL_SUFFIX: &str = "stor.localhost:8081";
/// Standard Anvil accounts 1..=3 are kept funded for buyers that import the public
/// Anvil test mnemonic. Account 0 is the unlocked local faucet/deployer.
pub const BUYER_SLOTS: usize = 3;
pub const ANVIL_MNEMONIC: &str = "test test test test test test test test test test test junk";

fn is_workspace(dir: &Path) -> bool {
    ["payroof", "stor", "wallet"]
        .iter()
        .all(|p| dir.join(p).join("Cargo.toml").is_file())
}

/// `ROOFER_ROOT`, else the closest ancestor of the current directory holding the
/// projects, else the workspace this binary was built from.
pub fn find_root() -> Result<PathBuf> {
    if let Some(root) = std::env::var_os("ROOFER_ROOT") {
        let root = PathBuf::from(root);
        if !is_workspace(&root) {
            bail!(
                "ROOFER_ROOT={} does not contain payroof/, stor/ and wallet/",
                root.display()
            );
        }
        return Ok(root.canonicalize()?);
    }
    let cwd = std::env::current_dir()?;
    if let Some(dir) = cwd.ancestors().find(|d| is_workspace(d)) {
        return Ok(dir.to_path_buf());
    }
    if let Some(dir) = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .filter(|d| is_workspace(d))
    {
        return Ok(dir.to_path_buf());
    }
    bail!(
        "not inside the Payroof workspace (payroof/, stor/, wallet/): cd into it or set ROOFER_ROOT"
    )
}

pub struct Ctx {
    pub root: PathBuf,
    pub state: PathBuf,
    pub logs: PathBuf,
    /// Unsigned (unsandboxed) local Wallet data.
    pub wallet_dir: PathBuf,
    /// A signed local Wallet is sandboxed like a release: its data lives in its own container.
    pub wallet_container_dir: PathBuf,
    pub wallet_app: PathBuf,
    pub wallet_bin: PathBuf,
    pub payroof_bin: PathBuf,
    pub stor_bin: PathBuf,
    /// Payroof's real networks; the local Payroof watches them next to Anvil.
    pub real_networks: PathBuf,
    /// SYMBOL<TAB>DECIMALS<TAB>ADDRESS<TAB>NAME
    pub tokens: PathBuf,
    pub bind_host: String,
    pub lan_ip: Option<String>,
    pub rpc_public: String,
    pub payroof_public: String,
    pub stor: String,
    pub store_suffix: String,
    pub default_tokens: String,
}

/// The Mac's address on the local network (Wi-Fi or Ethernet, en*), not a VPN tunnel:
/// the default route's interface when it is one, else the first en* with an IPv4 address.
fn lan_ip() -> Option<String> {
    let default =
        sys::try_capture(Command::new("route").args(["-n", "get", "default"])).and_then(|out| {
            out.lines().find_map(|l| {
                l.trim()
                    .strip_prefix("interface:")
                    .map(|i| i.trim().to_string())
            })
        });
    let all = sys::try_capture(Command::new("ifconfig").arg("-l")).unwrap_or_default();
    default
        .into_iter()
        .chain(all.split_whitespace().map(str::to_string))
        .filter(|i| i.starts_with("en"))
        .find_map(|i| {
            sys::try_capture(Command::new("ipconfig").args(["getifaddr", &i]))
                .filter(|ip| !ip.is_empty())
        })
}

pub fn is_ipv4(ip: &str) -> bool {
    let parts: Vec<&str> = ip.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| (1..=3).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
}

pub fn is_private_ipv4(ip: &str) -> bool {
    let octets: Vec<u32> = ip.split('.').filter_map(|p| p.parse().ok()).collect();
    is_ipv4(ip)
        && match octets[..] {
            [10, ..] => true,
            [192, 168, ..] => true,
            [172, b, ..] => (16..=31).contains(&b),
            _ => false,
        }
}

impl Ctx {
    pub fn new(root: PathBuf) -> Result<Self> {
        let state = root.join(".local/manual");
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        // Anvil, Payroof and Stor listen on every interface so devices on the same network
        // can reach them; PAYROOF_BIND_HOST=127.0.0.1 keeps everything on this Mac.
        let bind_host = std::env::var("PAYROOF_BIND_HOST").unwrap_or_else(|_| "0.0.0.0".into());
        let lan_ip = if bind_host == "127.0.0.1" {
            None
        } else {
            std::env::var("PAYROOF_LAN_IP")
                .ok()
                .filter(|ip| !ip.is_empty())
                .or_else(lan_ip)
        };
        if let Some(ip) = &lan_ip
            && !is_ipv4(ip)
        {
            bail!("PAYROOF_LAN_IP must be an IPv4 address");
        }
        let (rpc_public, payroof_public, stor) = match &lan_ip {
            Some(ip) => (
                format!("http://{ip}:8545"),
                format!("http://{ip}:8080"),
                format!("http://{ip}:8081"),
            ),
            None => (RPC.into(), PAYROOF.into(), STOR_LOCAL.into()),
        };
        // Storefronts are <store>.stor.localhost, which this Mac resolves without any DNS.
        // No public wildcard DNS (it fails behind routers that drop private answers).
        let store_suffix = LOCAL_SUFFIX.to_string();
        let wallet_app =
            root.join("wallet/target/apple-macosx-local/Build/Products/Debug/PayroofWallet.app");
        Ok(Ctx {
            logs: state.join("logs"),
            wallet_dir: state.join("wallet"),
            wallet_container_dir: home.join(
                "Library/Containers/top.payroof.wallet.local/Data/Library/Application Support/PayroofWallet-Local",
            ),
            wallet_bin: wallet_app.join("Contents/MacOS/PayroofWallet"),
            wallet_app,
            payroof_bin: root.join("payroof/target/release/payroof"),
            stor_bin: root.join("stor/target/release/stortop"),
            real_networks: root.join("payroof/config/payroof.example.toml"),
            tokens: state.join("tokens.tsv"),
            state,
            root,
            bind_host,
            lan_ip,
            rpc_public,
            payroof_public,
            stor,
            store_suffix,
            // Tokens deployed automatically on a fresh chain (SYMBOL:DECIMALS, space separated).
            default_tokens: std::env::var("PAYROOF_DEFAULT_TOKENS").unwrap_or_else(|_| "TEST:6 USDC:6 DAI:18".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_addresses() {
        assert!(is_ipv4("192.168.1.20"));
        assert!(!is_ipv4("192.168.1"));
        assert!(!is_ipv4("::1"));
        assert!(is_private_ipv4("10.0.0.2"));
        assert!(is_private_ipv4("172.20.1.1"));
        assert!(!is_private_ipv4("172.32.1.1"));
        assert!(!is_private_ipv4("8.8.8.8"));
    }
}
