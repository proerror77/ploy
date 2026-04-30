use crate::protocol::{WorkerLaunchSpec, WorkerStatus};
use chrono::Utc;
use ploy_operator_contracts::ObservedState;
use std::fs;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

#[derive(Debug)]
pub struct DeploymentRuntime {
    spec: WorkerLaunchSpec,
    status: WorkerStatus,
    child: Option<Child>,
}

impl DeploymentRuntime {
    pub fn new(spec: WorkerLaunchSpec) -> Self {
        let mut runtime = Self {
            status: WorkerStatus {
                deployment_id: spec.deployment_id.clone(),
                observed_state: ObservedState::Starting,
                last_heartbeat: Utc::now(),
                pid: None,
                last_error: None,
            },
            spec,
            child: None,
        };
        runtime.launch();
        runtime
    }

    fn launch(&mut self) {
        if let Some(pid) = self.existing_live_pid() {
            self.status.observed_state = ObservedState::Running;
            self.status.last_heartbeat = Utc::now();
            self.status.pid = Some(pid);
            self.status.last_error = None;
            self.child = None;
            return;
        }

        let mut command = Command::new(&self.spec.command);
        command
            .args(&self.spec.args)
            .current_dir(&self.spec.working_directory)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        match command.spawn() {
            Ok(child) => {
                self.status.observed_state = ObservedState::Starting;
                self.status.last_heartbeat = Utc::now();
                self.status.pid = Some(child.id());
                self.status.last_error = None;
                let _ = self.persist_pid(child.id());
                self.child = Some(child);
            }
            Err(error) => {
                self.status.observed_state = ObservedState::Failed;
                self.status.last_heartbeat = Utc::now();
                self.status.pid = None;
                self.status.last_error = Some(error.to_string());
                let _ = fs::remove_file(&self.spec.pid_file);
                self.child = None;
            }
        }
    }

    pub fn boot_status(&self) -> &WorkerStatus {
        &self.status
    }

    pub fn refresh_status(&mut self) -> &mut WorkerStatus {
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(None) => {
                    if self.status.observed_state == ObservedState::Starting {
                        self.status.observed_state = ObservedState::Running;
                    }
                    self.status.last_heartbeat = Utc::now();
                }
                Ok(Some(exit)) => {
                    self.child = None;
                    self.status.pid = None;
                    self.status.last_heartbeat = Utc::now();
                    let _ = fs::remove_file(&self.spec.pid_file);
                    if !matches!(
                        self.status.observed_state,
                        ObservedState::Paused | ObservedState::Stopped
                    ) {
                        self.status.observed_state = if exit.success() {
                            ObservedState::Stopped
                        } else {
                            ObservedState::Failed
                        };
                        self.status.last_error = Some(format!("worker exited with {exit}"));
                    }
                }
                Err(error) => {
                    self.child = None;
                    self.status.pid = None;
                    self.status.observed_state = ObservedState::Failed;
                    self.status.last_heartbeat = Utc::now();
                    self.status.last_error = Some(error.to_string());
                    let _ = fs::remove_file(&self.spec.pid_file);
                }
            }
        } else if let Some(pid) = self.existing_live_pid() {
            self.status.pid = Some(pid);
            self.status.observed_state = ObservedState::Running;
            self.status.last_heartbeat = Utc::now();
        } else if self.status.pid.is_some()
            && !matches!(
                self.status.observed_state,
                ObservedState::Paused | ObservedState::Stopped
            )
        {
            self.status.pid = None;
            self.status.observed_state = ObservedState::Failed;
            self.status.last_heartbeat = Utc::now();
            self.status.last_error =
                Some("worker pid missing or no longer matches launch spec".to_string());
        }

        &mut self.status
    }

    pub fn status(&self) -> &WorkerStatus {
        &self.status
    }

    pub fn fail(&mut self) -> &mut WorkerStatus {
        self.child = None;
        self.status.pid = None;
        self.status.observed_state = ObservedState::Failed;
        self.status.last_heartbeat = Utc::now();
        let _ = fs::remove_file(&self.spec.pid_file);
        &mut self.status
    }

    pub fn pause(&mut self) -> &mut WorkerStatus {
        self.kill_child();
        self.status.pid = None;
        self.status.observed_state = ObservedState::Paused;
        self.status.last_heartbeat = Utc::now();
        let _ = fs::remove_file(&self.spec.pid_file);
        &mut self.status
    }

    pub fn stop(&mut self) -> &mut WorkerStatus {
        self.kill_child();
        self.status.pid = None;
        self.status.observed_state = ObservedState::Stopped;
        self.status.last_heartbeat = Utc::now();
        let _ = fs::remove_file(&self.spec.pid_file);
        &mut self.status
    }

    pub fn restart(&mut self) -> &mut WorkerStatus {
        self.kill_child();
        self.launch();
        self.refresh_status()
    }

    fn kill_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        } else if let Some(pid) = self.status.pid {
            if process_matches_spec(pid, &self.spec) {
                kill_pid(pid);
            }
        }
        let _ = fs::remove_file(&self.spec.pid_file);
    }

    fn persist_pid(&self, pid: u32) -> std::io::Result<()> {
        if let Some(parent) = self.spec.pid_file.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.spec.pid_file, format!("{pid}\n"))
    }

    fn existing_live_pid(&self) -> Option<u32> {
        let raw = fs::read_to_string(&self.spec.pid_file).ok()?;
        let pid: u32 = raw.trim().parse().ok()?;
        if process_matches_spec(pid, &self.spec) {
            Some(pid)
        } else {
            let _ = fs::remove_file(&self.spec.pid_file);
            None
        }
    }
}

fn process_alive(pid: u32) -> bool {
    process_alive_impl(pid)
}

#[cfg(target_os = "linux")]
fn process_alive_impl(pid: u32) -> bool {
    PathBuf::from(format!("/proc/{pid}")).exists()
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_alive_impl(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn process_alive_impl(_pid: u32) -> bool {
    false
}

fn process_matches_spec(pid: u32, spec: &WorkerLaunchSpec) -> bool {
    process_alive(pid) && process_identity_matches(pid, spec)
}

#[cfg(target_os = "linux")]
fn process_identity_matches(pid: u32, spec: &WorkerLaunchSpec) -> bool {
    let Ok(raw) = fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };

    let parts = raw
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect::<Vec<_>>();

    if parts.is_empty() {
        return false;
    }

    let expected_command = spec.command.to_string_lossy().into_owned();
    if parts[0] != expected_command {
        return false;
    }

    parts[1..] == spec.args
}

#[cfg(not(target_os = "linux"))]
fn process_identity_matches(pid: u32, _spec: &WorkerLaunchSpec) -> bool {
    process_alive(pid)
}

fn kill_pid(pid: u32) {
    let _ = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status();

    for _ in 0..20 {
        if !process_alive(pid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let _ = Command::new("kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .status();
}

#[cfg(test)]
mod tests {
    use super::DeploymentRuntime;
    use super::process_alive;
    use crate::protocol::WorkerLaunchSpec;
    #[cfg(target_os = "linux")]
    use crate::protocol::WorkerStatus;
    #[cfg(target_os = "linux")]
    use chrono::Utc;
    use ploy_operator_contracts::{DesiredState, ObservedState};
    use std::path::PathBuf;
    #[cfg(target_os = "linux")]
    use std::thread;
    #[cfg(target_os = "linux")]
    use std::time::Duration;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_pid_file(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("duration")
            .as_nanos();
        std::env::temp_dir().join(format!("ploy-deployments-{label}-{unique}.pid"))
    }

    fn shell_sleep_spec() -> WorkerLaunchSpec {
        WorkerLaunchSpec {
            deployment_id: "example.paper".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: "paper".to_string(),
            desired_state: DesiredState::Running,
            command: PathBuf::from("/bin/sleep"),
            args: vec!["30".to_string()],
            working_directory: std::env::current_dir().expect("cwd"),
            pid_file: unique_pid_file("test"),
        }
    }

    #[cfg(target_os = "linux")]
    fn wait_for_process_identity(pid: u32, spec: &WorkerLaunchSpec) {
        for _ in 0..20 {
            if super::process_matches_spec(pid, spec) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }

        assert!(
            super::process_matches_spec(pid, spec),
            "process {pid} did not settle to the expected launch identity"
        );
    }

    #[test]
    fn starts_worker_process() {
        let runtime = DeploymentRuntime::new(shell_sleep_spec());
        assert_eq!(
            runtime.boot_status().observed_state,
            ObservedState::Starting
        );
        assert!(runtime.boot_status().pid.is_some());
    }

    #[test]
    fn heartbeat_marks_running_worker() {
        let mut runtime = DeploymentRuntime::new(shell_sleep_spec());
        let status = runtime.refresh_status();
        assert_eq!(status.observed_state, ObservedState::Running);
    }

    #[test]
    fn stop_marks_worker_stopped() {
        let mut runtime = DeploymentRuntime::new(shell_sleep_spec());
        let status = runtime.stop();
        assert_eq!(status.observed_state, ObservedState::Stopped);
        assert!(status.pid.is_none());
    }

    #[test]
    fn bad_command_marks_worker_failed() {
        let mut spec = shell_sleep_spec();
        spec.command = PathBuf::from("/definitely/missing/ploy-runner");
        let runtime = DeploymentRuntime::new(spec);
        assert_eq!(runtime.boot_status().observed_state, ObservedState::Failed);
        assert!(runtime.boot_status().last_error.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn process_alive_detects_current_process_on_unix() {
        assert!(process_alive(std::process::id()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn existing_pid_file_prevents_duplicate_spawn() {
        let pid_file = unique_pid_file("existing");
        let _ = std::fs::remove_file(&pid_file);

        let spec = WorkerLaunchSpec {
            pid_file: pid_file.clone(),
            ..shell_sleep_spec()
        };
        let first = DeploymentRuntime::new(spec.clone());
        let pid = first.boot_status().pid.expect("first pid");
        wait_for_process_identity(pid, &spec);

        let second = DeploymentRuntime::new(spec);
        assert_eq!(second.boot_status().pid, Some(pid));

        let _ = std::fs::remove_file(pid_file);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inherited_pid_can_be_stopped_without_child_handle() {
        let pid_file = unique_pid_file("inherited");
        let _ = std::fs::remove_file(&pid_file);

        let mut first = DeploymentRuntime::new(WorkerLaunchSpec {
            pid_file: pid_file.clone(),
            ..shell_sleep_spec()
        });
        let pid = first.boot_status().pid.expect("first pid");

        let mut inherited = DeploymentRuntime::new(WorkerLaunchSpec {
            pid_file: pid_file.clone(),
            ..shell_sleep_spec()
        });
        assert_eq!(inherited.boot_status().pid, Some(pid));

        let status = inherited.stop();
        assert_eq!(status.observed_state, ObservedState::Stopped);
        first.refresh_status();
        assert!(!process_alive(pid));

        let _ = std::fs::remove_file(pid_file);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mismatched_inherited_pid_spawns_new_worker_and_does_not_kill_foreign_process() {
        let pid_file = unique_pid_file("mismatch");
        let _ = std::fs::remove_file(&pid_file);

        let mut first = DeploymentRuntime::new(WorkerLaunchSpec {
            pid_file: pid_file.clone(),
            ..shell_sleep_spec()
        });
        let first_pid = first.boot_status().pid.expect("first pid");

        let mut second_spec = shell_sleep_spec();
        second_spec.pid_file = pid_file.clone();
        second_spec.args = vec!["31".to_string()];
        let mut second = DeploymentRuntime::new(second_spec);
        let second_pid = second.boot_status().pid.expect("second pid");

        assert_ne!(first_pid, second_pid);

        let second_status = second.stop();
        assert_eq!(second_status.observed_state, ObservedState::Stopped);
        assert!(process_alive(first_pid));

        let first_status = first.stop();
        assert_eq!(first_status.observed_state, ObservedState::Stopped);
        assert!(!process_alive(first_pid));

        let _ = std::fs::remove_file(pid_file);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn refresh_status_fails_stale_pid_when_process_no_longer_matches_spec() {
        let pid_file = unique_pid_file("stale");
        let _ = std::fs::remove_file(&pid_file);

        let mut foreign = DeploymentRuntime::new(WorkerLaunchSpec {
            pid_file: pid_file.clone(),
            ..shell_sleep_spec()
        });
        let foreign_pid = foreign.boot_status().pid.expect("foreign pid");

        let mut stale_spec = shell_sleep_spec();
        stale_spec.pid_file = pid_file.clone();
        stale_spec.args = vec!["31".to_string()];

        let mut runtime = DeploymentRuntime {
            spec: stale_spec,
            status: WorkerStatus {
                deployment_id: "example.paper".to_string(),
                observed_state: ObservedState::Running,
                last_heartbeat: Utc::now(),
                pid: Some(foreign_pid),
                last_error: None,
            },
            child: None,
        };

        let status = runtime.refresh_status();
        assert_eq!(status.observed_state, ObservedState::Failed);
        assert!(status.pid.is_none());
        assert!(status.last_error.is_some());
        assert!(process_alive(foreign_pid));

        let foreign_status = foreign.stop();
        assert_eq!(foreign_status.observed_state, ObservedState::Stopped);
        let _ = std::fs::remove_file(pid_file);
    }
}
