//! Per-model log files with daily rotation and retention (spec §9.4, §20).
//!
//! Responsibility: name today's log file and delete files older than the
//! retention period.
//! Not responsible for: writing lines (see `launcher`).

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Seconds in a day.
const DAY: u64 = 86_400;

/// Converts days since 1970-01-01 to a (year, month, day) civil date.
///
/// Howard Hinnant's `civil_from_days` algorithm; avoids pulling in a date
/// crate for one filename.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Formats `time` as `YYYY-MM-DD` (UTC).
pub fn date_string(time: SystemTime) -> String {
    let secs = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (y, m, d) = civil_from_days(i64::try_from(secs / DAY).unwrap_or(0));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Today's date as `YYYY-MM-DD` (UTC).
pub fn today() -> String {
    date_string(SystemTime::now())
}

/// Deletes `*.log` files in `dir` last modified more than `retention_days`
/// ago. Best effort: failures are ignored because logs are not critical.
pub fn prune(dir: &Path, retention_days: u32) -> usize {
    let cutoff = SystemTime::now().checked_sub(Duration::from_secs(u64::from(retention_days) * DAY));
    let (Some(cutoff), Ok(entries)) = (cutoff, std::fs::read_dir(dir)) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let old = entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < cutoff);
        if path.extension().is_some_and(|e| e == "log") && old && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_dates() {
        assert_eq!(date_string(UNIX_EPOCH), "1970-01-01");
        // 2026-10-21 00:00:00 UTC
        assert_eq!(date_string(UNIX_EPOCH + Duration::from_secs(1_792_540_800)), "2026-10-21");
        // Leap day.
        assert_eq!(date_string(UNIX_EPOCH + Duration::from_secs(1_709_164_800)), "2024-02-29");
    }

    #[test]
    fn prune_removes_only_old_log_files() {
        let dir = tempfile::tempdir().expect("dir");
        let old = dir.path().join("old.log");
        let fresh = dir.path().join("fresh.log");
        let other = dir.path().join("keep.txt");
        for p in [&old, &fresh, &other] {
            std::fs::write(p, "x").expect("write");
        }
        let long_ago = SystemTime::now() - Duration::from_secs(30 * DAY);
        std::fs::File::options().write(true).open(&old).expect("open").set_modified(long_ago).expect("mtime");
        std::fs::File::options().write(true).open(&other).expect("open").set_modified(long_ago).expect("mtime");
        assert_eq!(prune(dir.path(), 7), 1);
        assert!(!old.exists() && fresh.exists() && other.exists());
    }
}
