//! `reeve bench`: drives a communicator session through a fixed scenario with a simulated user
//! and scores the runs. Results are laid out as by the Node harness it replaces, so both compare.

pub mod check;
pub mod run;
pub mod score;
mod sim;

use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Child, Command};

/// The scenario and arms; the bench runs from the source tree.
pub fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/bench"))
}

pub fn home() -> PathBuf {
    std::env::home_dir().unwrap_or_default()
}

pub fn trials() -> PathBuf {
    std::env::var_os("REEVE_BENCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("work/reeve-trials"))
}

/// SIGTERM, as Node's `kill()` sends.
pub fn term(child: &Child) {
    let _ = Command::new("kill").arg(child.id().to_string()).status();
}

/// A number as JSON.stringify writes it: integral values without a fraction.
pub fn num(x: f64) -> Value {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        json!(x as i64)
    } else {
        json!(x)
    }
}

/// JavaScript's `Number.prototype.toFixed`, which rounds exact ties up, not to even.
pub fn to_fixed(x: f64, d: usize) -> String {
    let a = x.abs();
    let exact = format!("{a:.prec$}", prec = d + 30);
    let tail = &exact[exact.len() - 30..];
    let tie = tail.starts_with('5') && tail[1..].bytes().all(|b| b == b'0');
    let a = if tie {
        a + 0.5 * 10f64.powi(-(d as i32))
    } else {
        a
    };
    format!("{}{a:.d$}", if x < 0.0 { "-" } else { "" })
}

/// `+x.toFixed(d)`
pub fn round(x: f64, d: usize) -> Value {
    num(to_fixed(x, d).parse().unwrap_or(x))
}

/// `${v}` for a JSON value that may be missing.
pub fn js_str(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_fixed_rounds_ties_up_like_javascript() {
        assert_eq!(to_fixed(2.5, 0), "3");
        assert_eq!(to_fixed(0.5, 0), "1");
        assert_eq!(to_fixed(1.25, 1), "1.3");
        assert_eq!(to_fixed(10.6, 1), "10.6");
        assert_eq!(to_fixed(1.005, 2), "1.00");
        assert_eq!(to_fixed(0.0, 1), "0.0");
        assert_eq!(round(3.948_000_1, 3), json!(3.948));
        assert_eq!(round(2.0004, 3), json!(2));
    }
}
