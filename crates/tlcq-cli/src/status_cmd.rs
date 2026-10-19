//! The agents' follow-up commands: `tlcq status [id…]`, `ls`, `cancel <id>`
//! and `fetch <id>`.

use anyhow::Result;
use tlcq_cli::entry::Label;
use tlcq_cli::manage::{self, Done};
use tlcq_cli::status::Status;
use tlcq_cli::store::Store;
use tlcq_cli::{report, status, worktree};

use crate::cli::LaptopArgs;
use crate::laptop;

/// Run `status`; returns its exit code. The agent is the git worktree of the
/// current directory and `$TLCQ_AGENT`.
pub fn run(laptop: &LaptopArgs, ids: &[String]) -> Result<u8> {
    let store = laptop::store(laptop)?;
    let versions = laptop::versions(laptop)?;
    let agent = agent()?;
    let ids = ids
        .iter()
        .map(|id| resolve(&store, id))
        .collect::<Result<Vec<_>>>()?;
    let status = status::status(&store, &mut laptop::remote(laptop), &versions, &agent, &ids)?;
    print!("{}", report::render_status(&status));
    Ok(status.exit)
}

/// The agent: the git worktree of the current directory and `$TLCQ_AGENT`.
fn agent() -> Result<Label> {
    let here = std::env::current_dir()?;
    Ok(worktree::origin(&here, std::env::var("TLCQ_AGENT").ok())?.label)
}

/// Run `ls`; returns its exit code.
pub fn ls(laptop: &LaptopArgs) -> Result<u8> {
    let listing = manage::ls(&laptop::store(laptop)?, &mut laptop::remote(laptop))?;
    print!("{}", report::render_status(&listing));
    Ok(0)
}

/// Run `cancel`; returns its exit code.
pub fn cancel(laptop: &LaptopArgs, id: &str) -> Result<u8> {
    let store = laptop::store(laptop)?;
    let id = resolve(&store, id)?;
    let mut runs = laptop::runs(laptop)?;
    let mut remote = laptop::remote(laptop);
    let done = manage::cancel(&store, &mut remote, &mut runs, &agent()?, &id)?;
    Ok(print_done(&done))
}

/// Run `resume`, asking for `memory` and capped at `timeout` when given;
/// returns its exit code.
pub fn resume(
    laptop: &LaptopArgs,
    id: &str,
    memory: Option<&str>,
    timeout: Option<&str>,
) -> Result<u8> {
    let memory_mb = memory.map(tlcq_cli::config::parse_memory).transpose()?;
    let timeout_secs = timeout
        .map(tlcq_cli::config::parse_duration)
        .transpose()?
        .map(|d| d.as_secs());
    let store = laptop::store(laptop)?;
    let id = resolve(&store, id)?;
    let mut remote = laptop::remote(laptop);
    let done = manage::resume(&store, &mut remote, &agent()?, &id, memory_mb, timeout_secs)?;
    Ok(print_done(&done))
}

/// Run `fetch` into `./tmp/tlcq`; returns its exit code.
pub fn fetch(laptop: &LaptopArgs, id: &str) -> Result<u8> {
    let out = std::path::Path::new("tmp/tlcq");
    let store = laptop::store(laptop)?;
    let id = resolve(&store, id)?;
    let done = manage::fetch(&store, &mut laptop::remote(laptop), &agent()?, &id, out)?;
    Ok(print_done(&done))
}

/// The full id a short one names; an ambiguous one is a usage error (exit 2).
fn resolve(store: &Store, id: &str) -> Result<String> {
    manage::resolve(store, id)?.map_err(anyhow::Error::msg)
}

/// Print what a `cancel` or `fetch` did; returns its exit code.
fn print_done(done: &Done) -> u8 {
    match done {
        Done::Done { line, files } => {
            for f in files {
                println!("wrote {}", f.display());
            }
            let status = Status {
                lines: vec![line.clone()],
                ..Status::default()
            };
            print!("{}", report::render_status(&status));
        }
        Done::Refused(why) => eprintln!("refused: {why}"),
    }
    done.exit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tlcq_cli::status::{Line, View};

    #[test]
    fn a_done_fetch_lists_the_files_it_wrote() {
        let line = Line {
            key: None,
            id: "a".to_owned(),
            run: None,
            view: View::Laptop(tlcq_cli::laptop_runs::Progress::default()),
            eta: None,
            engine: None,
        };
        let done = Done::Done {
            line,
            files: vec!["tmp/tlcq/a/v1/1/tlc.log".into()],
        };
        assert_eq!(print_done(&done), 0);
        assert_eq!(print_done(&Done::Refused("no".to_owned())), 2);
    }
}
