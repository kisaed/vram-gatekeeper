//! Spawns real llama-server processes (spec §9.1).
//!
//! Responsibility: build the full command line, start the child hidden,
//! attach it to the kill-on-close job, capture its output, and report its
//! exit. Implements [`ProcessLauncher`] for production use.
//! Not responsible for: readiness (see `health`), state decisions (see
//! `gate::controller`), or engine selection (see `engine::install`).
//!
//! Implements NFR-5, FR-MOD-5.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use rand::RngExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, warn};

use super::ports::free_port;
use crate::config::schema::{Config, ModelConfig};
use crate::engine::install::resolve_engine_dir;
use crate::params::catalog::ParamCatalog;
use crate::params::{build_argv, render_command_line};
use crate::ports::{ExitInfo, LaunchError, LaunchedProcess, ProcessKiller, ProcessLauncher, Upstream};
use crate::sync::RwLockExt;

/// Process creation flag that stops Windows from opening a console window
/// for the child.
/// <https://learn.microsoft.com/windows/win32/procthread/process-creation-flags>
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Random bytes in a per-launch API key (spec §9.1 step 3).
const API_KEY_BYTES: usize = 32;

/// Where the pieces the launcher needs come from.
#[derive(Clone)]
pub struct LauncherContext {
    /// Live configuration.
    pub config: watch::Receiver<Arc<Config>>,
    /// Directory of the bundled engine, if the installer shipped one.
    pub bundled_engine_dir: Option<PathBuf>,
    /// Current parameter catalog (replaced when the engine changes).
    pub catalog: Arc<RwLock<ParamCatalog>>,
    /// Folder for per-model log files; `None` disables file logging.
    pub log_dir: Option<PathBuf>,
}

/// Launches `llama-server.exe` children.
pub struct RealLauncher {
    /// Shared inputs.
    ctx: LauncherContext,
    /// Kill-on-close job (Windows only).
    #[cfg(windows)]
    job: crate::platform::windows::job_object::JobObject,
}

impl RealLauncher {
    /// Creates a launcher. On Windows this creates the kill-on-close job
    /// object; if that fails the launcher refuses to exist rather than risk
    /// orphaned models in VRAM (NFR-5).
    ///
    /// # Errors
    /// Returns an error if the job object cannot be created.
    pub fn new(ctx: LauncherContext) -> crate::error::CoreResult<Self> {
        Ok(Self {
            ctx,
            #[cfg(windows)]
            job: crate::platform::windows::job_object::JobObject::new()?,
        })
    }
}

/// Assembles user arguments plus the reserved ones the user cannot override.
pub fn build_args(catalog: &ParamCatalog, cfg: &Config, model: &ModelConfig, port: u16, api_key: &str) -> Vec<String> {
    let mut args = build_argv(catalog, model).args;
    args.push("--model".into());
    args.push(model.files.model.clone());
    if let Some(mm) = &model.files.mmproj {
        args.push("--mmproj".into());
        args.push(mm.clone());
    }
    // The draft file is passed only together with a draft type (spec §10.6).
    args.extend(crate::speculative::launch_args(catalog, model).args);
    if let Some(dev) = &cfg.engine.device {
        args.push("--device".into());
        args.push(dev.clone());
    }
    // The GPU-layer placement line the overflow rule reads is only printed at
    // verbosity 4 and above; the default (3) hides it (checked against build
    // b11804, see tests/fixtures). Respect an explicit user choice.
    if catalog.resolve("verbosity").is_some() && !sets_verbosity(model) {
        args.extend(["--verbosity".into(), PLACEMENT_LOG_VERBOSITY.into()]);
    }
    // Reserved arguments last so nothing earlier can shadow them (spec §9.1).
    args.extend([
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
        "--api-key".into(),
        api_key.to_owned(),
    ]);
    args
}

/// Log verbosity that makes llama-server print `offloaded X/Y layers to GPU`.
const PLACEMENT_LOG_VERBOSITY: &str = "4";

/// Every spelling of the verbosity/verbose flags, without dashes.
const VERBOSITY_FLAGS: [&str; 6] = ["verbosity", "log-verbosity", "lv", "verbose", "log-verbose", "v"];

/// True if the user already chose a verbosity in `llama_args` or `extra_args`.
fn sets_verbosity(model: &ModelConfig) -> bool {
    model.llama_args.keys().any(|k| VERBOSITY_FLAGS.contains(&k.trim_start_matches('-')))
        || model.extra_args.iter().any(|a| VERBOSITY_FLAGS.contains(&a.trim_start_matches('-')))
}

/// Hex-encodes random bytes into a launch key.
fn new_api_key() -> String {
    let mut bytes = [0u8; API_KEY_BYTES];
    rand::rng().fill(&mut bytes);
    hex::encode(bytes)
}

/// Kills the child via a message to the task that owns it.
struct ChannelKiller {
    /// Sends "kill now" to the wait task.
    tx: mpsc::UnboundedSender<()>,
}

impl ProcessKiller for ChannelKiller {
    fn kill(&self) {
        // A closed channel means the process already ended.
        let _ = self.tx.send(());
    }
}

#[async_trait]
impl ProcessLauncher for RealLauncher {
    async fn launch(&self, model: &ModelConfig) -> Result<LaunchedProcess, LaunchError> {
        let cfg = self.ctx.config.borrow().clone();
        let engine_dir =
            resolve_engine_dir(&cfg, self.ctx.bundled_engine_dir.as_deref()).map_err(|e| LaunchError(e.to_string()))?;
        let exe = engine_dir.join("llama-server.exe");
        if !exe.is_file() {
            return Err(LaunchError(format!(
                "llama-server.exe not found in {}. Check Engine settings (or reinstall if this is the bundled engine).",
                engine_dir.display()
            )));
        }

        self.spawn_once(model, &cfg, &exe, &engine_dir).await
    }
}

impl RealLauncher {
    /// Starts one child on a fresh port.
    async fn spawn_once(
        &self,
        model: &ModelConfig,
        cfg: &Config,
        exe: &Path,
        engine_dir: &Path,
    ) -> Result<LaunchedProcess, LaunchError> {
        let port = free_port().map_err(|e| LaunchError(e.to_string()))?;
        let api_key = new_api_key();
        let args = {
            let catalog = self.ctx.catalog.read_ok();
            build_args(&catalog, cfg, model, port, &api_key)
        };
        debug!(model = %model.id, command = %render_command_line(&exe.to_string_lossy(), &args), "launching");

        let mut cmd = Command::new(exe);
        cmd.args(&args)
            .envs(&model.env)
            .current_dir(engine_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        let mut child = cmd.spawn().map_err(|e| LaunchError(format!("could not start {}: {e}", exe.display())))?;
        let pid = child.id().ok_or_else(|| LaunchError("child exited immediately".into()))?;

        #[cfg(windows)]
        if let Some(handle) = child.raw_handle() {
            // Failure is logged, not fatal: the child still works, it just
            // would not die with us if we crashed.
            if let Err(e) = self.job.assign(handle) {
                warn!(model = %model.id, error = %e, "could not attach child to the kill-on-close job");
            }
        }

        let (log_tx, logs) = mpsc::unbounded_channel::<String>();
        let file_tx = open_log_file(self.ctx.log_dir.as_deref(), &model.id).await;
        if let Some(out) = child.stdout.take() {
            tokio::spawn(pump_lines(out, log_tx.clone(), file_tx.clone()));
        }
        if let Some(err) = child.stderr.take() {
            tokio::spawn(pump_lines(err, log_tx.clone(), file_tx));
        }
        drop(log_tx);

        let (exit_tx, exited) = oneshot::channel();
        let (kill_tx, mut kill_rx) = mpsc::unbounded_channel::<()>();
        tokio::spawn(async move {
            let status = loop {
                tokio::select! {
                    status = child.wait() => break status,
                    Some(()) = kill_rx.recv() => {
                        // TerminateProcess; the loop then reaps the exit status.
                        let _ = child.start_kill();
                    }
                }
            };
            let code = status.ok().and_then(|s| s.code());
            // The controller fills the log tail from its ring buffer.
            let _ = exit_tx.send(ExitInfo { code, log_tail: Vec::new() });
        });

        Ok(LaunchedProcess {
            pid,
            upstream: Upstream { port, api_key },
            exited,
            killer: Arc::new(ChannelKiller { tx: kill_tx }),
            logs,
        })
    }
}

/// Reads lines from a child pipe into the controller channel and log file.
async fn pump_lines<R>(reader: R, tx: mpsc::UnboundedSender<String>, file: Option<mpsc::UnboundedSender<String>>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(f) = &file {
            let _ = f.send(line.clone());
        }
        if tx.send(line).is_err() {
            break;
        }
    }
}

/// Starts a writer task appending to today's log file for `model`.
async fn open_log_file(dir: Option<&Path>, model: &str) -> Option<mpsc::UnboundedSender<String>> {
    let dir = dir?;
    tokio::fs::create_dir_all(dir).await.ok()?;
    let path = dir.join(format!("{}-{}.log", sanitize(model), super::log_files::today()));
    let mut file = tokio::fs::OpenOptions::new().create(true).append(true).open(path).await.ok()?;
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if file.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                break;
            }
        }
    });
    Some(tx)
}

/// Makes a model id safe to use in a file name.
fn sanitize(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::help_parser::parse_help;

    #[test]
    fn reserved_arguments_come_last_and_use_the_given_port_and_key() {
        let catalog = parse_help("-c, --ctx-size N   context (default: 0)\n", "t", "t");
        let mut model = ModelConfig::default();
        model.files.model = "m.gguf".into();
        model.llama_args.insert("ctx-size".into(), 4096.into());
        let cfg = Config::default();
        let args = build_args(&catalog, &cfg, &model, 5555, "KEY");
        assert_eq!(
            args,
            ["--ctx-size", "4096", "--model", "m.gguf", "--host", "127.0.0.1", "--port", "5555", "--api-key", "KEY"]
        );
    }

    #[test]
    fn device_and_mmproj_are_added_when_configured() {
        let mut model = ModelConfig::default();
        model.files.model = "m.gguf".into();
        model.files.mmproj = Some("p.gguf".into());
        let mut cfg = Config::default();
        cfg.engine.device = Some("CUDA0".into());
        let args = build_args(&ParamCatalog::default(), &cfg, &model, 1, "k");
        assert!(args.windows(2).any(|w| w == ["--mmproj", "p.gguf"]));
        assert!(args.windows(2).any(|w| w == ["--device", "CUDA0"]));
    }

    #[test]
    fn spec_draft_model_pair_is_added_for_a_draft_type() {
        let catalog = crate::params::help_parser::parse_help(
            include_str!("../../tests/fixtures/llama-server-b11804-help.txt"),
            "b11804",
            "2026-10-27",
        );
        let mut model = ModelConfig::default();
        model.files.model = "m.gguf".into();
        model.files.draft = Some("C:\\My Models\\d.gguf".into());
        let without = build_args(&catalog, &Config::default(), &model, 1, "k");
        assert!(!without.iter().any(|a| a.contains("d.gguf")), "Off: the file is kept but not passed");
        model.llama_args.insert("spec-type".into(), "draft-simple".into());
        let with = build_args(&catalog, &Config::default(), &model, 1, "k");
        assert!(with.windows(2).any(|w| w == ["--spec-draft-model", "C:\\My Models\\d.gguf"]), "{with:?}");
    }

    #[test]
    fn api_keys_are_unique_hex() {
        let (a, b) = (new_api_key(), new_api_key());
        assert_ne!(a, b);
        assert_eq!(a.len(), API_KEY_BYTES * 2);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn model_ids_are_sanitised_for_file_names() {
        assert_eq!(sanitize("qwen3:8b/q4"), "qwen3_8b_q4");
    }
}
