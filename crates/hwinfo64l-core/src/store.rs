use std::collections::{BTreeMap, VecDeque};

use hwinfo64l_protocol::{
    HistorySample, InventoryNode, PollTier, ProbeHealth, SensorDescriptor, SensorReading,
    SensorStats, SensorStatus, SystemSummary,
};

use crate::probe::{now_epoch_ms, ProbeOutput, RawSample, SummaryPatch};

const FAST_STALE_MS: u64 = 15_000;
const SLOW_STALE_MS: u64 = 90_000;

/// Configuration for the bounded in-memory telemetry store.
#[derive(Debug, Clone)]
pub struct StoreConfig {
    /// Maximum ring-buffer samples retained per sensor (default: 3600 = 1 hr @ 1 Hz).
    pub history_capacity: usize,
    /// Maximum distinct sensor descriptors retained (prevents cardinality exhaustion).
    pub max_sensors: usize,
    /// Maximum distinct hardware inventory nodes retained.
    pub max_inventory_nodes: usize,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            history_capacity: 3600,
            max_sensors: 2048,
            max_inventory_nodes: 1024,
        }
    }
}

#[derive(Debug, Clone)]
struct SensorSeries {
    descriptor: SensorDescriptor,
    latest: Option<SensorReading>,
    mean: f64,
    samples: u64,
    min: f64,
    max: f64,
    history: VecDeque<HistorySample>,
}

/// Hard magnitude bound to prevent floating-point overflow in downstream stats/JSON.
const MAX_ABS_SENSOR_VALUE: f64 = 1e100;

impl SensorSeries {
    fn new(descriptor: SensorDescriptor, capacity: usize) -> Self {
        Self {
            descriptor,
            latest: None,
            mean: 0.0,
            samples: 0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            history: VecDeque::with_capacity(capacity.min(256)),
        }
    }

    fn push(&mut self, sample: RawSample, capacity: usize) {
        if !sample.value.is_finite() || sample.value.abs() > MAX_ABS_SENSOR_VALUE {
            return;
        }

        if self.samples == 0 {
            self.min = sample.value;
            self.max = sample.value;
            self.mean = sample.value;
            self.samples = 1;
        } else {
            if sample.value < self.min {
                self.min = sample.value;
            }
            if sample.value > self.max {
                self.max = sample.value;
            }
            self.samples = self.samples.saturating_add(1);
            // Welford's online running mean — immune to sum overflow even after decades.
            self.mean += (sample.value - self.mean) / (self.samples as f64);
        }

        let avg = self.mean;
        let status = sample
            .status_override
            .unwrap_or_else(|| self.evaluate_status(sample.value));

        self.latest = Some(SensorReading {
            id: sample.id,
            timestamp_ms: sample.timestamp_ms,
            value: sample.value,
            status,
            stats: SensorStats {
                min: self.min,
                max: self.max,
                avg,
                samples: self.samples,
            },
        });

        if capacity > 0 {
            if self.history.len() >= capacity {
                self.history.pop_front();
            }
            self.history.push_back(HistorySample {
                timestamp_ms: sample.timestamp_ms,
                value: sample.value,
            });
        }
    }

    fn evaluate_status(&self, value: f64) -> SensorStatus {
        if let Some(crit) = self.descriptor.crit_threshold {
            if value >= crit {
                return SensorStatus::Critical;
            }
        }
        if let Some(warn) = self.descriptor.warn_threshold {
            if value >= warn {
                return SensorStatus::Warning;
            }
        }
        SensorStatus::Ok
    }

    fn reset_stats(&mut self) {
        if let Some(ref mut latest) = self.latest {
            let v = latest.value;
            self.min = v;
            self.max = v;
            self.mean = v;
            self.samples = 1;
            latest.stats = SensorStats {
                min: v,
                max: v,
                avg: v,
                samples: 1,
            };
        } else {
            self.min = f64::INFINITY;
            self.max = f64::NEG_INFINITY;
            self.mean = 0.0;
            self.samples = 0;
        }
    }
}

/// Thread-safe (when wrapped in `RwLock`) bounded telemetry and inventory store.
#[derive(Debug)]
pub struct SensorStore {
    config: StoreConfig,
    series: BTreeMap<String, SensorSeries>,
    inventory: BTreeMap<String, InventoryNode>,
    probes: BTreeMap<String, ProbeHealth>,
    summary: SystemSummary,
}

impl SensorStore {
    pub fn new(config: StoreConfig) -> Self {
        Self {
            config,
            series: BTreeMap::new(),
            inventory: BTreeMap::new(),
            probes: BTreeMap::new(),
            summary: SystemSummary::default(),
        }
    }

    /// Ingest a full probe output batch atomically.
    pub fn ingest(&mut self, output: ProbeOutput) {
        for desc in output.descriptors {
            let id = desc.id.clone();
            if let Some(existing) = self.series.get_mut(&id) {
                existing.descriptor = desc;
            } else if self.series.len() < self.config.max_sensors {
                let s = SensorSeries::new(desc, self.config.history_capacity);
                self.series.insert(id, s);
            }
        }

        for sample in output.samples {
            if let Some(s) = self.series.get_mut(&sample.id) {
                s.push(sample, self.config.history_capacity);
            }
        }

        if let Some(ref source) = output.replace_source_inventory {
            self.inventory.retain(|_, node| node.source != *source);
        }

        for node in output.inventory {
            if self.inventory.contains_key(&node.id)
                || self.inventory.len() < self.config.max_inventory_nodes
            {
                self.inventory.insert(node.id.clone(), node);
            }
        }

        if let Some(patch) = output.summary_patch {
            self.apply_summary_patch(patch);
        }
    }

    pub fn update_probe_health(&mut self, health: ProbeHealth) {
        self.probes.insert(health.probe_id.clone(), health);
    }

    pub fn descriptors(&self) -> Vec<SensorDescriptor> {
        self.series.values().map(|s| s.descriptor.clone()).collect()
    }

    pub fn readings(&self, filter_ids: Option<&[String]>) -> Vec<SensorReading> {
        let now = now_epoch_ms();
        let format_reading = |s: &SensorSeries| -> Option<SensorReading> {
            let mut r = s.latest.clone()?;
            let max_age_ms = match s.descriptor.poll_tier {
                PollTier::Fast => Some(FAST_STALE_MS),
                PollTier::Slow => Some(SLOW_STALE_MS),
                PollTier::Static => None,
            };
            if let Some(limit) = max_age_ms {
                if now.saturating_sub(r.timestamp_ms) > limit {
                    r.status = SensorStatus::Stale;
                }
            }
            Some(r)
        };

        match filter_ids {
            Some(ids) => ids
                .iter()
                .filter_map(|id| self.series.get(id).and_then(format_reading))
                .collect(),
            None => self.series.values().filter_map(format_reading).collect(),
        }
    }

    pub fn probes(&self) -> Vec<ProbeHealth> {
        self.probes.values().cloned().collect()
    }

    pub fn inventory(&self) -> Vec<InventoryNode> {
        self.inventory.values().cloned().collect()
    }

    pub fn summary(&self) -> SystemSummary {
        self.summary.clone()
    }

    pub fn history(&self, id: &str, limit: usize) -> Vec<HistorySample> {
        let Some(s) = self.series.get(id) else {
            return Vec::new();
        };
        if limit == 0 || limit >= s.history.len() {
            return s.history.iter().copied().collect();
        }
        let skip = s.history.len().saturating_sub(limit);
        s.history.iter().skip(skip).copied().collect()
    }

    pub fn reset_stats(&mut self, filter_ids: Option<&[String]>) -> usize {
        match filter_ids {
            Some(ids) => {
                let mut count = 0usize;
                for id in ids {
                    if let Some(s) = self.series.get_mut(id) {
                        s.reset_stats();
                        count += 1;
                    }
                }
                count
            }
            None => {
                let count = self.series.len();
                for s in self.series.values_mut() {
                    s.reset_stats();
                }
                count
            }
        }
    }

    fn apply_summary_patch(&mut self, patch: SummaryPatch) {
        match patch {
            SummaryPatch::Full(summary) => {
                let mut incoming = *summary;
                // Preserve fields already populated by other probes (probe completion order is concurrent).
                if incoming.cpu.pl1_watts.is_none() {
                    incoming.cpu.pl1_watts = self.summary.cpu.pl1_watts;
                }
                if incoming.cpu.pl2_watts.is_none() {
                    incoming.cpu.pl2_watts = self.summary.cpu.pl2_watts;
                }
                if incoming.memory.total_physical_bytes == 0
                    && self.summary.memory.total_physical_bytes > 0
                {
                    incoming.memory = self.summary.memory.clone();
                }
                for d in &mut incoming.drives {
                    if d.smart_health.is_none() {
                        if let Some(prev) = self
                            .summary
                            .drives
                            .iter()
                            .find(|old| old.device == d.device)
                        {
                            d.smart_health = prev.smart_health.clone();
                        }
                    }
                }
                self.summary = incoming;
            }
            SummaryPatch::Hostname(hostname) => self.summary.hostname = hostname,
            SummaryPatch::Os(os) => self.summary.os = os,
            SummaryPatch::Cpu(mut cpu) => {
                if cpu.pl1_watts.is_none() {
                    cpu.pl1_watts = self.summary.cpu.pl1_watts;
                }
                if cpu.pl2_watts.is_none() {
                    cpu.pl2_watts = self.summary.cpu.pl2_watts;
                }
                self.summary.cpu = *cpu;
            }
            SummaryPatch::CpuPowerLimits {
                pl1_watts,
                pl2_watts,
            } => {
                if pl1_watts.is_some() {
                    self.summary.cpu.pl1_watts = pl1_watts;
                }
                if pl2_watts.is_some() {
                    self.summary.cpu.pl2_watts = pl2_watts;
                }
            }
            SummaryPatch::Motherboard(mb) => self.summary.motherboard = *mb,
            SummaryPatch::MemoryRuntime {
                usable_bytes,
                swap_total_bytes,
                zswap_enabled,
                zswap_compressor,
            } => {
                self.summary.memory.usable_bytes = usable_bytes;
                self.summary.memory.swap_total_bytes = swap_total_bytes;
                self.summary.memory.zswap_enabled = zswap_enabled;
                self.summary.memory.zswap_compressor = zswap_compressor;
                if self.summary.memory.total_physical_bytes == 0 {
                    self.summary.memory.total_physical_bytes = usable_bytes;
                }
            }
            SummaryPatch::MemoryDmi {
                total_physical_bytes,
                max_capacity_bytes,
                mem_type,
                configured_speed_mts,
                rated_speed_mts,
                channels_populated,
                modules,
            } => {
                if total_physical_bytes > 0 {
                    self.summary.memory.total_physical_bytes = total_physical_bytes;
                }
                self.summary.memory.max_capacity_bytes = max_capacity_bytes;
                self.summary.memory.mem_type = mem_type;
                self.summary.memory.configured_speed_mts = configured_speed_mts;
                self.summary.memory.rated_speed_mts = rated_speed_mts;
                self.summary.memory.channels_populated = channels_populated;
                self.summary.memory.modules = modules;
            }
            SummaryPatch::Gpus(gpus) => self.summary.gpus = gpus,
            SummaryPatch::Drives(mut drives) => {
                // Preserve any previously learned SMART health states.
                for d in &mut drives {
                    if d.smart_health.is_none() {
                        if let Some(prev) = self
                            .summary
                            .drives
                            .iter()
                            .find(|old| old.device == d.device)
                        {
                            d.smart_health = prev.smart_health.clone();
                        }
                    }
                }
                self.summary.drives = drives;
            }
            SummaryPatch::DriveSmartHealth { device, health } => {
                if let Some(d) = self
                    .summary
                    .drives
                    .iter_mut()
                    .find(|d| d.device == device)
                {
                    d.smart_health = Some(health);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hwinfo64l_protocol::{PollTier, SensorKind};

    fn test_desc(id: &str) -> SensorDescriptor {
        SensorDescriptor {
            id: id.to_string(),
            group: "Test".to_string(),
            name: "Temp".to_string(),
            kind: SensorKind::Temperature,
            unit: "°C".to_string(),
            min_expected: Some(0.0),
            max_expected: Some(105.0),
            warn_threshold: Some(85.0),
            crit_threshold: Some(100.0),
            poll_tier: PollTier::Fast,
            source: "test".to_string(),
        }
    }

    #[test]
    fn ring_buffer_and_stats_are_bounded_and_accurate() {
        let mut store = SensorStore::new(StoreConfig {
            history_capacity: 3,
            ..StoreConfig::default()
        });
        let mut out = ProbeOutput::default();
        out.descriptors.push(test_desc("test.dev.temp.a"));
        let base_ts = now_epoch_ms();
        for (i, v) in [50.0, 60.0, 90.0, 102.0].into_iter().enumerate() {
            out.samples
                .push(RawSample::new("test.dev.temp.a", v, base_ts + (i as u64)));
        }
        store.ingest(out);

        let readings = store.readings(None);
        assert_eq!(readings.len(), 1);
        let r = &readings[0];
        assert_eq!(r.value, 102.0);
        assert_eq!(r.status, SensorStatus::Critical);
        assert_eq!(r.stats.min, 50.0);
        assert_eq!(r.stats.max, 102.0);
        assert_eq!(r.stats.samples, 4);
        assert!((r.stats.avg - 75.5).abs() < 1e-6);

        let hist = store.history("test.dev.temp.a", 10);
        assert_eq!(hist.len(), 3);
        assert_eq!(hist[0].value, 60.0);
        assert_eq!(hist[2].value, 102.0);

        let reset = store.reset_stats(None);
        assert_eq!(reset, 1);
        let r2 = &store.readings(None)[0];
        assert_eq!(r2.stats.min, 102.0);
        assert_eq!(r2.stats.max, 102.0);
        assert_eq!(r2.stats.samples, 1);
    }

    #[test]
    fn cardinality_cap_bounds_sensor_and_inventory_growth() {
        let mut store = SensorStore::new(StoreConfig {
            history_capacity: 10,
            max_sensors: 2,
            max_inventory_nodes: 2,
        });
        let mut out = ProbeOutput::default();
        for i in 0..10 {
            out.descriptors.push(test_desc(&format!("test.dev.temp.s{i}")));
            out.samples
                .push(RawSample::new(format!("test.dev.temp.s{i}"), i as f64, 1000));
            out.inventory.push(InventoryNode {
                id: format!("node_{i}"),
                parent_id: None,
                kind: hwinfo64l_protocol::InventoryKind::Plugin,
                label: format!("Node {i}"),
                properties: BTreeMap::new(),
                source: "test".into(),
            });
        }
        store.ingest(out);
        assert_eq!(store.descriptors().len(), 2);
        assert_eq!(store.readings(None).len(), 2);
        assert_eq!(store.inventory().len(), 2);
    }

    #[test]
    fn extreme_values_never_overflow_mean_to_infinity() {
        let mut store = SensorStore::new(StoreConfig::default());
        let mut out = ProbeOutput::default();
        out.descriptors.push(test_desc("test.dev.temp.x"));
        out.samples.push(RawSample::new("test.dev.temp.x", 1e308, 1));
        out.samples.push(RawSample::new("test.dev.temp.x", 1e308, 2));
        out.samples.push(RawSample::new("test.dev.temp.x", 1e99, 3));
        out.samples.push(RawSample::new("test.dev.temp.x", 1e99, 4));
        store.ingest(out);

        let readings = store.readings(None);
        assert_eq!(readings.len(), 1);
        assert!(readings[0].value.is_finite());
        assert!(readings[0].stats.avg.is_finite());
        assert_eq!(readings[0].stats.samples, 2);
    }

    #[test]
    fn old_fast_readings_age_to_stale() {
        let mut store = SensorStore::new(StoreConfig::default());
        let mut out = ProbeOutput::default();
        out.descriptors.push(test_desc("test.dev.temp.old"));
        out.samples.push(RawSample::new("test.dev.temp.old", 45.0, 1_000));
        store.ingest(out);

        let readings = store.readings(None);
        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].status, SensorStatus::Stale);
    }
}
