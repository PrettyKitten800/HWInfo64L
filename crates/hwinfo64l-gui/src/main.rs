mod client;
mod graph_window;
mod inventory_view;
mod probes_view;
mod sensors_view;
mod summary_view;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use clap::Parser;
use gtk4::prelude::*;
use gtk4::{
    Application, ApplicationWindow, Box as GtkBox, Button, CssProvider, DropDown, HeaderBar, Label,
    Orientation, SearchEntry, Stack, StackSwitcher, ToggleButton,
};
use hwinfo64l_core::now_epoch_ms;
use hwinfo64l_protocol::DEFAULT_SOCKET_PATH;

use crate::client::{ConnectionMode, WorkerCommand, WorkerEvent};
use crate::graph_window::GraphWindow;
use crate::inventory_view::InventoryView;
use crate::probes_view::ProbesView;
use crate::sensors_view::SensorsView;
use crate::summary_view::SummaryView;

#[derive(Debug, Parser)]
#[command(
    name = "hwinfo64l-gui",
    version,
    about = "Native GTK4 HWiNFO64-style desktop UI for HWInfo64L"
)]
struct Args {
    /// Path to hwinfo64ld Unix domain socket.
    #[arg(long, env = "HWINFO64L_SOCKET", default_value = DEFAULT_SOCKET_PATH)]
    socket: PathBuf,

    /// Force local unprivileged probing instead of connecting to hwinfo64ld.
    #[arg(long)]
    local: bool,

    /// Startup tab: sensors, summary, inventory, or probes.
    #[arg(long, default_value = "sensors")]
    tab: String,
}

fn main() {
    let args = Args::parse();

    let app = Application::builder()
        .application_id("io.github.prettykitten800.HWInfo64L")
        .build();

    let socket = args.socket.clone();
    let force_local = args.local;
    let initial_tab = args.tab.clone();

    app.connect_activate(move |app| {
        load_css();
        build_ui(app, socket.clone(), force_local, &initial_tab);
    });

    // Pass empty argv to GTK so clap owns CLI argument parsing cleanly.
    let empty: [&str; 0] = [];
    app.run_with_args(&empty);
}

fn load_css() {
    let provider = CssProvider::new();
    provider.load_from_data(include_str!("style.css"));
    if let Some(display) = gtk4::gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn build_ui(app: &Application, socket: PathBuf, force_local: bool, initial_tab: &str) {
    let (cmd_tx, evt_rx) = client::spawn_worker(socket, force_local);
    let cmd_tx = Rc::new(cmd_tx);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("HWInfo64L — Hardware Info & Sensors")
        .default_width(1040)
        .default_height(720)
        .build();

    // Header bar with tab switcher + connection badge
    let header = HeaderBar::new();
    let stack = Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);

    let switcher = StackSwitcher::new();
    switcher.set_stack(Some(&stack));
    header.set_title_widget(Some(&switcher));

    let mode_badge = Label::new(Some("Connecting…"));
    mode_badge.add_css_class("mode-badge-local");
    header.pack_end(&mode_badge);
    window.set_titlebar(Some(&header));

    // Action toolbar
    let toolbar = GtkBox::new(Orientation::Horizontal, 8);
    toolbar.add_css_class("toolbar-bar");

    let search_entry = SearchEntry::new();
    search_entry.set_placeholder_text(Some("Filter sensors (e.g. temp, power, memory, zswap)…"));
    search_entry.set_hexpand(true);

    let interval_dd = DropDown::from_strings(&["500 ms", "1 s", "2 s", "5 s"]);
    interval_dd.set_selected(1);
    interval_dd.set_tooltip_text(Some("Polling interval"));

    let reset_btn = Button::with_label("Reset Min/Max");
    let pause_btn = ToggleButton::with_label("Pause");
    let csv_btn = ToggleButton::with_label("Log CSV");

    toolbar.append(&search_entry);
    toolbar.append(&interval_dd);
    toolbar.append(&reset_btn);
    toolbar.append(&pause_btn);
    toolbar.append(&csv_btn);

    // Pop-out graphs tracked by sensor ID
    let open_graphs: Rc<RefCell<BTreeMap<String, GraphWindow>>> =
        Rc::new(RefCell::new(BTreeMap::new()));

    let graphs_cb = Rc::clone(&open_graphs);
    let win_cb = window.clone();
    let tx_cb = Rc::clone(&cmd_tx);
    let sensors_view = Rc::new(RefCell::new(SensorsView::new(Rc::new(move |desc| {
        let mut map = graphs_cb.borrow_mut();
        if let Some(existing) = map.get(&desc.id) {
            if existing.is_visible() {
                return;
            }
        }
        let gw = GraphWindow::open(&win_cb, &desc, Vec::new());
        let _ = tx_cb.send(WorkerCommand::RequestHistory {
            sensor_id: desc.id.clone(),
            limit: 300,
        });
        map.insert(desc.id, gw);
    }))));

    let summary_view = Rc::new(RefCell::new(SummaryView::new()));
    let inventory_view = Rc::new(RefCell::new(InventoryView::new()));
    let probes_view = Rc::new(RefCell::new(ProbesView::new()));

    stack.add_titled(&sensors_view.borrow().root, Some("sensors"), "Sensors");
    stack.add_titled(&summary_view.borrow().root, Some("summary"), "Summary");
    stack.add_titled(
        &inventory_view.borrow().root,
        Some("inventory"),
        "Hardware Tree",
    );
    stack.add_titled(
        &probes_view.borrow().root,
        Some("probes"),
        "Probes & Plugins",
    );

    if matches!(initial_tab, "sensors" | "summary" | "inventory" | "probes") {
        stack.set_visible_child_name(initial_tab);
    }

    // Status bar
    let status_bar = Label::new(Some("Initializing hardware probes…"));
    status_bar.set_xalign(0.0);
    status_bar.add_css_class("status-bar");

    let root = GtkBox::new(Orientation::Vertical, 0);
    root.append(&toolbar);
    root.append(&stack);
    root.append(&status_bar);
    window.set_child(Some(&root));

    // Wire toolbar controls
    {
        let sv = Rc::clone(&sensors_view);
        search_entry.connect_search_changed(move |entry| {
            sv.borrow_mut().set_filter(&entry.text());
        });
    }
    {
        let tx = Rc::clone(&cmd_tx);
        interval_dd.connect_selected_notify(move |dd| {
            let ms = match dd.selected() {
                0 => 500,
                1 => 1000,
                2 => 2000,
                _ => 5000,
            };
            let _ = tx.send(WorkerCommand::SetIntervalMs(ms));
        });
    }
    {
        let tx = Rc::clone(&cmd_tx);
        reset_btn.connect_clicked(move |_| {
            let _ = tx.send(WorkerCommand::ResetStats);
        });
    }
    {
        let tx = Rc::clone(&cmd_tx);
        pause_btn.connect_toggled(move |btn| {
            let paused = btn.is_active();
            btn.set_label(if paused { "Resume" } else { "Pause" });
            let _ = tx.send(WorkerCommand::SetPaused(paused));
        });
    }
    {
        let tx = Rc::clone(&cmd_tx);
        csv_btn.connect_toggled(move |btn| {
            if btn.is_active() {
                let ts = now_epoch_ms() / 1000;
                let base = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."));
                let path = base.join(format!("hwinfo64l-{ts}.csv"));
                btn.set_label("Stop CSV");
                btn.add_css_class("logging-active");
                let _ = tx.send(WorkerCommand::StartCsvLog(path));
            } else {
                btn.set_label("Log CSV");
                btn.remove_css_class("logging-active");
                let _ = tx.send(WorkerCommand::StopCsvLog);
            }
        });
    }
    {
        let tx = Rc::clone(&cmd_tx);
        window.connect_close_request(move |_| {
            let _ = tx.send(WorkerCommand::Shutdown);
            gtk4::glib::Propagation::Proceed
        });
    }

    // Drain telemetry worker events on the GTK main loop every 100 ms
    let evt_rx = Rc::new(RefCell::new(evt_rx));
    let sv_poll = Rc::clone(&sensors_view);
    let sum_poll = Rc::clone(&summary_view);
    let inv_poll = Rc::clone(&inventory_view);
    let prb_poll = Rc::clone(&probes_view);
    let grp_poll = Rc::clone(&open_graphs);
    let badge_poll = mode_badge.clone();
    let status_poll = status_bar.clone();

    gtk4::glib::timeout_add_local(Duration::from_millis(100), move || {
        let rx = evt_rx.borrow();
        while let Ok(evt) = rx.try_recv() {
            match evt {
                WorkerEvent::Snapshot(snap) => {
                    badge_poll.remove_css_class("mode-badge-daemon");
                    badge_poll.remove_css_class("mode-badge-local");
                    match snap.mode {
                        ConnectionMode::Daemon { privileged: true } => {
                            badge_poll.set_text("Daemon (Full)");
                            badge_poll.add_css_class("mode-badge-daemon");
                        }
                        ConnectionMode::Daemon { privileged: false } => {
                            badge_poll.set_text("Daemon (User)");
                            badge_poll.add_css_class("mode-badge-local");
                        }
                        ConnectionMode::LocalUnprivileged => {
                            badge_poll.set_text("Local Mode");
                            badge_poll.add_css_class("mode-badge-local");
                        }
                    }

                    sv_poll.borrow_mut().update(&snap.descriptors, &snap.readings);
                    sum_poll.borrow_mut().update(&snap.summary);
                    inv_poll.borrow_mut().update(&snap.inventory);
                    prb_poll.borrow_mut().update(&snap.probes);

                    // Update open pop-out graphs
                    let mut graphs = grp_poll.borrow_mut();
                    graphs.retain(|_, gw| gw.is_visible());
                    for reading in &snap.readings {
                        if let Some(gw) = graphs.get(&reading.id) {
                            gw.push_reading(reading);
                        }
                    }

                    let healthy = snap
                        .probes
                        .iter()
                        .filter(|p| p.state == hwinfo64l_protocol::ProbeState::Healthy)
                        .count();
                    let csv_note = match snap.csv_logging_path {
                        Some(ref p) => format!("   |   Recording CSV: {}", p.display()),
                        None => String::new(),
                    };
                    status_poll.set_text(&format!(
                        "Sensors: {}   |   Probes Healthy: {}/{}{}",
                        snap.readings.len(),
                        healthy,
                        snap.probes.len(),
                        csv_note
                    ));
                }
                WorkerEvent::History { sensor_id, samples } => {
                    let graphs = grp_poll.borrow();
                    if let Some(gw) = graphs.get(&sensor_id) {
                        gw.set_history(samples);
                    }
                }
                WorkerEvent::Notice(msg) => {
                    status_poll.set_text(&msg);
                }
            }
        }
        gtk4::glib::ControlFlow::Continue
    });

    window.present();
}
