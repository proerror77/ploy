mod config;
mod events;
mod http;
mod runtime;

use config::PlatformConfig;
use events::EventBroker;
use http::{publish_snapshot_events, AppState};
use runtime::{run_shared_forever, PloyDaemon};
use std::sync::{Arc, Mutex};

fn main() {
    let config = PlatformConfig::from_env();
    let daemon = Arc::new(Mutex::new(PloyDaemon::boot(&config).expect("boot ployd")));
    let events = Arc::new(EventBroker::default());
    {
        let mut daemon = daemon.lock().expect("daemon lock");
        daemon
            .write_runtime_snapshots()
            .expect("write initial snapshots");
        publish_snapshot_events(&daemon, &events);
    }
    let state = Arc::new(AppState {
        daemon: daemon.clone(),
        events: events.clone(),
    });
    let _server = http::spawn_server(state).expect("start ployd http server");
    let daemon_guard = daemon.lock().expect("daemon lock");
    let status = daemon_guard.control_plane.system.status();
    let worker_count = daemon_guard.supervisor.workers().count();

    eprintln!("ployd booted");
    eprintln!("{}", http::render_status(&status));
    eprintln!("workers={worker_count}");

    drop(daemon_guard);
    run_shared_forever(daemon, events).expect("run ployd");
}
