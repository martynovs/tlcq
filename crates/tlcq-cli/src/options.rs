//! The TLC option allowlist (design D3).
//!
//! An option joins the list deliberately, and only if its result is
//! deterministic and the run ends on its own, so never `-simulate`, `-seed`,
//! `-depth` or `-continue`. Resource settings and time caps are not options in
//! this sense: tlcq sets them, and they are not part of the id.

/// The options a job may ask for with `--tlc-opt`.
pub const ALLOWED: &[&str] = &["-deadlock"];

/// An option off the allowlist.
#[derive(Debug, thiserror::Error)]
#[error("TLC option {0:?} is not allowed (allowed: {allowed})", allowed = ALLOWED.join(", "))]
pub struct NotAllowed(pub String);

/// Check `options` against the allowlist and return them sorted and deduplicated,
/// so their order never changes the id.
pub fn check(options: &[String]) -> Result<Vec<String>, NotAllowed> {
    if let Some(bad) = options.iter().find(|o| !ALLOWED.contains(&o.as_str())) {
        return Err(NotAllowed(bad.clone()));
    }
    let mut options = options.to_vec();
    options.sort();
    options.dedup();
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn allowed_options_pass_once() {
        assert_eq!(
            check(&strings(&["-deadlock", "-deadlock"])).unwrap(),
            ["-deadlock"]
        );
        assert_eq!(check(&[]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn others_are_refused() {
        for bad in ["-metadir", "-continue", "-simulate", "-seed"] {
            let err = check(&strings(&[bad])).unwrap_err();
            assert!(err.to_string().contains(bad), "{err}");
        }
    }
}
