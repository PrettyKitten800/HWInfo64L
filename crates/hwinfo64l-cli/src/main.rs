use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use hwinfo64l_core::{
    discover_plugin_executables, now_epoch_ms, CoreEngine, CsvLogger, NdjsonPluginProbe,
    SupervisorConfig,
};
use hwinfo64l_protocol::{
    HelloResponse, InventoryNode, JsonRpcRequest, JsonRpcResponse, ListSensorsResponse,
    ProbeHealth, PropertyValue, SensorDescriptor, SensorKind, SensorReading, SensorStatus,
    SystemSummary, DEFAULT_SOCKET_PATH, PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Debug, Parser)]
#[command(
    name = "hwinfo64l",
    version,
    about = "HWiNFO64-style hardware inventory and live sensor CLI for Linux"
)]
struct Cli {
    /// Path to the hwinfo64ld Unix domain socket.
    #[arg(long, env = "HWINFO64L_SOCKET", default_value = DEFAULT_SOCKET_PATH)]
    socket: PathBuf,

    /// Force local in-process probing instead of connecting to hwinfo64ld.
    #[arg(long)]
    local: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Display HWiNFO64-style system hardware summary.
    Summary {
        #[arg(long)]
        json: bool,
    },
    /// Display current sensor readings grouped by hardware category.
    Sensors {
        #[arg(long)]
        json: bool,
        /// Case-insensitive substring filter on sensor group, name, or ID.
        #[arg(long)]
        filter: Option<String>,
    },
    /// Live-updating terminal sensor monitor.
    Watch {
        /// Refresh interval in milliseconds.
        #[arg(long, default_value_t = 1000)]
        interval: u64,
        /// Case-insensitive substring filter on sensor group, name, or ID.
        #[arg(long)]
        filter: Option<String>,
    },
    /// Display diagnostic health and timing for every probe/plugin.
    Probes {
        #[arg(long)]
        json: bool,
    },
    /// Display full hardware inventory tree.
    Inventory {
        #[arg(long)]
        json: bool,
    },
    /// Log sensor readings to an HWiNFO64-compatible CSV file.
    LogCsv {
        /// Output CSV path.
        path: PathBuf,
        /// Sampling interval in milliseconds.
        #[arg(long, default_value_t = 1000)]
        interval: u64,
        /// Optional number of samples to record (0 = run until Ctrl+C).
        #[arg(long, default_value_t = 0)]
        samples: u64,
    },
}

enum Backend {
    Daemon {
        reader: BufReader<tokio::net::unix::OwnedReadHalf>,
        writer: tokio::net::unix::OwnedWriteHalf,
        next_id: u64,
    },
    Local {
        engine: Arc<CoreEngine>,
    },
}

impl Backend {
    async fn connect(socket: &Path, force_local: bool) -> Result<Self, Box<dyn std::error::Error>> {
        if !force_local {
            if let Ok(stream) = UnixStream::connect(socket).await {
                let (r, w) = stream.into_split();
                let mut b = Self::Daemon {
                    reader: BufReader::new(r),
                    writer: w,
                    next_id: 1,
                };
                if let Ok(hello) = b.hello().await {
                    if hello.protocol_version == PROTOCOL_VERSION {
                        return Ok(b);
                    }
                }
            }
        }

        if !force_local {
            eprintln!(
                "[hwinfo64l] daemon not reachable at {}; running in unprivileged local mode",
                socket.display()
            );
        }

        let engine = CoreEngine::new(SupervisorConfig::default());
        for probe in hwinfo64l_probes::default_probes() {
            engine.register_probe(probe);
        }

        if let Some(plugin_dir) = user_plugin_dir() {
            for exec in discover_plugin_executables(&plugin_dir) {
                engine.register_probe(Box::new(NdjsonPluginProbe::new(exec)));
            }
        }

        // Wait briefly so initial polls + delta-based probes (CPU util, network, RAPL)
        // collect at least two ticks where possible.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        Ok(Self::Local { engine })
    }

    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let Self::Daemon {
            reader,
            writer,
            next_id,
        } = self
        else {
            return Err("rpc called on local backend".into());
        };
        let id = *next_id;
        *next_id = next_id.saturating_add(1);
        let req = JsonRpcRequest::new(id, method, params);
        let mut line = serde_json::to_string(&req).map_err(|e| e.to_string())?;
        line.push('\n');
        writer
            .write_all(line.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        writer.flush().await.map_err(|e| e.to_string())?;

        let mut buf = String::new();
        loop {
            buf.clear();
            let n = reader.read_line(&mut buf).await.map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("daemon closed connection".into());
            }
            if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(buf.trim()) {
                if let Some(err) = resp.error {
                    return Err(err.message);
                }
                return Ok(resp.result.unwrap_or(Value::Null));
            }
        }
    }

    async fn hello(&mut self) -> Result<HelloResponse, String> {
        let v = self
            .rpc(
                "v1.hello",
                json!({ "client": "hwinfo64l-cli", "protocol_version": PROTOCOL_VERSION }),
            )
            .await?;
        serde_json::from_value(v).map_err(|e| e.to_string())
    }

    async fn summary(&mut self) -> Result<SystemSummary, String> {
        match self {
            Self::Daemon { .. } => {
                let v = self.rpc("v1.get_summary", json!({})).await?;
                serde_json::from_value(v).map_err(|e| e.to_string())
            }
            Self::Local { engine } => Ok(engine.summary()),
        }
    }

    async fn inventory(&mut self) -> Result<Vec<InventoryNode>, String> {
        match self {
            Self::Daemon { .. } => {
                let v = self.rpc("v1.get_inventory", json!({})).await?;
                serde_json::from_value(v).map_err(|e| e.to_string())
            }
            Self::Local { engine } => Ok(engine.inventory()),
        }
    }

    async fn list_sensors(&mut self) -> Result<ListSensorsResponse, String> {
        match self {
            Self::Daemon { .. } => {
                let v = self.rpc("v1.list_sensors", json!({})).await?;
                serde_json::from_value(v).map_err(|e| e.to_string())
            }
            Self::Local { engine } => Ok(ListSensorsResponse {
                descriptors: engine.descriptors(),
                readings: engine.readings(None),
                probes: engine.probes(),
            }),
        }
    }

    async fn shutdown(self) {
        if let Self::Local { engine } = self {
            engine.shutdown().await;
        }
    }
}

fn user_plugin_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/hwinfo64l/plugins"))
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let mut backend = Backend::connect(&cli.socket, cli.local).await?;

    match cli.command {
        Command::Summary { json: as_json } => {
            let summary = backend.summary().await?;
            if as_json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                print_summary(&summary);
            }
        }
        Command::Sensors {
            json: as_json,
            filter,
        } => {
            let data = backend.list_sensors().await?;
            if as_json {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else {
                print_sensors(&data.descriptors, &data.readings, filter.as_deref());
            }
        }
        Command::Watch { interval, filter } => {
            let dur = Duration::from_millis(interval.clamp(250, 60_000));
            let mut ticker = tokio::time::interval(dur);
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        let data = backend.list_sensors().await?;
                        // ANSI clear screen + move cursor home
                        print!("\x1b[2J\x1b[H");
                        println!(
                            "HWInfo64L Live Sensors (interval: {} ms — Ctrl+C to quit)\n",
                            dur.as_millis()
                        );
                        print_sensors(&data.descriptors, &data.readings, filter.as_deref());
                    }
                    _ = tokio::signal::ctrl_c() => {
                        break;
                    }
                }
            }
        }
        Command::Probes { json: as_json } => {
            let data = backend.list_sensors().await?;
            if as_json {
                println!("{}", serde_json::to_string_pretty(&data.probes)?);
            } else {
                print_probes(&data.probes);
            }
        }
        Command::Inventory { json: as_json } => {
            let nodes = backend.inventory().await?;
            if as_json {
                println!("{}", serde_json::to_string_pretty(&nodes)?);
            } else {
                print_inventory(&nodes);
            }
        }
        Command::LogCsv {
            path,
            interval,
            samples,
        } => {
            let initial = backend.list_sensors().await?;
            let mut logger = CsvLogger::create(&path, &initial.descriptors)?;
            eprintln!(
                "Logging {} sensors to {} (interval: {} ms)...",
                initial.descriptors.len(),
                path.display(),
                interval
            );
            let mut ticker = tokio::time::interval(Duration::from_millis(interval.clamp(250, 60_000)));
            let mut count = 0u64;
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        let data = backend.list_sensors().await?;
                        logger.write_row(now_epoch_ms(), &data.readings)?;
                        count += 1;
                        if samples > 0 && count >= samples {
                            break;
                        }
                    }
                    _ = tokio::signal::ctrl_c() => {
                        break;
                    }
                }
            }
            eprintln!("Wrote {count} rows to {}", path.display());
        }
    }

    backend.shutdown().await;
    Ok(())
}

fn print_summary(s: &SystemSummary) {
    println!("=== HWInfo64L System Summary ({}) ===", s.hostname);
    println!("OS          : {} ({}, {})", s.os.pretty_name, s.os.kernel, s.os.arch);
    println!(
        "CPU         : {} ({}C/{}T)",
        s.cpu.model_name, s.cpu.physical_cores, s.cpu.logical_cpus
    );
    if let (Some(min), Some(max)) = (s.cpu.min_mhz, s.cpu.max_mhz) {
        println!("CPU Clocks  : {:.0} MHz - {:.0} MHz", min, max);
    }
    if s.cpu.pl1_watts.is_some() || s.cpu.pl2_watts.is_some() {
        println!(
            "Power Limits: PL1 = {} | PL2 = {}",
            s.cpu
                .pl1_watts
                .map(|w| format!("{w:.1} W"))
                .unwrap_or_else(|| "N/A".into()),
            s.cpu
                .pl2_watts
                .map(|w| format!("{w:.1} W"))
                .unwrap_or_else(|| "N/A".into())
        );
    }
    println!(
        "Motherboard : {} {} (BIOS {} {}, {})",
        s.motherboard.board_vendor,
        s.motherboard.board_name,
        s.motherboard.bios_vendor,
        s.motherboard.bios_version,
        s.motherboard.bios_date
    );
    let phys_gib = (s.memory.total_physical_bytes as f64) / (1024.0 * 1024.0 * 1024.0);
    let usable_gib = (s.memory.usable_bytes as f64) / (1024.0 * 1024.0 * 1024.0);
    println!(
        "Memory      : {:.1} GiB physical ({:.2} GiB usable) {} @ {} MT/s ({} ch) | zswap: {}",
        phys_gib,
        usable_gib,
        s.memory.mem_type.as_deref().unwrap_or("RAM"),
        s.memory
            .configured_speed_mts
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".into()),
        s.memory.channels_populated,
        if s.memory.zswap_enabled {
            format!(
                "on ({})",
                s.memory.zswap_compressor.as_deref().unwrap_or("?")
            )
        } else {
            "off".into()
        }
    );
    for m in &s.memory.modules {
        let gb = (m.size_bytes as f64) / (1024.0 * 1024.0 * 1024.0);
        println!(
            "  - {:<20} {:.0} GB {} {} ({}, {})",
            m.locator, gb, m.mem_type, m.manufacturer, m.part_number, m.form_factor
        );
    }
    for gpu in &s.gpus {
        println!("GPU         : {} [driver: {}]", gpu.name, gpu.driver);
    }
    for d in &s.drives {
        let gb = (d.size_bytes as f64) / 1e9;
        println!(
            "Drive       : /dev/{} — {} ({:.1} GB {}, SMART: {})",
            d.device,
            d.model,
            gb,
            d.transport,
            d.smart_health.as_deref().unwrap_or("N/A")
        );
    }
}

fn print_sensors(
    descriptors: &[SensorDescriptor],
    readings: &[SensorReading],
    filter: Option<&str>,
) {
    let by_id: BTreeMap<&str, &SensorReading> =
        readings.iter().map(|r| (r.id.as_str(), r)).collect();
    let needle = filter.map(str::to_ascii_lowercase);

    let mut groups: BTreeMap<&str, Vec<(&SensorDescriptor, &SensorReading)>> = BTreeMap::new();
    for desc in descriptors {
        let Some(r) = by_id.get(desc.id.as_str()) else {
            continue;
        };
        if let Some(ref q) = needle {
            let hay = format!("{} {} {}", desc.group, desc.name, desc.id).to_ascii_lowercase();
            if !hay.contains(q) {
                continue;
            }
        }
        groups.entry(desc.group.as_str()).or_default().push((desc, r));
    }

    println!(
        "{:<34} {:>11} {:>11} {:>11} {:>11}  Status",
        "Sensor", "Current", "Min", "Max", "Avg"
    );
    println!("{}", "-".repeat(90));

    for (group, items) in groups {
        println!("[{group}]");
        for (d, r) in items {
            let status_tag = match r.status {
                SensorStatus::Ok => "OK",
                SensorStatus::Warning => "WARN",
                SensorStatus::Critical => "CRIT",
                SensorStatus::Stale => "STALE",
                SensorStatus::Unavailable => "N/A",
            };
            if d.kind == SensorKind::BooleanFlag {
                let yn = |v: f64| if v >= 0.5 { "Yes" } else { "No" };
                println!(
                    "  {:<32} {:>12} {:>12} {:>12} {:>12} {}",
                    truncate(&d.name, 32),
                    yn(r.value),
                    yn(r.stats.min),
                    yn(r.stats.max),
                    yn(r.stats.avg),
                    status_tag
                );
            } else {
                println!(
                    "  {:<32} {:>8.2} {:<3} {:>8.2} {:<3} {:>8.2} {:<3} {:>8.2} {:<3} {}",
                    truncate(&d.name, 32),
                    r.value,
                    d.unit,
                    r.stats.min,
                    d.unit,
                    r.stats.max,
                    d.unit,
                    r.stats.avg,
                    d.unit,
                    status_tag
                );
            }
        }
    }
}

fn print_probes(probes: &[ProbeHealth]) {
    println!(
        "{:<18} {:<28} {:<18} {:>9} {:>6}  Details",
        "Probe ID", "Name", "State", "Poll (µs)", "Errs"
    );
    println!("{}", "-".repeat(96));
    for p in probes {
        println!(
            "{:<18} {:<28} {:<18} {:>9} {:>6}  {}",
            truncate(&p.probe_id, 18),
            truncate(&p.display_name, 28),
            format!("{:?}", p.state),
            p.last_poll_duration_us,
            p.total_errors,
            p.last_error.as_deref().unwrap_or("-")
        );
    }
}

fn print_inventory(nodes: &[InventoryNode]) {
    for n in nodes {
        let indent = if n.parent_id.is_some() { "  " } else { "" };
        println!("{indent}* {} [{:?}] ({})", n.label, n.kind, n.id);
        for (k, v) in &n.properties {
            let val = match v {
                PropertyValue::Text(s) => s.clone(),
                PropertyValue::Integer(i) => i.to_string(),
                PropertyValue::Float(f) => format!("{f:.2}"),
                PropertyValue::Bool(b) => b.to_string(),
                PropertyValue::List(l) => l.join(", "),
            };
            println!("{indent}    {k}: {val}");
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}
