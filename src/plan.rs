//! Planning, applying, and undoing.
//!
//! Nothing is ever written blind. A plan is a list of `Change`s, each carrying
//! the value that was there *before* it. That single fact gives us three things
//! for free: `diff` is just the plan printed, `apply` is the plan executed, and
//! `rollback` is the plan executed backwards.

use crate::config::{BarBackend, Config, WindowsSettings};
use crate::{glazewm, registry, zebar};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const EXPLORER_ADVANCED: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced";
pub const PERSONALIZE: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
pub const DWM: &str = r"Software\Microsoft\Windows\DWM";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind")]
pub enum Change {
    File {
        path: String,
        /// `None` means the file did not exist, so undoing means deleting it.
        before: Option<String>,
        after: String,
    },
    Dword {
        subkey: String,
        name: String,
        /// `None` means the value did not exist, so undoing means removing it.
        before: Option<u32>,
        after: u32,
    },
}

impl Change {
    pub fn describe(&self) -> String {
        match self {
            Change::File { path, before, .. } => match before {
                None => format!("create  {path}"),
                Some(_) => format!("rewrite {path}"),
            },
            Change::Dword { subkey, name, before, after } => {
                let from = before
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unset".into());
                format!("set     HKCU\\{subkey}\\{name}: {from} -> {after}")
            }
        }
    }

    /// Is this a no-op? Planning drops these so `diff` shows only real work.
    fn is_noop(&self) -> bool {
        match self {
            Change::File { before, after, .. } => before.as_deref() == Some(after.as_str()),
            Change::Dword { before, after, .. } => *before == Some(*after),
        }
    }

    pub fn apply(&self) -> Result<()> {
        match self {
            Change::File { path, after, .. } => {
                let p = PathBuf::from(path);
                if let Some(dir) = p.parent() {
                    std::fs::create_dir_all(dir)
                        .with_context(|| format!("creating {}", dir.display()))?;
                }
                std::fs::write(&p, after).with_context(|| format!("writing {path}"))
            }
            Change::Dword { subkey, name, after, .. } => {
                registry::write_dword(subkey, name, *after)
            }
        }
    }

    pub fn revert(&self) -> Result<()> {
        match self {
            Change::File { path, before, .. } => match before {
                Some(text) => std::fs::write(path, text)
                    .with_context(|| format!("restoring {path}")),
                None => {
                    // We created it; removing an already-gone file is fine.
                    match std::fs::remove_file(path) {
                        Ok(()) => Ok(()),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                        Err(e) => Err(e).with_context(|| format!("removing {path}")),
                    }
                }
            },
            Change::Dword { subkey, name, before, .. } => match before {
                Some(v) => registry::write_dword(subkey, name, *v),
                None => registry::delete_value(subkey, name),
            },
        }
    }
}

/// The registry values a config asks for, independent of what is currently set.
///
/// Pure and therefore fully testable, which matters because several of these
/// settings are stored inverted and getting one backwards would silently do
/// the opposite of what the user asked.
pub fn desired_registry(w: &WindowsSettings) -> Vec<(&'static str, &'static str, u32)> {
    let mut out = Vec::new();

    // Stored as "uses LIGHT theme", so dark mode is the zero.
    if let Some(dark) = w.dark_mode {
        out.push((PERSONALIZE, "SystemUsesLightTheme", u32::from(!dark)));
    }
    if let Some(dark) = w.app_dark_mode {
        out.push((PERSONALIZE, "AppsUseLightTheme", u32::from(!dark)));
    }
    // Stored as "HIDE extensions", so showing them is the zero.
    if let Some(show) = w.show_file_extensions {
        out.push((EXPLORER_ADVANCED, "HideFileExt", u32::from(!show)));
    }
    // This one is not a bool: 1 shows hidden files, 2 hides them.
    if let Some(show) = w.show_hidden_files {
        out.push((EXPLORER_ADVANCED, "Hidden", if show { 1 } else { 2 }));
    }
    if let Some(small) = w.taskbar_small_icons {
        out.push((EXPLORER_ADVANCED, "TaskbarSmallIcons", u32::from(small)));
    }
    if let Some(colour) = &w.accent_color {
        if let Some(abgr) = hex_to_abgr(colour) {
            // DWM stores colour channels reversed, and the accent is spread
            // across three values that are expected to agree.
            for name in ["AccentColor", "ColorizationColor", "ColorizationAfterglow"] {
                out.push((DWM, name, abgr));
            }
        }
    }
    out
}

/// `#rrggbb` -> `0xAABBGGRR`. Windows stores DWM colours channel-reversed;
/// writing RGB straight through gives you a colour with red and blue swapped.
fn hex_to_abgr(hex: &str) -> Option<u32> {
    let h = hex.strip_prefix('#')?;
    if h.len() < 6 {
        return None;
    }
    let r = u32::from_str_radix(&h[0..2], 16).ok()?;
    let g = u32::from_str_radix(&h[2..4], 16).ok()?;
    let b = u32::from_str_radix(&h[4..6], 16).ok()?;
    Some(0xFF00_0000 | (b << 16) | (g << 8) | r)
}

/// Pair a target path with its rendered contents, snapshotting whatever is
/// there now so the change knows how to undo itself.
fn file_change(path: PathBuf, after: String) -> Change {
    let before = std::fs::read_to_string(&path).ok();
    Change::File {
        path: path.to_string_lossy().into_owned(),
        before,
        after,
    }
}

/// Work out everything that would have to change for the desktop to match
/// `cfg`. Reads current state; writes nothing.
pub fn build(cfg: &Config) -> Result<Vec<Change>> {
    let mut changes = Vec::new();

    if cfg.wm.backend == crate::config::Backend::Glazewm {
        changes.push(file_change(glazewm::config_path(), glazewm::render(cfg)?));
    }

    if cfg.bar.backend == BarBackend::Zebar {
        changes.push(file_change(zebar::settings_path(), zebar::render_settings(cfg)?));
        // Only worth writing if there is actually a palette to export.
        if !cfg.palette.is_empty() {
            changes.push(file_change(
                zebar::palette_css_path(),
                zebar::render_palette_css(cfg),
            ));
        }
    }

    for (subkey, name, after) in desired_registry(&cfg.windows) {
        changes.push(Change::Dword {
            subkey: subkey.to_string(),
            name: name.to_string(),
            before: registry::read_dword(subkey, name),
            after,
        });
    }

    changes.retain(|c| !c.is_noop());
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(toml: &str) -> WindowsSettings {
        Config::parse(toml).unwrap().windows
    }

    #[test]
    fn nothing_declared_means_nothing_touched() {
        assert!(desired_registry(&settings("")).is_empty());
    }

    #[test]
    fn dark_mode_is_stored_inverted() {
        let on = desired_registry(&settings("[windows]\ndark_mode = true"));
        assert_eq!(on, vec![(PERSONALIZE, "SystemUsesLightTheme", 0)]);

        let off = desired_registry(&settings("[windows]\ndark_mode = false"));
        assert_eq!(off, vec![(PERSONALIZE, "SystemUsesLightTheme", 1)]);
    }

    #[test]
    fn file_extensions_are_stored_inverted() {
        let show = desired_registry(&settings("[windows]\nshow_file_extensions = true"));
        assert_eq!(show, vec![(EXPLORER_ADVANCED, "HideFileExt", 0)]);
    }

    #[test]
    fn hidden_files_use_one_and_two_not_a_bool() {
        let show = desired_registry(&settings("[windows]\nshow_hidden_files = true"));
        assert_eq!(show, vec![(EXPLORER_ADVANCED, "Hidden", 1)]);
        let hide = desired_registry(&settings("[windows]\nshow_hidden_files = false"));
        assert_eq!(hide, vec![(EXPLORER_ADVANCED, "Hidden", 2)]);
    }

    #[test]
    fn accent_colour_is_channel_reversed() {
        // Pure red must not come out as pure blue.
        assert_eq!(hex_to_abgr("#ff0000"), Some(0xFF0000FF));
        assert_eq!(hex_to_abgr("#0000ff"), Some(0xFFFF0000));
        assert_eq!(hex_to_abgr("#8aadf4"), Some(0xFFF4AD8A));
        assert_eq!(hex_to_abgr("nope"), None);
    }

    #[test]
    fn accent_colour_writes_the_three_values_that_must_agree() {
        let out = desired_registry(&settings("[windows]\naccent_color = \"#8aadf4\""));
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|(k, _, v)| *k == DWM && *v == 0xFFF4AD8A));
    }

    #[test]
    fn no_op_changes_are_dropped_from_a_plan() {
        let same = Change::Dword {
            subkey: "x".into(),
            name: "y".into(),
            before: Some(1),
            after: 1,
        };
        assert!(same.is_noop());

        let different = Change::Dword {
            subkey: "x".into(),
            name: "y".into(),
            before: Some(0),
            after: 1,
        };
        assert!(!different.is_noop());

        // An absent value being set to zero is real work, not a no-op.
        let creating = Change::Dword {
            subkey: "x".into(),
            name: "y".into(),
            before: None,
            after: 0,
        };
        assert!(!creating.is_noop());
    }

    #[test]
    fn a_created_file_is_deleted_on_revert_not_blanked() {
        let dir = std::env::temp_dir().join("baton-test-revert");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("made-by-baton.yaml");

        let change = Change::File {
            path: path.to_string_lossy().into_owned(),
            before: None,
            after: "hello".into(),
        };
        change.apply().unwrap();
        assert!(path.exists());

        change.revert().unwrap();
        assert!(!path.exists(), "revert must remove a file baton created");
        // And reverting twice must not error.
        change.revert().unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_file_is_restored_byte_for_byte() {
        let dir = std::env::temp_dir().join("baton-test-restore");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        std::fs::write(&path, "original contents\n").unwrap();

        let change = Change::File {
            path: path.to_string_lossy().into_owned(),
            before: Some("original contents\n".into()),
            after: "replaced\n".into(),
        };
        change.apply().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "replaced\n");

        change.revert().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original contents\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn changes_survive_a_serialisation_round_trip() {
        let changes = vec![
            Change::File { path: "a.yaml".into(), before: None, after: "x".into() },
            Change::Dword {
                subkey: PERSONALIZE.into(),
                name: "SystemUsesLightTheme".into(),
                before: Some(1),
                after: 0,
            },
        ];
        let json = serde_json::to_string(&changes).unwrap();
        let back: Vec<Change> = serde_json::from_str(&json).unwrap();
        assert_eq!(changes, back);
    }

    #[test]
    fn descriptions_distinguish_creating_from_rewriting() {
        let create = Change::File { path: "a".into(), before: None, after: "x".into() };
        assert!(create.describe().starts_with("create"));
        let rewrite = Change::File {
            path: "a".into(),
            before: Some("old".into()),
            after: "x".into(),
        };
        assert!(rewrite.describe().starts_with("rewrite"));
    }
}
