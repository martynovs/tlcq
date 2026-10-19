//! `config.toml` (design D13): every setting a `TLCQ_*` variable sets, by the
//! same name in lower case without the prefix (`deadline_secs = 60` sets
//! `TLCQ_DEADLINE_SECS`). A flag beats its variable, the variable beats the
//! file, and the file beats the default: the file fills in only the variables
//! that are unset, before the command line is read. A key that names no
//! setting is refused.
//!
//! It is `$TLCQ_CONFIG` if set, else `$XDG_CONFIG_HOME/tlcq/config.toml`, else
//! `~/.config/tlcq/config.toml`; only an explicit `$TLCQ_CONFIG` must exist.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::CommandFactory;

/// The file's path, and whether it was named explicitly.
fn path(
    explicit: Option<OsString>,
    xdg: Option<OsString>,
    home: Option<OsString>,
) -> Option<(PathBuf, bool)> {
    if let Some(p) = explicit.filter(|p| !p.is_empty()) {
        return Some((PathBuf::from(p), true));
    }
    let base = match xdg.filter(|v| !v.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => PathBuf::from(home?).join(".config"),
    };
    Some((base.join("tlcq").join("config.toml"), false))
}

/// Every variable a setting reads, from the command line's definition.
fn known() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut todo = vec![crate::cli::Cli::command()];
    while let Some(cmd) = todo.pop() {
        for arg in cmd.get_arguments() {
            if let Some(env) = arg.get_env() {
                out.insert(env.to_string_lossy().into_owned());
            }
        }
        todo.extend(cmd.get_subcommands().cloned());
    }
    out
}

/// The variables `text` sets: each key's `TLCQ_*` name and its value.
fn read(text: &str, origin: &str, known: &BTreeSet<String>) -> Result<Vec<(String, String)>> {
    let table: toml::Table = toml::from_str(text).with_context(|| format!("reading {origin}"))?;
    let mut out = Vec::new();
    for (key, value) in table {
        let var = format!("TLCQ_{}", key.to_uppercase());
        if !known.contains(&var) || key.contains(|c: char| !c.is_ascii_lowercase() && c != '_') {
            bail!("{origin}: {key} is no setting");
        }
        let value = match value {
            toml::Value::String(s) => s,
            toml::Value::Integer(n) => n.to_string(),
            other => bail!(
                "{origin}: {key} is {}, not a string or a number",
                other.type_str()
            ),
        };
        out.push((var, value));
    }
    Ok(out)
}

/// Fill in, from the config file, every `TLCQ_*` variable that is unset.
/// Runs first thing, before any thread starts.
///
/// # Errors
/// If the file cannot be read, does not parse, or names no setting.
pub fn load() -> Result<()> {
    let found = path(
        std::env::var_os("TLCQ_CONFIG"),
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    );
    let Some((file, explicit)) = found else {
        return Ok(());
    };
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    for (var, value) in read(&text, &file.display().to_string(), &known())? {
        if std::env::var_os(&var).is_none() {
            std::env::set_var(var, value);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_is_named_explicitly_else_under_xdg_else_home() {
        let some = |s: &str| Some(OsString::from(s));
        assert_eq!(
            path(some("/x.toml"), some("/xdg"), some("/home")),
            Some((PathBuf::from("/x.toml"), true))
        );
        assert_eq!(
            path(some(""), some("/xdg"), None),
            Some((PathBuf::from("/xdg/tlcq/config.toml"), false))
        );
        assert_eq!(
            path(None, None, some("/home")),
            Some((PathBuf::from("/home/.config/tlcq/config.toml"), false))
        );
        assert_eq!(path(None, None, None), None);
    }

    #[test]
    fn each_key_sets_its_variable_and_an_unknown_key_is_refused() {
        let known = known();
        assert!(known.contains("TLCQ_CONTEXT") && known.contains("TLCQ_MAX_CAP_SECS"));
        let vars = read(
            "context = \"\"\nslots = 2\ncap_secs = 600\n",
            "c.toml",
            &known,
        )
        .unwrap();
        assert_eq!(
            vars,
            [
                ("TLCQ_CAP_SECS".to_owned(), "600".to_owned()),
                ("TLCQ_CONTEXT".to_owned(), String::new()),
                ("TLCQ_SLOTS".to_owned(), "2".to_owned()),
            ]
        );
        for (bad, why) in [
            ("nonsense = 1", "nonsense is no setting"),
            ("CONTEXT = \"x\"", "CONTEXT is no setting"),
            ("slots = [1]", "slots is array"),
            ("slots = ", "reading c.toml"),
        ] {
            let err = read(bad, "c.toml", &known).unwrap_err();
            assert!(format!("{err:#}").contains(why), "{err:#}");
        }
    }
}
