//! Replacing the running exe. Windows lets a running exe be renamed but not deleted or
//! overwritten, so the running image moves aside to `glint.exe.old` (still mapped by every
//! process started from it: the tray app, and any settings or trimmer windows, which keep
//! running), the new exe takes the original path and is started. The Run entry for launch at
//! startup names that path, so it keeps working.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use glint_core::log::log;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    Move { from: PathBuf, to: PathBuf },
    Launch { exe: PathBuf, args: Vec<String> },
}

impl Step {
    fn undo(&self) -> Option<Step> {
        match self {
            Step::Move { from, to } => Some(Step::Move { from: to.clone(), to: from.clone() }),
            Step::Launch { .. } => None,
        }
    }
}

/// Moves the running `exe` to `aside`, `new` into its place, and starts it with `args`.
pub(crate) fn plan(exe: &Path, aside: &Path, new: &Path, args: &[String]) -> Vec<Step> {
    vec![
        Step::Move { from: exe.to_path_buf(), to: aside.to_path_buf() },
        Step::Move { from: new.to_path_buf(), to: exe.to_path_buf() },
        Step::Launch { exe: exe.to_path_buf(), args: args.to_vec() },
    ]
}

/// Runs `steps` in order; when one fails, undoes the finished ones newest first.
pub(crate) fn run(steps: &[Step], exec: &mut dyn FnMut(&Step) -> Result<(), String>) -> Result<(), String> {
    for (i, step) in steps.iter().enumerate() {
        if let Err(e) = exec(step) {
            for undo in steps[..i].iter().rev().filter_map(Step::undo) {
                if let Err(e) = exec(&undo) {
                    log(&format!("update rollback: {e}"));
                }
            }
            return Err(e);
        }
    }
    Ok(())
}

fn exec(step: &Step) -> Result<(), String> {
    match step {
        Step::Move { from, to } => move_file(from, to),
        Step::Launch { exe, args } => {
            Command::new(exe).args(args).spawn().map(drop).map_err(|e| format!("can't start {}: {e}", exe.display()))
        }
    }
}

fn move_file(from: &Path, to: &Path) -> Result<(), String> {
    let failed = |e: std::io::Error| format!("can't move {} to {}: {e}", from.display(), to.display());
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        // the download folder (%APPDATA%) can be on another drive than glint.exe
        Err(e) if e.kind() == ErrorKind::CrossesDevices => {
            std::fs::copy(from, to).and_then(|_| std::fs::remove_file(from)).map_err(failed)
        }
        Err(e) => Err(failed(e)),
    }
}

/// Where the running exe can be moved: `glint.exe.old`, or `.old2`… while an older one is
/// still in use by a window left open from before.
fn asides(exe: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    (1..=9).map(move |n| {
        let mut name = exe.as_os_str().to_owned();
        name.push(if n == 1 { ".old".to_string() } else { format!(".old{n}") });
        PathBuf::from(name)
    })
}

/// Swaps `new` in for the running `exe` and starts it; on failure everything is put back.
pub(crate) fn swap(exe: &Path, new: &Path, args: &[String]) -> Result<(), String> {
    let aside = asides(exe)
        .find(|p| !p.exists() || std::fs::remove_file(p).is_ok())
        .ok_or_else(|| format!("old copies of {} are still in use; close glint's windows and try again", exe.display()))?;
    run(&plan(exe, &aside, new, args), &mut exec)
}

/// Deletes the copies left aside by earlier updates. Right after an update the old process
/// may still be exiting, so `patient` retries for a few seconds.
pub(crate) fn remove_asides(exe: &Path, patient: bool) {
    for aside in asides(exe).filter(|p| p.exists()) {
        let mut tries = if patient { 50 } else { 1 };
        loop {
            match std::fs::remove_file(&aside) {
                Ok(()) => break,
                Err(_) if tries > 1 => std::thread::sleep(Duration::from_millis(100)),
                Err(e) => {
                    log(&format!("can't delete {} yet: {e}", aside.display()));
                    break;
                }
            }
            tries -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (PathBuf, PathBuf, PathBuf) {
        (PathBuf::from(r"F:\apps\glint.exe"), PathBuf::from(r"F:\apps\glint.exe.old"), PathBuf::from(r"C:\u\glint\update\new.exe"))
    }

    fn mv(from: &Path, to: &Path) -> Step {
        Step::Move { from: from.to_path_buf(), to: to.to_path_buf() }
    }

    /// Runs the plan with the step at `fail` failing; returns every step tried.
    fn attempt(fail: Option<usize>) -> (Result<(), String>, Vec<Step>) {
        let (exe, aside, new) = paths();
        let args = ["--updated".to_string(), "0.1.0".to_string()];
        let steps = plan(&exe, &aside, &new, &args);
        let mut tried = Vec::new();
        let result = run(&steps, &mut |s| {
            tried.push(s.clone());
            if Some(tried.len() - 1) == fail { Err(format!("step {} failed", tried.len())) } else { Ok(()) }
        });
        (result, tried)
    }

    #[test]
    fn the_swap_renames_aside_moves_in_and_launches() {
        let (exe, aside, new) = paths();
        let (result, tried) = attempt(None);
        assert_eq!(result, Ok(()));
        let launch = Step::Launch { exe: exe.clone(), args: vec!["--updated".into(), "0.1.0".into()] };
        assert_eq!(tried, [mv(&exe, &aside), mv(&new, &exe), launch]);
    }

    #[test]
    fn a_failed_step_rolls_back_what_was_done() {
        let (exe, aside, new) = paths();
        let (result, tried) = attempt(Some(0));
        assert_eq!((result, tried), (Err("step 1 failed".into()), vec![mv(&exe, &aside)]));

        let (result, tried) = attempt(Some(1));
        assert_eq!(result, Err("step 2 failed".into()));
        assert_eq!(tried, [mv(&exe, &aside), mv(&new, &exe), mv(&aside, &exe)]);

        // the new exe wouldn't start: it goes back to the download folder, the old one returns
        let (result, tried) = attempt(Some(2));
        assert_eq!(result, Err("step 3 failed".into()));
        assert_eq!(tried[3..], [mv(&exe, &new), mv(&aside, &exe)]);
    }

    #[test]
    fn asides_are_numbered() {
        let names: Vec<_> = asides(Path::new(r"C:\a\glint.exe")).map(|p| p.display().to_string()).collect();
        assert_eq!(names[..3], [r"C:\a\glint.exe.old", r"C:\a\glint.exe.old2", r"C:\a\glint.exe.old3"]);
        assert_eq!(names.len(), 9);
    }
}
