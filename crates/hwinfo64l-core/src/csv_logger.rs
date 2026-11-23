use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use hwinfo64l_protocol::{SensorDescriptor, SensorReading};

/// HWiNFO64-compatible CSV sensor logger.
pub struct CsvLogger {
    writer: BufWriter<File>,
    columns: Vec<String>,
}

impl CsvLogger {
    pub fn create(path: &Path, descriptors: &[SensorDescriptor]) -> std::io::Result<Self> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);

        let mut columns = Vec::with_capacity(descriptors.len());
        write!(writer, "Timestamp_ms")?;
        for desc in descriptors {
            columns.push(desc.id.clone());
            let header = format!("{} - {} [{}]", desc.group, desc.name, desc.unit);
            write!(writer, ",{}", csv_escape(&header))?;
        }
        writeln!(writer)?;
        writer.flush()?;

        Ok(Self { writer, columns })
    }

    pub fn write_row(
        &mut self,
        timestamp_ms: u64,
        readings: &[SensorReading],
    ) -> std::io::Result<()> {
        let by_id: BTreeMap<&str, f64> = readings
            .iter()
            .map(|r| (r.id.as_str(), r.value))
            .collect();

        write!(self.writer, "{timestamp_ms}")?;
        for col in &self.columns {
            match by_id.get(col.as_str()) {
                Some(v) => write!(self.writer, ",{v:.3}")?,
                None => write!(self.writer, ",")?,
            }
        }
        writeln!(self.writer)?;
        self.writer.flush()
    }
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}
