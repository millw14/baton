//! The one config file.
//!
//! The whole point of Baton is that a colour is declared once and referenced
//! everywhere, so parsing happens in two passes: read the raw TOML, resolve
//! every `"@palette.name"` reference against the palette, and only then
//! deserialize into typed target settings. That way palette references work in
//! any target, at any depth, without each target knowing about the palette.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub meta: Meta,
    /// Named colours, referenced elsewhere as "@palette.<name>".
    #[serde(default)]
    pub palette: BTreeMap<String, String>,
    #[serde(default)]
    pub wm: Wm,
    /// Shell and theme settings applied through the registry.
    #[serde(default)]
    pub windows: WindowsSettings,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Glazewm,
    /// The from-scratch fallback in ../winrice. No external dependency.
    Winrice,
    /// Manage nothing; leave window management alone.
    None,
}

// Some fields are parsed and validated before the renderer that consumes them
// exists. That is deliberate: the schema is the contract, and locking it down
// early means the example config and its tests stay honest.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wm {
    #[serde(default = "d_backend")]
    pub backend: Backend,
    #[serde(default)]
    pub gaps: Gaps,
    #[serde(default = "d_workspaces")]
    pub workspaces: Vec<String>,
    #[serde(default)]
    pub focus_follows_cursor: bool,
    /// Border colour of the focused window. Usually "@palette.accent".
    #[serde(default)]
    pub focused_border: Option<String>,
    /// Keybind spec -> backend command.
    #[serde(default)]
    pub keys: BTreeMap<String, String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gaps {
    #[serde(default = "d_gap")]
    pub inner: u32,
    #[serde(default = "d_gap")]
    pub outer: u32,
}

/// Every field is optional on purpose: Baton only touches what you declare.
/// An absent field is never written and never rolled back.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowsSettings {
    #[serde(default)]
    pub dark_mode: Option<bool>,
    #[serde(default)]
    pub app_dark_mode: Option<bool>,
    #[serde(default)]
    pub show_file_extensions: Option<bool>,
    #[serde(default)]
    pub show_hidden_files: Option<bool>,
    #[serde(default)]
    pub taskbar_small_icons: Option<bool>,
    #[serde(default)]
    pub accent_color: Option<String>,
}

fn d_backend() -> Backend {
    Backend::Glazewm
}
fn d_gap() -> u32 {
    10
}
fn d_workspaces() -> Vec<String> {
    (1..=5).map(|n| n.to_string()).collect()
}

impl Default for Wm {
    fn default() -> Self {
        Wm {
            backend: d_backend(),
            gaps: Gaps::default(),
            workspaces: d_workspaces(),
            focus_follows_cursor: false,
            focused_border: None,
            keys: BTreeMap::new(),
        }
    }
}

impl Default for Gaps {
    fn default() -> Self {
        Gaps { inner: d_gap(), outer: d_gap() }
    }
}

const PALETTE_PREFIX: &str = "@palette.";

impl Config {
    pub fn parse(text: &str) -> Result<Config> {
        let mut raw: toml::Value = toml::from_str(text).context("config is not valid TOML")?;

        // Pass 1: pull the palette out before anything references it.
        let palette = read_palette(&raw)?;
        for (name, colour) in &palette {
            validate_colour(colour)
                .with_context(|| format!("palette.{name}"))?;
        }

        // Pass 2: substitute references everywhere else.
        resolve(&mut raw, &palette, "").context("resolving palette references")?;

        let cfg: Config = raw.try_into().context("config has an unexpected shape")?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        Config::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.wm.workspaces.is_empty(),
            "wm.workspaces cannot be empty"
        );
        anyhow::ensure!(
            self.wm.workspaces.len() <= 20,
            "wm.workspaces: {} is more than the 20 Baton supports",
            self.wm.workspaces.len()
        );
        let mut seen = std::collections::HashSet::new();
        for ws in &self.wm.workspaces {
            anyhow::ensure!(seen.insert(ws), "wm.workspaces has a duplicate: {ws:?}");
        }
        if let Some(c) = &self.wm.focused_border {
            validate_colour(c).context("wm.focused_border")?;
        }
        if let Some(c) = &self.windows.accent_color {
            validate_colour(c).context("windows.accent_color")?;
        }
        Ok(())
    }
}

fn read_palette(raw: &toml::Value) -> Result<BTreeMap<String, String>> {
    let Some(table) = raw.get("palette") else {
        return Ok(BTreeMap::new());
    };
    let table = table
        .as_table()
        .ok_or_else(|| anyhow!("[palette] must be a table of name = \"#rrggbb\""))?;

    let mut out = BTreeMap::new();
    for (name, value) in table {
        let colour = value
            .as_str()
            .ok_or_else(|| anyhow!("palette.{name} must be a string"))?;
        out.insert(name.clone(), colour.to_string());
    }
    Ok(out)
}

/// Walk the whole document replacing `"@palette.name"` with its colour.
///
/// `path` is carried purely so an unresolved reference can say where it was.
fn resolve(value: &mut toml::Value, palette: &BTreeMap<String, String>, path: &str) -> Result<()> {
    match value {
        toml::Value::String(s) => {
            // "@@palette.x" is the escape for a literal "@palette.x".
            if let Some(rest) = s.strip_prefix('@') {
                if rest.starts_with('@') {
                    *s = rest.to_string();
                    return Ok(());
                }
            }
            if let Some(name) = s.strip_prefix(PALETTE_PREFIX) {
                let colour = palette.get(name).ok_or_else(|| {
                    let known: Vec<&str> = palette.keys().map(String::as_str).collect();
                    anyhow!(
                        "{path} references @palette.{name}, which is not defined. \
                         Defined colours: {}",
                        if known.is_empty() { "(none)".into() } else { known.join(", ") }
                    )
                })?;
                *s = colour.clone();
            }
            Ok(())
        }
        toml::Value::Array(items) => {
            for (i, item) in items.iter_mut().enumerate() {
                resolve(item, palette, &format!("{path}[{i}]"))?;
            }
            Ok(())
        }
        toml::Value::Table(table) => {
            for (key, item) in table.iter_mut() {
                // The palette defines colours; it does not reference them.
                if path.is_empty() && key == "palette" {
                    continue;
                }
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                resolve(item, palette, &child)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_colour(c: &str) -> Result<()> {
    let Some(hex) = c.strip_prefix('#') else {
        bail!("{c:?} must start with '#'");
    };
    if !matches!(hex.len(), 6 | 8) {
        bail!("{c:?} must be #rrggbb or #rrggbbaa");
    }
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("{c:?} contains a non-hex digit");
    }
    Ok(())
}

/// `~/.config/baton/baton.toml`, matching the dotfile convention the rest of
/// this stack already uses.
pub fn config_path() -> PathBuf {
    if let Ok(explicit) = std::env::var("BATON_CONFIG") {
        return PathBuf::from(explicit);
    }
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    PathBuf::from(home).join(".config").join("baton").join("baton.toml")
}

/// Where snapshots and the rollback journal live. Used by `apply`.
#[allow(dead_code)]
pub fn state_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .or_else(|_| std::env::var("TEMP"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("baton")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_valid_and_defaults_sanely() {
        let cfg = Config::parse("").unwrap();
        assert_eq!(cfg.wm.backend, Backend::Glazewm);
        assert_eq!(cfg.wm.gaps.inner, 10);
        assert_eq!(cfg.wm.workspaces.len(), 5);
        // Nothing declared means nothing gets touched.
        assert!(cfg.windows.dark_mode.is_none());
    }

    #[test]
    fn palette_reference_resolves_across_targets() {
        let cfg = Config::parse(
            r##"
            [palette]
            accent = "#8aadf4"

            [wm]
            focused_border = "@palette.accent"

            [windows]
            accent_color = "@palette.accent"
            "##,
        )
        .unwrap();
        assert_eq!(cfg.wm.focused_border.as_deref(), Some("#8aadf4"));
        assert_eq!(cfg.windows.accent_color.as_deref(), Some("#8aadf4"));
    }

    #[test]
    fn unknown_reference_names_itself_and_lists_what_exists() {
        let err = Config::parse(
            r##"
            [palette]
            accent = "#8aadf4"
            [wm]
            focused_border = "@palette.acccent"
            "##,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("acccent"), "should name the typo: {msg}");
        assert!(msg.contains("accent"), "should list known colours: {msg}");
    }

    #[test]
    fn references_resolve_inside_nested_tables() {
        let cfg = Config::parse(
            r##"
            [palette]
            accent = "#ff0000"
            [wm.keys]
            "alt+j" = "focus --direction right"
            "##,
        )
        .unwrap();
        // Plain strings must survive untouched.
        assert_eq!(
            cfg.wm.keys.get("alt+j").map(String::as_str),
            Some("focus --direction right")
        );
    }

    #[test]
    fn double_at_escapes_a_literal() {
        let cfg = Config::parse(
            r#"
            [meta]
            name = "@@palette.accent"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.meta.name.as_deref(), Some("@palette.accent"));
    }

    #[test]
    fn bad_colours_are_rejected_with_the_field_name() {
        let err = Config::parse("[palette]\naccent = \"8aadf4\"").unwrap_err();
        assert!(format!("{err:#}").contains("palette.accent"));

        assert!(Config::parse("[palette]\nx = \"#12345\"").is_err());
        assert!(Config::parse("[palette]\nx = \"#gggggg\"").is_err());
        assert!(Config::parse("[palette]\nx = \"#12345678\"").is_ok(), "rgba is fine");
    }

    #[test]
    fn duplicate_workspaces_are_rejected() {
        let err = Config::parse(r#"[wm]
workspaces = ["1", "2", "1"]"#)
            .unwrap_err();
        assert!(format!("{err:#}").contains("duplicate"));
    }

    #[test]
    fn empty_workspaces_are_rejected() {
        assert!(Config::parse("[wm]\nworkspaces = []").is_err());
    }

    #[test]
    fn typos_in_key_names_are_caught_rather_than_silently_ignored() {
        // deny_unknown_fields: a misspelled setting must fail loudly, or the
        // user thinks they configured something they did not.
        assert!(Config::parse("[windows]\ndarkmode = true").is_err());
    }
}
