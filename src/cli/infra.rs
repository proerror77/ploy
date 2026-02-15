//! Infrastructure management commands
//!
//! ploy infra deploy   - Deploy to cloud infrastructure
//! ploy infra status   - Check infrastructure status
//! ploy infra ssh      - SSH into instance
//! ploy infra logs     - View infrastructure logs

use anyhow::{bail, Context, Result};
use clap::Subcommand;

/// Infrastructure-related commands
#[derive(Subcommand, Debug)]
pub enum InfraCommands {
    /// Deploy application to cloud
    Deploy {
        /// Target environment (production, staging)
        #[arg(short, long, default_value = "production")]
        env: String,

        /// Skip confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Check infrastructure status
    Status {
        /// Target environment
        #[arg(short, long, default_value = "production")]
        env: String,
    },

    /// SSH into instance
    Ssh {
        /// Target environment
        #[arg(short, long, default_value = "production")]
        env: String,

        /// Command to execute (optional)
        #[arg(short, long)]
        command: Option<String>,
    },

    /// View infrastructure logs
    Logs {
        /// Target environment
        #[arg(short, long, default_value = "production")]
        env: String,

        /// Number of lines to show
        #[arg(short = 'n', long, default_value = "100")]
        tail: usize,

        /// Follow log output
        #[arg(short, long)]
        follow: bool,
    },

    /// Update infrastructure
    Update {
        /// Target environment
        #[arg(short, long, default_value = "production")]
        env: String,

        /// Component to update (docker, config, all)
        #[arg(short, long, default_value = "all")]
        component: String,
    },
}

impl InfraCommands {
    pub async fn run(self) -> Result<()> {
        match self {
            Self::Deploy { env, yes } => deploy(&env, yes).await,
            Self::Status { env } => show_status(&env).await,
            Self::Ssh { env, command } => ssh_connect(&env, command.as_deref()).await,
            Self::Logs { env, tail, follow } => show_logs(&env, tail, follow).await,
            Self::Update { env, component } => update_infra(&env, &component).await,
        }
    }
}

fn get_infra_config(env: &str) -> Result<InfraConfig> {
    // In production, this would load from a config file
    match env {
        "production" => Ok(InfraConfig {
            name: "production".to_string(),
            region: "ap-northeast-1".to_string(),
            host: std::env::var("AWS_EC2_HOST").ok(),
            key_path: std::env::var("AWS_EC2_KEY_PATH")
                .unwrap_or_else(|_| "~/.ssh/ploy-production.pem".to_string()),
            user: "ec2-user".to_string(),
        }),
        "staging" => Ok(InfraConfig {
            name: "staging".to_string(),
            region: "ap-northeast-1".to_string(),
            host: std::env::var("AWS_EC2_HOST_STAGING").ok(),
            key_path: std::env::var("AWS_EC2_KEY_PATH_STAGING")
                .unwrap_or_else(|_| "~/.ssh/ploy-staging.pem".to_string()),
            user: "ec2-user".to_string(),
        }),
        _ => bail!(
            "Unknown environment: {}. Use 'production' or 'staging'",
            env
        ),
    }
}

struct InfraConfig {
    name: String,
    region: String,
    host: Option<String>,
    key_path: String,
    user: String,
}

async fn deploy(env: &str, skip_confirm: bool) -> Result<()> {
    let config = get_infra_config(env)?;

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!(
        "\x1b[36m║  Deploy to {}                                        ║\x1b[0m",
        format!("{:<15}", config.name)
    );
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    println!("  Environment: {}", config.name);
    println!("  Region:      {}", config.region);
    println!(
        "  Host:        {}",
        config.host.as_deref().unwrap_or("(not configured)")
    );

    if !skip_confirm {
        println!(
            "\n  \x1b[33m⚠ This will deploy the current code to {}\x1b[0m",
            config.name
        );
        print!("  Continue? [y/N] ");

        use std::io::{self, Write};
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("\n  Deployment cancelled.\n");
            return Ok(());
        }
    }

    println!("\n  \x1b[36m→ Building Docker image...\x1b[0m");

    // Build Docker image
    let build_status = std::process::Command::new("docker")
        .args(["build", "-t", "ploy-trading:latest", "."])
        .status()
        .context("Failed to build Docker image")?;

    if !build_status.success() {
        bail!("Docker build failed");
    }

    println!("  \x1b[32m✓ Docker image built\x1b[0m");

    // Push to ECR (if configured)
    if let Ok(ecr_registry) = std::env::var("ECR_REGISTRY") {
        println!("\n  \x1b[36m→ Pushing to ECR...\x1b[0m");

        let tag = format!("{}/ploy-trading:latest", ecr_registry);

        std::process::Command::new("docker")
            .args(["tag", "ploy-trading:latest", &tag])
            .status()
            .context("Failed to tag image")?;

        std::process::Command::new("docker")
            .args(["push", &tag])
            .status()
            .context("Failed to push to ECR")?;

        println!("  \x1b[32m✓ Pushed to ECR\x1b[0m");
    }

    // Deploy to EC2
    if let Some(host) = &config.host {
        println!("\n  \x1b[36m→ Deploying to EC2...\x1b[0m");

        let ssh_cmd = format!(
            "docker pull ploy-trading:latest && \
             docker stop ploy-trading 2>/dev/null || true && \
             docker rm ploy-trading 2>/dev/null || true && \
             docker run -d --name ploy-trading --restart unless-stopped ploy-trading:latest"
        );

        let status = std::process::Command::new("ssh")
            .args([
                "-i",
                &config.key_path,
                "-o",
                "StrictHostKeyChecking=no",
                &format!("{}@{}", config.user, host),
                &ssh_cmd,
            ])
            .status()
            .context("Failed to deploy to EC2")?;

        if !status.success() {
            bail!("EC2 deployment failed");
        }

        println!("  \x1b[32m✓ Deployed to EC2\x1b[0m");
    } else {
        println!(
            "\n  \x1b[33m⚠ No host configured for {}. Skipping remote deployment.\x1b[0m",
            env
        );
        println!("  Set AWS_EC2_HOST environment variable to enable remote deployment.");
    }

    println!("\n  \x1b[32m✓ Deployment complete!\x1b[0m\n");
    Ok(())
}

async fn show_status(env: &str) -> Result<()> {
    let config = get_infra_config(env)?;

    println!("\n{}", "=".repeat(60));
    println!("  INFRASTRUCTURE STATUS: {}", config.name.to_uppercase());
    println!("{}\n", "=".repeat(60));

    println!("  Environment: {}", config.name);
    println!("  Region:      {}", config.region);
    println!(
        "  Host:        {}",
        config.host.as_deref().unwrap_or("(not configured)")
    );

    if let Some(host) = &config.host {
        println!("\n  Checking remote status...\n");

        // Check Docker containers
        let output = std::process::Command::new("ssh")
            .args([
                "-i", &config.key_path,
                "-o", "StrictHostKeyChecking=no",
                "-o", "ConnectTimeout=5",
                &format!("{}@{}", config.user, host),
                "docker ps --format '{{.Names}}\t{{.Status}}\t{{.Image}}' 2>/dev/null || echo 'Docker not available'",
            ])
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                println!("  Docker Containers:");
                println!("  {}", "-".repeat(55));
                println!("  {:<20} {:<20} {}", "NAME", "STATUS", "IMAGE");
                println!("  {}", "-".repeat(55));

                for line in stdout.lines() {
                    if !line.is_empty() {
                        let parts: Vec<&str> = line.split('\t').collect();
                        if parts.len() >= 3 {
                            println!("  {:<20} {:<20} {}", parts[0], parts[1], parts[2]);
                        } else {
                            println!("  {}", line);
                        }
                    }
                }
            }
            Ok(_) => {
                println!("  \x1b[31m✗ Failed to connect to {}\x1b[0m", host);
            }
            Err(e) => {
                println!("  \x1b[31m✗ SSH error: {}\x1b[0m", e);
            }
        }
    } else {
        println!(
            "\n  \x1b[33m⚠ No host configured. Set AWS_EC2_HOST to enable remote status.\x1b[0m"
        );
    }

    println!("\n{}\n", "=".repeat(60));
    Ok(())
}

async fn ssh_connect(env: &str, command: Option<&str>) -> Result<()> {
    let config = get_infra_config(env)?;

    let host = config
        .host
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No host configured for {}", env))?;

    let mut ssh_args = vec![
        "-i".to_string(),
        config.key_path.clone(),
        "-o".to_string(),
        "StrictHostKeyChecking=no".to_string(),
        format!("{}@{}", config.user, host),
    ];

    if let Some(cmd) = command {
        ssh_args.push(cmd.to_string());

        let status = std::process::Command::new("ssh")
            .args(&ssh_args)
            .status()
            .context("Failed to execute SSH command")?;

        if !status.success() {
            bail!("SSH command failed");
        }
    } else {
        println!("Connecting to {} ({})...", config.name, host);

        let status = std::process::Command::new("ssh")
            .args(&ssh_args)
            .status()
            .context("Failed to connect via SSH")?;

        if !status.success() {
            bail!("SSH connection failed");
        }
    }

    Ok(())
}

async fn show_logs(env: &str, tail: usize, follow: bool) -> Result<()> {
    let config = get_infra_config(env)?;

    let host = config
        .host
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No host configured for {}", env))?;

    let docker_cmd = if follow {
        format!("docker logs ploy-trading --tail {} -f", tail)
    } else {
        format!("docker logs ploy-trading --tail {}", tail)
    };

    println!("Fetching logs from {}...\n", config.name);

    let status = std::process::Command::new("ssh")
        .args([
            "-i",
            &config.key_path,
            "-o",
            "StrictHostKeyChecking=no",
            &format!("{}@{}", config.user, host),
            &docker_cmd,
        ])
        .status()
        .context("Failed to fetch logs")?;

    if !status.success() {
        bail!("Failed to fetch logs");
    }

    Ok(())
}

async fn update_infra(env: &str, component: &str) -> Result<()> {
    let config = get_infra_config(env)?;

    let host = config
        .host
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No host configured for {}", env))?;

    println!("\n  Updating {} on {}...\n", component, config.name);

    let update_cmd = match component {
        "docker" => "docker pull ploy-trading:latest && docker restart ploy-trading",
        "config" => {
            "docker cp /opt/ploy/config ploy-trading:/opt/ploy/ && \
             docker exec ploy-trading kill -HUP 1"
        }
        "all" => {
            "docker pull ploy-trading:latest && \
             docker stop ploy-trading && \
             docker rm ploy-trading && \
             docker run -d --name ploy-trading --restart unless-stopped \
               -v /opt/ploy/config:/opt/ploy/config \
               ploy-trading:latest"
        }
        _ => bail!(
            "Unknown component: {}. Use 'docker', 'config', or 'all'",
            component
        ),
    };

    let status = std::process::Command::new("ssh")
        .args([
            "-i",
            &config.key_path,
            "-o",
            "StrictHostKeyChecking=no",
            &format!("{}@{}", config.user, host),
            update_cmd,
        ])
        .status()
        .context("Failed to update infrastructure")?;

    if !status.success() {
        bail!("Update failed");
    }

    println!("  \x1b[32m✓ Update complete\x1b[0m\n");
    Ok(())
}
