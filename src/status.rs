//! What is actually applied right now, and has anything moved underneath us.
//!
//! `diff` answers "what would apply do?" by comparing the desktop against the
//! config. That is not the same question as "did something change what Baton
//! wrote?", and the difference matters: if you hand-edit a generated file, a
//! diff shows a pending change but not *why*, and the next apply silently
//! destroys your edit. Drift detection says so out loud.
//!
//! The reference point is the newest history entry that touched each thing,
//! since that is the last value Baton is responsible for.

use crate::history::Entry;
use crate::plan::Change;
use crate::registry;
use std::collections::BTreeMap;

/// Something Baton has written and therefore still owns.
#[derive(Debug, Clone, PartialEq)]
pub enum Managed {
    File { path: String, written: String },
    Dword { subkey: String, name: String, written: u32 },
}

impl Managed {
    pub fn label(&self) -> String {
        match self {
            Managed::File { path, .. } => path.clone(),
            Managed::Dword { subkey, name, .. } => format!("HKCU\\{subkey}\\{name}"),
        }
    }

    /// Stable identity, so a later apply to the same target supersedes the
    /// earlier one rather than being listed twice.
    fn key(&self) -> String {
        match self {
            Managed::File { path, .. } => format!("file:{}", path.to_lowercase()),
            Managed::Dword { subkey, name, .. } => {
                format!("dword:{}\\{}", subkey.to_lowercase(), name.to_lowercase())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drift {
    /// Still exactly as Baton left it.
    InSync,
    /// Present, but no longer what Baton wrote.
    Changed,
    /// Gone entirely: file deleted, or registry value removed.
    Missing,
}

/// The last value Baton wrote for each thing it touched, newest apply winning.
///
/// Entries must arrive oldest-first, which is the order `history::list` gives.
pub fn last_written(entries: &[Entry]) -> Vec<Managed> {
    let mut by_key: BTreeMap<String, Managed> = BTreeMap::new();

    for entry in entries {
        for change in &entry.changes {
            let managed = match change {
                Change::File { path, after, .. } => Managed::File {
                    path: path.clone(),
                    written: after.clone(),
                },
                Change::Dword { subkey, name, after, .. } => Managed::Dword {
                    subkey: subkey.clone(),
                    name: name.clone(),
                    written: *after,
                },
            };
            by_key.insert(managed.key(), managed);
        }
    }
    by_key.into_values().collect()
}

pub fn check(managed: &Managed) -> Drift {
    match managed {
        Managed::File { path, written } => match std::fs::read_to_string(path) {
            Err(_) => Drift::Missing,
            Ok(current) if current == *written => Drift::InSync,
            Ok(_) => Drift::Changed,
        },
        Managed::Dword { subkey, name, written } => {
            match registry::read_dword(subkey, name) {
                None => Drift::Missing,
                Some(current) if current == *written => Drift::InSync,
                Some(_) => Drift::Changed,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(seq: u64, changes: Vec<Change>) -> Entry {
        Entry { seq, applied_at: 0, changes }
    }

    fn file(path: &str, after: &str) -> Change {
        Change::File {
            path: path.into(),
            before: None,
            after: after.into(),
        }
    }

    fn dword(name: &str, after: u32) -> Change {
        Change::Dword {
            subkey: "Some\\Key".into(),
            name: name.into(),
            before: None,
            after,
        }
    }

    #[test]
    fn nothing_applied_means_nothing_is_owned() {
        assert!(last_written(&[]).is_empty());
    }

    #[test]
    fn the_newest_apply_wins_for_the_same_target() {
        // Three applies rewriting one file: only the last value is ours.
        let entries = vec![
            entry(1, vec![file("a.yaml", "first")]),
            entry(2, vec![file("a.yaml", "second")]),
            entry(3, vec![file("a.yaml", "third")]),
        ];
        let owned = last_written(&entries);
        assert_eq!(owned.len(), 1, "one target, not three");
        assert_eq!(
            owned[0],
            Managed::File { path: "a.yaml".into(), written: "third".into() }
        );
    }

    #[test]
    fn separate_targets_are_tracked_separately() {
        let entries = vec![entry(
            1,
            vec![file("a.yaml", "x"), dword("HideFileExt", 1), file("b.yaml", "y")],
        )];
        assert_eq!(last_written(&entries).len(), 3);
    }

    #[test]
    fn paths_differing_only_in_case_are_the_same_target() {
        // Windows paths are case-insensitive; treating these as two targets
        // would report a phantom second thing to check.
        let entries = vec![
            entry(1, vec![file(r"C:\Users\x\Config.yaml", "first")]),
            entry(2, vec![file(r"c:\users\x\config.yaml", "second")]),
        ];
        let owned = last_written(&entries);
        assert_eq!(owned.len(), 1);
        match &owned[0] {
            Managed::File { written, .. } => assert_eq!(written, "second"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn registry_values_differing_only_in_case_are_the_same_target() {
        let entries = vec![
            entry(1, vec![dword("HideFileExt", 0)]),
            entry(2, vec![dword("hidefileext", 1)]),
        ];
        assert_eq!(last_written(&entries).len(), 1);
    }

    #[test]
    fn a_file_matching_what_we_wrote_is_in_sync() {
        let dir = std::env::temp_dir().join("baton-status-sync");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.yaml");
        std::fs::write(&path, "written by baton\n").unwrap();

        let managed = Managed::File {
            path: path.to_string_lossy().into_owned(),
            written: "written by baton\n".into(),
        };
        assert_eq!(check(&managed), Drift::InSync);

        // Someone edits it by hand.
        std::fs::write(&path, "edited by a human\n").unwrap();
        assert_eq!(check(&managed), Drift::Changed);

        // Or deletes it.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(check(&managed), Drift::Missing);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_registry_value_that_was_never_written_reads_as_missing() {
        let managed = Managed::Dword {
            subkey: r"Software\baton-does-not-exist".into(),
            name: "Nope".into(),
            written: 1,
        };
        assert_eq!(check(&managed), Drift::Missing);
    }

    #[test]
    fn labels_are_readable() {
        assert_eq!(
            Managed::Dword {
                subkey: "Software\\X".into(),
                name: "Y".into(),
                written: 1
            }
            .label(),
            "HKCU\\Software\\X\\Y"
        );
    }
}
