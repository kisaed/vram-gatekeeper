//! Windows' own game list (spec §11.6, experimental).
//!
//! Responsibility: read the executables Windows has recognised as games from
//! `HKCU\System\GameConfigStore\Children` (the list Game Bar uses).
//! Not responsible for deciding policy: the UI offers these as candidates, and
//! the process scanner can optionally treat them as games (default off).
//!
//! Phase 0 finding: only some entries carry `MatchedExeFullPath` (6 of 66 on the
//! development machine) and the list includes generic runtimes such as
//! `javaw.exe`. That is why this is an opt-in import, not an automatic source.

use std::collections::BTreeSet;

use crate::platform::windows::registry::RegistryKey;

/// Registry folder holding one child key per recognised game.
const CHILDREN: &str = r"System\GameConfigStore\Children";

/// Value holding the full path of the matched executable.
const MATCHED_EXE: &str = "MatchedExeFullPath";

/// Full paths of executables Windows recognises as games, without duplicates.
pub fn windows_game_paths() -> Vec<String> {
    let Ok(parent) = RegistryKey::open(CHILDREN) else { return Vec::new() };
    let mut paths = BTreeSet::new();
    for child in parent.subkeys() {
        if let Ok(key) = RegistryKey::open(&format!(r"{CHILDREN}\{child}"))
            && let Some(path) = key.read_string(MATCHED_EXE).filter(|p| !p.trim().is_empty())
        {
            paths.insert(path);
        }
    }
    paths.into_iter().collect()
}

/// The distinct executable file names from [`windows_game_paths`].
pub fn windows_game_names() -> Vec<String> {
    let names: BTreeSet<String> = windows_game_paths()
        .iter()
        .filter_map(|p| p.rsplit(['\\', '/']).next().map(str::to_owned))
        .filter(|n| n.to_ascii_lowercase().ends_with(".exe"))
        .collect();
    names.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_the_list_never_fails_and_only_yields_exe_names() {
        // The content depends on the machine; the contract is "no panic, exe names only".
        let names = windows_game_names();
        assert!(names.iter().all(|n| n.to_ascii_lowercase().ends_with(".exe") && !n.contains(['\\', '/'])));
    }
}
