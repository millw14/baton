//! Apply history.
//!
//! Baton used to keep one journal file holding the last apply, which meant
//! applying twice and rolling back once left you at the first apply rather
//! than at your original desktop. That is a surprising place to land, and in
//! practice it left files behind that had to be cleaned up by hand.
//!
//! So every apply appends its own entry. `rollback` undoes the newest and
//! removes it; `rollback --all` walks back to the desktop you started with.
//!
//! Entries must be reverted newest-first. Reverting an older entry while a
//! newer one sits on top of the same value would restore the wrong thing.

use crate::config::state_dir;
use crate::plan::Change;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub seq: u64,
    /// Unix seconds. Stored rather than derived from the file so that copying
    /// the history directory around does not rewrite history.
    pub applied_at: u64,
    pub changes: Vec<Change>,
}

pub fn dir() -> PathBuf {
    state_dir().join("history")
}

fn path_for(seq: u64) -> PathBuf {
    dir().join(format!("{seq:06}.json"))
}

fn legacy_path() -> PathBuf {
    state_dir().join("last-apply.json")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Carry a pre-history journal into the history rather than ignoring it.
/// Silently dropping it would strand changes that were never rolled back.
fn migrate_legacy() {
    let legacy = legacy_path();
    if !legacy.exists() {
        return;
    }
    let migrated = std::fs::read_to_string(&legacy)
        .ok()
        .and_then(|text| serde_json::from_str::<Vec<Change>>(&text).ok())
        .map(|changes| Entry { seq: 1, applied_at: now(), changes });

    if let Some(entry) = migrated {
        if !path_for(1).exists() && write_entry(&entry).is_ok() {
            let _ = std::fs::remove_file(&legacy);
        }
    } else {
        // Unreadable: move it aside so it stops being retried, but keep it.
        let _ = std::fs::rename(&legacy, state_dir().join("last-apply.json.unreadable"));
    }
}

fn write_entry(entry: &Entry) -> Result<()> {
    std::fs::create_dir_all(dir()).with_context(|| format!("creating {}", dir().display()))?;
    let text = serde_json::to_string_pretty(entry)?;
    let tmp = dir().join(format!("{:06}.json.tmp", entry.seq));
    std::fs::write(&tmp, text).context("writing history entry")?;
    std::fs::rename(&tmp, path_for(entry.seq)).context("committing history entry")?;
    Ok(())
}

/// Every entry, oldest first. Unreadable files are skipped rather than
/// aborting: one corrupt entry must not make the rest un-rollbackable.
pub fn list() -> Vec<Entry> {
    migrate_legacy();

    let Ok(read) = std::fs::read_dir(dir()) else {
        return Vec::new();
    };
    let mut out: Vec<Entry> = read
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter_map(|text| serde_json::from_str::<Entry>(&text).ok())
        .collect();
    out.sort_by_key(|e| e.seq);
    out
}

/// "1 apply" / "2 applies". Small, but these strings are the whole interface
/// for a command whose job is to be trusted.
pub fn applies(n: usize) -> String {
    if n == 1 {
        "1 apply".into()
    } else {
        format!("{n} applies")
    }
}

pub fn changes(n: usize) -> String {
    if n == 1 {
        "1 change".into()
    } else {
        format!("{n} changes")
    }
}

/// Record an apply. Called *before* the changes land, so a crash halfway
/// through still leaves the entry that describes how to undo it.
pub fn append(changes: &[Change]) -> Result<Entry> {
    let seq = list().last().map(|e| e.seq + 1).unwrap_or(1);
    let entry = Entry { seq, applied_at: now(), changes: changes.to_vec() };
    write_entry(&entry)?;
    Ok(entry)
}

pub fn remove(seq: u64) -> Result<()> {
    let path = path_for(seq);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

pub fn clear() {
    let _ = std::fs::remove_dir_all(dir());
    let _ = std::fs::remove_file(legacy_path());
}

/// "3 minutes ago". Avoids pulling in a date library for what is only ever
/// shown to a human deciding which apply to undo.
pub fn ago(then: u64, now_secs: u64) -> String {
    if then == 0 || now_secs < then {
        return "just now".into();
    }
    let secs = now_secs - then;
    let (n, unit) = match secs {
        0..=59 => return "just now".into(),
        60..=3599 => (secs / 60, "minute"),
        3600..=86_399 => (secs / 3600, "hour"),
        _ => (secs / 86_400, "day"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

pub fn ago_now(then: u64) -> String {
    ago(then, now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Change;

    fn change(name: &str) -> Change {
        Change::File {
            path: name.into(),
            before: None,
            after: "x".into(),
        }
    }

    /// Environment variables are process-wide but Rust runs tests in parallel,
    /// so anything redirecting `state_dir()` has to take a turn.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Point `state_dir()` at a scratch directory for the duration of a test.
    fn with_temp_state<R>(tag: &str, f: impl FnOnce() -> R) -> R {
        // A panic in one test poisons the lock; that must not cascade into
        // every other test failing for an unrelated reason.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let previous = std::env::var("LOCALAPPDATA").ok();
        let dir = std::env::temp_dir().join(format!("baton-history-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("LOCALAPPDATA", &dir);

        let out = f();

        match previous {
            Some(v) => std::env::set_var("LOCALAPPDATA", v),
            None => std::env::remove_var("LOCALAPPDATA"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    #[test]
    fn entries_accumulate_and_stay_ordered() {
        with_temp_state("accumulate", || {
            assert!(list().is_empty());
            append(&[change("a")]).unwrap();
            append(&[change("b")]).unwrap();
            append(&[change("c")]).unwrap();

            let all = list();
            assert_eq!(all.len(), 3);
            assert_eq!(all.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2, 3]);
            assert_eq!(list().pop().unwrap().seq, 3);
        });
    }

    #[test]
    fn removing_the_newest_exposes_the_one_before_it() {
        with_temp_state("pop", || {
            append(&[change("a")]).unwrap();
            append(&[change("b")]).unwrap();

            let top = list().pop().unwrap();
            assert_eq!(top.seq, 2);
            remove(top.seq).unwrap();

            // This is the whole point: we land on the previous apply, and can
            // keep going back rather than being stuck.
            assert_eq!(list().pop().unwrap().seq, 1);
            remove(1).unwrap();
            assert!(list().pop().is_none());
        });
    }

    #[test]
    fn sequence_numbers_do_not_get_reused_after_a_removal() {
        with_temp_state("seq", || {
            append(&[change("a")]).unwrap();
            append(&[change("b")]).unwrap();
            remove(2).unwrap();
            // Next append continues from the surviving max, so an old entry
            // file can never be silently overwritten.
            assert_eq!(append(&[change("c")]).unwrap().seq, 2);
        });
    }

    #[test]
    fn removing_something_already_gone_is_not_an_error() {
        with_temp_state("idempotent", || {
            remove(42).unwrap();
        });
    }

    #[test]
    fn a_corrupt_entry_does_not_hide_the_healthy_ones() {
        with_temp_state("corrupt", || {
            append(&[change("a")]).unwrap();
            append(&[change("b")]).unwrap();
            std::fs::write(dir().join("000002.json"), "{ not json").unwrap();

            let all = list();
            assert_eq!(all.len(), 1, "the readable entry must survive");
            assert_eq!(all[0].seq, 1);
        });
    }

    #[test]
    fn a_pre_history_journal_is_carried_over_not_dropped() {
        with_temp_state("migrate", || {
            std::fs::create_dir_all(state_dir()).unwrap();
            let legacy = serde_json::to_string(&vec![change("old")]).unwrap();
            std::fs::write(legacy_path(), legacy).unwrap();

            let all = list();
            assert_eq!(all.len(), 1, "legacy journal must become an entry");
            assert_eq!(all[0].seq, 1);
            assert!(!legacy_path().exists(), "and be consumed");
        });
    }

    #[test]
    fn entries_survive_a_round_trip() {
        with_temp_state("roundtrip", || {
            let written = append(&[change("a"), change("b")]).unwrap();
            let read = list().pop().unwrap();
            assert_eq!(written, read);
        });
    }

    #[test]
    fn counts_are_pluralised() {
        assert_eq!(applies(1), "1 apply");
        assert_eq!(applies(2), "2 applies");
        assert_eq!(applies(0), "0 applies");
        assert_eq!(changes(1), "1 change");
        assert_eq!(changes(3), "3 changes");
    }

    #[test]
    fn relative_times_read_naturally() {
        assert_eq!(ago(100, 100), "just now");
        assert_eq!(ago(100, 130), "just now");
        assert_eq!(ago(0, 60), "just now");
        assert_eq!(ago(100, 160), "1 minute ago");
        assert_eq!(ago(100, 400), "5 minutes ago");
        assert_eq!(ago(100, 100 + 3600), "1 hour ago");
        assert_eq!(ago(100, 100 + 86_400 * 3), "3 days ago");
        // A clock that went backwards must not underflow.
        assert_eq!(ago(500, 100), "just now");
    }
}
