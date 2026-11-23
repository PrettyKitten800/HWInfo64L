use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use hwinfo64l_core::{
    discover_plugin_executables, now_epoch_ms, CoreEngine, NdjsonPluginProbe, SupervisorConfig,
};
use hwinfo64l_protocol::{
    HelloResponse, HistoryResponse, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    ListSensorsResponse, ResetStatsResponse, SensorBatchParams, DEFAULT_SOCKET_PATH,
    PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, Semaphore};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

/// Maximum concurrent Unix socket clients accepted by `hwinfo64ld`.
const MAX_CONCURRENT_CLIENTS: usize = 64;
/// Maximum bytes accepted per JSON-RPC request line (64 KiB).
const MAX_RPC_LINE_BYTES: usize = 64 * 1024;
/// Idle read timeout for non-subscribed connections.
const IDLE_CLIENT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Parser)]
#[command(
    name = "hwinfo64ld",
    version,
    about = "Hardened read-only hardware telemetry daemon for HWInfo64L"
)]
struct Args {
    /// Path to the Unix domain socket.
    #[arg(long, env = "HWINFO64L_SOCKET", default_value = DEFAULT_SOCKET_PATH)]
    socket: PathBuf,

    /// Directory scanned for system-wide NDJSON plugins.
    #[arg(long, default_value = "/usr/lib/hwinfo64l/plugins")]
    plugin_dir: PathBuf,

    /// Ring-buffer history capacity per sensor.
    #[arg(long, default_value_t = 3600)]
    history_capacity: usize,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    if let Err(err) = run(args).await {
        error!(error = %err, "hwinfo64ld exited with error");
        std::process::exit(1);
    }
}

async fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = args.socket.parent() {
        fs::create_dir_all(parent)?;
    }
    if args.socket.exists() {
        let _ = fs::remove_file(&args.socket);
    }

    let listener = UnixListener::bind(&args.socket)?;
    // Socket serves strictly read-only telemetry; allow unprivileged local users to connect.
    fs::set_permissions(&args.socket, fs::Permissions::from_mode(0o666))?;

    let mut cfg = SupervisorConfig::default();
    cfg.store.history_capacity = args.history_capacity;
    let engine = CoreEngine::new(cfg);

    for probe in hwinfo64l_probes::default_probes() {
        engine.register_probe(probe);
    }

    for exec in discover_plugin_executables(&args.plugin_dir) {
        info!(plugin = %exec.display(), "registering system plugin");
        engine.register_probe(Box::new(NdjsonPluginProbe::new(exec)));
    }

    info!(
        socket = %args.socket.display(),
        version = env!("CARGO_PKG_VERSION"),
        "hwinfo64ld listening"
    );

    let privileged = is_effective_root();
    let hostname = fs::read_to_string("/etc/hostname")
        .or_else(|_| fs::read_to_string("/proc/sys/kernel/hostname"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    let conn_limit = Arc::new(Semaphore::new(MAX_CONCURRENT_CLIENTS));
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            accept_res = listener.accept() => {
                match accept_res {
                    Ok((stream, _addr)) => {
                        let Ok(permit) = Arc::clone(&conn_limit).try_acquire_owned() else {
                            warn!("max concurrent clients ({MAX_CONCURRENT_CLIENTS}) reached; dropping connection");
                            drop(stream);
                            continue;
                        };
                        let eng = Arc::clone(&engine);
                        let host = hostname.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            if let Err(e) = handle_client(stream, eng, host, privileged).await {
                                warn!(error = %e, "client disconnected with error");
                            }
                        });
                    }
                    Err(e) => {
                        warn!(error = %e, "accept failed");
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("received SIGINT, shutting down");
                break;
            }
            _ = sigterm.recv() => {
                info!("received SIGTERM, shutting down");
                break;
            }
        }
    }

    drop(listener);
    let _ = fs::remove_file(&args.socket);
    engine.shutdown().await;
    Ok(())
}

async fn handle_client(
    stream: UnixStream,
    engine: Arc<CoreEngine>,
    hostname: String,
    privileged: bool,
) -> std::io::Result<()> {
    let (read_half, write_half) = stream.into_split();
    let writer = Arc::new(Mutex::new(write_half));
    let mut reader = BufReader::new(read_half);
    let mut sub_task: Option<tokio::task::JoinHandle<()>> = None;

    loop {
        let read_fut = read_line_bounded_async(&mut reader, MAX_RPC_LINE_BYTES);
        let line_opt = if sub_task.is_some() {
            read_fut.await?
        } else {
            match tokio::time::timeout(IDLE_CLIENT_TIMEOUT, read_fut).await {
                Ok(res) => res?,
                Err(_) => break,
            }
        };

        let Some(line) = line_opt else {
            break;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                let resp = JsonRpcResponse::err(Value::Null, -32700, format!("parse error: {e}"));
                write_json_line(&writer, &resp).await?;
                continue;
            }
        };

        let resp = match req.method.as_str() {
            "v1.hello" => {
                let payload = HelloResponse {
                    daemon_version: env!("CARGO_PKG_VERSION").to_string(),
                    protocol_version: PROTOCOL_VERSION,
                    hostname: hostname.clone(),
                    uptime_secs: engine.uptime_secs(),
                    privileged,
                };
                serialize_ok(req.id, &payload)
            }
            "v1.get_summary" => serialize_ok(req.id, &engine.summary()),
            "v1.get_inventory" => serialize_ok(req.id, &engine.inventory()),
            "v1.list_sensors" => {
                let payload = ListSensorsResponse {
                    descriptors: engine.descriptors(),
                    readings: engine.readings(None),
                    probes: engine.probes(),
                };
                serialize_ok(req.id, &payload)
            }
            "v1.get_history" => {
                let id = req
                    .params
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let limit = req
                    .params
                    .get("limit")
                    .and_then(Value::as_u64)
                    .and_then(|v| usize::try_from(v).ok())
                    .unwrap_or(300);
                let payload = HistoryResponse {
                    samples: engine.history(&id, limit),
                    id,
                };
                serialize_ok(req.id, &payload)
            }
            "v1.reset_stats" => {
                let ids: Option<Vec<String>> = req
                    .params
                    .get("ids")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                let reset_count = engine.reset_stats(ids.as_deref());
                serialize_ok(req.id, &ResetStatsResponse { reset_count })
            }
            "v1.subscribe" => {
                let interval_ms = req
                    .params
                    .get("interval_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(1000)
                    .clamp(250, 60_000);
                let ids: Option<Vec<String>> = req
                    .params
                    .get("ids")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());

                if let Some(prev) = sub_task.take() {
                    prev.abort();
                }

                let eng_sub = Arc::clone(&engine);
                let writer_sub = Arc::clone(&writer);
                sub_task = Some(tokio::spawn(async move {
                    let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
                    loop {
                        ticker.tick().await;
                        let batch = SensorBatchParams {
                            timestamp_ms: now_epoch_ms(),
                            readings: eng_sub.readings(ids.as_deref()),
                            probes: eng_sub.probes(),
                        };
                        let Ok(params) = serde_json::to_value(&batch) else {
                            continue;
                        };
                        let notif = JsonRpcNotification::new("v1.sensor_batch", params);
                        if write_json_line(&writer_sub, &notif).await.is_err() {
                            break;
                        }
                    }
                }));

                JsonRpcResponse::ok(
                    req.id,
                    json!({ "subscribed": true, "interval_ms": interval_ms }),
                )
            }
            other => JsonRpcResponse::err(req.id, -32601, format!("unknown method: {other}")),
        };

        write_json_line(&writer, &resp).await?;
    }

    if let Some(task) = sub_task {
        task.abort();
    }
    Ok(())
}

async fn read_line_bounded_async<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<Option<String>> {
    let mut buf = Vec::new();
    let limit = (max_bytes as u64).saturating_add(1);
    let n = reader.take(limit).read_until(b'\n', &mut buf).await?;
    if n == 0 {
        return Ok(None);
    }
    if buf.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("RPC line exceeded {max_bytes} bytes"),
        ));
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn serialize_ok<T: serde::Serialize>(id: Value, value: &T) -> JsonRpcResponse {
    match serde_json::to_value(value) {
        Ok(v) => JsonRpcResponse::ok(id, v),
        Err(e) => JsonRpcResponse::err(id, -32603, format!("serialization error: {e}")),
    }
}

async fn write_json_line<T: serde::Serialize>(
    writer: &Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    value: &T,
) -> std::io::Result<()> {
    let mut encoded = serde_json::to_string(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    encoded.push('\n');
    let mut guard = writer.lock().await;
    guard.write_all(encoded.as_bytes()).await?;
    guard.flush().await
}

fn is_effective_root() -> bool {
    if let Ok(status) = fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                let mut cols = rest.split_whitespace();
                let _real = cols.next();
                if let Some(effective) = cols.next() {
                    return effective == "0";
                }
            }
        }
    }
    false
}
