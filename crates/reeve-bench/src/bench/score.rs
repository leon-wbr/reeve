//! Aggregates bench results per arm into a table.

use super::{to_fixed, trials};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io;

pub fn score(only: &[String]) -> io::Result<String> {
    let logs = trials().join("logs");
    let mut runs: Vec<_> = fs::read_dir(&logs)?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<io::Result<_>>()?;
    runs.sort();
    let mut by_arm: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for run in runs {
        let f = logs.join(run).join("result.json");
        if !f.exists() {
            continue;
        }
        let r: Value = serde_json::from_str(&fs::read_to_string(&f)?).map_err(io::Error::other)?;
        let arm = super::js_str(r.get("arm"));
        if only.is_empty() || only.contains(&arm) {
            by_arm.entry(arm).or_default().push(r);
        }
    }

    let header = [
        "arm",
        "n",
        "interventions",
        "kinds",
        "done",
        "false done",
        "min to done",
        "workers",
        "failed starts",
        "blocked min",
        "unchecked landings",
        "cost $",
    ];
    let mut rows: Vec<Vec<String>> = vec![header.map(String::from).to_vec()];
    for (arm, rs) in &by_arm {
        let interventions = |r: &Value| r["interventions"].as_array().cloned().unwrap_or_default();
        let mut kinds: Vec<(String, usize)> = vec![];
        for i in rs.iter().flat_map(interventions) {
            let k = super::js_str(i.get("kind"));
            match kinds.iter_mut().find(|(n, _)| *n == k) {
                Some((_, c)) => *c += 1,
                None => kinds.push((k, 1)),
            }
        }
        let kinds: Vec<String> = kinds.iter().map(|(k, v)| format!("{k}:{v}")).collect();
        let col =
            |f: &dyn Fn(&Value) -> Option<f64>, d| fmt(&rs.iter().map(f).collect::<Vec<_>>(), d);
        let count = |f: &dyn Fn(&Value) -> bool| rs.iter().filter(|r| f(r)).count();
        rows.push(vec![
            arm.clone(),
            rs.len().to_string(),
            col(&|r| Some(interventions(r).len() as f64), 0),
            if kinds.is_empty() {
                "–".into()
            } else {
                kinds.join(" ")
            },
            format!("{}/{}", count(&|r| r["final"]["done"] == true), rs.len()),
            count(&|r| r["falseDone"] == true).to_string(),
            col(&|r| r["doneAt"].as_f64().map(|t| t / 60e3), 1),
            col(&|r| r["workers"].as_f64(), 0),
            rs.iter()
                .map(|r| r["failedStarts"].as_i64().unwrap_or(0))
                .sum::<i64>()
                .to_string(),
            col(&|r| Some(blocked_ms(r) / 60e3), 1),
            col(&|r| r["uncheckedLandings"].as_f64(), 0),
            col(&|r| r["costUsd"].as_f64(), 2),
        ]);
    }
    let widths: Vec<usize> = (0..header.len())
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let lines: Vec<String> = rows
        .iter()
        .map(|r| {
            r.iter()
                .zip(&widths)
                .map(|(c, w)| format!("{c:w$}"))
                .collect::<Vec<_>>()
                .join("  ")
        })
        .collect();
    Ok(lines.join("\n"))
}

fn blocked_ms(r: &Value) -> f64 {
    r["blockedMs"]
        .as_object()
        .map_or(0.0, |m| m.values().filter_map(Value::as_f64).sum())
}

pub(super) fn median(xs: &[f64]) -> Option<f64> {
    let mut s = xs.to_vec();
    s.sort_by(f64::total_cmp);
    let m = s.len() / 2;
    match s.len() {
        0 => None,
        n if n % 2 == 1 => Some(s[m]),
        _ => Some((s[m - 1] + s[m]) / 2.0),
    }
}

/// `median [min–max]` of the values present.
pub(super) fn fmt(xs: &[Option<f64>], d: usize) -> String {
    let v: Vec<f64> = xs.iter().flatten().copied().collect();
    let Some(m) = median(&v) else {
        return "–".into();
    };
    let min = v.iter().copied().fold(f64::INFINITY, f64::min);
    let max = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    format!(
        "{} [{}–{}]",
        to_fixed(m, d),
        to_fixed(min, d),
        to_fixed(max, d)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_of_odd_even_and_empty() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), Some(2.5));
        assert_eq!(median(&[]), None);
    }

    #[test]
    fn fmt_skips_missing_values() {
        assert_eq!(fmt(&[Some(1.0), None, Some(3.0), Some(1.0)], 0), "1 [1–3]");
        assert_eq!(fmt(&[Some(2.0), Some(3.0)], 0), "3 [2–3]");
        assert_eq!(fmt(&[None], 1), "–");
    }
}
