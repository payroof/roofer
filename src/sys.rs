//! External tools and processes.

use anyhow::{Context, Result, bail};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

pub fn on_path(tool: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| is_executable(&dir.join(tool)))
    })
}

pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

pub fn need(tools: &[&str]) -> Result<()> {
    for tool in tools {
        if !on_path(tool) {
            bail!("required tool '{tool}' not found on PATH");
        }
    }
    Ok(())
}

/// Runs a command and returns its trimmed stdout; fails with its stderr.
pub fn capture(cmd: &mut Command) -> Result<String> {
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("run {:?}", cmd.get_program()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let detail = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        bail!(
            "{:?} failed: {}",
            cmd.get_program(),
            detail.trim().chars().take(400).collect::<String>()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Stdout of a command that succeeded, else `None`.
pub fn try_capture(cmd: &mut Command) -> Option<String> {
    let out = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Whether a command exits successfully, its output discarded.
pub fn quiet(cmd: &mut Command) -> bool {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Runs a command with its output appended to `log`; true on success.
pub fn logged(cmd: &mut Command, log: &Path, append: bool) -> Result<bool> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(log)?;
    let status = cmd
        .stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file)
        .status()
        .with_context(|| format!("run {:?}", cmd.get_program()))?;
    Ok(status.success())
}

/// Starts a daemon in its own session (immune to the terminal's hangup and Ctrl-C),
/// output appended to `log`.
pub fn spawn_daemon(cmd: &mut Command, log: &Path) -> Result<Child> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)?;
    // SAFETY: setsid is async-signal-safe and touches no memory of the parent.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file)
        .spawn()
        .with_context(|| format!("start {:?}", cmd.get_program()))
}

pub fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence and permission.
    unsafe {
        libc::kill(pid, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

pub fn signal(pid: i32, sig: i32) -> bool {
    // SAFETY: plain syscall on a pid we looked up.
    pid > 0 && unsafe { libc::kill(pid, sig) == 0 }
}

pub fn command_of(pid: i32) -> Option<String> {
    try_capture(Command::new("ps").args(["-p", &pid.to_string(), "-o", "command="]))
        .filter(|c| !c.is_empty())
}

/// Pids listening on a TCP port.
pub fn listeners(port: u16) -> Vec<i32> {
    try_capture(Command::new("lsof").args(["-nP", "-t", &format!("-iTCP:{port}"), "-sTCP:LISTEN"]))
        .map(|out| out.lines().filter_map(|l| l.trim().parse().ok()).collect())
        .unwrap_or_default()
}

pub fn listener_table(port: u16) -> String {
    try_capture(Command::new("lsof").args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN"]))
        .map(|out| out.lines().skip(1).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
}

pub fn tail(path: &Path, lines: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

pub fn random_hex32() -> Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn sleep_ms(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

pub fn chmod(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("chmod {}", path.display()))
}
