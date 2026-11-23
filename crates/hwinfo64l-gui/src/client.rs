use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use hwinfo64l_core::{
    discover_plugin_executables, now_epoch_ms, CoreEngine, CsvLogger, NdjsonPluginProbe,
    SupervisorConfig,
};
use hwinfo64l_protocol::{
    HelloResponse, HistoryResponse, HistorySample, InventoryNode, JsonRpcRequest, JsonRpcResponse,
    ListSensorsResponse, ProbeHealth, SensorDescriptor, SensorReading, SystemSummary,
    PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Connection mode displayed in the GUI header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionMode {
    Daemon { privileged: bool },
    LocalUnprivileged,
}

/// Snapshot pushed periodically from the background telemetry thread to GTK.
#[derive(Debug, Clone)]
pub struct TelemetrySnapshot {
    pub mode: ConnectionMode,
    pub summary: SystemSummary,
    pub inventory: Vec<InventoryNode>,
    pub descriptors: Vec<SensorDescriptor>,
    pub readings: Vec<SensorReading>,
    pub probes: Vec<ProbeHealth>,
    pub csv_logging_path: Option<PathBuf>,
}

/// Events sent from the background telemetry worker to the GTK UI thread.
#[derive(Debug)]
pub enum WorkerEvent {
    Snapshot(Box<TelemetrySnapshot>),
    History {
        sensor_id: String,
        samples: Vec<HistorySample>,
    },
    Notice(String),
}

/// Commands sent from the GTK UI thread to the background telemetry worker.
#[derive(Debug)]
pub enum WorkerCommand {
    ResetStats,
    SetPaused(bool),
    SetIntervalMs(u64),
    RequestHistory { sensor_id: String, limit: usize },
    StartCsvLog(PathBuf),
    StopCsvLog,
    Shutdown,
}

/// Spawn the background Tokio telemetry runtime and return channels for GTK.
pub fn spawn_worker(
    socket_path: PathBuf,
    force_local: bool,
) -> (mpsc::Sender<WorkerCommand>, mpsc::Receiver<WorkerEvent>) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCommand>();
    let (evt_tx, evt_rx) = mpsc::channel::<WorkerEvent>();

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = evt_tx.send(WorkerEvent::Notice(format!("runtime error: {e}")));
                return;
            }
        };

        rt.block_on(worker_loop(socket_path, force_local, cmd_rx, evt_tx));
    });

    (cmd_tx, evt_rx)
}

struct DaemonConn {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    next_id: u64,
    privileged: bool,
}

impl DaemonConn {
    async fn try_connect(socket: &PathBuf) -> Option<Self> {
        let stream = UnixStream::connect(socket).await.ok()?;
        let (r, w) = stream.into_split();
        let mut conn = Self {
            reader: BufReader::new(r),
            writer: w,
            next_id: 1,
            privileged: false,
        };
        let hello_val = conn
            .rpc(
                "v1.hello",
                json!({ "client": "hwinfo64l-gui", "protocol_version": PROTOCOL_VERSION }),
            )
            .await
            .ok()?;
        let hello: HelloResponse = serde_json::from_value(hello_val).ok()?;
        if hello.protocol_version != PROTOCOL_VERSION {
            return None;
        }
        conn.privileged = hello.privileged;
        Some(conn)
    }

    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let req = JsonRpcRequest::new(id, method, params);
        let mut line = serde_json::to_string(&req).map_err(|e| e.to_string())?;
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        self.writer.flush().await.map_err(|e| e.to_string())?;

        let mut buf = String::new();
        loop {
            buf.clear();
            let n = self
                .reader
                .read_line(&mut buf)
                .await
                .map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("daemon disconnected".into());
            }
            if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(buf.trim()) {
                if let Some(err) = resp.error {
                    return Err(err.message);
                }
                return Ok(resp.result.unwrap_or(Value::Null));
            }
        }
    }
}

async fn worker_loop(
    socket_path: PathBuf,
    force_local: bool,
    cmd_rx: mpsc::Receiver<WorkerCommand>,
    evt_tx: mpsc::Sender<WorkerEvent>,
) {
    // User-level plugins ALWAYS run unprivileged in the GUI process so user plugins never run as root.
    let user_plugin_engine = CoreEngine::new(SupervisorConfig::default());
    if let Some(dir) = user_plugin_dir() {
        for exec in discover_plugin_executables(&dir) {
            user_plugin_engine.register_probe(Box::new(NdjsonPluginProbe::new(exec)));
        }
    }

    let mut daemon: Option<DaemonConn> = if force_local {
        None
    } else {
        DaemonConn::try_connect(&socket_path).await
    };

    let mut local_fallback: Option<Arc<CoreEngine>> = None;
    if daemon.is_none() {
        local_fallback = Some(start_local_engine());
    }

    let mut paused = false;
    let mut interval_ms = 1000u64;
    let mut csv_logger: Option<(PathBuf, CsvLogger)> = None;
    let mut cached_summary = SystemSummary::default();
    let mut cached_inventory: Vec<InventoryNode> = Vec::new();
    let mut tick_count = 0u64;

    loop {
        // Drain pending UI commands
        let mut should_shutdown = false;
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                WorkerCommand::Shutdown => {
                    should_shutdown = true;
                    break;
                }
                WorkerCommand::SetPaused(p) => paused = p,
                WorkerCommand::SetIntervalMs(ms) => interval_ms = ms.clamp(250, 10_000),
                WorkerCommand::ResetStats => {
                    if let Some(ref mut d) = daemon {
                        let _ = d.rpc("v1.reset_stats", json!({})).await;
                    }
                    if let Some(ref eng) = local_fallback {
                        eng.reset_stats(None);
                    }
                    user_plugin_engine.reset_stats(None);
                }
                WorkerCommand::RequestHistory { sensor_id, limit } => {
                    let mut samples = user_plugin_engine.history(&sensor_id, limit);
                    if samples.is_empty() {
                        if let Some(ref mut d) = daemon {
                            if let Ok(val) = d
                                .rpc(
                                    "v1.get_history",
                                    json!({ "id": sensor_id, "limit": limit }),
                                )
                                .await
                            {
                                if let Ok(resp) = serde_json::from_value::<HistoryResponse>(val) {
                                    samples = resp.samples;
                                }
                            }
                        } else if let Some(ref eng) = local_fallback {
                            samples = eng.history(&sensor_id, limit);
                        }
                    }
                    let _ = evt_tx.send(WorkerEvent::History { sensor_id, samples });
                }
                WorkerCommand::StartCsvLog(path) => {
                    let descs = collect_descriptors(&mut daemon, &local_fallback, &user_plugin_engine).await;
                    match CsvLogger::create(&path, &descs) {
                        Ok(logger) => {
                            let _ = evt_tx.send(WorkerEvent::Notice(format!(
                                "CSV logging started: {}",
                                path.display()
                            )));
                            csv_logger = Some((path, logger));
                        }
                        Err(e) => {
                            let _ = evt_tx.send(WorkerEvent::Notice(format!(
                                "Failed to open CSV log: {e}"
                            )));
                        }
                    }
                }
                WorkerCommand::StopCsvLog => {
                    if let Some((path, _)) = csv_logger.take() {
                        let _ = evt_tx.send(WorkerEvent::Notice(format!(
                            "CSV logging stopped: {}",
                            path.display()
                        )));
                    }
                }
            }
        }
        if should_shutdown {
            break;
        }

        // Periodically try reconnecting to daemon if in fallback mode (and not forced local)
        if !force_local && daemon.is_none() && tick_count % 5 == 0 {
            if let Some(conn) = DaemonConn::try_connect(&socket_path).await {
                if let Some(old_local) = local_fallback.take() {
                    old_local.shutdown().await;
                }
                daemon = Some(conn);
                tick_count = 0; // force summary/inventory refresh
            }
        }

        if !paused {
            let refresh_static = tick_count % 5 == 0;
            let mut daemon_failed = false;

            let (mode, mut descriptors, mut readings, mut probes) =
                if let Some(ref mut d) = daemon {
                    let mode = ConnectionMode::Daemon {
                        privileged: d.privileged,
                    };
                    if refresh_static {
                        if let Ok(v) = d.rpc("v1.get_summary", json!({})).await {
                            if let Ok(s) = serde_json::from_value::<SystemSummary>(v) {
                                cached_summary = s;
                            }
                        }
                        if let Ok(v) = d.rpc("v1.get_inventory", json!({})).await {
                            if let Ok(inv) = serde_json::from_value::<Vec<InventoryNode>>(v) {
                                cached_inventory = inv;
                            }
                        }
                    }

                    match d.rpc("v1.list_sensors", json!({})).await {
                        Ok(v) => match serde_json::from_value::<ListSensorsResponse>(v) {
                            Ok(list) => (mode, list.descriptors, list.readings, list.probes),
                            Err(_) => {
                                daemon_failed = true;
                                (mode, Vec::new(), Vec::new(), Vec::new())
                            }
                        },
                        Err(_) => {
                            daemon_failed = true;
                            (mode, Vec::new(), Vec::new(), Vec::new())
                        }
                    }
                } else {
                    let eng = local_fallback.get_or_insert_with(start_local_engine);
                    cached_summary = eng.summary();
                    cached_inventory = eng.inventory();
                    (
                        ConnectionMode::LocalUnprivileged,
                        eng.descriptors(),
                        eng.readings(None),
                        eng.probes(),
                    )
                };

            if daemon_failed {
                daemon = None;
                if local_fallback.is_none() {
                    local_fallback = Some(start_local_engine());
                }
                let _ = evt_tx.send(WorkerEvent::Notice(
                    "Daemon disconnected — fell back to unprivileged local mode".into(),
                ));
            } else {
                // Merge unprivileged user plugins
                merge_user_plugins(
                    &user_plugin_engine,
                    &mut descriptors,
                    &mut readings,
                    &mut probes,
                    &mut cached_inventory,
                );

                let ts = now_epoch_ms();
                if let Some((_, ref mut logger)) = csv_logger {
                    let _ = logger.write_row(ts, &readings);
                }

                let snap = TelemetrySnapshot {
                    mode,
                    summary: cached_summary.clone(),
                    inventory: cached_inventory.clone(),
                    descriptors,
                    readings,
                    probes,
                    csv_logging_path: csv_logger.as_ref().map(|(p, _)| p.clone()),
                };

                if evt_tx.send(WorkerEvent::Snapshot(Box::new(snap))).is_err() {
                    break;
                }
            }
            tick_count = tick_count.saturating_add(1);
        }

        tokio::time::sleep(Duration::from_millis(interval_ms)).await;
    }

    if let Some(eng) = local_fallback {
        eng.shutdown().await;
    }
    user_plugin_engine.shutdown().await;
}

fn start_local_engine() -> Arc<CoreEngine> {
    let eng = CoreEngine::new(SupervisorConfig::default());
    for probe in hwinfo64l_probes::default_probes() {
        eng.register_probe(probe);
    }
    eng
}

async fn collect_descriptors(
    daemon: &mut Option<DaemonConn>,
    local_fallback: &Option<Arc<CoreEngine>>,
    user_plugins: &Arc<CoreEngine>,
) -> Vec<SensorDescriptor> {
    let mut descs = if let Some(ref mut d) = daemon {
        d.rpc("v1.list_sensors", json!({}))
            .await
            .ok()
            .and_then(|v| serde_json::from_value::<ListSensorsResponse>(v).ok())
            .map(|r| r.descriptors)
            .unwrap_or_default()
    } else if let Some(ref eng) = local_fallback {
        eng.descriptors()
    } else {
        Vec::new()
    };
    descs.extend(user_plugins.descriptors());
    descs
}

fn merge_user_plugins(
    user_plugins: &Arc<CoreEngine>,
    descriptors: &mut Vec<SensorDescriptor>,
    readings: &mut Vec<SensorReading>,
    probes: &mut Vec<ProbeHealth>,
    inventory: &mut Vec<InventoryNode>,
) {
    let mut seen_desc: BTreeMap<String, usize> = descriptors
        .iter()
        .enumerate()
        .map(|(i, d)| (d.id.clone(), i))
        .collect();
    for d in user_plugins.descriptors() {
        if !seen_desc.contains_key(&d.id) {
            seen_desc.insert(d.id.clone(), descriptors.len());
            descriptors.push(d);
        }
    }

    let seen_read: BTreeMap<String, ()> = readings.iter().map(|r| (r.id.clone(), ())).collect();
    for r in user_plugins.readings(None) {
        if !seen_read.contains_key(&r.id) {
            readings.push(r);
        }
    }

    probes.extend(user_plugins.probes());
    for node in user_plugins.inventory() {
        if !inventory.iter().any(|n| n.id == node.id) {
            inventory.push(node);
        }
    }
}

fn user_plugin_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/hwinfo64l/plugins"))
}
