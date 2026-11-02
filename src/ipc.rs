use crate::{
    config::Config,
    protocol::{LocalRequest, LocalResponse},
};
use anyhow::{Context, Result};
use std::{path::Path, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
pub type Stream = Box<dyn Io>;
const LIMIT: u64 = 16 * 1024;

pub async fn read_json<T: serde::de::DeserializeOwned>(stream: &mut Stream) -> Result<T> {
    let mut bytes = Vec::new();
    let count = tokio::time::timeout(
        Duration::from_secs(3),
        BufReader::new(stream.take(LIMIT + 1)).read_until(b'\n', &mut bytes),
    )
    .await??;
    anyhow::ensure!(
        count > 0 && count as u64 <= LIMIT && bytes.last() == Some(&b'\n'),
        "invalid or oversized IPC message"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

pub async fn write_json<T: serde::Serialize>(stream: &mut Stream, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    tokio::time::timeout(Duration::from_secs(3), stream.write_all(&bytes)).await??;
    Ok(())
}

pub async fn request(dir: &Path, command: LocalRequest) -> Result<LocalResponse> {
    let config = Config::load(dir)?;
    let mut stream = connect(dir, &config)
        .await
        .context("cannot connect to Agent; is it running?")?;
    write_json(&mut stream, &command).await?;
    read_json(&mut stream).await
}

#[cfg(unix)]
pub struct Listener {
    socket: tokio::net::UnixListener,
    path: std::path::PathBuf,
    _lock: std::fs::File,
}

#[cfg(unix)]
impl Listener {
    pub fn bind(dir: &Path, _config: &Config) -> Result<Self> {
        use fs2::FileExt;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(dir.join("agent.lock"))?;
        lock.try_lock_exclusive()
            .context("Agent is already running for this data directory")?;
        let path = dir.join("agent.sock");
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let socket = tokio::net::UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            socket,
            path,
            _lock: lock,
        })
    }
    pub async fn accept(&mut self) -> Result<Stream> {
        Ok(Box::new(self.socket.accept().await?.0))
    }
}
#[cfg(unix)]
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
#[cfg(unix)]
async fn connect(dir: &Path, _config: &Config) -> Result<Stream> {
    Ok(Box::new(
        tokio::net::UnixStream::connect(dir.join("agent.sock")).await?,
    ))
}

#[cfg(windows)]
fn pipe_name(config: &Config) -> String {
    format!(r"\\.\pipe\cofleet-{}", config.device_id)
}

#[cfg(windows)]
pub struct Listener {
    pending: Option<tokio::net::windows::named_pipe::NamedPipeServer>,
    name: String,
}

#[cfg(windows)]
fn create_pipe(
    name: &str,
    first: bool,
) -> Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    let descriptor = crate::windows_security::Descriptor::current_user(false)?;
    let mut attrs = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let result = unsafe {
        tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                name,
                (&mut attrs as *mut SECURITY_ATTRIBUTES).cast(),
            )
    };
    Ok(result?)
}
#[cfg(windows)]
impl Listener {
    pub fn bind(_dir: &Path, config: &Config) -> Result<Self> {
        let name = pipe_name(config);
        Ok(Self {
            pending: Some(create_pipe(&name, true)?),
            name,
        })
    }
    pub async fn accept(&mut self) -> Result<Stream> {
        let pipe = self.pending.take().context("pipe listener unavailable")?;
        pipe.connect().await?;
        self.pending = Some(create_pipe(&self.name, false)?);
        Ok(Box::new(pipe))
    }
}
#[cfg(windows)]
async fn connect(_dir: &Path, config: &Config) -> Result<Stream> {
    use tokio::net::windows::named_pipe::ClientOptions;
    for _ in 0..30 {
        match ClientOptions::new().open(pipe_name(config)) {
            Ok(pipe) => return Ok(Box::new(pipe)),
            Err(e) if e.raw_os_error() == Some(231) => {
                tokio::time::sleep(Duration::from_millis(50)).await
            }
            Err(e) => return Err(e.into()),
        }
    }
    anyhow::bail!("Agent pipe is busy")
}
