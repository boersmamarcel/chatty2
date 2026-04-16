use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine as _;
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::exec::CreateExecOptions;
use bollard::models::{ContainerCreateBody, HostConfig, PortBinding};
use bollard::query_parameters::{
    CreateContainerOptions, CreateImageOptions, RemoveContainerOptions, StartContainerOptions,
};
use futures::StreamExt;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tokio::time::timeout;
use tracing::debug;
use uuid::Uuid;

use super::backend::{ExecutionResult, Language, SandboxBackend, SandboxConfig};

/// Build a list of common Docker socket paths to try as fallbacks.
fn fallback_socket_paths() -> Vec<String> {
    let mut paths = Vec::new();

    // XDG_RUNTIME_DIR/docker.sock (rootless Docker on Linux)
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        paths.push(format!("{}/docker.sock", xdg));
    }

    // Docker Desktop and other common paths
    if let Ok(home) = std::env::var("HOME") {
        paths.push(format!("{}/.docker/run/docker.sock", home));
        paths.push(format!("{}/.docker/desktop/docker.sock", home));
    }

    paths
}

/// Try to connect to a Docker daemon at a specific socket path and verify with ping.
async fn try_socket(path: &str) -> Result<Docker, String> {
    let docker = Docker::connect_with_socket(path, 120, bollard::API_DEFAULT_VERSION)
        .map_err(|e| format!("connect failed: {}", e))?;
    docker
        .ping()
        .await
        .map_err(|e| format!("ping failed: {}", e))?;
    Ok(docker)
}

/// Connect to Docker, trying multiple strategies:
/// 1. If `docker_host` is Some, use that explicitly
/// 2. Try Bollard's `connect_with_local_defaults()` (checks DOCKER_HOST env + platform default)
/// 3. Try common fallback socket paths (rootless Docker, Docker Desktop, etc.)
async fn connect_docker(docker_host: Option<&str>) -> Result<Docker> {
    // Strategy 1: User-configured docker host
    if let Some(host) = docker_host {
        let path = host.strip_prefix("unix://").unwrap_or(host);
        return try_socket(path).await.map_err(|e| {
            anyhow::anyhow!(
                "Cannot connect to Docker at configured host '{}': {}",
                host,
                e
            )
        });
    }

    // Strategy 2: Bollard defaults (DOCKER_HOST env + /var/run/docker.sock)
    if let Ok(docker) = Docker::connect_with_local_defaults()
        && docker.ping().await.is_ok()
    {
        return Ok(docker);
    }
    debug!("Docker connect_with_local_defaults failed, trying fallback socket paths");

    // Strategy 3: Try common fallback socket paths
    let fallbacks = fallback_socket_paths();
    let mut tried = vec!["default (/var/run/docker.sock or DOCKER_HOST)".to_string()];

    for path in &fallbacks {
        if !Path::new(path).exists() {
            tried.push(format!("{} (not found)", path));
            continue;
        }
        match try_socket(path).await {
            Ok(docker) => {
                debug!(path, "Connected to Docker via fallback socket");
                return Ok(docker);
            }
            Err(e) => {
                tried.push(format!("{} ({})", path, e));
            }
        }
    }

    anyhow::bail!(
        "Cannot connect to Docker. Tried:\n  - {}\n\
         Tip: Set \"Docker Host\" in Settings → Execution to specify the socket path.",
        tried.join("\n  - ")
    )
}

/// Docker-based sandbox using bollard to manage containers.
pub struct DockerSandbox {
    docker: Docker,
    container_id: String,
    config: SandboxConfig,
    /// Actual host ports mapped from the container (container_port → host_port)
    port_mappings: HashMap<u16, u16>,
}

impl DockerSandbox {
    /// Create a new Docker sandbox container with the given configuration.
    pub async fn create(config: SandboxConfig) -> Result<Self> {
        let docker = connect_docker(config.docker_host.as_deref()).await?;

        Self::ensure_image(&docker, config.language.docker_image()).await?;

        let container_name = format!("chatty-sandbox-{}", Uuid::new_v4());

        let binds = config
            .workspace_path
            .as_ref()
            .map(|p| vec![format!("{}:/workspace", p)]);

        let (working_dir, home) = if config.workspace_path.is_some() {
            ("/workspace".to_string(), "/tmp".to_string())
        } else {
            ("/tmp".to_string(), "/tmp".to_string())
        };

        // When ports are exposed we need bridge networking; otherwise disable networking entirely.
        let needs_network = config.network || !config.expose_ports.is_empty();

        // Build port bindings: publish each requested container port to a random host port.
        let port_bindings: Option<HashMap<String, Option<Vec<PortBinding>>>> =
            if config.expose_ports.is_empty() {
                None
            } else {
                Some(
                    config
                        .expose_ports
                        .iter()
                        .map(|&p| {
                            (
                                format!("{}/tcp", p),
                                Some(vec![PortBinding {
                                    host_ip: Some("127.0.0.1".to_string()),
                                    host_port: Some(String::new()), // "" = auto-assign
                                }]),
                            )
                        })
                        .collect(),
                )
            };

        // Expose the ports in the container config (required alongside port_bindings).
        let exposed_ports: Option<Vec<String>> = if config.expose_ports.is_empty() {
            None
        } else {
            Some(
                config
                    .expose_ports
                    .iter()
                    .map(|&p| format!("{}/tcp", p))
                    .collect(),
            )
        };

        let host_config = HostConfig {
            network_mode: if needs_network {
                None
            } else {
                Some("none".to_string())
            },
            readonly_rootfs: Some(true),
            tmpfs: Some(HashMap::from([(
                "/tmp".to_string(),
                "size=256m,exec".to_string(),
            )])),
            binds,
            port_bindings,
            memory: Some((config.memory_mb * 1024 * 1024) as i64),
            cpu_quota: Some(config.cpu_quota),
            pids_limit: Some(64),
            security_opt: Some(vec!["no-new-privileges".to_string()]),
            cap_drop: Some(vec!["ALL".to_string()]),
            ..Default::default()
        };

        let container_config = ContainerCreateBody {
            image: Some(config.language.docker_image().to_string()),
            cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
            working_dir: Some(working_dir),
            network_disabled: Some(!needs_network),
            exposed_ports,
            host_config: Some(host_config),
            env: Some(vec![
                format!("HOME={}", home),
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
            ]),
            ..Default::default()
        };

        let container = docker
            .create_container(
                Some(CreateContainerOptions {
                    name: Some(container_name),
                    platform: String::new(),
                }),
                container_config,
            )
            .await
            .context("Failed to create container")?;

        docker
            .start_container(&container.id, None::<StartContainerOptions>)
            .await
            .context("Failed to start container")?;

        // Inspect the container to discover the actual host ports Docker assigned.
        let port_mappings = if config.expose_ports.is_empty() {
            HashMap::new()
        } else {
            let info = docker
                .inspect_container(&container.id, None)
                .await
                .context("Failed to inspect container for port mappings")?;

            info.network_settings
                .and_then(|ns| ns.ports)
                .map(|ports| {
                    let mut mappings = HashMap::new();
                    for (key, bindings) in ports {
                        // key is like "8080/tcp"
                        if let Some(container_port) =
                            key.split('/').next().and_then(|p| p.parse::<u16>().ok())
                            && let Some(Some(binding_list)) = bindings
                                .as_ref()
                                .map(|b| if b.is_empty() { None } else { Some(b) })
                            && let Some(host_port_str) =
                                binding_list.first().and_then(|b| b.host_port.as_deref())
                            && let Ok(host_port) = host_port_str.parse::<u16>()
                        {
                            mappings.insert(container_port, host_port);
                        }
                    }
                    mappings
                })
                .unwrap_or_default()
        };

        Ok(Self {
            docker,
            container_id: container.id,
            config,
            port_mappings,
        })
    }

    /// Ensure the Docker image is pulled locally.
    async fn ensure_image(docker: &Docker, image: &str) -> Result<()> {
        if docker.inspect_image(image).await.is_ok() {
            return Ok(());
        }

        let mut stream = docker.create_image(
            Some(CreateImageOptions {
                from_image: Some(image.to_string()),
                ..Default::default()
            }),
            None,
            None,
        );

        while let Some(event) = stream.next().await {
            event.context("Error downloading Docker image")?;
        }

        Ok(())
    }

    /// Write code into a file inside the container via exec + base64 encoding.
    async fn write_code_file(&self, code: &str, language: &Language) -> Result<String> {
        let filename = format!("/tmp/code.{}", language.file_extension());
        let encoded = base64::engine::general_purpose::STANDARD.encode(code);
        let write_cmd = format!("echo '{}' | base64 -d > {}", encoded, filename);

        let exec = self
            .docker
            .create_exec(
                &self.container_id,
                CreateExecOptions {
                    cmd: Some(vec!["sh".to_string(), "-c".to_string(), write_cmd]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await?;

        if let bollard::exec::StartExecResults::Attached { mut output, .. } =
            self.docker.start_exec(&exec.id, None).await?
        {
            while output.next().await.is_some() {}
        }

        Ok(filename)
    }

    /// Execute a command inside the container and capture output.
    async fn run_exec(&self, cmd: Vec<String>, timeout_secs: u64) -> Result<ExecutionResult> {
        let exec = self
            .docker
            .create_exec(
                &self.container_id,
                CreateExecOptions {
                    cmd: Some(cmd),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await?;

        let exec_id = exec.id.clone();
        let docker = &self.docker;

        let collect = async {
            let mut stdout = String::new();
            let mut stderr = String::new();

            if let bollard::exec::StartExecResults::Attached { mut output, .. } =
                docker.start_exec(&exec_id, None).await?
            {
                while let Some(chunk) = output.next().await {
                    match chunk? {
                        LogOutput::StdOut { message } => {
                            stdout.push_str(&String::from_utf8_lossy(&message));
                        }
                        LogOutput::StdErr { message } => {
                            stderr.push_str(&String::from_utf8_lossy(&message));
                        }
                        _ => {}
                    }
                }
            }

            let inspect = docker.inspect_exec(&exec_id).await?;
            let exit_code = inspect.exit_code.unwrap_or(-1);

            Ok::<ExecutionResult, anyhow::Error>(ExecutionResult {
                stdout,
                stderr,
                exit_code,
                timed_out: false,
                port_mappings: HashMap::new(), // filled in by caller
            })
        };

        match timeout(Duration::from_secs(timeout_secs), collect).await {
            Ok(result) => result,
            Err(_) => Ok(ExecutionResult {
                stdout: String::new(),
                stderr: "Execution timeout exceeded.".to_string(),
                exit_code: -1,
                timed_out: true,
                port_mappings: HashMap::new(),
            }),
        }
    }
}

#[async_trait]
impl SandboxBackend for DockerSandbox {
    async fn execute(&self, code: &str, language: &Language) -> Result<ExecutionResult> {
        let filename = self.write_code_file(code, language).await?;
        let cmd = language.run_command(&filename);
        let mut result = self.run_exec(cmd, self.config.timeout_secs).await?;
        result.port_mappings = self.port_mappings.clone();
        Ok(result)
    }

    async fn destroy(self: Box<Self>) -> Result<()> {
        self.docker
            .remove_container(
                &self.container_id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .context("Failed to remove container")?;
        Ok(())
    }

    fn has_port_exposed(&self, port: u16) -> bool {
        self.config.expose_ports.contains(&port)
    }

    async fn is_available(docker_host: Option<&str>) -> Result<bool> {
        Ok(connect_docker(docker_host).await.is_ok())
    }
}
