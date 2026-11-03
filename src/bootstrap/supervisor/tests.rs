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
