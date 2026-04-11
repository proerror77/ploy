fn main() {
    let config = ploy_daemon_host::config::PlatformConfig::from_env();
    let daemon = std::sync::Arc::new(std::sync::Mutex::new(
        ploy_daemon_host::runtime::PloyDaemon::boot(&config).expect("boot new-ployd"),
    ));
    let events = std::sync::Arc::new(ploy_daemon_host::events::EventBroker::default());
    {
        let mut daemon = daemon.lock().expect("daemon lock");
        if let Err(err) = daemon.write_runtime_snapshots() {
            eprintln!("new-ployd boot degraded: {err}");
        }
        ploy_daemon_host::http::publish_snapshot_events(&daemon, &events);
    }
    let state = std::sync::Arc::new(ploy_daemon_host::http::AppState {
        daemon: daemon.clone(),
        events: events.clone(),
    });
    let _server = ploy_daemon_host::http::spawn_server(state).expect("start new-ployd http server");
    let daemon_guard = daemon.lock().expect("daemon lock");
    let status = daemon_guard.control_plane.system.status();
    let worker_count = daemon_guard.supervisor.workers().count();
    eprintln!("new-ployd booted");
    eprintln!("{}", ploy_daemon_host::http::render_status(&status));
    eprintln!("workers={worker_count}");
    drop(daemon_guard);
    ploy_daemon_host::runtime::run_shared_forever(daemon, events).expect("run new-ployd");
}
