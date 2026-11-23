use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{ApplicationWindow, Box as GtkBox, DrawingArea, Label, Orientation, Window};
use hwinfo64l_protocol::{HistorySample, SensorDescriptor, SensorReading};

/// Pop-out real-time graph window for an individual sensor.
pub struct GraphWindow {
    window: Window,
    samples: Rc<RefCell<VecDeque<f64>>>,
    unit: String,
    stats_label: Label,
    canvas: DrawingArea,
}

impl GraphWindow {
    pub fn open(
        parent: &ApplicationWindow,
        desc: &SensorDescriptor,
        initial_history: Vec<HistorySample>,
    ) -> Self {
        let samples = Rc::new(RefCell::new(VecDeque::with_capacity(300)));
        {
            let mut buf = samples.borrow_mut();
            for pt in initial_history {
                if buf.len() >= 300 {
                    buf.pop_front();
                }
                buf.push_back(pt.value);
            }
        }

        let window = Window::builder()
            .title(format!("{} — {} [{}]", desc.group, desc.name, desc.unit))
            .transient_for(parent)
            .default_width(560)
            .default_height(280)
            .build();

        let root = GtkBox::new(Orientation::Vertical, 6);
        root.set_margin_top(10);
        root.set_margin_bottom(10);
        root.set_margin_start(12);
        root.set_margin_end(12);

        let stats_label = Label::new(Some("Waiting for samples..."));
        stats_label.set_xalign(0.0);
        stats_label.add_css_class("mono");
        root.append(&stats_label);

        let canvas = DrawingArea::new();
        canvas.set_hexpand(true);
        canvas.set_vexpand(true);
        canvas.set_content_height(200);

        let draw_samples = Rc::clone(&samples);
        let unit_clone = desc.unit.clone();
        canvas.set_draw_func(move |_area, cr, width, height| {
            let w = f64::from(width);
            let h = f64::from(height);
            let pad_l = 52.0;
            let pad_r = 12.0;
            let pad_t = 12.0;
            let pad_b = 18.0;
            let plot_w = (w - pad_l - pad_r).max(10.0);
            let plot_h = (h - pad_t - pad_b).max(10.0);

            // Background
            cr.set_source_rgba(0.08, 0.10, 0.13, 1.0);
            let _ = cr.paint();

            let data = draw_samples.borrow();
            let (mut lo, mut hi) = data
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &v| {
                    (a.min(v), b.max(v))
                });
            if !lo.is_finite() || !hi.is_finite() {
                lo = 0.0;
                hi = 100.0;
            }
            if (hi - lo).abs() < 1e-6 {
                lo = (lo - 1.0).max(0.0);
                hi += 1.0;
            } else {
                let margin = (hi - lo) * 0.12;
                lo = (lo - margin).max(0.0);
                hi += margin;
            }

            // Grid lines + Y-axis labels
            cr.set_line_width(1.0);
            cr.select_font_face("Monospace", gtk4::cairo::FontSlant::Normal, gtk4::cairo::FontWeight::Normal);
            cr.set_font_size(10.0);

            for step in 0..=4 {
                let frac = f64::from(step) / 4.0;
                let y = pad_t + plot_h * (1.0 - frac);
                let val = lo + (hi - lo) * frac;

                cr.set_source_rgba(1.0, 1.0, 1.0, 0.10);
                cr.move_to(pad_l, y);
                cr.line_to(pad_l + plot_w, y);
                let _ = cr.stroke();

                cr.set_source_rgba(0.82, 0.86, 0.90, 0.85);
                cr.move_to(4.0, y + 3.5);
                let _ = cr.show_text(&format!("{val:.1}"));
            }

            if data.len() >= 2 {
                let n = (data.len() - 1) as f64;

                // Area fill
                cr.set_source_rgba(0.24, 0.65, 0.95, 0.22);
                for (i, &v) in data.iter().enumerate() {
                    let x = pad_l + (i as f64 / n) * plot_w;
                    let y = pad_t + plot_h * (1.0 - ((v - lo) / (hi - lo)).clamp(0.0, 1.0));
                    if i == 0 {
                        cr.move_to(x, pad_t + plot_h);
                        cr.line_to(x, y);
                    } else {
                        cr.line_to(x, y);
                    }
                }
                cr.line_to(pad_l + plot_w, pad_t + plot_h);
                cr.close_path();
                let _ = cr.fill();

                // Line stroke
                cr.set_source_rgba(0.35, 0.78, 1.0, 0.95);
                cr.set_line_width(1.8);
                for (i, &v) in data.iter().enumerate() {
                    let x = pad_l + (i as f64 / n) * plot_w;
                    let y = pad_t + plot_h * (1.0 - ((v - lo) / (hi - lo)).clamp(0.0, 1.0));
                    if i == 0 {
                        cr.move_to(x, y);
                    } else {
                        cr.line_to(x, y);
                    }
                }
                let _ = cr.stroke();
            } else {
                cr.set_source_rgba(0.8, 0.8, 0.8, 0.6);
                cr.move_to(pad_l + 12.0, pad_t + plot_h / 2.0);
                let _ = cr.show_text("Collecting samples...");
            }

            // Unit label in top-right
            cr.set_source_rgba(0.8, 0.85, 0.9, 0.65);
            cr.move_to(pad_l + plot_w - 36.0, pad_t + 12.0);
            let _ = cr.show_text(&unit_clone);
        });

        root.append(&canvas);
        window.set_child(Some(&root));
        window.present();

        Self {
            window,
            samples,
            unit: desc.unit.clone(),
            stats_label,
            canvas,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.window.is_visible()
    }

    pub fn set_history(&self, hist: Vec<HistorySample>) {
        let mut buf = self.samples.borrow_mut();
        buf.clear();
        for pt in hist {
            if buf.len() >= 300 {
                buf.pop_front();
            }
            buf.push_back(pt.value);
        }
        drop(buf);
        self.canvas.queue_draw();
    }

    pub fn push_reading(&self, reading: &SensorReading) {
        {
            let mut buf = self.samples.borrow_mut();
            if buf.len() >= 300 {
                buf.pop_front();
            }
            buf.push_back(reading.value);
        }
        self.stats_label.set_text(&format!(
            "Cur: {:.2} {}   |   Min: {:.2} {}   |   Max: {:.2} {}   |   Avg: {:.2} {}",
            reading.value,
            self.unit,
            reading.stats.min,
            self.unit,
            reading.stats.max,
            self.unit,
            reading.stats.avg,
            self.unit
        ));
        self.canvas.queue_draw();
    }
}
