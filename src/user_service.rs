//! Linux current-user service lifecycle. Never initializes or removes node identity/data.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

const UNIT: &str = "cofleet-agent.service";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Options {
    pub data_dir: PathBuf,
    pub docker: bool,
    pub docker_observe: bool,
    pub workspace_image: Option<String>,
    pub build_deploy: bool,
}
struct Paths {
    unit: PathBuf,
    state: PathBuf,
    binary: PathBuf,
}
fn paths() -> Result<Paths> {
    ensure!(
        cfg!(target_os = "linux"),
        "user services currently support Linux systemd only"
    );
    let base = directories::BaseDirs::new().context("cannot resolve current user directories")?;
    let data = base.data_local_dir().join("cofleet/service");
    Ok(Paths {
        unit: base.config_dir().join("systemd/user").join(UNIT),
        state: data.join("service.json"),
        binary: data.join("cofleet-agent"),
    })
}
fn run(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program).args(args).output().with_context(|| {
        format!("cannot run {program}; a logged-in systemd user session is required")
    })?;
    ensure!(
        output.status.success(),
        "{program} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
fn systemctl(args: &[&str]) -> Result<String> {
    let mut all = vec!["--user"];
    all.extend_from_slice(args);
    run("systemctl", &all)
}
fn quote(value: &str) -> Result<String> {
    ensure!(
        !value.chars().any(char::is_control),
        "service arguments cannot contain control characters"
    );
    Ok(format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    ))
}
fn unit(binary: &Path, options: &Options) -> Result<String> {
    let mut args = vec![
        binary
            .to_str()
            .context("non-UTF8 executable path")?
            .to_owned(),
        "--data-dir".into(),
        options
            .data_dir
            .to_str()
            .context("non-UTF8 data path")?
            .to_owned(),
    ];
    if options.docker {
        args.push("--docker".into());
    }
    if options.docker_observe {
        args.push("--docker-observe".into());
    }
    if let Some(image) = &options.workspace_image {
        args.extend(["--workspace-image".into(), image.clone()]);
    }
    if options.build_deploy {
        args.push("--build-deploy".into());
    }
    let command = args
        .iter()
        .map(|v| quote(v))
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    Ok(format!(
        "# Managed by cofleetctl user-service; node data is never removed.\n[Unit]\nDescription=CoFleet current-user Agent\nStartLimitIntervalSec=60\nStartLimitBurst=5\n\n[Service]\nType=simple\nExecStart={command}\nRestart=on-failure\nRestartSec=5\nTimeoutStopSec=20\nNoNewPrivileges=true\nUMask=0077\n\n[Install]\nWantedBy=default.target\n"
    ))
}
fn write_atomic(path: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    use std::io::Write;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path.parent().context("missing parent")?)?;
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(if executable { 0o700 } else { 0o600 });
        }
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}
fn load(paths: &Paths) -> Result<Options> {
    let spec: Options = serde_json::from_slice(
        &std::fs::read(&paths.state)
            .context("user service not installed; run user-service install")?,
    )?;
    ensure!(
        std::fs::read_to_string(&paths.unit)? == unit(&paths.binary, &spec)?,
        "service unit was modified; preserve/review it before reinstalling"
    );
    Ok(spec)
}
fn copy_agent(source: &Path, paths: &Paths) -> Result<()> {
    ensure!(source.is_file(), "Agent executable not found");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            std::fs::metadata(source)?.permissions().mode() & 0o111 != 0,
            "Agent file is not executable"
        );
    }
    write_atomic(&paths.binary, &std::fs::read(source)?, true)
}
fn healthy(options: &Options) -> Result<()> {
    let executable = std::env::current_exe()?;
    let mut consecutive = 0;
    for _ in 0..20 {
        let active = systemctl(&["is-active", UNIT]).is_ok();
        let ready = active
            && Command::new(&executable)
                .arg("--data-dir")
                .arg(&options.data_dir)
                .arg("status")
                .output()
                .is_ok_and(|v| v.status.success());
        if ready {
            consecutive += 1;
            if consecutive >= 2 {
                return Ok(());
            }
        } else {
            consecutive = 0;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    anyhow::bail!("Agent did not become ready; inspect user-service logs")
}
pub fn install(source: &Path, mut options: Options, no_start: bool) -> Result<()> {
    options.data_dir = std::fs::canonicalize(&options.data_dir)
        .context("initialize node configuration before installing service")?;
    crate::config::Config::load(&options.data_dir)?;
    ensure!(
        !options.build_deploy || options.workspace_image.is_some(),
        "build/deploy requires a workspace image"
    );
    ensure!(
        options.workspace_image.is_none() || options.docker,
        "workspace requires Docker executor"
    );
    let paths = paths()?;
    if paths.unit.exists() || paths.state.exists() {
        ensure!(
            load(&paths)? == options,
            "service already installed with different options; use upgrade to preserve settings, or uninstall service first"
        );
    }
    if !no_start {
        use fs2::FileExt;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(options.data_dir.join("agent.lock"))?;
        ensure!(
            lock.try_lock_exclusive().is_ok(),
            "an Agent is already running; use --no-start, stop the old Agent, then user-service start"
        );
    }
    // Verify user manager availability before creating any files.
    systemctl(&["show", "--property=Version", "--value"])?;
    copy_agent(source, &paths)?;
    write_atomic(&paths.state, &serde_json::to_vec_pretty(&options)?, false)?;
    write_atomic(
        &paths.unit,
        unit(&paths.binary, &options)?.as_bytes(),
        false,
    )?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", UNIT])?;
    if !no_start {
        systemctl(&["start", UNIT])?;
        healthy(&options)?;
    }
    println!(
        "Installed current-user service; existing node configuration and Docker data preserved."
    );
    Ok(())
}
pub fn upgrade(source: &Path) -> Result<()> {
    let paths = paths()?;
    let options = load(&paths)?;
    systemctl(&["show", "--property=Version", "--value"])?;
    let previous = std::fs::read(&paths.binary)?;
    copy_agent(source, &paths)?;
    if let Err(error) = systemctl(&["restart", UNIT]).and_then(|_| healthy(&options)) {
        write_atomic(&paths.binary, &previous, true)?;
        let _ = systemctl(&["reset-failed", UNIT]);
        let _ = systemctl(&["restart", UNIT]);
        return Err(error.context("upgrade failed; previous executable restored"));
    }
    println!("Agent upgraded; original service flags and node data preserved.");
    Ok(())
}
pub fn action(action: &str) -> Result<()> {
    ensure!(
        ["start", "stop", "restart", "status", "uninstall", "logs"].contains(&action),
        "unsupported service action"
    );
    let paths = paths()?;
    if action == "status" {
        let installed = paths.state.exists() && paths.unit.exists();
        let status = systemctl(&[
            "show",
            UNIT,
            "--property=ActiveState,SubState,UnitFileState,MainPID,ExecMainStatus",
        ])?;
        println!(
            "{}",
            serde_json::json!({"installed":installed,"unit":paths.unit,"state":status})
        );
        return Ok(());
    }
    if action == "logs" {
        print!(
            "{}",
            run(
                "journalctl",
                &["--user-unit", UNIT, "--no-pager", "-n", "80"]
            )?
        );
        return Ok(());
    }
    let _ = load(&paths)?;
    if action == "uninstall" {
        systemctl(&["disable", "--now", UNIT])?;
        std::fs::remove_file(paths.unit)?;
        std::fs::remove_file(paths.state)?;
        systemctl(&["daemon-reload"])?;
        println!("Service removed; executable, node identity, projects and containers preserved.");
    } else {
        systemctl(&[action, UNIT])?;
        println!("{action}: {UNIT}");
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arguments_are_literal_and_keep_capabilities() {
        let spec = Options {
            data_dir: PathBuf::from("/a path/100%/$node"),
            docker: true,
            docker_observe: true,
            workspace_image: Some("sha256:abcd".into()),
            build_deploy: true,
        };
        let text = unit(Path::new("/some path/agent"), &spec).unwrap();
        assert!(text.contains("100%%/$$node"));
        assert!(text.contains("--docker-observe"));
        assert!(text.contains("--build-deploy"));
        assert!(!text.contains("User=root"));
        assert!(quote("bad\npath").is_err());
    }
}
