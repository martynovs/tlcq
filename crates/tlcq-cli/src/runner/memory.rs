//! Sizing an attempt's memory from what the VM has free when it starts
//! (design D9). pve's memory is shared with things the runner does not
//! control, such as a VM that pins 32 GiB while it runs, so a slot's share of
//! the total is only a ceiling. A new attempt gets no more than what is free
//! now, less the reserve and less the room the running attempts may still grow
//! into, so their caps never sum past what the machine has and no host-wide
//! OOM kill picks a TLC job.

/// The least memory, in MB, an attempt starts with; below it, it waits.
pub const MIN_MB: u64 = 4096;

/// A `/proc/meminfo` field (`MemTotal`, `MemAvailable`), in MB.
pub fn field_mb(meminfo: &str, name: &str) -> Option<u64> {
    let line = meminfo
        .lines()
        .find(|l| l.split(':').next() == Some(name))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024)
}

/// A size as `docker stats` prints it (`28.25GiB`, `512MiB`, `1.5GB`), in bytes.
pub fn bytes(size: &str) -> Option<u64> {
    let size = size.trim();
    let at = size.find(|c: char| c.is_ascii_alphabetic())?;
    let (number, unit) = size.split_at(at);
    let number: f64 = number.trim().parse().ok()?;
    let scale: f64 = match unit {
        "B" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => return None,
    };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a non-negative size well within u64"
    )]
    Some((number * scale) as u64)
}

/// The room, in MB, running attempts may still grow into: each one's cap less
/// its use, from `docker stats` lines of `used / cap`. A line it cannot read
/// counts for nothing.
pub fn promised_mb(stats: &str) -> u64 {
    stats
        .lines()
        .filter_map(|line| {
            let (used, cap) = line.split_once('/')?;
            Some(bytes(cap)?.saturating_sub(bytes(used)?))
        })
        .sum::<u64>()
        / (1024 * 1024)
}

/// A new attempt's memory cap, in MB: its slot's `ceiling`, but no more than
/// `available` now less the `reserve` and the room `promised` to the running
/// attempts; `None` when that is below [`MIN_MB`], so it waits.
pub fn cap_mb(ceiling: u64, available: u64, promised: u64, reserve: u64) -> Option<u64> {
    let free = available.saturating_sub(reserve.saturating_add(promised));
    let cap = ceiling.min(free);
    (cap >= MIN_MB).then_some(cap)
}

/// The status hint of a run tlx ended with "liveness needs more memory: …
/// the process needs about N MiB" in `errors` (job-memory M6): the request
/// that would give tlx that much, as its `tlx_percent` share of an attempt's
/// memory, rounded up to a whole GB; or, past `most_mb` (what one job can
/// get), that the VM has too little.
pub fn needed_hint(errors: &[String], tlx_percent: u64, most_mb: u64) -> Option<String> {
    let needs = errors.iter().find_map(|e| {
        let (_, rest) = e.split_once("liveness needs more memory")?;
        let (_, rest) = rest.split_once("needs about ")?;
        rest.split_once(" MiB")?.0.parse::<u64>().ok()
    })?;
    let attempt_mb = (needs * 100).div_ceil(tlx_percent.max(1));
    Some(if attempt_mb > most_mb {
        format!(
            "needs more memory than the VM gives: about {}G, past the {}G one job gets at most",
            attempt_mb.div_ceil(1024),
            most_mb / 1024
        )
    } else {
        format!(
            "needs more memory: resubmit with --memory {}G",
            attempt_mb.div_ceil(1024)
        )
    })
}

/// The status hint of a run killed for memory: the memory its attempt had,
/// which a rerun needs more than, and the most one job can get, `most_mb`,
/// as the request to ask for.
pub fn killed_hint(ran_with_mb: u64, most_mb: u64) -> String {
    if ran_with_mb < most_mb {
        format!(
            "needs more memory than its {ran_with_mb} MB: ask for up to --memory {}G, the most one job gets on the VM",
            most_mb / 1024
        )
    } else {
        format!("needs more memory than its {ran_with_mb} MB, the most one job gets on the VM")
    }
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
