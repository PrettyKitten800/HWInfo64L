use std::collections::BTreeMap;

use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Grid, Label, Orientation, ScrolledWindow};
use hwinfo64l_protocol::{ProbeHealth, ProbeState};

struct ProbeRowWidgets {
    name_lbl: Label,
    state_lbl: Label,
    tier_lbl: Label,
    us_lbl: Label,
    err_lbl: Label,
    detail_lbl: Label,
}

/// Zero-churn live diagnostics view showing health, poll latency, and errors
/// for every built-in probe and external NDJSON plugin (updates rows in-place).
pub struct ProbesView {
    pub root: GtkBox,
    grid: Grid,
    rows: BTreeMap<String, ProbeRowWidgets>,
    next_row: i32,
}

impl ProbesView {
    pub fn new() -> Self {
        let root = GtkBox::new(Orientation::Vertical, 8);
        root.set_margin_top(10);
        root.set_margin_bottom(10);
        root.set_margin_start(12);
        root.set_margin_end(12);

        let hint = Label::new(Some(
            "Drop-in user plugins: ~/.config/hwinfo64l/plugins/ (unprivileged)   |   System plugins: /usr/lib/hwinfo64l/plugins/",
        ));
        hint.add_css_class("status-muted");
        hint.set_xalign(0.0);
        hint.set_selectable(true);
        root.append(&hint);

        let grid = Grid::new();
        grid.set_column_spacing(14);
        grid.set_row_spacing(5);

        let headers = [
            "Probe ID",
            "Name",
            "State",
            "Tier",
            "Poll (µs)",
            "Errors",
            "Diagnostics",
        ];
        for (col, h) in headers.iter().enumerate() {
            let lbl = Label::new(Some(h));
            lbl.add_css_class("prop-key");
            lbl.set_xalign(0.0);
            grid.attach(&lbl, i32::try_from(col).unwrap_or(0), 0, 1, 1);
        }

        let scrolled = ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&grid)
            .build();
        root.append(&scrolled);

        Self {
            root,
            grid,
            rows: BTreeMap::new(),
            next_row: 1,
        }
    }

    pub fn update(&mut self, probes: &[ProbeHealth]) {
        for p in probes {
            let widgets = self.rows.entry(p.probe_id.clone()).or_insert_with(|| {
                let row = self.next_row;
                self.next_row = self.next_row.saturating_add(1);

                let id_lbl = Label::new(Some(&p.probe_id));
                id_lbl.add_css_class("mono");
                id_lbl.set_xalign(0.0);

                let name_lbl = Label::new(Some(&p.display_name));
                name_lbl.set_xalign(0.0);

                let state_lbl = Label::new(None);
                state_lbl.set_xalign(0.0);

                let tier_lbl = Label::new(None);
                tier_lbl.set_xalign(0.0);

                let us_lbl = Label::new(None);
                us_lbl.add_css_class("mono");
                us_lbl.set_xalign(1.0);

                let err_lbl = Label::new(None);
                err_lbl.add_css_class("mono");
                err_lbl.set_xalign(1.0);

                let detail_lbl = Label::new(None);
                detail_lbl.set_xalign(0.0);
                detail_lbl.set_hexpand(true);
                detail_lbl.set_wrap(true);

                self.grid.attach(&id_lbl, 0, row, 1, 1);
                self.grid.attach(&name_lbl, 1, row, 1, 1);
                self.grid.attach(&state_lbl, 2, row, 1, 1);
                self.grid.attach(&tier_lbl, 3, row, 1, 1);
                self.grid.attach(&us_lbl, 4, row, 1, 1);
                self.grid.attach(&err_lbl, 5, row, 1, 1);
                self.grid.attach(&detail_lbl, 6, row, 1, 1);

                ProbeRowWidgets {
                    name_lbl,
                    state_lbl,
                    tier_lbl,
                    us_lbl,
                    err_lbl,
                    detail_lbl,
                }
            });

            widgets.name_lbl.set_text(&p.display_name);
            widgets.state_lbl.set_text(&format!("{:?}", p.state));
            widgets.state_lbl.remove_css_class("status-ok");
            widgets.state_lbl.remove_css_class("status-warn");
            widgets.state_lbl.remove_css_class("status-crit");
            widgets.state_lbl.remove_css_class("status-muted");
            match p.state {
                ProbeState::Healthy => widgets.state_lbl.add_css_class("status-ok"),
                ProbeState::Degraded | ProbeState::RequiresPrivilege => {
                    widgets.state_lbl.add_css_class("status-warn")
                }
                ProbeState::CircuitOpen => widgets.state_lbl.add_css_class("status-crit"),
                ProbeState::Unsupported => widgets.state_lbl.add_css_class("status-muted"),
            }
            widgets.tier_lbl.set_text(&format!("{:?}", p.poll_tier));
            widgets
                .us_lbl
                .set_text(&format!("{}", p.last_poll_duration_us));
            widgets.err_lbl.set_text(&format!("{}", p.total_errors));
            widgets
                .detail_lbl
                .set_text(p.last_error.as_deref().unwrap_or("—"));
        }
    }
}
