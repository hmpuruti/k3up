use crate::{
    platform,
    protocol::{Command, MAX_FRAME, Request, Response, VERSION},
};
use anyhow::{Context, Result, bail};
use std::time::Duration;
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct Client {
    pub data_dir: PathBuf,
}
impl Client {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_path_buf(),
        }
    }
    pub fn send(&self, command: Command) -> Result<Response> {
        let data = std::fs::canonicalize(&self.data_dir)
            .context("Agent data directory does not exist. Start k3up-agent first")?;
        let request = serde_json::to_vec(&Request {
            version: VERSION,
            command,
        })?;
        if request.len() > MAX_FRAME {
            bail!("Request exceeds 1 MiB");
        }
        #[cfg(unix)]
        let mut stream = {
            let stream = std::os::unix::net::UnixStream::connect(platform::endpoint(&data)?)
                .context("Cannot reach agent. Start k3up-agent with the same --data-dir")?;
            stream.set_read_timeout(Some(Duration::from_secs(120)))?;
            stream.set_write_timeout(Some(Duration::from_secs(10)))?;
            stream
        };
        #[cfg(windows)]
        let mut stream = {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::{
                Foundation::ERROR_PIPE_BUSY,
                Storage::FileSystem::{SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT},
            };
            let endpoint = platform::endpoint(&data)?;
            // The agent serves one connection at a time and may be busy stopping workloads.
            let deadline = std::time::Instant::now() + Duration::from_secs(120);
            let pipe = loop {
                match std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .security_qos_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)
                    .open(&endpoint)
                {
                    Ok(pipe) => break pipe,
                    Err(error)
                        if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(error) => {
                        return Err(error).context(
                            "Cannot reach agent. Start k3up-agent with the same --data-dir",
                        );
                    }
                }
            };
            crate::win32::verify_pipe_owner(&pipe)?;
            pipe
        };
        stream.write_all(&request)?;
        stream.write_all(b"\n")?;
        let mut line = String::new();
        BufReader::new(stream.take((MAX_FRAME + 1) as u64)).read_line(&mut line)?;
        if line.len() > MAX_FRAME {
            bail!("Response exceeds 1 MiB");
        }
        serde_json::from_str(&line).context("Invalid agent response")
    }
}
