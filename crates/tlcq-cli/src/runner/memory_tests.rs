//! Tests of sizing an attempt's memory from what is free.

use super::*;

#[test]
fn meminfo_fields_come_in_mb() {
    let meminfo = "MemTotal:       131571900 kB\nMemFree:  1 kB\nMemAvailable:   57671680 kB\n";
    assert_eq!(field_mb(meminfo, "MemTotal"), Some(128_488));
    assert_eq!(field_mb(meminfo, "MemAvailable"), Some(56_320));
    assert_eq!(field_mb(meminfo, "Mem"), None);
    assert_eq!(field_mb("MemAvailable: lots kB\n", "MemAvailable"), None);
}

#[test]
fn docker_stats_sizes_read_in_either_unit_family() {
    assert_eq!(bytes("28.25GiB"), Some(30_333_206_528));
    assert_eq!(bytes(" 512MiB "), Some(536_870_912));
    assert_eq!(bytes("1.5GB"), Some(1_500_000_000));
    for (size, want) in [
        ("7B", 7),
        ("2KiB", 2048),
        ("3kB", 3000),
        ("1TiB", 1 << 40),
        ("2MB", 2_000_000),
        ("1TB", 1_000_000_000_000),
    ] {
        assert_eq!(bytes(size), Some(want), "{size}");
    }
    for bad in ["", "12", "GiB", "x1GiB", "1.2.3GiB", "5parsecs"] {
        assert_eq!(bytes(bad), None, "{bad}");
    }
}

#[test]
fn the_promised_room_is_each_running_cap_less_its_use() {
    // 36.44 - 28.25 = 8.19 GiB, 85.03 - 54.26 = 30.77 GiB; a garbled line counts for nothing.
    let stats = "28.25GiB / 36.44GiB\n54.26GiB / 85.03GiB\nnonsense\n1GiB / ?\n";
    assert_eq!(promised_mb(stats), 39_895);
    assert_eq!(promised_mb(""), 0);
    // Over its cap (a moment's swap) promises nothing, not a negative.
    assert_eq!(promised_mb("10GiB / 8GiB\n"), 0);
}

#[test]
fn a_cap_is_the_share_unless_less_is_free_and_none_below_the_minimum() {
    // Plenty free: the slot's share.
    assert_eq!(cap_mb(60_000, 120_000, 10_000, 8_192), Some(60_000));
    // The VM back on: what is free, less the reserve and the promised room.
    assert_eq!(cap_mb(60_000, 50_000, 20_000, 8_192), Some(21_808));
    // Nothing left worth starting with: it waits.
    assert_eq!(cap_mb(60_000, 12_000, 4_000, 8_192), None);
    assert_eq!(cap_mb(60_000, 1_000, u64::MAX, u64::MAX), None);
    assert_eq!(cap_mb(MIN_MB, u64::MAX, 0, 0), Some(MIN_MB));
}

#[test]
fn a_run_short_of_memory_says_what_to_resubmit_with() {
    // 6950449c9c0d's line: 48049 MiB as 80% of an attempt is 60062 MB, 59 GB.
    let line = "Error: liveness needs more memory: with the liveness automaton and product (267079582 nodes, 1073882957 edges so far), the process needs about 48049 MiB, over the memory budget of 43504 MiB (-maxmem); rerun with -maxmem 48064 or more.";
    let errors = vec!["Error: something else".to_owned(), line.to_owned()];
    assert_eq!(
        needed_hint(&errors, 80, 112_640).as_deref(),
        Some("needs more memory: resubmit with --memory 59G")
    );
    assert_eq!(
        needed_hint(&errors, 80, 40_960).as_deref(),
        Some("needs more memory than the VM gives: about 59G, past the 40G one job gets at most")
    );
    assert_eq!(needed_hint(&errors[..1], 80, 112_640), None);
    let garbled = vec!["Error: liveness needs more memory: needs about lots MiB".to_owned()];
    assert_eq!(needed_hint(&garbled, 80, 112_640), None);
    assert_eq!(
        killed_hint(54_380, 112_640),
        "needs more memory than its 54380 MB: ask for up to --memory 110G, the most one job gets on the VM"
    );
    assert_eq!(
        killed_hint(112_640, 112_640),
        "needs more memory than its 112640 MB, the most one job gets on the VM"
    );
}
