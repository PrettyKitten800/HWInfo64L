use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use hwinfo64l_protocol::{
    HistorySample, InventoryNode, PollTier, ProbeHealth, ProbeState, SensorDescriptor,
    SensorReading, SystemSummary,
};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::probe::{now_epoch_ms, Probe, ProbeError};
use crate::store::{SensorStore, StoreConfig};

/// Supervisor resilience configuration.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub store: StoreConfig,
    /// Consecutive failures before tripping the circuit breaker for a probe.
    pub circuit_breaker_threshold: u32,
    /// Backoff schedule (in seconds) once a circuit opens.
    pub backoff_secs: Vec<u64>,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            store: StoreConfig::default(),
            circuit_breaker_threshold: 3,
            backoff_secs: vec![5, 15, 60, 300],
        }
    }
}

/// Core runtime engine that supervises probes, maintains bounded history/stats,
/// and broadcasts sensor updates to subscribers.
pub struct CoreEngine {
    store: Arc<RwLock<SensorStore>>,
    config: SupervisorConfig,
    started_at: Instant,
    shutdown_tx: watch::Sender<bool>,
    update_tx: broadcast::Sender<u64>,
    handles: Mutex<Vec<JoinHandle<()>>>,
}

impl CoreEngine {
    pub fn new(config: SupervisorConfig) -> Arc<Self> {
        let (shutdown_tx, _) = watch::channel(false);
        let (update_tx, _) = broadcast::channel(64);
        Arc::new(Self {
            store: Arc::new(RwLock::new(SensorStore::new(config.store.clone()))),
            config,
            started_at: Instant::now(),
            shutdown_tx,
            update_tx,
            handles: Mutex::new(Vec::new()),
        })
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    pub fn subscribe_ticks(&self) -> broadcast::Receiver<u64> {
        self.update_tx.subscribe()
    }

    /// Spawn a supervised task for a single probe.
    pub fn register_probe(self: &Arc<Self>, probe: Box<dyn Probe>) {
        let engine = Arc::clone(self);
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        let handle = tokio::spawn(async move {
            let probe_id = probe.id().to_string();
            let display_name = probe.display_name().to_string();
            let poll_tier = probe.poll_tier();
            let interval = probe.poll_interval();
            let timeout = probe.timeout();

            let probe_cell = Arc::new(Mutex::new(probe));
            let mut health = ProbeHealth {
                probe_id: probe_id.clone(),
                display_name,
                state: ProbeState::Healthy,
                poll_tier,
                last_ok_ms: None,
                last_poll_duration_us: 0,
                consecutive_errors: 0,
                total_errors: 0,
                last_error: None,
            };
            let mut backoff_idx = 0usize;

            loop {
                if *shutdown_rx.borrow() {
                    break;
                }

                let start = Instant::now();
                let poll_cell = Arc::clone(&probe_cell);
                let poll_task = tokio::task::spawn_blocking(move || {
                    let result = catch_unwind(AssertUnwindSafe(move || {
                        let Ok(mut guard) = poll_cell.lock() else {
                            return Err(ProbeError::Transient(
                                "probe mutex poisoned".to_string(),
                            ));
                        };
                        guard.poll()
                    }));
                    match result {
                        Ok(inner) => inner,
                        Err(_) => Err(ProbeError::Transient("probe panicked".to_string())),
                    }
                });

                let outcome = match tokio::time::timeout(timeout, poll_task).await {
                    Ok(Ok(res)) => res,
                    Ok(Err(join_err)) => Err(ProbeError::Transient(format!(
                        "worker join error: {join_err}"
                    ))),
                    Err(_) => Err(ProbeError::Timeout(timeout)),
                };

                let elapsed_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
                health.last_poll_duration_us = elapsed_us;

                let sleep_duration = match outcome {
                    Ok(output) => {
                        let ts = now_epoch_ms();
                        health.state = ProbeState::Healthy;
                        health.last_ok_ms = Some(ts);
                        health.consecutive_errors = 0;
                        health.last_error = None;
                        backoff_idx = 0;

                        if let Ok(mut store) = engine.store.write() {
                            store.ingest(output);
                            store.update_probe_health(health.clone());
                        }
                        let _ = engine.update_tx.send(ts);

                        if poll_tier == PollTier::Static && interval.is_zero() {
                            debug!(probe = %probe_id, "static probe completed");
                            break;
                        }
                        if interval.is_zero() {
                            Duration::from_secs(60)
                        } else {
                            interval
                        }
                    }
                    Err(ProbeError::Unsupported(reason)) => {
                        health.state = ProbeState::Unsupported;
                        health.last_error = Some(reason);
                        if let Ok(mut store) = engine.store.write() {
                            store.update_probe_health(health.clone());
                        }
                        debug!(probe = %probe_id, "probe unsupported on this system");
                        break;
                    }
                    Err(ProbeError::RequiresPrivilege(reason)) => {
                        health.state = ProbeState::RequiresPrivilege;
                        health.last_error = Some(reason);
                        if let Ok(mut store) = engine.store.write() {
                            store.update_probe_health(health.clone());
                        }
                        debug!(probe = %probe_id, "probe requires elevated privileges");
                        break;
                    }
                    Err(err) => {
                        health.consecutive_errors = health.consecutive_errors.saturating_add(1);
                        health.total_errors = health.total_errors.saturating_add(1);
                        health.last_error = Some(err.to_string());

                        if health.consecutive_errors >= engine.config.circuit_breaker_threshold {
                            health.state = ProbeState::CircuitOpen;
                            let secs = engine
                                .config
                                .backoff_secs
                                .get(backoff_idx)
                                .copied()
                                .or_else(|| engine.config.backoff_secs.last().copied())
                                .unwrap_or(60);
                            if backoff_idx + 1 < engine.config.backoff_secs.len() {
                                backoff_idx += 1;
                            }
                            warn!(
                                probe = %probe_id,
                                errors = health.consecutive_errors,
                                backoff_secs = secs,
                                error = %err,
                                "probe circuit opened"
                            );
                            if let Ok(mut store) = engine.store.write() {
                                store.update_probe_health(health.clone());
                            }
                            Duration::from_secs(secs)
                        } else {
                            health.state = ProbeState::Degraded;
                            warn!(probe = %probe_id, error = %err, "probe poll error");
                            if let Ok(mut store) = engine.store.write() {
                                store.update_probe_health(health.clone());
                            }
                            if interval.is_zero() {
                                Duration::from_secs(5)
                            } else {
                                interval
                            }
                        }
                    }
                };

                tokio::select! {
                    _ = tokio::time::sleep(sleep_duration) => {}
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            break;
                        }
                    }
                }
            }
        });

        if let Ok(mut handles) = self.handles.lock() {
            handles.push(handle);
        }
    }

    pub fn descriptors(&self) -> Vec<SensorDescriptor> {
        self.store
            .read()
            .map(|s| s.descriptors())
            .unwrap_or_default()
    }

    pub fn readings(&self, filter_ids: Option<&[String]>) -> Vec<SensorReading> {
        self.store
            .read()
            .map(|s| s.readings(filter_ids))
            .unwrap_or_default()
    }

    pub fn probes(&self) -> Vec<ProbeHealth> {
        self.store.read().map(|s| s.probes()).unwrap_or_default()
    }

    pub fn inventory(&self) -> Vec<InventoryNode> {
        self.store.read().map(|s| s.inventory()).unwrap_or_default()
    }

    pub fn summary(&self) -> SystemSummary {
        self.store.read().map(|s| s.summary()).unwrap_or_default()
    }

    pub fn history(&self, id: &str, limit: usize) -> Vec<HistorySample> {
        self.store
            .read()
            .map(|s| s.history(id, limit))
            .unwrap_or_default()
    }

    pub fn reset_stats(&self, filter_ids: Option<&[String]>) -> usize {
        self.store
            .write()
            .map(|mut s| s.reset_stats(filter_ids))
            .unwrap_or(0)
    }

    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        let handles = self
            .handles
            .lock()
            .map(|mut h| std::mem::take(&mut *h))
            .unwrap_or_default();
        for h in handles {
            let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{ProbeOutput, RawSample};
    use hwinfo64l_protocol::SensorKind;

    struct HealthyProbe;
    impl Probe for HealthyProbe {
        fn id(&self) -> &str {
            "healthy"
        }
        fn display_name(&self) -> &str {
            "Healthy"
        }
        fn poll_tier(&self) -> PollTier {
            PollTier::Fast
        }
        fn poll_interval(&self) -> Duration {
            Duration::from_millis(20)
        }
        fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
            let mut out = ProbeOutput::default();
            out.descriptors.push(SensorDescriptor {
                id: "healthy.dev.temp.a".to_string(),
                group: "Healthy".to_string(),
                name: "Temp".to_string(),
                kind: SensorKind::Temperature,
                unit: "°C".to_string(),
                min_expected: None,
                max_expected: None,
                warn_threshold: None,
                crit_threshold: None,
                poll_tier: PollTier::Fast,
                source: "healthy".to_string(),
            });
            out.samples
                .push(RawSample::new("healthy.dev.temp.a", 42.5, now_epoch_ms()));
            Ok(out)
        }
    }

    struct FailingProbe;
    impl Probe for FailingProbe {
        fn id(&self) -> &str {
            "failing"
        }
        fn display_name(&self) -> &str {
            "Failing"
        }
        fn poll_tier(&self) -> PollTier {
            PollTier::Fast
        }
        fn poll_interval(&self) -> Duration {
            Duration::from_millis(10)
        }
        fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
            Err(ProbeError::Transient("boom".to_string()))
        }
    }

    #[tokio::test]
    async fn failing_probe_trips_circuit_without_affecting_healthy_probe() {
        let engine = CoreEngine::new(SupervisorConfig {
            circuit_breaker_threshold: 2,
            backoff_secs: vec![10],
            ..SupervisorConfig::default()
        });
        engine.register_probe(Box::new(HealthyProbe));
        engine.register_probe(Box::new(FailingProbe));

        tokio::time::sleep(Duration::from_millis(120)).await;

        let readings = engine.readings(None);
        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].value, 42.5);

        let probes = engine.probes();
        let failing = probes.iter().find(|p| p.probe_id == "failing");
        assert!(failing.is_some());
        if let Some(f) = failing {
            assert_eq!(f.state, ProbeState::CircuitOpen);
        }

        engine.shutdown().await;
    }

    struct HangingProbe;
    impl Probe for HangingProbe {
        fn id(&self) -> &str {
            "hanging"
        }
        fn display_name(&self) -> &str {
            "Hanging"
        }
        fn poll_tier(&self) -> PollTier {
            PollTier::Fast
        }
        fn poll_interval(&self) -> Duration {
            Duration::from_millis(10)
        }
        fn timeout(&self) -> Duration {
            Duration::from_millis(40)
        }
        fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
            std::thread::sleep(Duration::from_millis(250));
            Ok(ProbeOutput::default())
        }
    }

    #[tokio::test]
    async fn hanging_probe_times_out_without_blocking_healthy_probe() {
        let engine = CoreEngine::new(SupervisorConfig {
            circuit_breaker_threshold: 1,
            backoff_secs: vec![10],
            ..SupervisorConfig::default()
        });
        engine.register_probe(Box::new(HealthyProbe));
        engine.register_probe(Box::new(HangingProbe));

        tokio::time::sleep(Duration::from_millis(120)).await;

        assert_eq!(engine.readings(None).len(), 1);
        let hanging = engine.probes().into_iter().find(|p| p.probe_id == "hanging");
        assert!(hanging.is_some());
        if let Some(h) = hanging {
            assert_eq!(h.state, ProbeState::CircuitOpen);
        }

        engine.shutdown().await;
    }
}
