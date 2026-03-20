use crate::config::PlatformConfig;
use ploy_deployments::WorkerSupervisor;
use ploy_platform::ControlPlane;

#[derive(Debug, Default)]
pub struct PloyDaemon {
    pub control_plane: ControlPlane,
    pub supervisor: WorkerSupervisor,
}

impl PloyDaemon {
    pub fn boot(config: &PlatformConfig) -> Self {
        let mut control_plane = ControlPlane::default();
        control_plane.system.set_status(format!("listening@{}", config.listen_addr));

        Self {
            control_plane,
            supervisor: WorkerSupervisor::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PloyDaemon;
    use crate::config::PlatformConfig;

    #[test]
    fn daemon_loads_platform_config() {
        let config = PlatformConfig {
            listen_addr: "127.0.0.1:9090".to_string(),
        };

        let daemon = PloyDaemon::boot(&config);
        let status = daemon.control_plane.system.status();
        assert!(status.status.contains("127.0.0.1:9090"));
    }
}
