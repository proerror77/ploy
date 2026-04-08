mod config;
mod events;
mod http;
mod runtime;

use config::PlatformConfig;
use events::EventBroker;
use http::{publish_snapshot_events, AppState};
use runtime::{run_shared_forever, PloyDaemon};
use std::sync::{Arc, RwLock};

extern "C" fn shutdown_signal_handler(_sig: libc::c_int) {
    runtime::request_shutdown();
}

fn main() {
    let config = PlatformConfig::from_env();
    let daemon = Arc::new(RwLock::new(PloyDaemon::boot(&config).expect("boot ployd")));
    let events = Arc::new(EventBroker::default());
    {
        let mut daemon = daemon.write().expect("daemon lock");
        if let Err(err) = daemon.write_runtime_snapshots() {
            eprintln!("ployd boot degraded: {err}");
        }
        publish_snapshot_events(&daemon, &events);
    }
    let state = Arc::new(AppState {
        daemon: daemon.clone(),
        events: events.clone(),
    });
    let _server = http::spawn_server(state).expect("start ployd http server");
    let daemon_guard = daemon.read().expect("daemon lock");
    let status = daemon_guard.control_plane.system.status();
    let worker_count = daemon_guard.supervisor.workers().count();

    eprintln!("ployd booted");
    eprintln!("{}", http::render_status(&status));
    eprintln!("workers={worker_count}");

    drop(daemon_guard);

    // Register signal handlers for graceful shutdown
    unsafe {
        libc::signal(libc::SIGTERM, shutdown_signal_handler as libc::sighandler_t);
        libc::signal(libc::SIGINT, shutdown_signal_handler as libc::sighandler_t);
    }

    run_shared_forever(daemon, events).expect("run ployd");
}
