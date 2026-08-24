//! CoreSupervisor: exclusive owner of the process Tokio runtime, the
//! cancellation hierarchy, and the deterministic shutdown sequence.
//!
//! Ownership rules:
//! - every background task spawns through [`CoreSupervisor::spawn_owned`],
//!   attaching it to the cancellation tree;
//! - detached tasks are forbidden outside test-support;
//! - account-scoped work attaches to an account token.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::events::InvalidationPublisher;
use crate::runtime::{CoreRuntime, RuntimeConfig};

#[derive(Clone, Debug)]
pub struct SupervisorConfig {
    pub runtime: RuntimeConfig,
    /// Root data directory: `<data_dir>/accounts/<id>/store.sqlite3`,
    /// `<data_dir>/media-cache`, `<data_dir>/logs`.
    pub data_dir: PathBuf,
}

impl SupervisorConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            runtime: RuntimeConfig::default(),
            data_dir: data_dir.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("runtime build failed: {0}")]
    Runtime(#[from] std::io::Error),
    #[error("supervisor already shut down")]
    ShutDown,
}

struct Inner {
    command_gate_open: AtomicBool,
    /// Owned tasks that have not finished yet. Shutdown drains until zero or
    /// the timeout elapses — never a fixed sleep.
    active_tasks: AtomicU64,
}

/// The single execution-domain owner for runtime tasks and shutdown.
pub struct CoreSupervisor {
    runtime: Option<CoreRuntime>,
    config: SupervisorConfig,
    root_token: CancellationToken,
    inner: Arc<Inner>,
    invalidations: InvalidationPublisher,
}

impl CoreSupervisor {
    /// Build the runtime and prepare the cancellation hierarchy. Does not
    /// touch the network or storage yet; startup is deterministic.
    pub fn start(config: SupervisorConfig) -> Result<Self, SupervisorError> {
        let runtime = CoreRuntime::build(config.runtime.clone())?;
        info!(
            workers = config.runtime.worker_threads,
            blocking = config.runtime.max_blocking_threads,
            "core runtime started"
        );
        Ok(Self {
            runtime: Some(runtime),
            config,
            root_token: CancellationToken::new(),
            inner: Arc::new(Inner {
                command_gate_open: AtomicBool::new(true),
                active_tasks: AtomicU64::new(0),
            }),
            invalidations: InvalidationPublisher::new(),
        })
    }

    pub fn config(&self) -> &SupervisorConfig {
        &self.config
    }

    /// Handle for spawning core-domain work. GPUI code must NOT hold this for
    /// foreground work; it crosses the service boundary only.
    pub fn handle(&self) -> tokio::runtime::Handle {
        self.runtime
            .as_ref()
            .expect("runtime until shutdown")
            .handle()
    }

    /// Root of the cancellation tree. Account tokens are children.
    pub fn root_cancellation(&self) -> CancellationToken {
        self.root_token.clone()
    }

    /// Child token for a named subsystem/account subtree.
    pub fn child_token(&self, name: &str) -> CancellationToken {
        debug!(subsystem = name, "registered cancellation subtree");
        self.root_token.child_token()
    }

    pub fn invalidations(&self) -> &InvalidationPublisher {
        &self.invalidations
    }

    /// Command gate: UI commands are refused once shutdown begins (step 1 of
    /// the shutdown sequence).
    pub fn commands_accepted(&self) -> bool {
        self.inner.command_gate_open.load(Ordering::Acquire)
    }

    /// Spawn a task tied to the given cancellation token. When the token
    /// fires, `fut` is dropped at its next await point (real cancellation).
    /// The join handle is deliberately NOT detached: callers keep it or drop
    /// it after linking into their own supervision. Panics surface via the
    /// handle.
    pub fn spawn_owned<F>(
        &self,
        token: &CancellationToken,
        name: &'static str,
        fut: F,
    ) -> tokio::task::JoinHandle<Option<F::Output>>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let token = token.clone();
        let _ = name; // surfaced via tracing spans once tracing-util lands
        let inner = Arc::clone(&self.inner);
        inner.active_tasks.fetch_add(1, Ordering::AcqRel);
        self.handle().spawn(async move {
            let out = tokio::select! {
                biased;
                _ = token.cancelled() => None,
                out = fut => Some(out),
            };
            inner.active_tasks.fetch_sub(1, Ordering::AcqRel);
            out
        })
    }

    /// Deterministic shutdown:
    /// 1. stop accepting UI commands
    /// 2. cancel ephemeral/background work (token tree)
    /// 3. durable boundaries were flushed by owners reacting to (1)/(2)
    ///    before their tasks observe cancellation — enforced by phase tests
    /// 4. storage closes when its last handle drops (repository owns pools)
    /// 5. runtime shuts down in the background after the drain window
    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        let rt = match self.runtime.take() {
            Some(rt) => rt,
            None => return,
        };
        if !self.inner.command_gate_open.swap(false, Ordering::AcqRel) {
            warn!("shutdown: command gate already closed");
        }
        info!("shutdown: cancelling background work");
        self.root_token.cancel();
        let timeout = rt.config().shutdown_timeout;
        // Bounded drain: wait only while owned tasks are actually finishing.
        let inner = Arc::clone(&self.inner);
        rt.block_on(async move {
            let deadline = tokio::time::Instant::now() + timeout;
            while inner.active_tasks.load(Ordering::Acquire) > 0 {
                if tokio::time::Instant::now() >= deadline {
                    warn!("shutdown: drain window elapsed with tasks outstanding");
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
        });
        info!("shutdown: releasing runtime");
        rt.into_inner().shutdown_background();
    }
}

impl Drop for CoreSupervisor {
    fn drop(&mut self) {
        // Safety net mirroring upstream PR #218 discipline: dropping without
        // explicit shutdown still tears down deterministically instead of
        // leaking threads.
        self.shutdown_inner();
    }
}
