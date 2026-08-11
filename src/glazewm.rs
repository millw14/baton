//! Renders `[wm]` into GlazeWM's own config format.
//!
//! The shape here is not guessed: it is the schema verified to start
//! GlazeWM 3.10.1 successfully on Windows 10. GlazeWM is a GUI-subsystem
//! binary, so a bad config produces no console output at all and it simply
//! dies -- the errors go to `~/.glzr/glazewm/errors.log`.

use crate::config::{Config, Rule, RuleAction};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Serialize)]
struct GlazeConfig {
    general: General,
    gaps: Gaps,
    window_effects: WindowEffects,
    window_behavior: WindowBehavior,
    workspaces: Vec<Workspace>,
    window_rules: Vec<WindowRule>,
    keybindings: Vec<Keybinding>,
}

#[derive(Serialize)]
struct WindowRule {
    commands: Vec<String>,
    /// GlazeWM ORs the entries in this list. Baton emits exactly one entry per
    /// rule, whose fields AND together, which is the intuitive reading of
    /// "exe = x, title = y".
    #[serde(rename = "match")]
    matchers: Vec<Matcher>,
}

#[derive(Serialize, Default)]
struct Matcher {
    #[serde(skip_serializing_if = "Option::is_none")]
    window_process: Option<Pattern>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_class: Option<Pattern>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_title: Option<Pattern>,
}

#[derive(Serialize)]
struct Pattern {
    regex: String,
}

#[derive(Serialize)]
struct General {
    startup_commands: Vec<String>,
    shutdown_commands: Vec<String>,
    config_reload_commands: Vec<String>,
    focus_follows_cursor: bool,
    toggle_workspace_on_refocus: bool,
    hide_method: String,
    show_all_in_taskbar: bool,
}

#[derive(Serialize)]
struct Gaps {
    scale_with_dpi: bool,
    inner_gap: String,
    outer_gap: OuterGap,
}

#[derive(Serialize)]
struct OuterGap {
    top: String,
    right: String,
    bottom: String,
    left: String,
}

#[derive(Serialize)]
struct WindowEffects {
    focused_window: EffectGroup,
    other_windows: EffectGroup,
}

#[derive(Serialize)]
struct EffectGroup {
    border: Border,
}

#[derive(Serialize)]
struct Border {
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<String>,
}

#[derive(Serialize)]
struct WindowBehavior {
    initial_state: String,
    state_defaults: StateDefaults,
}

#[derive(Serialize)]
struct StateDefaults {
    floating: FloatingDefaults,
    fullscreen: FullscreenDefaults,
}

#[derive(Serialize)]
struct FloatingDefaults {
    centered: bool,
    shown_on_top: bool,
}

#[derive(Serialize)]
struct FullscreenDefaults {
    maximized: bool,
    shown_on_top: bool,
}

#[derive(Serialize)]
struct Workspace {
    name: String,
}

#[derive(Serialize)]
struct Keybinding {
    commands: Vec<String>,
    bindings: Vec<String>,
}

/// Translate one of Baton's matcher values into a GlazeWM regex.
///
/// Always a regex, never `equals`, so that matching is uniformly
/// case-insensitive: `(?i)^chrome$` is exactly a case-insensitive equals, and
/// `*` becomes `.*` without the user needing to know regex.
fn to_regex(pattern: &str) -> String {
    let mut out = String::from("(?i)^");
    for ch in pattern.chars() {
        match ch {
            '*' => out.push_str(".*"),
            // Anything with meaning in a regex has to survive as a literal.
            c if r"\.+?()|[]{}^$".contains(c) => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push('$');
    out
}

/// GlazeWM matches on the process name without its extension, but people write
/// "chrome.exe" out of habit. Accept both.
fn process_name(exe: &str) -> &str {
    exe.strip_suffix(".exe")
        .or_else(|| exe.strip_suffix(".EXE"))
        .unwrap_or(exe)
}

fn render_rule(rule: &Rule) -> WindowRule {
    let mut commands = Vec::new();
    match rule.action {
        Some(RuleAction::Ignore) => commands.push("ignore".to_string()),
        Some(RuleAction::Float) => commands.push("set-floating".to_string()),
        Some(RuleAction::Tile) => commands.push("set-tiling".to_string()),
        None => {}
    }
    if let Some(ws) = &rule.workspace {
        commands.push(format!("move --workspace {ws}"));
    }

    WindowRule {
        commands,
        matchers: vec![Matcher {
            window_process: rule
                .exe
                .as_deref()
                .map(|e| Pattern { regex: to_regex(process_name(e)) }),
            window_class: rule
                .class
                .as_deref()
                .map(|c| Pattern { regex: to_regex(c) }),
            window_title: rule
                .title
                .as_deref()
                .map(|t| Pattern { regex: to_regex(t) }),
        }],
    }
}

pub fn config_path() -> PathBuf {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    PathBuf::from(home).join(".glzr").join("glazewm").join("config.yaml")
}

/// Where the CLI usually lives. The installer does not put it on PATH, so
/// looking in the known locations first avoids an unhelpful "not found".
fn cli_path() -> PathBuf {
    let candidates = [
        std::env::var("ProgramFiles")
            .map(|p| PathBuf::from(p).join(r"glzr.io\GlazeWM\cli\glazewm.exe")),
        std::env::var("ProgramFiles")
            .map(|p| PathBuf::from(p).join(r"glzr.io\GlazeWM\glazewm.exe")),
        std::env::var("LOCALAPPDATA")
            .map(|p| PathBuf::from(p).join(r"Programs\glzr.io\GlazeWM\cli\glazewm.exe")),
    ];
    for candidate in candidates.into_iter().flatten() {
        if candidate.exists() {
            return candidate;
        }
    }
    // Last resort: hope it is on PATH.
    PathBuf::from("glazewm")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reload {
    /// GlazeWM picked up the new config.
    Done,
    /// GlazeWM is not running, so there is nothing to reload. Not an error:
    /// writing a config for a WM you will start later is perfectly normal.
    NotRunning,
    /// The CLI could not be found or run at all.
    Unavailable(&'static str),
}

/// Decide what a CLI invocation actually meant.
///
/// Split out from the process call so the classification is testable: the
/// difference between "not running" and "broken" decides whether `apply`
/// reports a problem or stays quiet.
fn classify(success: bool, stderr: &str) -> Reload {
    if success {
        return Reload::Done;
    }
    let lower = stderr.to_ascii_lowercase();
    // GlazeWM's CLI says "Failed to connect to IPC server" with a WSAECONNREFUSED
    // (10061) underneath when no instance is listening.
    if lower.contains("connect") || lower.contains("10061") || lower.contains("ipc") {
        Reload::NotRunning
    } else {
        Reload::Unavailable("the GlazeWM CLI returned an error")
    }
}

/// Read-only liveness check: a query that needs no side effects.
pub fn is_running() -> bool {
    std::process::Command::new(cli_path())
        .args(["query", "app-metadata"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Ask a running GlazeWM to re-read its config.
pub fn reload() -> Reload {
    let output = std::process::Command::new(cli_path())
        .args(["command", "wm-reload-config"])
        .output();

    match output {
        Ok(out) => classify(out.status.success(), &String::from_utf8_lossy(&out.stderr)),
        Err(_) => Reload::Unavailable("could not run the GlazeWM CLI"),
    }
}

/// Turn Baton's `[wm]` section into a complete GlazeWM config document.
pub fn render(cfg: &Config) -> anyhow::Result<String> {
    let wm = &cfg.wm;

    let doc = GlazeConfig {
        general: General {
            startup_commands: Vec::new(),
            shutdown_commands: Vec::new(),
            config_reload_commands: Vec::new(),
            focus_follows_cursor: wm.focus_follows_cursor,
            toggle_workspace_on_refocus: false,
            // Cloaking beats SW_HIDE: a cloaked window keeps its state and
            // cannot be stranded invisible the way a hidden one can.
            hide_method: "cloak".into(),
            show_all_in_taskbar: false,
        },
        gaps: Gaps {
            scale_with_dpi: true,
            inner_gap: format!("{}px", wm.gaps.inner),
            outer_gap: OuterGap {
                top: format!("{}px", wm.gaps.outer),
                right: format!("{}px", wm.gaps.outer),
                bottom: format!("{}px", wm.gaps.outer),
                left: format!("{}px", wm.gaps.outer),
            },
        },
        window_effects: WindowEffects {
            focused_window: EffectGroup {
                border: Border {
                    enabled: wm.focused_border.is_some(),
                    color: wm.focused_border.clone(),
                },
            },
            other_windows: EffectGroup {
                border: Border { enabled: false, color: None },
            },
        },
        window_behavior: WindowBehavior {
            initial_state: "tiling".into(),
            state_defaults: StateDefaults {
                floating: FloatingDefaults { centered: true, shown_on_top: false },
                fullscreen: FullscreenDefaults { maximized: false, shown_on_top: false },
            },
        },
        workspaces: wm
            .workspaces
            .iter()
            .map(|name| Workspace { name: name.clone() })
            .collect(),
        window_rules: cfg.rules.iter().map(render_rule).collect(),
        keybindings: wm
            .keys
            .iter()
            .map(|(binding, command)| Keybinding {
                commands: vec![command.clone()],
                bindings: vec![binding.clone()],
            })
            .collect(),
    };

    let body = serde_yaml::to_string(&doc)?;
    Ok(format!(
        "# Generated by baton. Do not edit; edit baton.toml and run `baton apply`.\n\
         # Your previous file was saved and can be restored with `baton rollback`.\n\
         {body}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Config {
        Config::parse(text).unwrap()
    }

    #[test]
    fn renders_a_document_with_every_section_glazewm_requires() {
        let out = render(&parse("")).unwrap();
        for section in [
            "general:",
            "gaps:",
            "window_effects:",
            "window_behavior:",
            "workspaces:",
            "keybindings:",
        ] {
            assert!(out.contains(section), "missing {section} in:\n{out}");
        }
    }

    #[test]
    fn gaps_are_emitted_in_the_px_form_glazewm_expects() {
        let out = render(&parse("[wm.gaps]\ninner = 14\nouter = 7")).unwrap();
        assert!(out.contains("inner_gap: 14px"), "{out}");
        assert!(out.contains("top: 7px"), "{out}");
    }

    #[test]
    fn resolved_palette_colour_reaches_the_border() {
        let cfg = parse(
            r##"
            [palette]
            accent = "#8aadf4"
            [wm]
            focused_border = "@palette.accent"
            "##,
        );
        let out = render(&cfg).unwrap();
        assert!(out.contains("#8aadf4"), "{out}");
        assert!(out.contains("enabled: true"), "{out}");
        // No unresolved reference may ever reach a downstream tool.
        assert!(!out.contains("@palette"), "{out}");
    }

    #[test]
    fn no_border_colour_means_the_border_is_disabled() {
        let out = render(&parse("")).unwrap();
        assert!(out.contains("enabled: false"), "{out}");
        assert!(!out.contains("color:"), "absent colour must be omitted:\n{out}");
    }

    #[test]
    fn keybinds_become_one_binding_entry_each() {
        let cfg = parse(
            r##"
            [wm.keys]
            "alt+j" = "focus --direction down"
            "alt+k" = "focus --direction up"
            "##,
        );
        let out = render(&cfg).unwrap();
        assert_eq!(out.matches("- commands:").count(), 2, "{out}");
        assert!(out.contains("alt+j"), "{out}");
        assert!(out.contains("focus --direction down"), "{out}");
    }

    #[test]
    fn workspaces_are_rendered_in_order() {
        let cfg = parse(r#"[wm]
workspaces = ["web", "code", "chat"]"#);
        let out = render(&cfg).unwrap();
        let web = out.find("web").unwrap();
        let code = out.find("code").unwrap();
        let chat = out.find("chat").unwrap();
        assert!(web < code && code < chat, "{out}");
    }

    #[test]
    fn output_is_valid_yaml_that_round_trips() {
        let out = render(&parse("")).unwrap();
        let parsed: serde_yaml::Value =
            serde_yaml::from_str(&out).expect("rendered config must be valid YAML");
        assert!(parsed.get("general").is_some());
    }

    #[test]
    fn plain_names_become_case_insensitive_exact_matches() {
        assert_eq!(to_regex("chrome"), "(?i)^chrome$");
    }

    #[test]
    fn wildcards_become_regex_without_the_user_writing_regex() {
        assert_eq!(to_regex("*Settings*"), "(?i)^.*Settings.*$");
    }

    #[test]
    fn regex_metacharacters_in_a_name_stay_literal() {
        // "#32770" is the real Win32 dialog class; a title with (), [] or . is
        // common. None of it may be interpreted as regex syntax.
        assert_eq!(to_regex("a.b"), r"(?i)^a\.b$");
        assert_eq!(to_regex("x(1)"), r"(?i)^x\(1\)$");
        assert_eq!(to_regex("a+b[c]"), r"(?i)^a\+b\[c\]$");
    }

    #[test]
    fn exe_suffix_is_optional() {
        assert_eq!(process_name("chrome.exe"), "chrome");
        assert_eq!(process_name("chrome"), "chrome");
        assert_eq!(process_name("Taskmgr.EXE"), "Taskmgr");
    }

    #[test]
    fn actions_map_to_glazewm_commands() {
        let cfg = parse(
            r#"[[rules]]
exe = "Taskmgr.exe"
action = "float"

[[rules]]
class = "Progman"
action = "ignore""#,
        );
        let out = render(&cfg).unwrap();
        assert!(out.contains("set-floating"), "{out}");
        assert!(out.contains("- ignore"), "{out}");
        assert!(out.contains("(?i)^Taskmgr$"), "{out}");
        assert!(out.contains("(?i)^Progman$"), "{out}");
    }

    #[test]
    fn a_workspace_rule_emits_a_move_command() {
        let cfg = parse(
            r#"[wm]
workspaces = ["1", "2"]

[[rules]]
exe = "chrome"
workspace = "2""#,
        );
        let out = render(&cfg).unwrap();
        assert!(out.contains("move --workspace 2"), "{out}");
    }

    #[test]
    fn several_matchers_on_one_rule_and_together() {
        // One match entry with two fields is an AND in GlazeWM. Two entries
        // would be an OR, which is not what "exe = x, title = y" means.
        let cfg = parse(
            r#"[[rules]]
exe = "ApplicationFrameHost"
title = "*Settings*"
action = "float""#,
        );
        let out = render(&cfg).unwrap();
        let rules: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
        let matchers = rules["window_rules"][0]["match"].as_sequence().unwrap();
        assert_eq!(matchers.len(), 1, "must be one ANDed entry, not two ORed");
        assert!(matchers[0].get("window_process").is_some());
        assert!(matchers[0].get("window_title").is_some());
        assert!(matchers[0].get("window_class").is_none(), "unset matchers omitted");
    }

    #[test]
    fn no_rules_still_emits_the_key_glazewm_expects() {
        let out = render(&parse("")).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
        assert!(doc.get("window_rules").is_some());
    }

    #[test]
    fn a_refused_connection_means_not_running_not_broken() {
        // This is the exact wording GlazeWM's CLI produces with no instance up.
        let stderr = "Error: Failed to connect to IPC server.\n\nCaused by:\n    0: IO error: \
                      No connection could be made because the target machine actively refused \
                      it. (os error 10061)";
        assert_eq!(classify(false, stderr), Reload::NotRunning);
    }

    #[test]
    fn success_is_success() {
        assert_eq!(classify(true, ""), Reload::Done);
    }

    #[test]
    fn an_unrecognised_failure_is_not_silently_swallowed() {
        assert!(matches!(
            classify(false, "some other catastrophe"),
            Reload::Unavailable(_)
        ));
    }

    #[test]
    fn the_generated_file_warns_against_hand_editing() {
        let out = render(&parse("")).unwrap();
        assert!(out.starts_with("# Generated by baton"));
        assert!(out.contains("baton rollback"));
    }
}
