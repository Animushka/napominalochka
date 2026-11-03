//! Owns mandatory workers and bounds shutdown, including dispatcher cleanup.

use std::future::Future;
use std::time::Duration;

use anyhow::anyhow;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub(crate) struct Supervisor {
    tasks: JoinSet<(&'static str, anyhow::Result<()>)>,
    grace: Duration,
}

impl Supervisor {
    pub(crate) fn new(grace: Duration) -> Self {
        Self {
            tasks: JoinSet::new(),
            grace,
        }
    }

    pub(crate) fn spawn<F>(&mut self, name: &'static str, work: F)
    where
        F: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        self.tasks.spawn(async move { (name, work.await) });
    }

    pub(crate) async fn run_until<S, C, F>(
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
mod tests;
