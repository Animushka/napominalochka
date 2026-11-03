use super::*;

#[test]
fn stale_planner_and_shutdown_make_the_process_unready() {
    let mut health = Health::new();
    assert!(health.is_ready());
    Arc::get_mut(&mut health.planner.0).unwrap().started =
        Instant::now() - Duration::from_secs(120);
    assert!(!health.is_ready());
    health.heartbeat().beat();
    assert!(health.is_ready());
    health.stop();
    health.heartbeat().beat();
    assert!(!health.is_ready());
}
