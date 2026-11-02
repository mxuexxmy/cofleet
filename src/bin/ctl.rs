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
    },
    Status,
    Pause,
    Resume,
    /// 显示配置路径，不输出凭据；修改连接设置后重启 Agent
    ConfigPath,
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
        } => {
            if args.data_dir.join("agent.toml").exists() {
                bail!("configuration already exists; edit it instead of replacing device identity");
            }
            let token = match auth {
                cofleet::auth::AuthMode::Development => std::env::var("COFLEET_TOKEN")
                    .map_err(|_| anyhow::anyhow!("set COFLEET_TOKEN before init"))?,
                cofleet::auth::AuthMode::Lazycat => String::new(),
            };
            Config {
                device_id: uuid::Uuid::new_v4(),
                name,
                manager_url: manager.trim_end_matches('/').into(),
                token,
                auth,
                lzc_core_config,
                heartbeat_secs,
                accepting_tasks: true,
            }
            .save(&args.data_dir)?;
            println!("Created {}", args.data_dir.join("agent.toml").display());
        }
        Command::ConfigPath => println!("{}", args.data_dir.join("agent.toml").display()),
        command => {
            let command = match command {
                Command::Status => LocalRequest::Status,
                Command::Pause => LocalRequest::Pause,
                Command::Resume => LocalRequest::Resume,
                _ => unreachable!(),
            };
            let response = ipc::request(&args.data_dir, command).await?;
            if let Some(error) = response.error {
                bail!("{error}");
            }
            println!("{}", serde_json::to_string_pretty(&response.status)?);
        }
    }
    Ok(())
}
