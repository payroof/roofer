//! The isolated local Wallet app on this Mac and the Wallet on a paired iPhone.

use crate::ctx::{Ctx, is_private_ipv4};
use crate::{sys, ui};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

/// Explicit `PAYROOF_APPLE_TEAM`, else the first team signed in to Xcode.
fn apple_team() -> Option<String> {
    if let Some(team) = std::env::var("PAYROOF_APPLE_TEAM")
        .ok()
        .filter(|t| !t.is_empty())
    {
        return Some(team);
    }
    let out = sys::try_capture(Command::new("defaults").args([
        "read",
        "com.apple.dt.Xcode",
        "IDEProvisioningTeamByIdentifier",
    ]))?;
    out.lines().find_map(|l| {
        let rest = l.split("teamID = ").nth(1)?;
        let id: String = rest
            .chars()
            .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            .collect();
        (!id.is_empty() && rest[id.len()..].starts_with(';')).then_some(id)
    })
}

/// Whether any source file under `paths` is newer than `than` (generated Xcode files aside).
fn newer_than(paths: &[&Path], than: SystemTime) -> bool {
    fn walk(path: &Path, than: SystemTime) -> bool {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name == "Generated" || name.ends_with(".xcodeproj") {
            return false;
        }
        let Ok(meta) = path.symlink_metadata() else {
            return false;
        };
        if meta.is_dir() {
            return std::fs::read_dir(path)
                .into_iter()
                .flatten()
                .flatten()
                .any(|e| walk(&e.path(), than));
        }
        meta.is_file() && meta.modified().is_ok_and(|m| m > than)
    }
    paths.iter().any(|p| walk(p, than))
}

fn grep_lines(log: &Path, any_of: &[&str], limit: usize) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let mut lines: Vec<&str> = text
        .lines()
        .filter(|l| any_of.iter().any(|p| l.contains(p)))
        .collect();
    lines.dedup();
    lines.into_iter().take(limit).collect::<Vec<_>>().join("\n")
}

impl Ctx {
    fn build_apple(
        &self,
        target: &str,
        envs: &[(&str, &str)],
        log: &Path,
        append: bool,
    ) -> Result<bool> {
        let mut cmd = Command::new("./build-apple.sh");
        cmd.arg(target).current_dir(self.root.join("wallet"));
        for (k, v) in envs {
            cmd.env(k, v);
        }
        sys::logged(&mut cmd, log, append)
    }

    pub fn build_wallet(&self, force: bool) -> Result<()> {
        let wallet = self.root.join("wallet");
        let reason = if force {
            Some("requested")
        } else if !sys::is_executable(&self.wallet_bin) {
            Some("app missing")
        } else {
            let built = self.wallet_bin.metadata()?.modified()?;
            let sources = [
                "src",
                "apple",
                "macos",
                "assets",
                "Cargo.toml",
                "Cargo.lock",
            ]
            .map(|p| wallet.join(p));
            newer_than(
                &sources.iter().map(|p| p.as_path()).collect::<Vec<_>>(),
                built,
            )
            .then_some("sources changed")
        };
        let Some(reason) = reason else {
            ui::ok("Wallet app up to date");
            return Ok(());
        };
        sys::need(&["xcodebuild", "xcodegen", "cargo", "rustup"])?;
        if !self.wallet_pids().is_empty() {
            ui::warn(format!(
                "Wallet app is open; skipping rebuild ({reason}). Quit it and run roofer up --rebuild-wallet"
            ));
            return Ok(());
        }
        let team = apple_team();
        ui::say(format!(
            "building local Wallet app ({reason}; takes a few minutes)"
        ));
        let log = self.log("build-wallet");
        // Signed with your Xcode team, device unlock (Touch ID) works like in a release build.
        if let Some(team) = &team {
            if self.build_apple("macos-local", &[("DEVELOPMENT_TEAM", team)], &log, false)? {
                ui::ok(format!("Wallet app built (signed, team {team})"));
                return Ok(());
            }
            ui::warn(format!(
                "signed Wallet build failed (log: {}); building unsigned, without device unlock",
                log.display()
            ));
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if text.contains("has no devices") || text.contains("No profiles for") {
                let udid =
                    sys::try_capture(Command::new("system_profiler").arg("SPHardwareDataType"))
                        .and_then(|o| {
                            o.lines().find_map(|l| {
                                l.split("Provisioning UDID: ").nth(1).map(str::to_string)
                            })
                        })
                        .unwrap_or_default();
                ui::warn(format!(
                    "register this Mac with team {team} once: developer.apple.com → Devices → + (macOS),"
                ));
                ui::warn(format!(
                    "Provisioning UDID {udid}, then roofer up --rebuild-wallet"
                ));
            }
        }
        if !self.build_apple("macos-local", &[], &log, true)? {
            eprintln!("{}", grep_lines(&log, &["error"], 30));
            bail!("Wallet build failed (log: {})", log.display());
        }
        ui::ok("Wallet app built (unsigned: sign in to Xcode for device unlock)");
        Ok(())
    }

    pub fn stop_wallet(&self) {
        for pid in self.wallet_pids() {
            sys::signal(pid, libc::SIGTERM);
            ui::ok(format!("closed local Wallet app (pid {pid})"));
        }
    }

    pub fn open_wallet(&self) -> Result<()> {
        if !self.wallet_app.is_dir() {
            ui::warn("Wallet app not built (run roofer up --rebuild-wallet)");
            return Ok(());
        }
        if !self.wallet_pids().is_empty() {
            ui::ok("local Wallet already open (only one instance per data dir)");
            return Ok(());
        }
        let sandboxed = sys::try_capture(
            Command::new("codesign")
                .args(["-d", "--entitlements", "-"])
                .arg(&self.wallet_app),
        )
        .is_some_and(|o| o.contains("app-sandbox"));
        let (dir, shown) = if sandboxed {
            // Created by the app inside its container.
            (
                self.wallet_container_dir.clone(),
                self.wallet_container_dir.display().to_string(),
            )
        } else {
            std::fs::create_dir_all(&self.wallet_dir)?;
            sys::chmod(&self.wallet_dir, 0o700)?;
            (self.wallet_dir.clone(), ".local/manual/wallet".to_string())
        };
        sys::capture(
            Command::new("open")
                .args([
                    "-n",
                    "-F",
                    "--env",
                    "PAYROOF_WALLET_LOCAL_DEVELOPMENT=1",
                    "--env",
                ])
                .arg(format!("PAYROOF_WALLET_DATA_DIR={}", dir.display()))
                .arg(&self.wallet_app),
        )?;
        ui::ok(format!(
            "opened local Wallet (data: {shown}; if no window appears: File → New Window)"
        ));
        Ok(())
    }

    /// Builds, installs and launches the iOS Wallet on a paired iPhone.
    pub fn ios(&self, device: Option<String>, local: bool, build: bool) -> Result<()> {
        let lan_ip = self.lan_ip.clone().unwrap_or_default();
        if local {
            if lan_ip.is_empty() {
                bail!(
                    "--local needs this Mac's Wi-Fi IP (not found; set PAYROOF_LAN_IP, and don't set PAYROOF_BIND_HOST=127.0.0.1)"
                );
            }
            if !is_private_ipv4(&lan_ip) {
                bail!(
                    "--local needs a private LAN IP (10.x, 172.16-31.x, 192.168.x), got {lan_ip}"
                );
            }
        }
        sys::need(&["xcrun"])?;
        std::fs::create_dir_all(&self.logs)?;
        let mut device = device
            .or_else(|| std::env::var("PAYROOF_IOS_DEVICE").ok())
            .filter(|d| !d.is_empty());
        ui::say(match &device {
            Some(d) => format!("finding iPhone '{d}'"),
            None => "finding the paired iPhone".into(),
        });
        let json_path =
            std::env::temp_dir().join(format!("roofer-devices-{}.json", std::process::id()));
        let listed = sys::quiet(
            Command::new("xcrun")
                .args(["devicectl", "list", "devices", "--json-output"])
                .arg(&json_path),
        );
        let text = std::fs::read_to_string(&json_path).unwrap_or_default();
        let _ = std::fs::remove_file(&json_path);
        if !listed {
            bail!("could not list devices (is Xcode installed and set up?)");
        }
        let json: Value = serde_json::from_str(&text).context("devicectl output")?;
        let devices = json["result"]["devices"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let name = |d: &Value| {
            d["deviceProperties"]["name"]
                .as_str()
                .unwrap_or("")
                .to_string()
        };
        let is_ios = |d: &Value| d["hardwareProperties"]["platform"] == "iOS";
        let physical = |d: &Value| is_ios(d) && d["hardwareProperties"]["reality"] == "physical";
        let paired_state = |d: &Value| d["connectionProperties"]["pairingState"] == "paired";
        if device.is_none() {
            let paired: Vec<String> = devices
                .iter()
                .filter(|d| physical(d) && paired_state(d))
                .map(name)
                .collect();
            match paired.len() {
                1 => device = paired.into_iter().next(),
                0 => bail!(
                    "no iPhone is paired with this Mac: connect it by cable, unlock it, tap Trust (Xcode → Window → Devices and Simulators shows it)"
                ),
                _ => bail!(
                    "several paired iPhones ({}): pass --device NAME or set PAYROOF_IOS_DEVICE",
                    paired.join(", ")
                ),
            }
        }
        let device = device.unwrap_or_default();
        let udid = devices
            .iter()
            .find(|d| name(d) == device && is_ios(d))
            .and_then(|d| d["hardwareProperties"]["udid"].as_str())
            .map(str::to_string);
        let paired = devices
            .iter()
            .find(|d| name(d) == device)
            .is_some_and(paired_state);
        let known: Vec<String> = devices.iter().filter(|d| physical(d)).map(name).collect();
        let Some(udid) = udid else {
            bail!(
                "no iPhone named '{device}' (known: {}). Connect it by cable or Wi-Fi, or pass --device NAME",
                if known.is_empty() {
                    "none".into()
                } else {
                    known.join(", ")
                }
            );
        };
        if !paired {
            bail!("'{device}' is not paired with this Mac: connect it, unlock it and tap Trust");
        }
        ui::ok(format!("{device} ({udid})"));

        let again = if local { " --local" } else { "" };
        let app = self.root.join(if local {
            "wallet/target/apple-iphoneos-device-local/Build/Products/Debug-iphoneos/PayroofWallet.app"
        } else {
            "wallet/target/apple-iphoneos-device/Build/Products/Debug-iphoneos/PayroofWallet.app"
        });
        if build {
            sys::need(&["xcodebuild", "xcodegen", "cargo", "rustup"])?;
            let team = apple_team().ok_or_else(|| {
                anyhow!("no Apple development team found. Sign in under Xcode → Settings → Accounts, or set PAYROOF_APPLE_TEAM")
            })?;
            let target = if local {
                "ios-device-local"
            } else {
                "ios-device"
            };
            ui::say(format!(
                "building signed {}iOS Wallet for {device} (team {team}; takes a few minutes)",
                if local {
                    format!("local (stack at {lan_ip}) ")
                } else {
                    String::new()
                }
            ));
            let log = self.log("build-ios");
            let bundle_id = std::env::var("PAYROOF_IOS_BUNDLE_ID").unwrap_or_default();
            let envs = [
                ("DEVELOPMENT_TEAM", team.as_str()),
                ("DEVICE_ID", udid.as_str()),
                ("BUNDLE_ID", bundle_id.as_str()),
                ("PAYROOF_LOCAL_HOST", lan_ip.as_str()),
            ];
            if !self.build_apple(target, &envs, &log, false)? {
                let mut lines: Vec<String> = grep_lines(
                    &log,
                    &["error:", "No profiles", "provisioning", "Signing"],
                    usize::MAX,
                )
                .lines()
                .map(str::to_string)
                .collect();
                lines.sort();
                lines.dedup();
                eprintln!(
                    "{}",
                    lines.into_iter().take(20).collect::<Vec<_>>().join("\n")
                );
                bail!(
                    "iOS build failed (log: {}). A taken bundle ID can be replaced with PAYROOF_IOS_BUNDLE_ID=your.unique.id",
                    log.display()
                );
            }
            ui::ok("iOS Wallet built");
        }
        if !app.is_dir() {
            bail!("no signed iOS build yet (run roofer ios{again} without --no-build)");
        }
        let bundle = sys::capture(
            Command::new("/usr/libexec/PlistBuddy")
                .args(["-c", "Print :CFBundleIdentifier"])
                .arg(app.join("Info.plist")),
        )
        .with_context(|| format!("could not read the bundle ID of {}", app.display()))?;

        ui::say(format!("installing on {device}"));
        let install_log = self.log("ios-install");
        if !sys::logged(
            Command::new("xcrun")
                .args(["devicectl", "device", "install", "app", "--device", &udid])
                .arg(&app),
            &install_log,
            false,
        )? {
            eprintln!("{}", sys::tail(&install_log, 15));
            bail!(
                "install failed: unlock {device} and keep it nearby (log: {})",
                install_log.display()
            );
        }
        ui::ok(format!("installed {bundle}"));

        ui::say("launching");
        let launch_log = self.log("ios-launch");
        let launched = sys::logged(
            Command::new("xcrun").args([
                "devicectl",
                "device",
                "process",
                "launch",
                "--device",
                &udid,
                "--terminate-existing",
                &bundle,
            ]),
            &launch_log,
            false,
        )?;
        let text = std::fs::read_to_string(&launch_log).unwrap_or_default();
        if launched {
            ui::ok(format!("Payroof Wallet is open on {device}"));
            if local {
                println!(
                    "        Local test app: Anvil {} and Payroof {} are preset for a new wallet.",
                    self.rpc_public, self.payroof_public
                );
                println!(
                    "        Allow \"Local Network\" when iOS asks (else: Settings → Privacy & Security → Local Network)."
                );
            }
            Ok(())
        } else if text.contains("explicitly trusted") {
            ui::warn(
                "installed, but iOS won't open apps from a new developer until you trust it (once per Apple ID):",
            );
            println!(
                "        on {device}: Settings → General → VPN & Device Management → Apple Development: … → Trust"
            );
            println!(
                "        then: roofer ios{again} --no-build   (or just tap Payroof Wallet on the phone)"
            );
            Err(ui::Exit(1).into())
        } else if text.to_lowercase().contains("locked") || text.to_lowercase().contains("passcode")
        {
            bail!("{device} is locked: unlock it, then run roofer ios{again} --no-build")
        } else {
            eprintln!("{}", sys::tail(&launch_log, 15));
            bail!(
                "launch failed (log: {}). Unlock {device} and run roofer ios{again} --no-build",
                launch_log.display()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_newer_sources_and_skips_generated() {
        let dir = std::env::temp_dir().join(format!("roofer-newer-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Generated")).unwrap();
        let before = SystemTime::now() - std::time::Duration::from_secs(5);
        std::fs::write(dir.join("Generated/x.swift"), "").unwrap();
        assert!(!newer_than(&[&dir], before));
        std::fs::write(dir.join("a.rs"), "").unwrap();
        assert!(newer_than(&[&dir], before));
        assert!(!newer_than(&[&dir.join("missing")], before));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
