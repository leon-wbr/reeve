//! `reeve handoff`: writes a handoff file that lists the session's named workers, and starts the
//! successor as a named worker whose launch prompt points at that file.

use crate::workers::{self, Backend, Registry, State, Worker};
use std::fs;
use std::io;
use std::path::Path;

pub fn doc(notes: &str, workers: &[(Worker, State)]) -> String {
    let mut d = notes.trim_end().to_string();
    d += "\n\n## Workers\n\n";
    if workers.is_empty() {
        d += "No named workers.\n";
        return d;
    }
    for (w, s) in workers {
        d += &format!("- {}: {s}, report at {}\n", w.name, w.report.display());
    }
    d += "\nThey keep running without the previous session. `reeve ls` shows them, `reeve wait NAME` \
          blocks until one has written its report and `reeve report NAME` prints it. Don't restart \
          a worker that is still running.\n";
    d
}

pub fn successor_prompt(file: &Path) -> String {
    format!(
        "You take over from a previous session that handed off. Read {} and continue the work \
         from there.",
        file.display()
    )
}

/// Writes the handoff for `next` and starts it; the calling session should stop afterwards.
pub fn handoff(
    r: &Registry,
    next: &str,
    backend: Backend,
    notes: &Path,
    args: &[String],
) -> io::Result<()> {
    workers::check_name(next).map_err(io::Error::other)?;
    let notes = fs::read_to_string(notes)?;
    let all: Vec<(Worker, State)> = r
        .all()?
        .into_iter()
        .filter(|w| w.name != next)
        .map(|w| {
            let s = workers::state(&w);
            (w, s)
        })
        .collect();
    let file = r.handoff_path(next);
    fs::create_dir_all(file.parent().expect("handoffs dir"))?;
    fs::write(&file, doc(&notes, &all))?;
    let args: Vec<String> = args
        .iter()
        .cloned()
        .chain([successor_prompt(&file)])
        .collect();
    workers::run(r, next, backend, &args)?;
    println!("{}", file.display());
    eprintln!("reeve: handed off to {next}; stop now and leave the workers running");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(name: &str) -> Worker {
        Worker {
            name: name.into(),
            backend: Backend::Bg,
            cwd: "/work/x".into(),
            started: 1,
            report: format!("/st/reports/{name}.md").into(),
            handle: "ab12".into(),
            starting: false,
            error: None,
        }
    }

    #[test]
    fn the_handoff_keeps_the_notes_and_lists_every_worker_with_its_report() {
        let d = doc(
            "Goal: ship it.\n",
            &[(w("fix-a"), State::Running), (w("fix-b"), State::Done)],
        );
        assert!(d.starts_with("Goal: ship it.\n"));
        assert!(d.contains("- fix-a: running, report at /st/reports/fix-a.md"));
        assert!(d.contains("- fix-b: done, report at /st/reports/fix-b.md"));
        assert!(d.contains("reeve wait"));
        assert!(d.contains("reeve report"));
    }

    #[test]
    fn a_handoff_without_workers_says_so() {
        assert!(doc("Goal.", &[]).contains("No named workers"));
    }

    #[test]
    fn the_successor_is_told_to_read_the_file_in_one_line() {
        let p = successor_prompt(Path::new("/st/handoffs/next.md"));
        assert!(p.contains("/st/handoffs/next.md"));
        assert!(!p.chars().any(char::is_control));
    }
}
