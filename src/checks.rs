//! Cross-project checks: the end-to-end test suite (`e2e/`) and the hardened containers.

use crate::{http, sys, ui};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Builds the Payroof release binary, then runs the end-to-end tests with `cargo test`.
pub fn e2e(root: &Path, cargo_args: &[String]) -> Result<()> {
    sys::need(&["cargo"])?;
    if !sys::on_path("anvil") {
        bail!("anvil (Foundry) not found on PATH");
    }
    if !sys::on_path("solc") {
        bail!("solc not found on PATH (test tokens are compiled with it)");
    }
    ui::say("cargo build --release (payroof)");
    let built = Command::new("cargo")
        .args(["build", "--locked", "--release", "--manifest-path"])
        .arg(root.join("payroof/Cargo.toml"))
        .status()
        .context("run cargo")?;
    if !built.success() {
        bail!("payroof build failed");
    }
    ui::say("cargo test (roofer-e2e)");
    let status = Command::new("cargo")
        .args(["test", "--locked", "--manifest-path"])
        .arg(root.join("roofer/Cargo.toml"))
        .args(["-p", "roofer-e2e"])
        .args(cargo_args)
        .env("PAYROOF_BIN", root.join("payroof/target/release/payroof"))
        .status()
        .context("run cargo")?;
    if !status.success() {
        return Err(ui::Exit(status.code().unwrap_or(1)).into());
    }
    Ok(())
}

const ANVIL_PORT: u16 = 18545;
const PAYROOF_PORT: u16 = 18080;
const STOR_PORT: u16 = 18081;

/// Containers, network, Anvil and the work directory of one `roofer docker` run.
struct DockerRun {
    network: String,
    work: PathBuf,
    anvil: Option<Child>,
}

impl Drop for DockerRun {
    fn drop(&mut self) {
        let names = [
            format!("{}-payroof", self.network),
            format!("{}-stor", self.network),
        ];
        sys::quiet(Command::new("docker").args(["rm", "-f", "-v"]).args(&names));
        sys::quiet(Command::new("docker").args(["network", "rm", &self.network]));
        if let Some(anvil) = &mut self.anvil {
            let _ = anvil.kill();
            let _ = anvil.wait();
        }
        let _ = std::fs::remove_dir_all(&self.work);
    }
}

fn docker(args: &[&str]) -> Result<String> {
    sys::capture(Command::new("docker").args(args))
}

fn inspect(format: &str, target: &str, image: bool) -> String {
    let kind: &[&str] = if image {
        &["image", "inspect"]
    } else {
        &["inspect"]
    };
    sys::try_capture(
        Command::new("docker")
            .args(kind)
            .args(["-f", format, target]),
    )
    .unwrap_or_default()
}

fn get_ok(url: &str, contains: &str) -> bool {
    http::get(url, Duration::from_secs(10))
        .is_ok_and(|r| r.status < 400 && r.body.contains(contains))
}

fn container_logs(name: &str) -> String {
    let out = Command::new("docker").args(["logs", name]).output();
    out.map(|o| {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(20)..].join("\n")
    })
    .unwrap_or_default()
}

/// Builds the Payroof and Stor images and runs them together, hardened as in their
/// compose.yaml files (read-only, no capabilities, no privilege escalation, UID 10001),
/// against an Anvil chain on this host. Checks image size, the built-in healthcheck,
/// readiness, the public routes and that Payroof scans the chain from its container.
pub fn docker_checks(root: &Path) -> Result<()> {
    sys::need(&["docker", "anvil", "cast"])?;
    let network = format!("payroof-system-{}", std::process::id());
    let work = std::env::temp_dir().join(&network);
    std::fs::create_dir_all(&work)?;
    sys::chmod(&work, 0o700)?;
    let mut run = DockerRun {
        network: network.clone(),
        work: work.clone(),
        anvil: None,
    };

    for image in ["payroof", "stor"] {
        let tag = format!("{image}:dev");
        let dir = root.join(image);
        docker(&["build", "-q", "-t", &tag, &dir.to_string_lossy()])?;
        let bytes: u64 = inspect("{{.Size}}", &tag, true).parse().unwrap_or(u64::MAX);
        if bytes >= 20_000_000 {
            bail!("FAIL: {image} image is {bytes} bytes");
        }
        if inspect("{{.Config.User}}", &tag, true) != "10001:10001" {
            bail!("FAIL: {image} image does not run as 10001");
        }
        ui::ok(format!("{tag} {} KiB, user 10001", bytes / 1024));
    }
    for repo in ["payroof", "stor"] {
        let valid = sys::quiet(
            Command::new("docker")
                .args(["compose", "config", "-q"])
                .current_dir(root.join(repo))
                .envs(
                    [
                        "PAYROOF_MASTER_KEY",
                        "PAYROOF_CURSOR_KEY",
                        "PAYROOF_ALTCHA_KEY",
                        "STORTOP_MASTER_KEY",
                        "STORTOP_ALTCHA_KEY",
                    ]
                    .map(|k| (k, "x")),
                ),
        );
        if !valid {
            bail!("FAIL: {repo} compose.yaml is invalid");
        }
    }
    ui::ok("compose files valid");

    let anvil_log = std::fs::File::create(work.join("anvil.log"))?;
    run.anvil = Some(
        Command::new("anvil")
            .args([
                "--host",
                "0.0.0.0",
                "--port",
                &ANVIL_PORT.to_string(),
                "--chain-id",
                "31337",
                "--block-time",
                "1",
            ])
            .stdin(Stdio::null())
            .stdout(anvil_log.try_clone()?)
            .stderr(anvil_log)
            .spawn()
            .context("start anvil")?,
    );
    let anvil_url = format!("http://127.0.0.1:{ANVIL_PORT}");
    for _ in 0..60 {
        if http::rpc(&anvil_url, "eth_chainId", serde_json::json!([])).is_ok() {
            break;
        }
        sys::sleep_ms(500);
    }

    let payroof_toml = work.join("payroof.toml");
    std::fs::write(
        &payroof_toml,
        format!(
            r#"bind = "0.0.0.0:8080"
origin = "http://127.0.0.1:{PAYROOF_PORT}"
database = "sqlite:///data/payroof.sqlite"
development = true
[[chains]]
id = "eip155:31337"
name = "Anvil"
rpc = "http://host.docker.internal:{ANVIL_PORT}"
confirmations = 1
start_height = 0
[[chains.assets]]
id = "eip155:31337/slip44:60"
kind = "native"
symbol = "ETH"
decimals = 18
"#
        ),
    )?;
    let stor_toml = work.join("stortop.toml");
    std::fs::write(
        &stor_toml,
        format!(
            r#"bind = "0.0.0.0:8081"
origin = "http://127.0.0.1:{STOR_PORT}"
store_suffix = "stor.localhost:{STOR_PORT}"
database = "sqlite:///data/stortop.sqlite"
payroof_origins = ["http://payroof:8080"]
development = true
"#
        ),
    )?;
    sys::chmod(&payroof_toml, 0o644)?;
    sys::chmod(&stor_toml, 0o644)?;

    let hardened = [
        "--read-only",
        "-v",
        "/data",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges:true",
        "--tmpfs",
        "/tmp",
        "--add-host",
        "host.docker.internal:host-gateway",
    ];
    docker(&["network", "create", &network])?;
    let key = || sys::random_hex32();
    let payroof = format!("{network}-payroof");
    let stor = format!("{network}-stor");
    let mut args: Vec<String> = [
        "run",
        "-d",
        "--name",
        &payroof,
        "--network",
        &network,
        "--network-alias",
        "payroof",
    ]
    .map(String::from)
    .to_vec();
    args.extend(hardened.map(String::from));
    args.extend(["-p".into(), format!("127.0.0.1:{PAYROOF_PORT}:8080")]);
    for k in [
        "PAYROOF_MASTER_KEY",
        "PAYROOF_CURSOR_KEY",
        "PAYROOF_ALTCHA_KEY",
    ] {
        args.extend(["-e".into(), format!("{k}={}", key()?)]);
    }
    args.extend([
        "-v".into(),
        format!("{}:/etc/payroof/payroof.toml:ro", payroof_toml.display()),
        "payroof:dev".into(),
    ]);
    docker(&args.iter().map(String::as_str).collect::<Vec<_>>())?;

    let mut args: Vec<String> = [
        "run",
        "-d",
        "--name",
        &stor,
        "--network",
        &network,
        "--network-alias",
        "stor",
    ]
    .map(String::from)
    .to_vec();
    args.extend(hardened.map(String::from));
    args.extend(["-p".into(), format!("127.0.0.1:{STOR_PORT}:8081")]);
    for k in ["STORTOP_MASTER_KEY", "STORTOP_ALTCHA_KEY"] {
        args.extend(["-e".into(), format!("{k}={}", key()?)]);
    }
    args.extend([
        "-v".into(),
        format!("{}:/etc/stortop/stortop.toml:ro", stor_toml.display()),
        "stor:dev".into(),
    ]);
    docker(&args.iter().map(String::as_str).collect::<Vec<_>>())?;

    let healthy = |name: &str| inspect("{{.State.Health.Status}}", name, false) == "healthy";
    for (short, name) in [("payroof", &payroof), ("stor", &stor)] {
        for _ in 0..90 {
            if healthy(name) {
                break;
            }
            sys::sleep_ms(1000);
        }
        if !healthy(name) {
            eprintln!("{}", container_logs(name));
            bail!("FAIL: {short} not healthy");
        }
        ui::ok(format!("{short} container healthy (image healthcheck)"));
    }

    let p = format!("http://127.0.0.1:{PAYROOF_PORT}");
    let s = format!("http://127.0.0.1:{STOR_PORT}");
    let checks = [
        (format!("{p}/readyz"), "\"ready\":true", "payroof readyz"),
        (format!("{s}/readyz"), "\"ready\":true", "stor readyz"),
        (
            format!("{p}/v1/altcha?purpose=project"),
            "\"signature\"",
            "payroof altcha",
        ),
        (
            format!("{s}/v1/info"),
            "\"store_suffix\":\"stor.localhost",
            "stor /v1/info",
        ),
        (format!("{s}/"), "", "stor creation page"),
    ];
    for (url, needle, label) in &checks {
        if !get_ok(url, needle) {
            bail!("FAIL: {label}");
        }
    }
    ui::ok("readiness, Payroof ALTCHA, Stor /v1/info and creation page");

    // Payroof follows the chain from inside its container: a new block keeps it ready.
    sys::capture(Command::new("cast").args(["rpc", "--rpc-url", &anvil_url, "anvil_mine", "3"]))?;
    sys::sleep_ms(3000);
    if !get_ok(&format!("{p}/readyz"), "\"ready\":true") {
        bail!("FAIL: payroof scanning");
    }
    // Stor reaches Payroof by its network alias.
    let reaches = sys::quiet(Command::new("docker").args([
        "run",
        "--rm",
        "--network",
        &network,
        "--entrypoint",
        "/usr/local/bin/stortop",
        "stor:dev",
        "healthcheck",
        "http://payroof:8080/readyz",
    ]));
    if !reaches {
        bail!("FAIL: stor network cannot reach payroof");
    }
    ui::ok("Payroof scans Anvil; Stor's network reaches Payroof");

    for (short, name) in [("payroof", &payroof), ("stor", &stor)] {
        if sys::quiet(Command::new("docker").args(["exec", name, "/bin/sh", "-c", "true"])) {
            bail!("FAIL: {short} image has a shell");
        }
        if inspect("{{.HostConfig.ReadonlyRootfs}}", name, false) != "true" {
            bail!("FAIL: {short} rootfs");
        }
    }
    docker(&["stop", "-t", "20", &payroof, &stor])?;
    for (short, name) in [("payroof", &payroof), ("stor", &stor)] {
        let code = inspect("{{.State.ExitCode}}", name, false);
        if code != "0" {
            eprintln!("{}", container_logs(name));
            bail!("FAIL: {short} exited {code} on SIGTERM");
        }
    }
    ui::ok("both stop cleanly on SIGTERM");
    ui::say("all container checks passed");
    drop(run);
    Ok(())
}
