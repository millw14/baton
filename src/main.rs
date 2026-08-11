//! baton - one declarative config for your whole Windows desktop.
//!
//! Baton does not manage windows or draw a bar. It conducts the tools that
//! already do, from a single config, and can put everything back.

mod config;
mod glazewm;
mod history;
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
        ["rollback"] => cmd_rollback(false, false),
        ["rollback", "--yes"] | ["rollback", "-y"] => cmd_rollback(true, false),
        ["rollback", "--all"] => cmd_rollback(false, true),
        ["rollback", "--all", "--yes"] | ["rollback", "--all", "-y"]
        | ["rollback", "--yes", "--all"] | ["rollback", "-y", "--all"] => {
            cmd_rollback(true, true)
        }
        ["history"] => cmd_history(),
        ["history", "--clear"] => cmd_history_clear(false),
        ["history", "--clear", "--yes"] | ["history", "--clear", "-y"] => {
            cmd_history_clear(true)
        }
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
  baton rollback [-y] undo the most recent apply, exactly
  baton rollback --all
                      undo every apply, back to before baton touched anything
  baton history       list what has been applied, newest first
  baton history --clear
                      forget the history WITHOUT undoing it
  baton reload        tell the running window manager to re-read its config

Every value apply writes is read first and recorded, so rollback restores
what was there before -- including deleting values that did not exist.
Each apply is its own history entry, so rollback can be run repeatedly to
step back through them.

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
    println!("{}:", history::changes(changes.len()));
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

    println!("{}:", history::changes(changes.len()));
    print_plan(&changes);

    if !assume_yes && !confirm("\napply these?")? {
        println!("nothing applied");
        return Ok(());
    }

    // Record first. If we die halfway through, rollback still knows the whole
    // plan and every original value.
    let entry = history::append(&changes)
        .context("could not record the plan in history; nothing applied")?;

    let mut applied = 0usize;
    for change in &changes {
        if let Err(e) = change.apply() {
            eprintln!("baton: failed on {}: {e:#}", change.describe());
            eprintln!(
                "baton: {} already made. Run `baton rollback` to undo them.",
                history::changes(applied)
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

    println!("applied {} as apply #{}", history::changes(applied), entry.seq);
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

/// Undo one entry. Returns how many changes failed, leaving the entry in place
/// if any did so the user can retry.
fn revert_entry(entry: &history::Entry) -> usize {
    // Reverse order, so a change layered on another comes off first.
    let mut failures = 0usize;
    for change in entry.changes.iter().rev() {
        if let Err(e) = change.revert() {
            eprintln!("baton: could not revert {}: {e:#}", change.describe());
            failures += 1;
        }
    }
    if entry.changes.iter().any(|c| matches!(c, Change::Dword { .. })) {
        registry::broadcast_setting_change();
    }
    failures
}

fn cmd_rollback(assume_yes: bool, all: bool) -> Result<()> {
    let entries = history::list();
    anyhow::ensure!(!entries.is_empty(), "nothing to roll back");

    // Newest first: an older entry must never be reverted while a newer one
    // sits on top of the same value.
    let targets: Vec<history::Entry> = if all {
        entries.into_iter().rev().collect()
    } else {
        vec![entries.into_iter().next_back().unwrap()]
    };

    let total: usize = targets.iter().map(|e| e.changes.len()).sum();
    println!(
        "undoing {} ({}):",
        history::applies(targets.len()),
        history::changes(total)
    );
    for entry in &targets {
        println!(
            "  apply #{} from {}",
            entry.seq,
            history::ago_now(entry.applied_at)
        );
        for c in &entry.changes {
            println!("    revert  {}", c.describe());
        }
    }
    let remaining = history::list().len() - targets.len();
    if remaining > 0 {
        println!(
            "\n{} earlier will remain; `--all` undoes everything",
            history::applies(remaining)
        );
    }

    if !assume_yes && !confirm("\nroll these back?")? {
        println!("nothing rolled back");
        return Ok(());
    }

    let mut undone = 0usize;
    for entry in &targets {
        let failures = revert_entry(entry);
        if failures > 0 {
            eprintln!(
                "baton: {} in apply #{} could not be reverted; \
                 it has been kept so you can retry",
                history::changes(failures),
                entry.seq
            );
            anyhow::bail!(
                "rollback incomplete after undoing {}",
                history::applies(undone)
            );
        }
        history::remove(entry.seq)?;
        undone += 1;
    }

    println!(
        "rolled back {} ({})",
        history::applies(undone),
        history::changes(total)
    );
    // The tools may now be running configs that changed underneath them.
    if let Ok((cfg, _)) = load() {
        let flattened: Vec<Change> =
            targets.iter().flat_map(|e| e.changes.clone()).collect();
        settle(&cfg, &flattened);
    }
    if history::list().is_empty() {
        println!("history is empty; the desktop is back to before baton touched it");
    }
    Ok(())
}

fn cmd_history() -> Result<()> {
    let entries = history::list();
    if entries.is_empty() {
        println!("no applies recorded");
        return Ok(());
    }
    println!("{}, newest first:", history::applies(entries.len()));
    for entry in entries.iter().rev() {
        println!(
            "  #{:<4} {:<16} {}",
            entry.seq,
            history::ago_now(entry.applied_at),
            history::changes(entry.changes.len())
        );
        for c in &entry.changes {
            println!("         {}", c.describe());
        }
    }
    println!("\n`baton rollback` undoes #{}; `--all` undoes everything",
        entries.last().unwrap().seq);
    Ok(())
}

/// Forget the history without reverting anything. For when the current state
/// is the one you want to keep.
fn cmd_history_clear(assume_yes: bool) -> Result<()> {
    let entries = history::list();
    if entries.is_empty() {
        println!("no applies recorded");
        return Ok(());
    }
    println!(
        "this forgets {} WITHOUT undoing them.",
        history::applies(entries.len())
    );
    println!("the desktop keeps its current settings and they become the new baseline.");
    if !assume_yes && !confirm("\nforget history?")? {
        println!("history kept");
        return Ok(());
    }
    history::clear();
    println!("history cleared");
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
