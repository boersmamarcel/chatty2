use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine as _;
use bollard::Docker;
use bollard::container::{
    Config, CreateContainerOptions, LogOutput, RemoveContainerOptions, StartContainerOptions,
};
use bollard::exec::CreateExecOptions;
use bollard::image::CreateImageOptions;
use bollard::models::HostConfig;
use futures::StreamExt;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::timeout;
use uuid::Uuid;

use super::backend::{ExecutionResult, Language, SandboxBackend, SandboxConfig};

/// Docker-based sandbox using bollard to manage containers.
pub struct DockerSandbox {
    docker: Docker,
    container_id: String,
    config: SandboxConfig,
}

impl DockerSandbox {
    /// Create a new Docker sandbox container with the given configuration.
    pub async fn create(config: SandboxConfig) -> Result<Self> {
        let docker = Docker::connect_with_local_defaults()
            .context("Cannot connect to Docker. Is Docker Desktop running?")?;

        Self::ensure_image(&docker, config.language.docker_image()).await?;

        let container_name = format!("chatty-sandbox-{}", Uuid::new_v4());

        let host_config = HostConfig {
            network_mode: if config.network {
                None
            } else {
                Some("none".to_string())
            },
            readonly_rootfs: Some(true),
            tmpfs: Some(HashMap::from([(
                "/tmp".to_string(),
                "size=256m,noexec=off".to_string(),
            )])),
            memory: Some((config.memory_mb * 1024 * 1024) as i64),
            cpu_quota: Some(config.cpu_quota),
            pids_limit: Some(64),
            security_opt: Some(vec!["no-new-privileges".to_string()]),
            cap_drop: Some(vec!["ALL".to_string()]),
            ..Default::default()
        };

        let container_config = Config {
            image: Some(config.language.docker_image().to_string()),
            cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
            working_dir: Some("/tmp".to_string()),
            network_disabled: Some(!config.network),
            host_config: Some(host_config),
            env: Some(vec![
                "HOME=/tmp".to_string(),
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
            ]),
            ..Default::default()
        };

        let container = docker
            .create_container(
                Some(CreateContainerOptions {
                    name: container_name,
                    platform: None,
                }),
                container_config,
            )
            .await
            .context("Failed to create container")?;

        docker
            .start_container(&container.id, None::<StartContainerOptions<String>>)
            .await
            .context("Failed to start container")?;

        Ok(Self {
            docker,
            container_id: container.id,
            config,
        })
    }

    /// Ensure the Docker image is pulled locally.
    async fn ensure_image(docker: &Docker, image: &str) -> Result<()> {
        if docker.inspect_image(image).await.is_ok() {
            return Ok(());
        }

        let mut stream = docker.create_image(
            Some(CreateImageOptions {
                from_image: image.to_string(),
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
            })
        };

        match timeout(Duration::from_secs(timeout_secs), collect).await {
            Ok(result) => result,
            Err(_) => Ok(ExecutionResult {
                stdout: String::new(),
                stderr: "Execution timeout exceeded.".to_string(),
                exit_code: -1,
                timed_out: true,
            }),
        }
    }
}

#[async_trait]
impl SandboxBackend for DockerSandbox {
    async fn execute(&self, code: &str, language: &Language) -> Result<ExecutionResult> {
        let filename = self.write_code_file(code, language).await?;
        let cmd = language.run_command(&filename);
        self.run_exec(cmd, self.config.timeout_secs).await
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

    async fn is_available() -> Result<bool> {
        match Docker::connect_with_local_defaults() {
            Ok(docker) => Ok(docker.ping().await.is_ok()),
            Err(_) => Ok(false),
        }
    }
}
