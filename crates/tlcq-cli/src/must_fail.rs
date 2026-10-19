//! The `must fail` header and the `--must-fail-only` rewrite (design D8).
//!
//! A configuration whose leading `\*` lines say `must fail <Property>` (or
//! `must fail deadlock`), case-insensitive, first match, must end with that
//! violation; any other must pass. TLC stops at the first violation, so a
//! variant breaking several properties may report any of them: `--must-fail-only`
//! rewrites the configuration, before hashing, to check only the named one.

/// What a configuration's header says it must fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MustFail {
    /// `must fail deadlock`.
    Deadlock,
    /// `must fail <Property>`: an invariant, a temporal property or a `_POSSIBLE` predicate.
    Property(String),
}

/// Why a configuration could not be rewritten.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The named property is in none of the configuration's lists.
    #[error(
        "must fail {0}: the configuration lists no such invariant, property or _POSSIBLE predicate"
    )]
    NotListed(String),
}

/// Keywords that start a section of a TLC configuration.
const KEYWORDS: &[&str] = &[
    "CONSTANT",
    "CONSTANTS",
    "INIT",
    "NEXT",
    "SPECIFICATION",
    "INVARIANT",
    "INVARIANTS",
    "PROPERTY",
    "PROPERTIES",
    "_POSSIBLE",
    "SYMMETRY",
    "VIEW",
    "CONSTRAINT",
    "CONSTRAINTS",
    "ACTION_CONSTRAINT",
    "ACTION_CONSTRAINTS",
    "CHECK_DEADLOCK",
    "POSTCONDITION",
    "ALIAS",
];

/// Sections whose entries are names of things TLC checks.
const LISTS: &[&str] = &[
    "INVARIANT",
    "INVARIANTS",
    "PROPERTY",
    "PROPERTIES",
    "_POSSIBLE",
];

/// Split a configuration into its leading `\*` lines and the rest.
fn split_header(cfg: &str) -> (&str, &str) {
    let mut end = 0;
    for line in cfg.split_inclusive('\n') {
        if !line.starts_with("\\*") {
            break;
        }
        end += line.len();
    }
    (cfg.get(..end).unwrap_or(""), cfg.get(end..).unwrap_or(""))
}

/// What the configuration's header says it must fail, if anything.
pub fn expectation(cfg: &str) -> Option<MustFail> {
    let (header, _) = split_header(cfg);
    header.lines().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        lower.match_indices("must fail").find_map(|(at, m)| {
            let rest = line.get(at + m.len()..)?;
            let trimmed = rest.trim_start_matches([' ', '\t']);
            if trimmed.len() == rest.len() {
                return None;
            }
            let word: String = trimmed
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            match word.as_str() {
                "" => None,
                w if w.eq_ignore_ascii_case("deadlock") => Some(MustFail::Deadlock),
                w => Some(MustFail::Property(w.to_owned())),
            }
        })
    })
}

/// Whether `cfg` must fail a temporal property: its header names one that its
/// `PROPERTY` or `PROPERTIES` lists. tlx checks those only after the whole
/// search, where TLC stops at the violation (tlx-engine E6).
pub fn temporal(cfg: &str) -> bool {
    let Some(MustFail::Property(p)) = expectation(cfg) else {
        return false;
    };
    let (_, body) = split_header(cfg);
    let body = strip_comments(body);
    sections(&body)
        .into_iter()
        .filter(|(k, _)| matches!(*k, "PROPERTY" | "PROPERTIES"))
        .any(|(_, text)| text.split_whitespace().skip(1).any(|name| name == p))
}

/// Rewrite `cfg` to check only `target`: the leading `\*` block kept verbatim;
/// every invariant, property and `_POSSIBLE` entry dropped but the named one (all
/// of them for deadlock), an emptied list dropped with them; and exactly one
/// `CHECK_DEADLOCK` line, `TRUE` for deadlock, else `FALSE`.
pub fn rewrite(cfg: &str, target: &MustFail) -> Result<String, Error> {
    let (header, body) = split_header(cfg);
    let body = strip_comments(body);
    let mut out = header.to_owned();
    let mut found = false;
    for (keyword, text) in sections(&body) {
        if keyword == "CHECK_DEADLOCK" {
            continue;
        }
        if !LISTS.contains(&keyword) {
            out.push_str(text.trim());
            out.push('\n');
            continue;
        }
        let kept: Vec<&str> = text
            .split_whitespace()
            .skip(1)
            .filter(|name| matches!(target, MustFail::Property(p) if p == name))
            .collect();
        if !kept.is_empty() {
            found = true;
            out = out + keyword + " " + &kept.join(" ") + "\n";
        }
    }
    match target {
        MustFail::Property(p) if !found => Err(Error::NotListed(p.clone())),
        MustFail::Property(_) => Ok(out + "CHECK_DEADLOCK FALSE\n"),
        MustFail::Deadlock => Ok(out + "CHECK_DEADLOCK TRUE\n"),
    }
}

/// The body's sections, keyword to keyword: each keyword with its text, the
/// keyword included. Text before the first keyword is kept as a section of its own.
fn sections(body: &str) -> Vec<(&'static str, &str)> {
    let mut starts: Vec<(usize, &'static str)> = Vec::new();
    let mut at = 0;
    for token in body.split_inclusive(char::is_whitespace) {
        let word = token.trim_end();
        if let Some(k) = KEYWORDS.iter().find(|k| **k == word) {
            starts.push((at, k));
        }
        at += token.len();
    }
    let mut out = Vec::new();
    let first = starts.first().map_or(body.len(), |(at, _)| *at);
    let before = body.get(..first).unwrap_or("");
    if !before.trim().is_empty() {
        out.push(("", before));
    }
    for (i, (start, keyword)) in starts.iter().enumerate() {
        let end = starts.get(i + 1).map_or(body.len(), |(at, _)| *at);
        out.push((*keyword, body.get(*start..end).unwrap_or("")));
    }
    out
}

/// The body with its `\*` line comments and `(* *)` block comments (nested, as in
/// TLA+) turned into spaces, so words keep their places.
fn strip_comments(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut depth = 0usize;
    let mut line_comment = false;
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' {
            line_comment = false;
            out.push(c);
            continue;
        }
        let next = chars.peek().copied();
        if line_comment {
            out.push(' ');
        } else if c == '(' && next == Some('*') {
            depth += 1;
            chars.next();
            out.push_str("  ");
        } else if depth > 0 && c == '*' && next == Some(')') {
            depth -= 1;
            chars.next();
            out.push_str("  ");
        } else if depth > 0 {
            out.push(' ');
        } else if c == '\\' && next == Some('*') {
            line_comment = true;
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const VARIANT: &str = "\\* Bug: a deletion is a plain bit; must fail SafetyC.
\\* second header line
CONSTANTS
  Keys = {k1, k2}
  MaxWrites = 5
SPECIFICATION Spec
INVARIANTS SafetyC
           SafetyD \\* a comment
CHECK_DEADLOCK FALSE
PROPERTY (* block (* nested *) *) Live
_POSSIBLE PossA PossB
";

    #[test]
    fn a_must_fail_is_temporal_when_a_property_list_names_it() {
        assert!(!temporal(VARIANT));
        assert!(temporal(
            &VARIANT.replace("must fail SafetyC", "must fail Live")
        ));
        assert!(temporal(
            "\\* must fail Settles\nSPECIFICATION Spec\nPROPERTIES\n  Safe\n  Settles\n"
        ));
        assert!(!temporal("\\* must fail deadlock\nPROPERTY Live\n"));
        assert!(!temporal("SPECIFICATION Spec\nPROPERTY Live\n"));
        // A comment naming it is no entry.
        assert!(!temporal(
            "\\* must fail Live\nPROPERTY Other \\* not Live\n"
        ));
    }

    #[test]
    fn reads_the_first_must_fail_in_the_header() {
        assert_eq!(
            expectation(VARIANT),
            Some(MustFail::Property("SafetyC".to_owned()))
        );
        assert_eq!(
            expectation("\\* MUST FAIL Deadlock here\n\\* must fail X\n"),
            Some(MustFail::Deadlock)
        );
        assert_eq!(
            expectation("\\* must fail\n\\* must fail Y\n"),
            Some(MustFail::Property("Y".to_owned()))
        );
    }

    #[test]
    fn only_the_header_counts() {
        assert_eq!(expectation("\\* passes\nINIT Init \\* must fail X\n"), None);
        assert_eq!(expectation(" \\* must fail X\n"), None);
        assert_eq!(expectation("\\* must failX\n"), None);
        assert_eq!(expectation(""), None);
    }

    #[test]
    fn keeps_only_the_named_property() {
        let target = MustFail::Property("SafetyC".to_owned());
        assert_eq!(
            rewrite(VARIANT, &target).unwrap(),
            "\\* Bug: a deletion is a plain bit; must fail SafetyC.
\\* second header line
CONSTANTS
  Keys = {k1, k2}
  MaxWrites = 5
SPECIFICATION Spec
INVARIANTS SafetyC
CHECK_DEADLOCK FALSE
"
        );
    }

    #[test]
    fn keeps_a_property_from_a_later_list() {
        let out = rewrite(VARIANT, &MustFail::Property("PossB".to_owned())).unwrap();
        assert!(out.contains("_POSSIBLE PossB\n"), "{out}");
        assert!(!out.contains("INVARIANTS"), "{out}");
        let out = rewrite(VARIANT, &MustFail::Property("Live".to_owned())).unwrap();
        assert!(out.contains("PROPERTY Live\n"), "{out}");
    }

    #[test]
    fn deadlock_drops_every_list() {
        let out = rewrite(VARIANT, &MustFail::Deadlock).unwrap();
        assert!(
            out.ends_with("SPECIFICATION Spec\nCHECK_DEADLOCK TRUE\n"),
            "{out}"
        );
        assert_eq!(out.matches("CHECK_DEADLOCK").count(), 1);
    }

    #[test]
    fn a_property_not_listed_is_refused() {
        let err = rewrite(VARIANT, &MustFail::Property("Nope".to_owned())).unwrap_err();
        assert!(err.to_string().contains("Nope"), "{err}");
    }

    #[test]
    fn text_before_the_first_keyword_is_kept() {
        let out = rewrite("stray\nINVARIANT I\n", &MustFail::Property("I".to_owned())).unwrap();
        assert_eq!(out, "stray\nINVARIANT I\nCHECK_DEADLOCK FALSE\n");
    }
}
