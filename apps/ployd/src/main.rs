mod config;
mod http;
mod runtime;

use config::PlatformConfig;
use runtime::PloyDaemon;

fn main() {
    let config = PlatformConfig::from_env();
    let _server = http::spawn_server(config.clone()).expect("start ployd http server");
    let mut daemon = PloyDaemon::boot(&config).expect("boot ployd");
    let status = daemon.control_plane.system.status();
    let worker_count = daemon.supervisor.workers().count();

    eprintln!("ployd booted");
    eprintln!("{}", http::render_status(&status));
    eprintln!("workers={worker_count}");

    daemon.run_forever().expect("run ployd");
}
