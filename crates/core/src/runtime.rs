//! Deliberately configured Tokio runtime (charter §8).
//!
//! One runtime per process. Worker counts start conservative and change only
//! with benchmark evidence (§114). Thread names make leaks visible in
//! thread-count tests (INV-2).

use std::time::Duration;

use tokio::runtime::{Builder, Runtime};

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub worker_threads: usize,
    pub max_blocking_threads: usize,
    pub thread_name_prefix: String,
    /// Grace period for the final drain before forced teardown.
    pub shutdown_timeout: Duration,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            worker_threads: 4,
            max_blocking_threads: 8,
            thread_name_prefix: "wasabi-core".into(),
            shutdown_timeout: Duration::from_secs(5),
        }
    }
}

/// Owned runtime handle. Dropping it after `shutdown` is safe; dropping it
/// without shutdown would block on in-flight tasks, so `CoreSupervisor` always
/// shuts down explicitly first (deterministic lifecycle, INV-17).
pub struct CoreRuntime {
    rt: Runtime,
    config: RuntimeConfig,
}

impl CoreRuntime {
    pub fn build(config: RuntimeConfig) -> Result<Self, std::io::Error> {
        let rt = Builder::new_multi_thread()
            .worker_threads(config.worker_threads.max(1))
            .max_blocking_threads(config.max_blocking_threads.max(1))
            .thread_name(format!("{}-w", config.thread_name_prefix))
            .enable_all()
            .build()?;
        Ok(Self { rt, config })
    }

    pub fn handle(&self) -> tokio::runtime::Handle {
        self.rt.handle().clone()
    }

    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    /// Run a future to completion on the runtime from a foreign thread.
    /// Only the supervisor's control paths may use this — never GPUI code
    /// (INV-1).
    pub fn block_on<F: std::future::Future>(&self, fut: F) -> F::Output {
        self.rt.block_on(fut)
    }

    pub(crate) fn into_inner(self) -> Runtime {
        self.rt
    }
}
