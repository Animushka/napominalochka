//! Owns mandatory workers and bounds shutdown, including dispatcher cleanup.

use std::future::Future;
use std::time::Duration;

use anyhow::anyhow;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub struct Supervisor {
    tasks: JoinSet<(&'static str, anyhow::Result<()>)>,
    grace: Duration,
}

impl Supervisor {
    pub fn new(grace: Duration) -> Self {
        Self {
            tasks: JoinSet::new(),
            grace,
        }
    }

    pub fn spawn<F>(&mut self, name: &'static str, work: F)
    where
        F: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        self.tasks.spawn(async move { (name, work.await) });
    }

    pub async fn run_until<S, C, F>(
        mut self,
        signal: S,
        shutdown: CancellationToken,
        on_shutdown: C,
    ) -> anyhow::Result<()>
    where
        S: Future<Output = ()>,
        C: FnOnce() -> F,
        F: Future<Output = ()>,
    {
        let mut failure = tokio::select! {
            biased;
            () = signal => None,
            outcome = self.tasks.join_next() => Some(match outcome {
                Some(Ok((name, Ok(())))) => anyhow!("mandatory worker {name} stopped unexpectedly"),
                Some(Ok((name, Err(error)))) => error.context(format!("mandatory worker {name} failed")),
                Some(Err(error)) => anyhow!(error).context("mandatory worker panicked or was cancelled"),
                None => anyhow!("no mandatory workers are running"),
            }),
        };
        shutdown.cancel();
        let drain = async {
            on_shutdown().await;
            while let Some(outcome) = self.tasks.join_next().await {
                let error = match outcome {
                    Ok((_, Ok(()))) => None,
                    Ok((name, Err(error))) => {
                        Some(error.context(format!("worker {name} failed during shutdown")))
                    }
                    Err(error) => Some(anyhow!(error).context("worker panicked during shutdown")),
                };
                if let Some(error) = error {
                    tracing::error!(?error, "worker shutdown failed");
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
            }
        };
        if tokio::time::timeout(self.grace, drain).await.is_err() {
            self.tasks.abort_all();
            while self.tasks.join_next().await.is_some() {}
            return Err(
                failure.unwrap_or_else(|| anyhow!("workers did not stop within {:?}", self.grace))
            );
        }
        failure.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[tokio::test]
    async fn a_worker_error_cancels_siblings_and_is_reported() {
        let shutdown = CancellationToken::new();
        let stopped = Arc::new(AtomicBool::new(false));
        let mut supervisor = Supervisor::new(Duration::from_secs(1));
        supervisor.spawn("web", async { anyhow::bail!("port occupied") });
        let worker_shutdown = shutdown.clone();
        let observed = stopped.clone();
        supervisor.spawn("planner", async move {
            worker_shutdown.cancelled().await;
            observed.store(true, Ordering::SeqCst);
            Ok(())
        });
        let error = supervisor
            .run_until(std::future::pending(), shutdown, || async {})
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("port occupied"));
        assert!(stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn panics_are_observed_while_other_workers_are_running() {
        let shutdown = CancellationToken::new();
        let mut supervisor = Supervisor::new(Duration::from_secs(1));
        supervisor.spawn("broken", async { panic!("worker failure") });
        let error = supervisor
            .run_until(std::future::pending(), shutdown.clone(), || async {})
            .await
            .unwrap_err();
        assert!(shutdown.is_cancelled());
        assert!(format!("{error:#}").contains("panicked"));
    }

    #[tokio::test]
    async fn a_stuck_worker_has_a_bounded_shutdown() {
        let shutdown = CancellationToken::new();
        let mut supervisor = Supervisor::new(Duration::from_millis(20));
        supervisor.spawn("stuck", std::future::pending());
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            supervisor.run_until(async {}, shutdown, || async {}),
        )
        .await;
        assert!(result.unwrap().is_err());
    }

    #[tokio::test]
    async fn requested_shutdown_drains_workers_successfully() {
        let shutdown = CancellationToken::new();
        let worker_shutdown = shutdown.clone();
        let mut supervisor = Supervisor::new(Duration::from_secs(1));
        supervisor.spawn("worker", async move {
            worker_shutdown.cancelled().await;
            Ok(())
        });
        supervisor
            .run_until(async {}, shutdown, || async {})
            .await
            .unwrap();
    }
}
