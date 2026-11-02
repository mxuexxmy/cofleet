#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use anyhow::Result;
use clap::Parser;
use cofleet::{
    config::{Config, data_dir},
    ipc,
    protocol::{LocalRequest, LocalResponse},
};
use std::{path::PathBuf, time::Duration};
use tao::{
    event::{Event, StartCause},
    event_loop::{ControlFlow, EventLoopBuilder},
};
use tray_icon::{
    Icon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

#[derive(Parser)]
#[command(about = "CoFleet 轻量托盘；退出不会停止 Agent")]
struct Args {
    #[arg(long, default_value_os_t = data_dir())]
    data_dir: PathBuf,
}
#[derive(Debug)]
enum UiEvent {
    Menu(String),
    Status(std::result::Result<Box<LocalResponse>, String>),
}
enum Action {
    Pause,
    Resume,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let config = Config::load(&args.data_dir)?;
    let event_loop = EventLoopBuilder::<UiEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    let event_loop = {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        let mut event_loop = event_loop;
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        event_loop
    };
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let _ = proxy.send_event(UiEvent::Menu(event.id.0));
    }));
    let status = MenuItem::with_id("status", "正在连接 Agent…", false, None);
    let feedback = MenuItem::with_id("feedback", "", false, None);
    let pause = MenuItem::with_id("pause", "暂停接收新任务", false, None);
    let resume = MenuItem::with_id("resume", "恢复接收新任务", false, None);
    let settings = MenuItem::with_id("settings", "本地设置（配置文件）", true, None);
    let manage = MenuItem::with_id("manage", "打开管理页面", true, None);
    let quit = MenuItem::with_id("quit", "退出托盘（后台继续运行）", true, None);
    let menu = Menu::new();
    menu.append_items(&[
        &status,
        &feedback,
        &PredefinedMenuItem::separator(),
        &pause,
        &resume,
        &settings,
        &manage,
        &PredefinedMenuItem::separator(),
        &quit,
    ])?;
    let proxy = event_loop.create_proxy();
    let (sender, receiver) = std::sync::mpsc::channel();
    let dir = args.data_dir.clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("IPC runtime");
        loop {
            let action = receiver.recv_timeout(Duration::from_secs(3));
            let command = match action {
                Ok(Action::Pause) => LocalRequest::Pause,
                Ok(Action::Resume) => LocalRequest::Resume,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => LocalRequest::Status,
                Err(_) => break,
            };
            let result = runtime
                .block_on(ipc::request(&dir, command))
                .map(Box::new)
                .map_err(|_| "Agent 未运行或本地连接失败".into());
            if proxy.send_event(UiEvent::Status(result)).is_err() {
                break;
            }
        }
    });
    let mut tray = None;
    event_loop.run(move |event, _, control| {
        *control = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {
                let rgba: Vec<u8> = (0..32 * 32)
                    .flat_map(|i| {
                        let (x, y) = (i % 32, i / 32);
                        if (6..26).contains(&x) && (6..26).contains(&y) {
                            [48, 158, 123, 255]
                        } else {
                            [0, 0, 0, 0]
                        }
                    })
                    .collect();
                match Icon::from_rgba(rgba, 32, 32)
                    .map_err(anyhow::Error::from)
                    .and_then(|icon| {
                        TrayIconBuilder::new()
                            .with_tooltip("聚机 · CoFleet")
                            .with_menu(Box::new(menu.clone()))
                            .with_icon(icon)
                            .build()
                            .map_err(Into::into)
                    }) {
                    Ok(icon) => tray = Some(icon),
                    Err(e) => {
                        eprintln!("Cannot create tray: {e}");
                        *control = ControlFlow::Exit;
                    }
                }
            }
            Event::UserEvent(UiEvent::Menu(id)) => match id.as_str() {
                "quit" => {
                    tray.take();
                    *control = ControlFlow::Exit;
                }
                "pause" => {
                    let _ = sender.send(Action::Pause);
                }
                "resume" => {
                    let _ = sender.send(Action::Resume);
                }
                "manage" => {
                    if open::that_detached(format!("{}/", config.manager_url.trim_end_matches('/')))
                        .is_err()
                    {
                        feedback.set_text("无法打开浏览器");
                    }
                }
                "settings" => match open::that_detached(args.data_dir.join("agent.toml")) {
                    Ok(()) => {}
                    Err(_) => feedback.set_text("无法打开编辑器；请使用 cofleetctl config-path"),
                },
                _ => {}
            },
            Event::UserEvent(UiEvent::Status(Ok(response))) => {
                feedback.set_text(
                    response
                        .error
                        .as_deref()
                        .unwrap_or("设置修改后需重启 Agent"),
                );
                if let Some(s) = response.status {
                    status.set_text(format!(
                        "{} · {}",
                        s.name,
                        if s.connected {
                            "已连接微服"
                        } else {
                            "微服未连接"
                        }
                    ));
                    pause.set_enabled(s.accepting_tasks);
                    resume.set_enabled(!s.accepting_tasks);
                }
            }
            Event::UserEvent(UiEvent::Status(Err(error))) => {
                status.set_text(error);
                pause.set_enabled(false);
                resume.set_enabled(false);
            }
            _ => {}
        }
    });
}
