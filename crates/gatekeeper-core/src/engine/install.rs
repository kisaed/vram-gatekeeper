//! Locating and validating the llama.cpp engine (spec §16.1).
//!
//! Responsibility: decide which engine folder to use (bundled or the
//! user's), check it contains a runnable `llama-server.exe`, and read its
//! version and parameter catalog by running it.
//! Not responsible for: downloading engines (see `releases`, Phase 5).
//!
//! Implements FR-MOD-1, NFR-7.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::config::schema::Config;
use crate::error::{CoreError, CoreResult};
use crate::params::catalog::ParamCatalog;
use crate::params::help_parser::parse_help;

/// Name of the server executable inside an engine folder.
pub const SERVER_EXE: &str = "llama-server.exe";

/// How long `--version` / `--help` may take before we give up.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// `CREATE_NO_WINDOW`; see `supervisor::launcher`.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Picks the engine folder: the user's override, else the bundled engine.
///
/// # Errors
/// Returns an error if neither is available.
pub fn resolve_engine_dir(cfg: &Config, bundled: Option<&Path>) -> CoreResult<PathBuf> {
    if let Some(dir) = cfg.engine.llama_server_dir.as_deref().filter(|d| !d.trim().is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    bundled.map(Path::to_path_buf).ok_or_else(|| {
        CoreError::Other(
            "No llama.cpp engine is configured and none is bundled. Set the engine folder in Engine settings."
                .to_owned(),
        )
    })
}

/// What we learned about an engine folder.
#[derive(Debug, Clone)]
pub struct EngineInfo {
    /// Full path of `llama-server.exe`.
    pub exe: PathBuf,
    /// First line of `--version` output, e.g. `version: 6123 (abc)`.
    pub version: String,
}

/// Checks that `dir` holds a runnable server and reads its version.
///
/// # Errors
/// Returns a [`CoreError::Other`] with an actionable message when the file is
/// missing, will not start (typically a CUDA build without its runtime
/// DLLs), or times out.
pub async fn validate_engine(dir: &Path) -> CoreResult<EngineInfo> {
    let exe = dir.join(SERVER_EXE);
    if !exe.is_file() {
        return Err(CoreError::Other(format!("{} was not found in {}", SERVER_EXE, dir.display())));
    }
    let out = run_capture(&exe, dir, &["--version"]).await?;
    let version = out
        .lines()
        .find(|l| l.to_lowercase().contains("version"))
        .or_else(|| out.lines().next())
        .unwrap_or("unknown")
        .trim()
        .to_owned();
    Ok(EngineInfo { exe, version })
}

/// Runs `llama-server --help` and parses it into a catalog (spec §10.2).
///
/// # Errors
/// Returns an error if the server cannot be run.
pub async fn generate_catalog(dir: &Path, version: &str, generated_at: &str) -> CoreResult<ParamCatalog> {
    let exe = dir.join(SERVER_EXE);
    let help = run_capture(&exe, dir, &["--help"]).await?;
    Ok(parse_help(&help, version, generated_at))
}

/// Runs a command to completion and returns stdout and stderr combined
/// (llama.cpp prints some of this on stderr).
async fn run_capture(exe: &Path, cwd: &Path, args: &[&str]) -> CoreResult<String> {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let fut = cmd.output();
    let output = tokio::time::timeout(PROBE_TIMEOUT, fut)
        .await
        .map_err(|_| CoreError::Other(format!("{} did not finish within {PROBE_TIMEOUT:?}", exe.display())))?
        .map_err(|e| {
            CoreError::Other(format!(
                "could not run {}: {e}. For CUDA builds make sure the matching cudart DLLs sit next to the executable.",
                exe.display()
            ))
        })?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_override_wins_over_bundled() {
        let mut cfg = Config::default();
        cfg.engine.llama_server_dir = Some("D:\\mine".into());
        assert_eq!(resolve_engine_dir(&cfg, Some(Path::new("C:\\bundled"))).expect("dir"), PathBuf::from("D:\\mine"));
    }

    #[test]
    fn null_or_blank_uses_bundled() {
        let mut cfg = Config::default();
        assert_eq!(
            resolve_engine_dir(&cfg, Some(Path::new("C:\\bundled"))).expect("dir"),
            PathBuf::from("C:\\bundled")
        );
        cfg.engine.llama_server_dir = Some("  ".into());
        assert_eq!(
            resolve_engine_dir(&cfg, Some(Path::new("C:\\bundled"))).expect("dir"),
            PathBuf::from("C:\\bundled")
        );
    }

    #[test]
    fn nothing_available_is_an_actionable_error() {
        let err = resolve_engine_dir(&Config::default(), None).expect_err("error");
        assert!(err.to_string().contains("Engine settings"));
    }

    #[tokio::test]
    async fn missing_exe_is_reported() {
        let dir = tempfile::tempdir().expect("dir");
        let err = validate_engine(dir.path()).await.expect_err("missing");
        assert!(err.to_string().contains("was not found"));
    }
}
