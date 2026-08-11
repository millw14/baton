//! baton - one declarative config for your whole Windows desktop.
//!
//! Baton does not manage windows or draw a bar. It conducts the tools that
//! already do, from a single config, and can put everything back.

mod config;

use anyhow::{Context, Result};
use config::Config;
use std::path::Path;

const EXAMPLE_CONFIG: &str = include_str!("../baton.example.toml");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();

    let result = match argv.as_slice() {
        ["init"] => cmd_init(),
        ["check"] => cmd_check(),
        ["show"] => cmd_show(),
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
  baton init     write a starter config
  baton check    validate the config and resolve every palette reference
  baton show     print the fully resolved config Baton would act on

NOT YET IMPLEMENTED
  baton diff     preview what apply would change
  baton apply    make the desktop match the config
  baton rollback undo the last apply

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
    println!("edit it, then run: baton check");
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
    println!(
        "  palette    {} colour(s)",
        cfg.palette.len()
    );
    println!(
        "  wm         backend {:?}, {} workspace(s), {} keybind(s)",
        cfg.wm.backend,
        cfg.wm.workspaces.len(),
        cfg.wm.keys.len()
    );
    println!("  windows    {} setting(s) managed", managed_count(&cfg));
    Ok(())
}

/// How many registry-backed settings this config actually claims. Anything not
/// listed is left alone entirely, so this number is the blast radius.
fn managed_count(cfg: &Config) -> usize {
    let w = &cfg.windows;
    [
        w.dark_mode.is_some(),
        w.app_dark_mode.is_some(),
        w.show_file_extensions.is_some(),
        w.show_hidden_files.is_some(),
        w.taskbar_small_icons.is_some(),
        w.accent_color.is_some(),
    ]
    .iter()
    .filter(|x| **x)
    .count()
}

fn cmd_show() -> Result<()> {
    let (cfg, _) = load()?;
    println!("{cfg:#?}");
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
        // And its palette references must have actually resolved.
        assert_eq!(cfg.wm.focused_border.as_deref(), Some("#8aadf4"));
        assert_eq!(cfg.windows.accent_color.as_deref(), Some("#8aadf4"));
        assert_eq!(managed_count(&cfg), 6);
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
    fn missing_config_reports_the_path_not_a_panic() {
        std::env::set_var("BATON_CONFIG", "Z:\\definitely\\not\\here\\baton.toml");
        let err = load().unwrap_err();
        std::env::remove_var("BATON_CONFIG");
        assert!(format!("{err:#}").contains("baton init"));
    }
}

// Keep `Path` imported for the signature above without tripping dead-code.
const _: fn(&Path) -> Result<Config> = Config::load;
