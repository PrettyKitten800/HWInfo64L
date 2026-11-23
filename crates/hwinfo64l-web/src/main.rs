use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use hwinfo64l_core::{
    discover_plugin_executables, CoreEngine, NdjsonPluginProbe, SupervisorConfig,
};
use hwinfo64l_protocol::{
    InventoryNode, JsonRpcRequest, JsonRpcResponse, ListSensorsResponse, DEFAULT_SOCKET_PATH,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const INDEX_HTML: &str = include_str!("index.html");

#[derive(Debug, Parser)]
#[command(
    name = "hwinfo64l-web",
    version,
    about = "Lightweight LAN Web Dashboard & Prometheus exporter for HWInfo64L"
)]
struct Args {
    /// Listen address (e.g. 0.0.0.0:8640 or 127.0.0.1:8640).
    #[arg(long, default_value = "0.0.0.0:8640")]
    bind: SocketAddr,

    /// Path to hwinfo64ld Unix domain socket.
    #[arg(long, env = "HWINFO64L_SOCKET", default_value = DEFAULT_SOCKET_PATH)]
    socket: PathBuf,

    /// Optional directory of unprivileged NDJSON plugins to merge alongside hwinfo64ld.
    #[arg(long, env = "HWINFO64L_PLUGIN_DIR")]
    plugin_dir: Option<PathBuf>,
}

struct AppState {
    socket: PathBuf,
    fallback: Arc<tokio::sync::Mutex<Option<Arc<CoreEngine>>>>,
    plugins: Arc<CoreEngine>,
}

impl AppState {
    async fn daemon_rpc(&self, method: &str, params: Value) -> Option<Value> {
        let stream = UnixStream::connect(&self.socket).await.ok()?;
        let (r, mut w) = stream.into_split();
        let req = JsonRpcRequest::new(1, method, params);
        let mut line = serde_json::to_string(&req).ok()?;
        line.push('\n');
        w.write_all(line.as_bytes()).await.ok()?;
        w.flush().await.ok()?;

        let mut reader = BufReader::new(r);
        let mut buf = String::new();
        let n = reader.read_line(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        let resp: JsonRpcResponse = serde_json::from_str(buf.trim()).ok()?;
        if resp.error.is_some() {
            return None;
        }
        resp.result
    }

    async fn get_fallback(&self) -> Arc<CoreEngine> {
        let mut guard = self.fallback.lock().await;
        if let Some(ref eng) = *guard {
            return Arc::clone(eng);
        }
        let eng = CoreEngine::new(SupervisorConfig::default());
        for probe in hwinfo64l_probes::default_probes() {
            eng.register_probe(probe);
        }
        *guard = Some(Arc::clone(&eng));
        eng
    }

    async fn query_sensors(&self) -> ListSensorsResponse {
        let mut resp = if let Some(v) = self.daemon_rpc("v1.list_sensors", json!({})).await {
            serde_json::from_value::<ListSensorsResponse>(v).unwrap_or_else(|_| {
                ListSensorsResponse {
                    descriptors: Vec::new(),
                    readings: Vec::new(),
                    probes: Vec::new(),
                }
            })
        } else {
            let eng = self.get_fallback().await;
            ListSensorsResponse {
                descriptors: eng.descriptors(),
                readings: eng.readings(None),
                probes: eng.probes(),
            }
        };

        for d in self.plugins.descriptors() {
            if !resp.descriptors.iter().any(|x| x.id == d.id) {
                resp.descriptors.push(d);
            }
        }
        for r in self.plugins.readings(None) {
            if !resp.readings.iter().any(|x| x.id == r.id) {
                resp.readings.push(r);
            }
        }
        resp.probes.extend(self.plugins.probes());
        resp
    }

    async fn query_summary_json(&self) -> String {
        if let Some(v) = self.daemon_rpc("v1.get_summary", json!({})).await {
            if let Ok(s) = serde_json::to_string(&v) {
                return s;
            }
        }
        let eng = self.get_fallback().await;
        serde_json::to_string(&eng.summary()).unwrap_or_else(|_| "{}".into())
    }

    async fn query_inventory_json(&self) -> String {
        let mut nodes: Vec<InventoryNode> = if let Some(v) = self.daemon_rpc("v1.get_inventory", json!({})).await {
            serde_json::from_value(v).unwrap_or_default()
        } else {
            let eng = self.get_fallback().await;
            eng.inventory()
        };
        for n in self.plugins.inventory() {
            if !nodes.iter().any(|x| x.id == n.id) {
                nodes.push(n);
            }
        }
        serde_json::to_string(&nodes).unwrap_or_else(|_| "[]".into())
    }

    async fn reset_stats(&self) -> String {
        let plugin_resets = self.plugins.reset_stats(None);
        if let Some(v) = self.daemon_rpc("v1.reset_stats", json!({})).await {
            if let Ok(s) = serde_json::to_string(&v) {
                return s;
            }
        }
        let eng = self.get_fallback().await;
        let count = eng.reset_stats(None) + plugin_resets;
        format!("{{\"reset_count\":{count}}}")
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    if let Err(e) = run(args).await {
        error!(error = %e, "hwinfo64l-web failed");
        std::process::exit(1);
    }
}

async fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(args.bind).await?;
    let plugins = CoreEngine::new(SupervisorConfig::default());
    if let Some(ref dir) = args.plugin_dir {
        for exec in discover_plugin_executables(dir) {
            info!(plugin = %exec.display(), "loading web plugin");
            plugins.register_probe(Box::new(NdjsonPluginProbe::new(exec)));
        }
    }
    let state = Arc::new(AppState {
        socket: args.socket,
        fallback: Arc::new(tokio::sync::Mutex::new(None)),
        plugins,
    });

    info!(bind = %args.bind, "hwinfo64l-web listening (dashboard at / and Prometheus at /metrics)");
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            accept = listener.accept() => {
                match accept {
                    Ok((stream, _)) => {
                        let st = Arc::clone(&state);
                        tokio::spawn(async move {
                            if let Err(e) = handle_http(stream, st).await {
                                warn!(error = %e, "http connection error");
                            }
                        });
                    }
                    Err(e) => warn!(error = %e, "tcp accept failed"),
                }
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
        }
    }
    Ok(())
}

async fn handle_http(mut stream: TcpStream, state: Arc<AppState>) -> std::io::Result<()> {
    let mut reader = BufReader::new(&mut stream);
    let mut req_line = String::new();
    if reader.read_line(&mut req_line).await? == 0 {
        return Ok(());
    }
    // Drain headers
    let mut hdr = String::new();
    loop {
        hdr.clear();
        let n = reader.read_line(&mut hdr).await?;
        if n == 0 || hdr == "\r\n" || hdr == "\n" {
            break;
        }
    }

    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let raw_target = parts.next().unwrap_or("/");
    let path = raw_target.split('?').next().unwrap_or("/");

    let (status, content_type, body) = match (method, path) {
        ("GET", "/") | ("GET", "/index.html") => {
            ("200 OK", "text/html; charset=utf-8", INDEX_HTML.to_string())
        }
        ("GET", "/api/v1/sensors") => {
            let data = state.query_sensors().await;
            let json = serde_json::to_string(&data).unwrap_or_else(|_| "{}".into());
            ("200 OK", "application/json", json)
        }
        ("GET", "/api/v1/summary") => {
            ("200 OK", "application/json", state.query_summary_json().await)
        }
        ("GET", "/api/v1/inventory") => (
            "200 OK",
            "application/json",
            state.query_inventory_json().await,
        ),
        ("POST", "/api/v1/reset") => ("200 OK", "application/json", state.reset_stats().await),
        ("GET", "/metrics") => {
            let data = state.query_sensors().await;
            let prom = format_prometheus(&data);
            ("200 OK", "text/plain; version=0.0.4; charset=utf-8", prom)
        }
        _ => (
            "404 Not Found",
            "application/json",
            "{\"error\":\"not found\"}".into(),
        ),
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}

fn format_prometheus(data: &ListSensorsResponse) -> String {
    let mut out = String::new();
    out.push_str("# HELP hwinfo64l_sensor_value Current hardware sensor reading\n");
    out.push_str("# TYPE hwinfo64l_sensor_value gauge\n");

    let by_id: BTreeMap<&str, _> = data
        .descriptors
        .iter()
        .map(|d| (d.id.as_str(), d))
        .collect();

    for r in &data.readings {
        if let Some(d) = by_id.get(r.id.as_str()) {
            out.push_str(&format!(
                "hwinfo64l_sensor_value{{id=\"{}\",group=\"{}\",name=\"{}\",unit=\"{}\"}} {}\n",
                prom_escape(&d.id),
                prom_escape(&d.group),
                prom_escape(&d.name),
                prom_escape(&d.unit),
                r.value
            ));
        }
    }

    out.push_str("# HELP hwinfo64l_probe_poll_duration_microseconds Last probe poll duration\n");
    out.push_str("# TYPE hwinfo64l_probe_poll_duration_microseconds gauge\n");
    for p in &data.probes {
        out.push_str(&format!(
            "hwinfo64l_probe_poll_duration_microseconds{{probe=\"{}\",state=\"{:?}\"}} {}\n",
            prom_escape(&p.probe_id),
            p.state,
            p.last_poll_duration_us
        ));
    }
    out
}

fn prom_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', " ")
}
