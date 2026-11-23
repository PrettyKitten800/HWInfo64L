use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Grid, Label, Orientation, ScrolledWindow};
use hwinfo64l_protocol::SystemSummary;

/// Classic HWiNFO64-inspired System Summary panel with dense hardware cards.
pub struct SummaryView {
    pub root: ScrolledWindow,
    container: GtkBox,
    last_summary: Option<SystemSummary>,
}

impl SummaryView {
    pub fn new() -> Self {
        let container = GtkBox::new(Orientation::Vertical, 10);
        container.set_margin_top(10);
        container.set_margin_bottom(10);
        container.set_margin_start(12);
        container.set_margin_end(12);

        let root = ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&container)
            .build();

        Self {
            root,
            container,
            last_summary: None,
        }
    }

    pub fn update(&mut self, summary: &SystemSummary) {
        if self.last_summary.as_ref() == Some(summary) {
            return;
        }
        self.last_summary = Some(summary.clone());

        while let Some(child) = self.container.first_child() {
            self.container.remove(&child);
        }

        // Row 1: CPU + Motherboard side by side
        let top_row = GtkBox::new(Orientation::Horizontal, 10);
        top_row.set_homogeneous(true);

        let mut cpu_rows = vec![
            ("Processor", summary.cpu.model_name.clone()),
            (
                "Topology",
                format!(
                    "{} Physical Cores / {} Logical CPUs",
                    summary.cpu.physical_cores, summary.cpu.logical_cpus
                ),
            ),
        ];
        if let (Some(min), Some(max)) = (summary.cpu.min_mhz, summary.cpu.max_mhz) {
            cpu_rows.push(("Clock Range", format!("{min:.0} MHz – {max:.0} MHz")));
        }
        if summary.cpu.pl1_watts.is_some() || summary.cpu.pl2_watts.is_some() {
            cpu_rows.push((
                "Power Limits",
                format!(
                    "PL1: {}  |  PL2: {}",
                    summary
                        .cpu
                        .pl1_watts
                        .map(|w| format!("{w:.1} W"))
                        .unwrap_or_else(|| "N/A".into()),
                    summary
                        .cpu
                        .pl2_watts
                        .map(|w| format!("{w:.1} W"))
                        .unwrap_or_else(|| "N/A".into())
                ),
            ));
        }
        if let Some(ref c) = summary.cpu.l1_cache {
            cpu_rows.push(("L1 Cache", c.clone()));
        }
        if let Some(ref c) = summary.cpu.l2_cache {
            cpu_rows.push(("L2 Cache", c.clone()));
        }
        if let Some(ref c) = summary.cpu.l3_cache {
            cpu_rows.push(("L3 Cache", c.clone()));
        }
        top_row.append(&build_card("Central Processor (CPU)", &cpu_rows));

        let mb_rows = vec![
            ("Hostname", summary.hostname.clone()),
            ("OS", summary.os.pretty_name.clone()),
            (
                "Kernel",
                format!("{} ({})", summary.os.kernel, summary.os.arch),
            ),
            (
                "Board",
                format!(
                    "{} {}",
                    summary.motherboard.board_vendor, summary.motherboard.board_name
                )
                .trim()
                .to_string(),
            ),
            (
                "System",
                format!(
                    "{} {}",
                    summary.motherboard.sys_vendor, summary.motherboard.product_name
                )
                .trim()
                .to_string(),
            ),
            (
                "BIOS",
                format!(
                    "{} {} ({})",
                    summary.motherboard.bios_vendor,
                    summary.motherboard.bios_version,
                    summary.motherboard.bios_date
                )
                .trim()
                .to_string(),
            ),
        ];
        top_row.append(&build_card("Motherboard & Platform", &mb_rows));
        self.container.append(&top_row);

        // Row 2: Memory Card
        let phys_gib = (summary.memory.total_physical_bytes as f64) / (1024.0 * 1024.0 * 1024.0);
        let usable_gib = (summary.memory.usable_bytes as f64) / (1024.0 * 1024.0 * 1024.0);
        let swap_gib = (summary.memory.swap_total_bytes as f64) / (1024.0 * 1024.0 * 1024.0);

        let mut mem_rows = vec![
            (
                "Capacity",
                format!(
                    "{phys_gib:.1} GiB Physical ({usable_gib:.2} GiB Usable) | Swap: {swap_gib:.1} GiB"
                ),
            ),
            (
                "Configuration",
                format!(
                    "{} @ {} MT/s (Rated: {} MT/s, {} channel(s))",
                    summary.memory.mem_type.as_deref().unwrap_or("DRAM"),
                    summary
                        .memory
                        .configured_speed_mts
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "?".into()),
                    summary
                        .memory
                        .rated_speed_mts
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "?".into()),
                    summary.memory.channels_populated
                ),
            ),
            (
                "Zswap",
                if summary.memory.zswap_enabled {
                    format!(
                        "Enabled ({})",
                        summary
                            .memory
                            .zswap_compressor
                            .as_deref()
                            .unwrap_or("default")
                    )
                } else {
                    "Disabled".into()
                },
            ),
        ];

        for m in &summary.memory.modules {
            let gb = (m.size_bytes as f64) / (1024.0 * 1024.0 * 1024.0);
            let volt = m
                .voltage_mv
                .map(|mv| format!("{:.2} V", f64::from(mv) / 1000.0))
                .unwrap_or_else(|| "?".into());
            mem_rows.push((
                "Module",
                format!(
                    "{}: {:.0} GB {} — {} ({}, {}, {})",
                    m.locator, gb, m.mem_type, m.manufacturer, m.part_number, m.form_factor, volt
                ),
            ));
        }
        self.container.append(&build_card("Memory Subsystem", &mem_rows));

        // Row 3: GPUs + Storage side by side
        let bottom_row = GtkBox::new(Orientation::Horizontal, 10);
        bottom_row.set_homogeneous(true);

        let gpu_rows: Vec<(&str, String)> = if summary.gpus.is_empty() {
            vec![("GPU", "None detected".into())]
        } else {
            summary
                .gpus
                .iter()
                .map(|g| {
                    (
                        "GPU",
                        format!(
                            "{} [driver: {}, slot: {}]",
                            g.name,
                            g.driver,
                            g.pci_slot.as_deref().unwrap_or("-")
                        ),
                    )
                })
                .collect()
        };
        bottom_row.append(&build_card("Graphics (GPU)", &gpu_rows));

        let drive_rows: Vec<(&str, String)> = if summary.drives.is_empty() {
            vec![("Drive", "None detected".into())]
        } else {
            summary
                .drives
                .iter()
                .map(|d| {
                    let gb = (d.size_bytes as f64) / 1e9;
                    (
                        "Drive",
                        format!(
                            "/dev/{} — {} ({:.1} GB {}, SMART: {})",
                            d.device,
                            d.model,
                            gb,
                            d.transport,
                            d.smart_health.as_deref().unwrap_or("N/A")
                        ),
                    )
                })
                .collect()
        };
        bottom_row.append(&build_card("Storage Drives", &drive_rows));
        self.container.append(&bottom_row);
    }
}

fn build_card(title: &str, rows: &[(&str, String)]) -> GtkBox {
    let card = GtkBox::new(Orientation::Vertical, 4);
    card.add_css_class("summary-card");

    let heading = Label::new(Some(title));
    heading.add_css_class("card-title");
    heading.set_xalign(0.0);
    card.append(&heading);

    let grid = Grid::new();
    grid.set_column_spacing(12);
    grid.set_row_spacing(4);

    for (i, (k, v)) in rows.iter().enumerate() {
        let row_idx = i32::try_from(i).unwrap_or(0);
        let key_lbl = Label::new(Some(k));
        key_lbl.add_css_class("prop-key");
        key_lbl.set_xalign(0.0);
        key_lbl.set_valign(gtk4::Align::Start);

        let val_lbl = Label::new(Some(v));
        val_lbl.set_xalign(0.0);
        val_lbl.set_selectable(true);
        val_lbl.set_wrap(true);
        val_lbl.set_hexpand(true);

        grid.attach(&key_lbl, 0, row_idx, 1, 1);
        grid.attach(&val_lbl, 1, row_idx, 1, 1);
    }

    card.append(&grid);
    card
}
