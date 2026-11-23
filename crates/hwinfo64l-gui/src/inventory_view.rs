use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Grid, Label, ListBox, Orientation, Paned, ScrolledWindow, SelectionMode};
use hwinfo64l_protocol::{InventoryNode, PropertyValue};

/// HWiNFO64-style split Hardware Tree + Property Inspector view.
pub struct InventoryView {
    pub root: Paned,
    tree_list: ListBox,
    detail_box: GtkBox,
    nodes: Rc<RefCell<Vec<InventoryNode>>>,
    last_hash_len: usize,
}

impl InventoryView {
    pub fn new() -> Self {
        let nodes: Rc<RefCell<Vec<InventoryNode>>> = Rc::new(RefCell::new(Vec::new()));

        let tree_list = ListBox::new();
        tree_list.set_selection_mode(SelectionMode::Single);

        let left_scroll = ScrolledWindow::builder()
            .hexpand(false)
            .vexpand(true)
            .min_content_width(270)
            .child(&tree_list)
            .build();

        let detail_box = GtkBox::new(Orientation::Vertical, 6);
        detail_box.set_margin_top(12);
        detail_box.set_margin_bottom(12);
        detail_box.set_margin_start(14);
        detail_box.set_margin_end(14);

        let right_scroll = ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&detail_box)
            .build();

        let nodes_sel = Rc::clone(&nodes);
        let detail_sel = detail_box.clone();
        tree_list.connect_row_selected(move |_list, maybe_row| {
            let Some(row) = maybe_row else {
                return;
            };
            let Ok(idx) = usize::try_from(row.index()) else {
                return;
            };
            let list = nodes_sel.borrow();
            if let Some(node) = list.get(idx) {
                render_node_details(&detail_sel, node);
            }
        });

        let root = Paned::new(Orientation::Horizontal);
        root.set_start_child(Some(&left_scroll));
        root.set_end_child(Some(&right_scroll));
        root.set_position(290);
        root.set_hexpand(true);
        root.set_vexpand(true);

        Self {
            root,
            tree_list,
            detail_box,
            nodes,
            last_hash_len: 0,
        }
    }

    pub fn update(&mut self, incoming: &[InventoryNode]) {
        if incoming.len() == self.last_hash_len && *self.nodes.borrow() == incoming {
            return;
        }
        self.last_hash_len = incoming.len();
        *self.nodes.borrow_mut() = incoming.to_vec();

        while let Some(child) = self.tree_list.first_child() {
            self.tree_list.remove(&child);
        }

        for node in incoming {
            let prefix = if node.parent_id.is_none() {
                "💻 "
            } else if node.parent_id.as_deref() == Some("memory_array") {
                "      ▪ "
            } else {
                "   ▸ "
            };
            let lbl = Label::new(Some(&format!("{prefix}{}", node.label)));
            lbl.set_xalign(0.0);
            lbl.set_margin_top(4);
            lbl.set_margin_bottom(4);
            lbl.set_margin_start(6);
            lbl.set_margin_end(6);
            self.tree_list.append(&lbl);
        }

        if let Some(first_row) = self.tree_list.row_at_index(0) {
            self.tree_list.select_row(Some(&first_row));
        } else if let Some(first) = incoming.first() {
            render_node_details(&self.detail_box, first);
        }
    }
}

fn render_node_details(detail_box: &GtkBox, node: &InventoryNode) {
    while let Some(child) = detail_box.first_child() {
        detail_box.remove(&child);
    }

    let title = Label::new(Some(&node.label));
    title.add_css_class("card-title");
    title.set_xalign(0.0);
    detail_box.append(&title);

    let meta = Label::new(Some(&format!(
        "Category: {:?}   |   ID: {}   |   Source: {}",
        node.kind, node.id, node.source
    )));
    meta.add_css_class("status-muted");
    meta.set_xalign(0.0);
    detail_box.append(&meta);

    let grid = Grid::new();
    grid.set_column_spacing(18);
    grid.set_row_spacing(6);
    grid.set_margin_top(8);

    for (i, (k, v)) in node.properties.iter().enumerate() {
        let row = i32::try_from(i).unwrap_or(0);
        let k_lbl = Label::new(Some(k));
        k_lbl.add_css_class("prop-key");
        k_lbl.set_xalign(0.0);

        let text = match v {
            PropertyValue::Text(s) => s.clone(),
            PropertyValue::Integer(n) => n.to_string(),
            PropertyValue::Float(f) => format!("{f:.2}"),
            PropertyValue::Bool(b) => b.to_string(),
            PropertyValue::List(l) => l.join(", "),
        };
        let v_lbl = Label::new(Some(&text));
        v_lbl.add_css_class("mono");
        v_lbl.set_xalign(0.0);
        v_lbl.set_selectable(true);
        v_lbl.set_wrap(true);
        v_lbl.set_hexpand(true);

        grid.attach(&k_lbl, 0, row, 1, 1);
        grid.attach(&v_lbl, 1, row, 1, 1);
    }

    detail_box.append(&grid);
}
