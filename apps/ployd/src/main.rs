mod config;
mod http;
mod runtime;

use config::PlatformConfig;
use runtime::{run_shared_forever, PloyDaemon};
use std::sync::{Arc, Mutex};

fn main() {
    let config = PlatformConfig::from_env();
    let daemon = Arc::new(Mutex::new(PloyDaemon::boot(&config).expect("boot ployd")));
    {
        let mut daemon = daemon.lock().expect("daemon lock");
        daemon
            .write_runtime_snapshots()
            .expect("write initial snapshots");
    }
    let _server = http::spawn_server(daemon.clone()).expect("start ployd http server");
    let daemon_guard = daemon.lock().expect("daemon lock");
    let status = daemon_guard.control_plane.system.status();
    let worker_count = daemon_guard.supervisor.workers().count();

    eprintln!("ployd booted");
    eprintln!("{}", http::render_status(&status));
    eprintln!("workers={worker_count}");

    drop(daemon_guard);
    run_shared_forever(daemon).expect("run ployd");
}
