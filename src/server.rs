use crate::{
    engine::Engine,
    platform,
    protocol::{Command, MAX_FRAME, Request, Response, VERSION},
};
use anyhow::{Context, Result};
use fs2::FileExt;
use std::{fs::OpenOptions, path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{mpsc, oneshot},
};

type Envelope = (Request, oneshot::Sender<Response>);

pub async fn run(data_dir: PathBuf, shutdown: impl std::future::Future<Output = ()>) -> Result<()> {
    let data = platform::prepare_dir(&data_dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(data.join("agent.lock"))?;
    lock.try_lock_exclusive()
        .context("An agent is already running for this data directory")?;
    let (tx, mut rx) = mpsc::channel::<Envelope>(32);
    let mut engine = Engine::open(&data)?;
    let endpoint = platform::endpoint(&data)?;
    #[cfg(unix)]
    let mut listener = {
        use std::os::unix::fs::PermissionsExt;
        if std::path::Path::new(&endpoint).exists() {
            std::fs::remove_file(&endpoint)?;
        }
        let listener = tokio::net::UnixListener::bind(&endpoint).context("Bind agent socket")?;
        std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600))?;
        listener
    };
    #[cfg(windows)]
    let mut listener = pipe(&endpoint, true)?;
    println!("K3 Up agent listening at {endpoint}");
    let generation = engine.subscribe();
    let mut exits = ChildExits::new()?;
    tokio::pin!(shutdown);
    // The first pass recovers persisted services right away. A fixed deadline, not a fresh
    // sleep per iteration, so a stream of connections or accept errors cannot postpone it.
    let mut deadline = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tokio::time::sleep_until(deadline) => engine_tick(&mut engine).await,
            _ = exits.next() => engine_tick(&mut engine).await,
            Some((request, reply)) = rx.recv() => {
                if request.version == VERSION && matches!(request.command, Command::Shutdown) {
                    let _ = reply.send(Response::success("Agent stopping; workloads will be stopped first"));
                    // The serving task writes the reply; give it a turn before the loop ends,
                    // because with nothing to stop the runtime is gone within microseconds.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    break;
                }
                let response = if request.version != VERSION { Response::error("Unsupported API version") }
                else { match engine.handle(request.command).await { Ok(response) => response, Err(error) => Response::error(format!("{error:#}")) } };
                let _ = reply.send(response);
            }
            accepted = accept(&mut listener, &endpoint) => {
                let stream = match accepted {
                    Ok(stream) => stream,
                    Err(error) => {
                        // Usually handle exhaustion; back off instead of spinning or exiting.
                        eprintln!("Accept failed: {error:#}");
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        continue;
                    }
                };
                let tx = tx.clone();
                let generation = generation.clone();
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(Duration::from_secs(120), serve(stream, tx, generation)).await;
                });
            }
        }
        deadline = tokio::time::Instant::now() + engine.next_wake();
    }
    engine.shutdown().await?;
    #[cfg(unix)]
    let _ = std::fs::remove_file(endpoint);
    drop(lock);
    Ok(())
}

async fn engine_tick(engine: &mut Engine) {
    if let Err(error) = engine.tick().await {
        eprintln!("Supervision tick failed: {error:#}");
    }
}

/// Wakes supervision as soon as a workload exits, so it can sleep until the next deadline
/// instead of polling. Windows has no equivalent here and polls instead.
struct ChildExits {
    #[cfg(unix)]
    signal: tokio::signal::unix::Signal,
}

impl ChildExits {
    fn new() -> Result<Self> {
        Ok(Self {
            #[cfg(unix)]
            signal: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())
                .context("Watch for child exits")?,
        })
    }

    async fn next(&mut self) {
        #[cfg(unix)]
        self.signal.recv().await;
        #[cfg(not(unix))]
        std::future::pending::<()>().await;
    }
}

#[cfg(unix)]
async fn accept(
    listener: &mut tokio::net::UnixListener,
    _: &str,
) -> Result<tokio::net::UnixStream> {
    Ok(listener.accept().await.map(|(stream, _)| stream)?)
}

#[cfg(windows)]
fn pipe(endpoint: &str, first: bool) -> Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    let mut attributes = crate::win32::SecurityAttributes::from_sddl(crate::win32::PIPE_SDDL)?;
    // SAFETY: attributes outlives the call and holds a valid security descriptor.
    Ok(unsafe {
        tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(endpoint, attributes.as_mut_ptr().cast())?
    })
}

/// Hands out the connected instance and leaves a fresh one listening in its place.
#[cfg(windows)]
async fn accept(
    listener: &mut tokio::net::windows::named_pipe::NamedPipeServer,
    endpoint: &str,
) -> Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    listener.connect().await?;
    let next = pipe(endpoint, false)?;
    Ok(std::mem::replace(listener, next))
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    tx: mpsc::Sender<Envelope>,
    generation: tokio::sync::watch::Receiver<u64>,
) -> Result<()> {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader.take((MAX_FRAME + 1) as u64));
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line)).await??;
    let response = if line.len() > MAX_FRAME {
        Response::error("Request exceeds 1 MiB")
    } else {
        match serde_json::from_str::<Request>(&line) {
            Err(error) => Response::error(format!("Invalid request: {error}")),
            Ok(Request {
                version: VERSION,
                command: Command::Watch { since, timeout_ms },
            }) => watch(generation, since, timeout_ms).await,
            Ok(request) => {
                let (reply, received) = oneshot::channel();
                tx.send((request, reply)).await?;
                received.await?
            }
        }
    };
    let mut bytes = serde_json::to_vec(&response)?;
    if bytes.len() > MAX_FRAME {
        bytes = serde_json::to_vec(&Response::error(
            "Response too large; request a smaller result",
        ))?;
    }
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    Ok(())
}

/// Answers without the engine, so a waiting client never delays supervision.
async fn watch(
    mut generation: tokio::sync::watch::Receiver<u64>,
    since: u64,
    timeout_ms: u64,
) -> Response {
    let timeout = Duration::from_millis(timeout_ms.min(60_000));
    let _ = tokio::time::timeout(timeout, generation.wait_for(|current| *current > since)).await;
    let current = *generation.borrow();
    Response {
        generation: Some(current),
        ..Response::success("Generation")
    }
}

pub async fn signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Install SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
