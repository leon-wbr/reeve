use serde_json::{Value, json};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn dir() -> io::Result<PathBuf> {
    let home = std::env::var_os("REEVE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".reeve")))
        .ok_or_else(|| io::Error::other("no REEVE_HOME or HOME"))?;
    let d = home.join("asks");
    fs::create_dir_all(&d)?;
    Ok(d)
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Queues a question for the user and blocks until it is answered, then prints the answer.
/// Meant to run in the background so the agent keeps working meanwhile.
pub fn ask(question: &str) -> io::Result<i32> {
    let d = dir()?;
    let id = format!("{:x}", now_ms() ^ (std::process::id() as u128) << 40);
    let cwd = std::env::current_dir()?;
    let q = json!({ "id": id, "cwd": cwd, "question": question, "askedAt": now_ms() as u64 });
    fs::write(d.join(format!("{id}.json")), q.to_string())?;
    let answer = d.join(format!("{id}.answer"));
    while !answer.exists() {
        sleep(Duration::from_secs(2));
    }
    println!(
        "The user answered your question \"{question}\":\n{}",
        fs::read_to_string(answer)?.trim()
    );
    Ok(0)
}

/// Open questions, oldest first.
pub fn open() -> io::Result<Vec<Value>> {
    let d = dir()?;
    let mut out: Vec<Value> = fs::read_dir(&d)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().is_some_and(|x| x == "json") && !p.with_extension("answer").exists()
        })
        .filter_map(|p| fs::read_to_string(p).ok()?.parse().ok())
        .collect();
    out.sort_by_key(|q: &Value| q["askedAt"].as_u64());
    Ok(out)
}

pub fn list() -> io::Result<i32> {
    for q in open()? {
        let age = (now_ms() as u64).saturating_sub(q["askedAt"].as_u64().unwrap_or(0)) / 60_000;
        println!(
            "{}  {age}m  {}\n    {}",
            q["id"].as_str().unwrap_or(""),
            q["cwd"].as_str().unwrap_or(""),
            q["question"].as_str().unwrap_or("")
        );
    }
    Ok(0)
}

pub fn answer(id: &str, text: &str) -> io::Result<i32> {
    let d = dir()?;
    if !d.join(format!("{id}.json")).exists() {
        return Err(io::Error::other(format!("no question {id}")));
    }
    fs::write(d.join(format!("{id}.answer")), text)?;
    Ok(0)
}
