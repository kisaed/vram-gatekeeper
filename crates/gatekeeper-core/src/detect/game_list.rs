//! Helpers for building the game list (spec §11.7, FR-GAME-8).
//!
//! Responsibility: validate a browsed or typed executable before it is added,
//! with the warnings the spec asks for (generic names, launchers, duplicates).
//! Not responsible for: showing dialogs (the UI does) or detecting games.
//!
//! Implements FR-GAME-8.

use serde::Serialize;

use crate::config::schema::GameExecutable;

/// File names too generic to be a safe game entry: any process with this name
/// would close the gate (spec §11.7).
const GENERIC_NAMES: [&str; 16] = [
    "game.exe",
    "launcher.exe",
    "start.exe",
    "client.exe",
    "app.exe",
    "main.exe",
    "play.exe",
    "run.exe",
    "setup.exe",
    "updater.exe",
    "server.exe",
    "bin.exe",
    "java.exe",
    "javaw.exe",
    "python.exe",
    "node.exe",
];

/// Game launchers: listing one would treat the launcher itself as a game,
/// which is usually not intended (spec §11.7).
const LAUNCHERS: [(&str, &str); 8] = [
    ("steam.exe", "Steam"),
    ("steamwebhelper.exe", "Steam"),
    ("epicgameslauncher.exe", "Epic Games Launcher"),
    ("eadesktop.exe", "EA app"),
    ("ubisoftconnect.exe", "Ubisoft Connect"),
    ("upc.exe", "Ubisoft Connect"),
    ("battle.net.exe", "Battle.net"),
    ("galaxyclient.exe", "GOG Galaxy"),
];

/// Result of checking a candidate entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GameCheck {
    /// File name that would be stored (empty if the input was unusable).
    pub name: String,
    /// Original path if the user browsed for the file.
    pub path: Option<String>,
    /// False if the entry must not be added.
    pub ok: bool,
    /// Blocking problem, when `ok` is false.
    pub error: Option<String>,
    /// Non-blocking warnings the user should confirm.
    pub warnings: Vec<String>,
}

/// Extracts the file name from a path or bare name using either separator.
fn file_name(input: &str) -> &str {
    input.rsplit(['\\', '/']).next().unwrap_or(input).trim()
}

/// Validates `input` (a full path from the file dialog, or a typed name)
/// against the existing list.
pub fn check_executable(input: &str, existing: &[GameExecutable]) -> GameCheck {
    let trimmed = input.trim().trim_matches('"');
    let name = file_name(trimmed).to_owned();
    let path = (trimmed.contains(['\\', '/'])).then(|| trimmed.to_owned());
    let reject = |error: &str| GameCheck {
        name: name.clone(),
        path: path.clone(),
        ok: false,
        error: Some(error.to_owned()),
        warnings: Vec::new(),
    };

    if name.is_empty() {
        return reject("Enter the name of the program, for example game.exe.");
    }
    if !name.to_ascii_lowercase().ends_with(".exe") {
        return reject("The name must end in .exe.");
    }
    // A typed entry is a bare name; separators there would never match a process.
    if path.is_none() && trimmed.contains(['\\', '/']) {
        return reject("Type only the file name, without folders.");
    }

    if let Some(dup) = existing.iter().find(|e| e.name.eq_ignore_ascii_case(&name)) {
        let from = dup.path.as_deref().map_or_else(String::new, |p| format!(" (added from {p})"));
        return reject(&format!("{} is already on the list{from}.", dup.name));
    }

    let lower = name.to_ascii_lowercase();
    let mut warnings = Vec::new();
    if GENERIC_NAMES.contains(&lower.as_str()) {
        warnings.push(format!(
            "'{name}' is a very generic name: any program with this name will close the gate. \
             Consider starting the game and choosing it from the running apps instead."
        ));
    }
    if let Some((_, launcher)) = LAUNCHERS.iter().find(|(exe, _)| *exe == lower) {
        warnings.push(format!(
            "'{name}' is the {launcher} launcher. Listing it would treat the launcher itself as a game, \
             which is usually not what you want."
        ));
    }
    GameCheck { name, path, ok: true, error: None, warnings }
}

/// A running program offered in "Add from running apps".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunningApp {
    /// Executable file name.
    pub name: String,
    /// One process id of that program.
    pub pid: u32,
    /// Title of its visible window, when it has one.
    pub title: Option<String>,
    /// Dedicated GPU memory it uses, in MiB (0 if unknown).
    pub gpu_mib: u64,
}

/// Merges process names, window titles and GPU use into one list, one entry
/// per executable name, programs with windows first and heavy GPU users on top.
pub fn merge_running_apps(
    processes: &[(u32, String)],
    titles: &std::collections::HashMap<u32, String>,
    gpu_mib: &std::collections::HashMap<u32, u64>,
) -> Vec<RunningApp> {
    let mut by_name: std::collections::BTreeMap<String, RunningApp> = std::collections::BTreeMap::new();
    for (pid, name) in processes {
        let key = name.to_ascii_lowercase();
        let gpu = gpu_mib.get(pid).copied().unwrap_or(0);
        let title = titles.get(pid).cloned();
        let entry =
            by_name.entry(key).or_insert_with(|| RunningApp { name: name.clone(), pid: *pid, title: None, gpu_mib: 0 });
        entry.gpu_mib += gpu;
        // Prefer the instance that owns a window, so the title is shown.
        if entry.title.is_none() && title.is_some() {
            entry.title = title;
            entry.pid = *pid;
        }
    }
    let mut list: Vec<RunningApp> = by_name.into_values().collect();
    list.sort_by(|a, b| {
        b.gpu_mib
            .cmp(&a.gpu_mib)
            .then(b.title.is_some().cmp(&a.title.is_some()))
            .then(a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
    });
    list
}

/// Running programs with window titles and GPU use (Windows).
#[cfg(windows)]
pub fn running_apps(vram: Option<&crate::gpu::VramSample>) -> Vec<RunningApp> {
    use crate::platform::windows::{process_list::list_processes, window_list::window_titles};
    let procs: Vec<(u32, String)> = list_processes().unwrap_or_default().into_iter().map(|p| (p.pid, p.name)).collect();
    let gpu: std::collections::HashMap<u32, u64> =
        vram.map(|s| s.per_process.iter().map(|p| (p.pid, p.dedicated_mib)).collect()).unwrap_or_default();
    merge_running_apps(&procs, &window_titles(), &gpu)
}

/// Folder where the file dialog should start: the Steam library when Steam
/// is installed, else Program Files (spec §11.7).
#[cfg(windows)]
pub fn default_browse_dir() -> String {
    use crate::platform::windows::registry::RegistryKey;
    let steam = RegistryKey::open(r"Software\Valve\Steam")
        .ok()
        .and_then(|k| k.read_string("SteamPath"))
        .map(|p| std::path::PathBuf::from(p.replace('/', "\\")).join("steamapps").join("common"))
        .filter(|p| p.is_dir());
    steam.map_or_else(|| r"C:\Program Files".to_owned(), |p| p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_apps_are_merged_by_name_and_sorted_for_picking() {
        let procs = vec![
            (10, "explorer.exe".to_owned()),
            (11, "Game.exe".to_owned()),
            (12, "game.exe".to_owned()),
            (13, "svchost.exe".to_owned()),
        ];
        let titles = std::collections::HashMap::from([(12u32, "My Game".to_owned()), (10, "Files".to_owned())]);
        let gpu = std::collections::HashMap::from([(11u32, 3000u64), (12, 1000)]);
        let list = merge_running_apps(&procs, &titles, &gpu);
        assert_eq!(list[0].name.to_ascii_lowercase(), "game.exe", "heaviest GPU user first");
        assert_eq!((list[0].gpu_mib, list[0].title.as_deref(), list[0].pid), (4000, Some("My Game"), 12));
        assert_eq!(list[1].name, "explorer.exe", "windowed apps before background ones");
        assert_eq!(list.len(), 3, "game.exe instances are merged");
    }

    fn existing(names: &[(&str, Option<&str>)]) -> Vec<GameExecutable> {
        names
            .iter()
            .map(|(n, p)| GameExecutable { name: (*n).into(), path: p.map(str::to_owned), enabled: true })
            .collect()
    }

    #[test]
    fn a_browsed_path_keeps_the_original_and_stores_the_file_name() {
        let c = check_executable(r#""D:\SteamLibrary\steamapps\common\ELDEN RING\Game\eldenring.exe""#, &[]);
        assert!(c.ok);
        assert_eq!(c.name, "eldenring.exe");
        assert_eq!(c.path.as_deref(), Some(r"D:\SteamLibrary\steamapps\common\ELDEN RING\Game\eldenring.exe"));
        assert!(c.warnings.is_empty());
    }

    #[test]
    fn typed_names_must_be_bare_exe_names() {
        assert!(check_executable("game2.exe", &[]).ok);
        assert_eq!(check_executable("game2", &[]).error.as_deref(), Some("The name must end in .exe."));
        assert!(check_executable("   ", &[]).error.is_some());
        assert!(!check_executable("", &[]).ok);
    }

    #[test]
    fn duplicates_are_rejected_and_point_at_the_existing_entry() {
        let list = existing(&[("eldenring.exe", Some(r"D:\Games\eldenring.exe"))]);
        let c = check_executable(r"E:\Other\ELDENRING.EXE", &list);
        assert!(!c.ok);
        let msg = c.error.expect("error");
        assert!(msg.contains("already on the list") && msg.contains(r"D:\Games\eldenring.exe"), "{msg}");
    }

    #[test]
    fn generic_names_and_launchers_warn_but_can_be_added() {
        let generic = check_executable("Launcher.exe", &[]);
        assert!(generic.ok && generic.warnings.len() == 1 && generic.warnings[0].contains("generic"));
        let steam = check_executable(r"C:\Program Files (x86)\Steam\steam.exe", &[]);
        assert!(steam.ok && steam.warnings[0].contains("Steam launcher"));
    }
}
