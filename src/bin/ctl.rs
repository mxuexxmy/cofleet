use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use cofleet::{
    config::{Config, data_dir},
    ipc,
    protocol::LocalRequest,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "CoFleet 本地配置与 Agent 控制")]
struct Args {
    #[arg(long, global = true, default_value_os_t = data_dir())]
    data_dir: PathBuf,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// 创建本地配置；凭据从环境变量读取，不出现在命令行参数中
    Init {
        #[arg(long, default_value = "http://127.0.0.1:9760")]
        manager: String,
        #[arg(long, value_enum, default_value_t = cofleet::auth::AuthMode::Development)]
        auth: cofleet::auth::AuthMode,
        #[arg(long)]
        lzc_core_config: Option<PathBuf>,
        #[arg(long)]
        name: String,
        #[arg(long, default_value_t = 5)]
        heartbeat_secs: u64,
        /// Shared by Windows and WSL agents on the same physical computer
        #[arg(long)]
        physical_host_id: Option<uuid::Uuid>,
    },
    /// View containers, set test workload intent, or read its bounded logs
    Docker {
        #[command(subcommand)]
        command: DockerCommand,
    },
    Pipeline {
        #[command(subcommand)]
        command: PipelineCommand,
    },
    Workspace {
        #[command(subcommand)]
        command: DockerCommand,
    },
    Status,
    Pause,
    Resume,
    /// 本机资源预留，用于规划额度；不会限制已运行进程
    Reserve {
        #[arg(long)]
        cpu_threads: u32,
        #[arg(long)]
        memory_mib: u64,
    },
    /// Configure scheduler-visible node capabilities (replaces the previous set)
    Capabilities {
        #[arg(long)]
        physical_host_id: Option<uuid::Uuid>,
        #[arg(long, requires = "gpu_memory_mib")]
        gpu_name: Option<String>,
        #[arg(long, requires = "gpu_name")]
        gpu_memory_mib: Option<u64>,
        #[arg(long)]
        data_location: Vec<String>,
        #[arg(long, value_enum, default_value_t = cofleet::resources::AvailabilityPreference::Opportunistic)]
        availability: cofleet::resources::AvailabilityPreference,
    },
    /// 显示配置路径，不输出凭据；修改连接设置后重启 Agent
    ConfigPath,
    /// Install/upgrade a Linux login-session Agent without root or new device identity
    UserService {
        #[command(subcommand)]
        command: UserServiceCommand,
    },
}
#[derive(Subcommand)]
enum UserServiceCommand {
    Install {
        #[arg(long)]
        agent: PathBuf,
        #[arg(long)]
        no_start: bool,
        #[arg(long)]
        docker: bool,
        #[arg(long)]
        docker_observe: bool,
        #[arg(long)]
        workspace_image: Option<String>,
        #[arg(long)]
        build_deploy: bool,
    },
    Upgrade {
        #[arg(long)]
        agent: PathBuf,
    },
    Start,
    Stop,
    Restart,
    Status,
    Logs,
    Uninstall,
}
#[derive(Subcommand)]
enum DockerCommand {
    List,
    Start {
        #[arg(long)]
        request_id: uuid::Uuid,
    },
    Stop {
        #[arg(long)]
        request_id: uuid::Uuid,
    },
    Logs,
}
#[derive(Subcommand)]
enum PipelineCommand {
    Status,
    Logs {
        job_id: uuid::Uuid,
    },
    Build {
        #[arg(long)]
        request_id: uuid::Uuid,
        #[arg(long)]
        project_dir: String,
    },
    Deploy {
        #[arg(long)]
        request_id: uuid::Uuid,
        #[arg(long)]
        build_id: uuid::Uuid,
        #[arg(long)]
        expected_current: Option<uuid::Uuid>,
    },
    Rollback {
        #[arg(long)]
        request_id: uuid::Uuid,
        #[arg(long)]
        expected_current: uuid::Uuid,
    },
    Stop {
        #[arg(long)]
        request_id: uuid::Uuid,
        #[arg(long)]
        expected_current: Option<uuid::Uuid>,
    },
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Command::Init {
            manager,
            auth,
            lzc_core_config,
            name,
            heartbeat_secs,
            physical_host_id,
        } => {
            if args.data_dir.join("agent.toml").exists() {
                bail!("configuration already exists; edit it instead of replacing device identity");
            }
            let token = match auth {
                cofleet::auth::AuthMode::Development => std::env::var("COFLEET_TOKEN")
                    .map_err(|_| anyhow::anyhow!("set COFLEET_TOKEN before init"))?,
                cofleet::auth::AuthMode::Lazycat => String::new(),
            };
            let device_id = uuid::Uuid::new_v4();
            Config {
                device_id,
                name,
                manager_url: manager.trim_end_matches('/').into(),
                token,
                auth,
                lzc_core_config,
                heartbeat_secs,
                accepting_tasks: true,
                resource_policy: Default::default(),
                capabilities: cofleet::resources::NodeCapabilities {
                    physical_host_id: Some(physical_host_id.unwrap_or(device_id)),
                    ..Default::default()
                },
            }
            .save(&args.data_dir)?;
            println!("Created {}", args.data_dir.join("agent.toml").display());
        }
        Command::UserService { command } => {
            use cofleet::user_service as service;
            match command {
                UserServiceCommand::Install {
                    agent,
                    no_start,
                    docker,
                    docker_observe,
                    workspace_image,
                    build_deploy,
                } => service::install(
                    &agent,
                    service::Options {
                        data_dir: args.data_dir,
                        docker,
                        docker_observe,
                        workspace_image,
                        build_deploy,
                    },
                    no_start,
                )?,
                UserServiceCommand::Upgrade { agent } => service::upgrade(&agent)?,
                action => service::action(match action {
                    UserServiceCommand::Start => "start",
                    UserServiceCommand::Stop => "stop",
                    UserServiceCommand::Restart => "restart",
                    UserServiceCommand::Status => "status",
                    UserServiceCommand::Logs => "logs",
                    UserServiceCommand::Uninstall => "uninstall",
                    _ => unreachable!(),
                })?,
            }
        }
        Command::ConfigPath => println!("{}", args.data_dir.join("agent.toml").display()),
        command => {
            let command = match command {
                Command::Docker { command } => LocalRequest::Docker {
                    request: match command {
                        DockerCommand::List => cofleet::docker::DockerRequest::List,
                        DockerCommand::Start { request_id } => {
                            cofleet::docker::DockerRequest::SetTest {
                                request_id,
                                running: true,
                            }
                        }
                        DockerCommand::Stop { request_id } => {
                            cofleet::docker::DockerRequest::SetTest {
                                request_id,
                                running: false,
                            }
                        }
                        DockerCommand::Logs => cofleet::docker::DockerRequest::TestLogs,
                    },
                },
                Command::Workspace { command } => LocalRequest::Docker {
                    request: cofleet::docker::DockerRequest::Workspace {
                        request: match command {
                            DockerCommand::List => cofleet::workspace::WorkspaceRequest::Status,
                            DockerCommand::Start { request_id } => {
                                cofleet::workspace::WorkspaceRequest::SetRunning {
                                    request_id,
                                    running: true,
                                }
                            }
                            DockerCommand::Stop { request_id } => {
                                cofleet::workspace::WorkspaceRequest::SetRunning {
                                    request_id,
                                    running: false,
                                }
                            }
                            DockerCommand::Logs => cofleet::workspace::WorkspaceRequest::Logs,
                        },
                    },
                },
                Command::Pipeline { command } => LocalRequest::Docker {
                    request: cofleet::docker::DockerRequest::Pipeline {
                        request: match command {
                            PipelineCommand::Status => cofleet::pipeline::PipelineRequest::Status,
                            PipelineCommand::Logs { job_id } => {
                                cofleet::pipeline::PipelineRequest::Logs { job_id }
                            }
                            PipelineCommand::Build {
                                request_id,
                                project_dir,
                            } => cofleet::pipeline::PipelineRequest::Build {
                                request_id,
                                project_dir,
                            },
                            PipelineCommand::Deploy {
                                request_id,
                                build_id,
                                expected_current,
                            } => cofleet::pipeline::PipelineRequest::Deploy {
                                request_id,
                                build_id,
                                expected_current,
                            },
                            PipelineCommand::Rollback {
                                request_id,
                                expected_current,
                            } => cofleet::pipeline::PipelineRequest::Rollback {
                                request_id,
                                expected_current,
                            },
                            PipelineCommand::Stop {
                                request_id,
                                expected_current,
                            } => cofleet::pipeline::PipelineRequest::Stop {
                                request_id,
                                expected_current,
                            },
                        },
                    },
                },
                Command::Status => LocalRequest::Status,
                Command::Pause => LocalRequest::Pause,
                Command::Resume => LocalRequest::Resume,
                Command::Reserve {
                    cpu_threads,
                    memory_mib,
                } => LocalRequest::SetResourcePolicy {
                    policy: cofleet::resources::ResourcePolicy {
                        reserved_cpu_threads: cpu_threads,
                        reserved_memory_mib: memory_mib,
                    },
                },
                Command::Capabilities {
                    physical_host_id,
                    gpu_name,
                    gpu_memory_mib,
                    data_location,
                    availability,
                } => LocalRequest::SetCapabilities {
                    capabilities: cofleet::resources::NodeCapabilities {
                        physical_host_id,
                        gpu: gpu_name.map(|name| cofleet::resources::GpuCapability {
                            name,
                            memory_mib: gpu_memory_mib.unwrap_or_default(),
                        }),
                        data_locations: data_location,
                        availability,
                    },
                },
                _ => unreachable!(),
            };
            let response = ipc::request(&args.data_dir, command).await?;
            if let Some(error) = response.error {
                bail!("{error}");
            }
            if let Some(docker) = response.docker {
                println!("{}", serde_json::to_string_pretty(&docker)?);
                if docker.error.is_some() {
                    bail!("Docker request failed; see result above");
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&response.status)?);
            }
        }
    }
    Ok(())
}
