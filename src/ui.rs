//! Terminal output in the style of the original launcher: `==>` steps, `ok` and `warn` lines.

use std::io::IsTerminal;
use std::sync::OnceLock;

fn color() -> bool {
    static COLOR: OnceLock<bool> = OnceLock::new();
    *COLOR.get_or_init(|| std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none())
}

fn paint(code: &str, text: &str) -> String {
    if color() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

pub fn bold(text: &str) -> String {
    paint("1", text)
}

pub fn say(msg: impl AsRef<str>) {
    println!("{} {}", paint("1", "==>"), msg.as_ref());
}

pub fn ok(msg: impl AsRef<str>) {
    println!("  {}  {}", paint("32", "ok"), msg.as_ref());
}

pub fn warn(msg: impl AsRef<str>) {
    eprintln!("  {} {}", paint("33", "warn"), msg.as_ref());
}

pub fn error(msg: impl AsRef<str>) {
    eprintln!("{} {}", paint("31", "error:"), msg.as_ref());
}

/// Ends the program with this status after the message was already printed.
#[derive(Debug)]
pub struct Exit(pub i32);

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit status {}", self.0)
    }
}

impl std::error::Error for Exit {}
