mod config;
mod http;
mod runtime;

use config::PlatformConfig;
use runtime::PloyDaemon;

fn main() {
    let config = PlatformConfig::default();
    let daemon = PloyDaemon::boot(&config);
    let status = daemon.control_plane.system.status();
    let worker_count = daemon.supervisor.workers().count();

    eprintln!("ployd booted");
    eprintln!("{}", http::render_status(&status));
    eprintln!("workers={worker_count}");
}
