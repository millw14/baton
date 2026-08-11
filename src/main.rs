//! baton - one declarative config for your whole Windows desktop.
//!
//! Baton does not manage windows or draw a bar. It conducts the tools that
//! already do, from a single config, and can put everything back.

mod config;
mod glazewm;
mod plan;
mod registry;
mod zebar;

use anyhow::{Context, Result};
use config::Config;
use plan::Change;
use std::io::Write;

const EXAMPLE_CONFIG: &str = include_str!("../baton.example.toml");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();

    let result = match argv.as_slice() {
        ["init"] => cmd_init(),
        ["check"] => cmd_check(),
        ["show"] => cmd_show(),
        ["diff"] => cmd_diff(),
        ["apply"] => cmd_apply(false),
        ["apply", "--yes"] | ["apply", "-y"] => cmd_apply(true),
        ["rollback"] => cmd_rollback(false),
        ["rollback", "--yes"] | ["rollback", "-y"] => cmd_rollback(true),
        ["reload"] => cmd_reload(),
        [] | ["-h"] | ["--help"] | ["help"] => {
            print_help();
            Ok(())
        }
        ["-V"] | ["--version"] => {
            println!("baton {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        [other, ..] => Err(anyhow::anyhow!(
            "unknown command '{other}'. Try `baton --help`."
        )),
    };

    if let Err(e) = result {
        eprintln!("baton: {e:#}");
        std::process::exit(1);
    }
}

fn print_help() {
    println!(
        r#"baton {version} - one declarative config for your whole Windows desktop

USAGE
  baton init          write a starter config
  baton check         validate the config and resolve every palette reference
  baton show          print the fully resolved config
  baton diff          show what apply would change, without doing it
  baton apply [-y]    make the desktop match the config
  baton rollback [-y] undo the last apply, exactly
  baton reload        tell the running window manager to re-read its config

Every value apply writes is read first and journaled, so rollback restores
what was there before -- including deleting values that did not exist.

apply and rollback reload the window manager for you, so there is no manual
step afterwards.

CONFIG
  {config}
"#,
        version = env!("CARGO_PKG_VERSION"),
        config = config::config_path().display()
    );
}

fn cmd_init() -> Result<()> {
    let path = config::config_path();
    if path.exists() {
        println!("config already exists at {}", path.display());
    } else {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(&path, EXAMPLE_CONFIG)
            .with_context(|| format!("writing {}", path.display()))?;
        println!("wrote {}", path.display());
    }
    println!("edit it, then run: baton diff");
    Ok(())
}

fn load() -> Result<(Config, std::path::PathBuf)> {
    let path = config::config_path();
    anyhow::ensure!(
        path.exists(),
        "no config at {} - run `baton init`",
        path.display()
    );
    Ok((Config::load(&path)?, path))
}

fn cmd_check() -> Result<()> {
    let (cfg, path) = load()?;
    println!("{} is valid.", path.display());
    println!("  palette    {} colour(s)", cfg.palette.len());
    println!(
        "  wm         backend {:?}, {} workspace(s), {} keybind(s)",
        cfg.wm.backend,
        cfg.wm.workspaces.len(),
        cfg.wm.keys.len()
    );
    println!("  windows    {} setting(s) managed", managed_count(&cfg));
    println!(
        "  bar        backend {:?}, {} widget(s)",
        cfg.bar.backend,
        cfg.bar.widgets.len()
    );
    Ok(())
}

/// How many registry-backed settings this config actually claims. Anything not
/// listed is left alone entirely, so this number is the blast radius.
fn managed_count(cfg: &Config) -> usize {
    plan::desired_registry(&cfg.windows).len()
}

fn cmd_show() -> Result<()> {
    let (cfg, _) = load()?;
    println!("{cfg:#?}");
    Ok(())
}

fn print_plan(changes: &[Change]) {
    for c in changes {
        println!("  {}", c.describe());
    }
}

fn cmd_diff() -> Result<()> {
    let (cfg, _) = load()?;
    let changes = plan::build(&cfg)?;
    if changes.is_empty() {
        println!("no changes - the desktop already matches the config");
        return Ok(());
    }
    println!("{} change(s):", changes.len());
    print_plan(&changes);
    println!("\nrun `baton apply` to make them");
    Ok(())
}

/// Ask before touching a live desktop. A non-interactive stdin reads as EOF,
/// which we treat as "no" rather than steamrolling a scripted run.
fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt} [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
        println!();
        return Ok(false);
    }
    Ok(matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

fn cmd_apply(assume_yes: bool) -> Result<()> {
    let (cfg, _) = load()?;
    let changes = plan::build(&cfg)?;

    if changes.is_empty() {
        println!("no changes - the desktop already matches the config");
        return Ok(());
    }

    println!("{} change(s):", changes.len());
    print_plan(&changes);

    if !assume_yes && !confirm("\napply these?")? {
        println!("nothing applied");
        return Ok(());
    }

    // Journal first. If we die halfway through, rollback still knows the whole
    // plan and every original value.
    plan::save_journal(&changes).context("could not journal the plan; nothing applied")?;

    let mut applied = 0usize;
    for change in &changes {
        if let Err(e) = change.apply() {
            eprintln!("baton: failed on {}: {e:#}", change.describe());
            eprintln!(
                "baton: {applied} change(s) already made. Run `baton rollback` to undo them."
            );
            return Err(e);
        }
        applied += 1;
    }

    let touched_registry = changes
        .iter()
        .any(|c| matches!(c, Change::Dword { .. }));
    if touched_registry {
        registry::broadcast_setting_change();
    }

    println!("applied {applied} change(s)");
    settle(&cfg, &changes);
    if touched_registry {
        println!("note: some shell settings only fully apply after signing out and back in");
    }
    println!("undo with: baton rollback");
    Ok(())
}

/// Whether a plan rewrote the window manager's own config, in which case the
/// running WM is now out of date until it re-reads it.
fn touched_wm_config(cfg: &Config, changes: &[Change]) -> bool {
    if cfg.wm.backend != config::Backend::Glazewm {
        return false;
    }
    let target = glazewm::config_path().to_string_lossy().into_owned();
    changes
        .iter()
        .any(|c| matches!(c, Change::File { path, .. } if *path == target))
}

fn touched(changes: &[Change], target: std::path::PathBuf) -> bool {
    let target = target.to_string_lossy().into_owned();
    changes
        .iter()
        .any(|c| matches!(c, Change::File { path, .. } if *path == target))
}

/// Push the new config into the running tools, so `apply` is the last step
/// rather than the second to last.
fn settle(cfg: &Config, changes: &[Change]) {
    if touched_wm_config(cfg, changes) {
        match glazewm::reload() {
            glazewm::Reload::Done => println!("reloaded GlazeWM"),
            glazewm::Reload::NotRunning => {
                println!("GlazeWM is not running; its config is ready for when you start it")
            }
            glazewm::Reload::Unavailable(why) => {
                println!("could not reload GlazeWM ({why}); restart it to pick up the new config")
            }
        }
    }

    // Zebar reads its startup list only at launch, so this is a restart rather
    // than a reload. The palette stylesheet alone does not need one: a widget
    // that imports it picks it up on its own refresh.
    if cfg.bar.backend == config::BarBackend::Zebar && touched(changes, zebar::settings_path()) {
        match zebar::restart() {
            zebar::Restart::Done => println!("restarted Zebar"),
            zebar::Restart::NotRunning => {
                println!("Zebar is not running; its settings are ready for when you start it")
            }
            zebar::Restart::Unavailable(why) => {
                println!("could not restart Zebar ({why}); restart it to pick up the new settings")
            }
        }
    }
}

fn cmd_reload() -> Result<()> {
    match glazewm::reload() {
        glazewm::Reload::Done => {
            println!("reloaded GlazeWM");
            Ok(())
        }
        glazewm::Reload::NotRunning => {
            println!("GlazeWM is not running");
            Ok(())
        }
        glazewm::Reload::Unavailable(why) => Err(anyhow::anyhow!(why)),
    }
}

fn cmd_rollback(assume_yes: bool) -> Result<()> {
    let changes = plan::load_journal()?;
    if changes.is_empty() {
        println!("last apply made no changes; nothing to undo");
        plan::clear_journal();
        return Ok(());
    }

    println!("undoing {} change(s):", changes.len());
    for c in &changes {
        println!("  revert  {}", c.describe());
    }

    if !assume_yes && !confirm("\nroll these back?")? {
        println!("nothing rolled back");
        return Ok(());
    }

    // Reverse order, so a change layered on another comes off first.
    let mut failures = 0usize;
    for change in changes.iter().rev() {
        if let Err(e) = change.revert() {
            eprintln!("baton: could not revert {}: {e:#}", change.describe());
            failures += 1;
        }
    }

    if changes.iter().any(|c| matches!(c, Change::Dword { .. })) {
        registry::broadcast_setting_change();
    }

    if failures == 0 {
        plan::clear_journal();
        println!("rolled back {} change(s)", changes.len());
        // The WM is now running a config that no longer exists on disk.
        if let Ok((cfg, _)) = load() {
            settle(&cfg, &changes);
        }
    } else {
        // Keep the journal: the user should be able to try again.
        eprintln!("baton: {failures} change(s) could not be reverted; journal kept");
        anyhow::bail!("rollback incomplete");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_example_config_is_valid() {
        // If this fails, `baton init` hands the user a broken config.
        let cfg = Config::parse(EXAMPLE_CONFIG).expect("example config must parse");
        assert_eq!(cfg.wm.workspaces.len(), 5);
        assert_eq!(cfg.wm.focused_border.as_deref(), Some("#8aadf4"));
        assert_eq!(cfg.windows.accent_color.as_deref(), Some("#8aadf4"));
    }

    #[test]
    fn example_config_has_no_unresolved_references_left() {
        let cfg = Config::parse(EXAMPLE_CONFIG).unwrap();
        let rendered = format!("{cfg:?}");
        assert!(
            !rendered.contains("@palette."),
            "an @palette reference survived into the parsed config"
        );
    }

    #[test]
    fn example_config_manages_the_settings_it_declares() {
        let cfg = Config::parse(EXAMPLE_CONFIG).unwrap();
        // 5 booleans plus the 3 DWM values one accent colour expands into.
        assert_eq!(managed_count(&cfg), 8);
    }

    #[test]
    fn missing_config_reports_the_path_not_a_panic() {
        std::env::set_var("BATON_CONFIG", "Z:\\definitely\\not\\here\\baton.toml");
        let err = load().unwrap_err();
        std::env::remove_var("BATON_CONFIG");
        assert!(format!("{err:#}").contains("baton init"));
    }
}
