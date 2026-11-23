use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{
    Box as GtkBox, Button, DrawingArea, Expander, Label, Orientation, ScrolledWindow,
};
use hwinfo64l_protocol::{SensorDescriptor, SensorReading, SensorStatus};

type GraphCallback = Rc<dyn Fn(SensorDescriptor)>;

struct SensorRowWidgets {
    row: GtkBox,
    descriptor: SensorDescriptor,
    cur_label: Label,
    min_label: Label,
    max_label: Label,
    avg_label: Label,
    status_label: Label,
    sparkline_data: Rc<RefCell<VecDeque<f64>>>,
    sparkline: DrawingArea,
}

struct GroupWidgets {
    expander: Expander,
    list_box: GtkBox,
    sensor_ids: Vec<String>,
}

/// High-density HWiNFO64-style live sensor view with in-place updates and inline sparklines.
pub struct SensorsView {
    pub root: GtkBox,
    content_box: GtkBox,
    groups: BTreeMap<String, GroupWidgets>,
    rows: BTreeMap<String, SensorRowWidgets>,
    on_open_graph: GraphCallback,
    filter_text: String,
}

impl SensorsView {
    pub fn new(on_open_graph: GraphCallback) -> Self {
        let root = GtkBox::new(Orientation::Vertical, 0);
        root.set_hexpand(true);
        root.set_vexpand(true);

        // Sticky column header row
        let header = GtkBox::new(Orientation::Horizontal, 8);
        header.add_css_class("sensor-header-row");
        let h_name = make_col_label("Sensor", 0.0, true, 0);
        let h_cur = make_col_label("Current", 1.0, false, 96);
        let h_min = make_col_label("Minimum", 1.0, false, 96);
        let h_max = make_col_label("Maximum", 1.0, false, 96);
        let h_avg = make_col_label("Average", 1.0, false, 96);
        let h_spark = make_col_label("Trend", 0.5, false, 88);
        let h_status = make_col_label("State", 0.5, false, 48);
        let h_act = make_col_label("", 0.5, false, 34);
        for w in [h_name, h_cur, h_min, h_max, h_avg, h_spark, h_status, h_act] {
            header.append(&w);
        }
        root.append(&header);

        let content_box = GtkBox::new(Orientation::Vertical, 4);
        content_box.set_margin_top(4);
        content_box.set_margin_bottom(8);
        content_box.set_margin_start(6);
        content_box.set_margin_end(6);

        let scrolled = ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&content_box)
            .build();
        root.append(&scrolled);

        Self {
            root,
            content_box,
            groups: BTreeMap::new(),
            rows: BTreeMap::new(),
            on_open_graph,
            filter_text: String::new(),
        }
    }

    pub fn set_filter(&mut self, query: &str) {
        self.filter_text = query.trim().to_ascii_lowercase();
        self.apply_filter();
    }

    pub fn update(&mut self, descriptors: &[SensorDescriptor], readings: &[SensorReading]) {
        let reading_map: BTreeMap<&str, &SensorReading> =
            readings.iter().map(|r| (r.id.as_str(), r)).collect();

        let mut added_any = false;

        for desc in descriptors {
            if !self.rows.contains_key(&desc.id) {
                self.insert_sensor_row(desc);
                added_any = true;
            }

            if let (Some(row), Some(r)) = (self.rows.get_mut(&desc.id), reading_map.get(desc.id.as_str()))
            {
                let u = &desc.unit;
                row.cur_label.set_text(&format_val(r.value, u));
                row.min_label.set_text(&format_val(r.stats.min, u));
                row.max_label.set_text(&format_val(r.stats.max, u));
                row.avg_label.set_text(&format_val(r.stats.avg, u));

                row.status_label.remove_css_class("status-ok");
                row.status_label.remove_css_class("status-warn");
                row.status_label.remove_css_class("status-crit");
                row.status_label.remove_css_class("status-muted");

                match r.status {
                    SensorStatus::Ok => {
                        row.status_label.set_text("OK");
                        row.status_label.add_css_class("status-ok");
                    }
                    SensorStatus::Warning => {
                        row.status_label.set_text("WARN");
                        row.status_label.add_css_class("status-warn");
                    }
                    SensorStatus::Critical => {
                        row.status_label.set_text("CRIT");
                        row.status_label.add_css_class("status-crit");
                    }
                    SensorStatus::Stale => {
                        row.status_label.set_text("STALE");
                        row.status_label.add_css_class("status-muted");
                    }
                    SensorStatus::Unavailable => {
                        row.status_label.set_text("N/A");
                        row.status_label.add_css_class("status-muted");
                    }
                }

                {
                    let mut buf = row.sparkline_data.borrow_mut();
                    if buf.len() >= 60 {
                        buf.pop_front();
                    }
                    buf.push_back(r.value);
                }
                row.sparkline.queue_draw();
            }
        }

        if added_any && !self.filter_text.is_empty() {
            self.apply_filter();
        }
    }

    fn insert_sensor_row(&mut self, desc: &SensorDescriptor) {
        let group_entry = self.groups.entry(desc.group.clone()).or_insert_with(|| {
            let list_box = GtkBox::new(Orientation::Vertical, 0);
            let title = Label::new(Some(&desc.group));
            title.add_css_class("sensor-group-title");
            title.set_xalign(0.0);

            let expander = Expander::new(None);
            expander.set_label_widget(Some(&title));
            expander.set_expanded(true);
            expander.set_child(Some(&list_box));

            self.content_box.append(&expander);
            GroupWidgets {
                expander,
                list_box,
                sensor_ids: Vec::new(),
            }
        });

        let row = GtkBox::new(Orientation::Horizontal, 8);
        row.add_css_class("sensor-row");

        let name_label = make_col_label(&desc.name, 0.0, true, 0);
        name_label.set_tooltip_text(Some(&desc.id));

        let cur_label = make_mono_label("—", 96);
        let min_label = make_mono_label("—", 96);
        let max_label = make_mono_label("—", 96);
        let avg_label = make_mono_label("—", 96);

        let sparkline_data = Rc::new(RefCell::new(VecDeque::with_capacity(60)));
        let sparkline = DrawingArea::new();
        sparkline.set_content_width(88);
        sparkline.set_content_height(18);
        let draw_buf = Rc::clone(&sparkline_data);
        sparkline.set_draw_func(move |_area, cr, width, height| {
            let data = draw_buf.borrow();
            if data.len() < 2 {
                return;
            }
            let w = f64::from(width);
            let h = f64::from(height);
            let (mut lo, mut hi) = data
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &v| {
                    (a.min(v), b.max(v))
                });
            if (hi - lo).abs() < 1e-6 {
                lo -= 1.0;
                hi += 1.0;
            }
            let n = (data.len() - 1) as f64;
            cr.set_source_rgba(0.35, 0.78, 1.0, 0.9);
            cr.set_line_width(1.3);
            for (i, &v) in data.iter().enumerate() {
                let x = (i as f64 / n) * (w - 2.0) + 1.0;
                let norm = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
                let y = (h - 2.0) * (1.0 - norm) + 1.0;
                if i == 0 {
                    cr.move_to(x, y);
                } else {
                    cr.line_to(x, y);
                }
            }
            let _ = cr.stroke();
        });

        let status_label = make_col_label("—", 0.5, false, 48);

        let graph_btn = Button::with_label("📈");
        graph_btn.add_css_class("graph-btn");
        graph_btn.set_tooltip_text(Some("Open live graph"));
        let cb = Rc::clone(&self.on_open_graph);
        let desc_clone = desc.clone();
        graph_btn.connect_clicked(move |_| {
            cb(desc_clone.clone());
        });

        row.append(&name_label);
        row.append(&cur_label);
        row.append(&min_label);
        row.append(&max_label);
        row.append(&avg_label);
        row.append(&sparkline);
        row.append(&status_label);
        row.append(&graph_btn);

        group_entry.list_box.append(&row);
        group_entry.sensor_ids.push(desc.id.clone());

        self.rows.insert(
            desc.id.clone(),
            SensorRowWidgets {
                row,
                descriptor: desc.clone(),
                cur_label,
                min_label,
                max_label,
                avg_label,
                status_label,
                sparkline_data,
                sparkline,
            },
        );
    }

    fn apply_filter(&self) {
        for group in self.groups.values() {
            let mut any_visible = false;
            for id in &group.sensor_ids {
                if let Some(r) = self.rows.get(id) {
                    let visible = if self.filter_text.is_empty() {
                        true
                    } else {
                        let hay = format!(
                            "{} {} {}",
                            r.descriptor.group, r.descriptor.name, r.descriptor.id
                        )
                        .to_ascii_lowercase();
                        hay.contains(&self.filter_text)
                    };
                    r.row.set_visible(visible);
                    if visible {
                        any_visible = true;
                    }
                }
            }
            group.expander.set_visible(any_visible);
        }
    }
}

fn make_col_label(text: &str, xalign: f32, hexpand: bool, width: i32) -> Label {
    let lbl = Label::new(Some(text));
    lbl.set_xalign(xalign);
    lbl.set_hexpand(hexpand);
    lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    if width > 0 {
        lbl.set_width_request(width);
    }
    lbl
}

fn make_mono_label(text: &str, width: i32) -> Label {
    let lbl = make_col_label(text, 1.0, false, width);
    lbl.add_css_class("mono");
    lbl
}

fn format_val(v: f64, unit: &str) -> String {
    if unit.is_empty() {
        format!("{v:.2}")
    } else {
        format!("{v:.2} {unit}")
    }
}
