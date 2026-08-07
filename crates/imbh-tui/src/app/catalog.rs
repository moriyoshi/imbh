//! The Metrics catalog tree: expansion, discovered dimensions, checked series, and the PromQL the
//! selection visualizes.

use std::collections::HashMap;

use crate::app::App;
use crate::model::{DimNode, MetricNode, Screen, TableData, TreeRowRef};
use crate::promql::build_metric_query;

impl App {
    /// Whether the Metrics catalog tree is the current view (Metrics screen with an empty query).
    pub(crate) fn on_catalog(&self) -> bool {
        self.screen() == Screen::Metrics && self.active_query().trim().is_empty()
    }

    /// (Re)build the catalog tree from a freshly-arrived flat catalog snapshot, then render it.
    /// Per-metric UI state (expansion, discovered dimensions, and the checked series/whole-metric
    /// selection) is carried over by name, so a catalog refresh — including navigating away to the
    /// series list and back — preserves the selection instead of resetting it.
    pub(crate) fn build_metric_tree(&mut self) {
        let Some(table) = &self.snapshot.table else {
            return;
        };
        // Only the catalog table (Metric/Kind/Unit/Temporality) is a tree; the series table is not.
        if table.header.first().map(String::as_str) != Some("Metric") {
            return;
        }
        let mut prior: HashMap<String, MetricNode> = self
            .metric_tree
            .drain(..)
            .map(|node| (node.name.clone(), node))
            .collect();
        self.metric_tree = table
            .rows
            .iter()
            .map(|row| {
                let name = row.first().cloned().unwrap_or_default();
                let kind = row.get(1).cloned().unwrap_or_default();
                let unit = row.get(2).cloned().unwrap_or_default();
                let temporality = row.get(3).cloned().unwrap_or_default();
                match prior.remove(&name) {
                    // Keep expansion/dims/selection; refresh the (possibly changed) static metadata.
                    Some(mut node) => {
                        node.kind = kind;
                        node.unit = unit;
                        node.temporality = temporality;
                        node
                    }
                    None => MetricNode {
                        name,
                        kind,
                        unit,
                        temporality,
                        expanded: false,
                        whole_selected: false,
                        dims: None,
                        loading: false,
                    },
                }
            })
            .collect();
        self.rebuild_catalog_table();
    }

    /// Flatten the catalog tree into `snapshot.table` and the parallel `tree_rows`, preserving the
    /// cursor position within the new row count.
    pub(crate) fn rebuild_catalog_table(&mut self) {
        // Plain ASCII disclosure markers `v`/`>` on every terminal. The small-triangle glyphs (▾/▸)
        // render too small to read as open/closed on many terminals, and the full-size ▼/▶ are
        // East-Asian-width *ambiguous* (they desync the width-aware column sizing), so `v`/`>` it is.
        let (branch_open, branch_closed) = ("v ", "> ");
        let mut rows = Vec::new();
        let mut refs = Vec::new();
        for (mi, metric) in self.metric_tree.iter().enumerate() {
            let marker = if metric.expanded {
                branch_open
            } else {
                branch_closed
            };
            rows.push(vec![
                format!("{marker}{}", metric.name),
                metric.kind.clone(),
                metric.unit.clone(),
                metric.temporality.clone(),
            ]);
            refs.push(TreeRowRef::Metric(mi));
            if !metric.expanded {
                continue;
            }
            match &metric.dims {
                None => {
                    rows.push(vec![
                        "    (loading dimensions...)".to_owned(),
                        String::new(),
                        String::new(),
                        String::new(),
                    ]);
                    refs.push(TreeRowRef::Metric(mi));
                }
                Some(dims) if dims.is_empty() => {
                    // A checkbox to select the whole metric (there are no series to check).
                    let check = if metric.whole_selected { "[x]" } else { "[ ]" };
                    rows.push(vec![
                        format!("    {check} (no dimensions)"),
                        String::new(),
                        String::new(),
                        String::new(),
                    ]);
                    refs.push(TreeRowRef::NoDims(mi));
                }
                Some(dims) => {
                    for (di, dim) in dims.iter().enumerate() {
                        let marker = if dim.expanded {
                            branch_open
                        } else {
                            branch_closed
                        };
                        // Show the checked value (if any) on the dimension row so it stays visible
                        // when the axis is collapsed.
                        let chosen = dim
                            .selected
                            .and_then(|vi| dim.values.get(vi))
                            .map(|value| format!(" = {value}"))
                            .unwrap_or_default();
                        rows.push(vec![
                            format!("  {marker}by {} ({}){chosen}", dim.label, dim.values.len()),
                            String::new(),
                            String::new(),
                            String::new(),
                        ]);
                        refs.push(TreeRowRef::Dim(mi, di));
                        if dim.expanded {
                            for (vi, value) in dim.values.iter().enumerate() {
                                let check = if dim.selected == Some(vi) {
                                    "[x]"
                                } else {
                                    "[ ]"
                                };
                                rows.push(vec![
                                    format!("      {check} {}={}", dim.label, value),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                ]);
                                refs.push(TreeRowRef::Value(mi, di, vi));
                            }
                        }
                    }
                }
            }
        }
        self.tree_rows = refs;
        let header = self
            .snapshot
            .table
            .as_ref()
            .map(|table| table.header.clone())
            .unwrap_or_else(|| {
                vec![
                    "Metric".to_owned(),
                    "Kind".to_owned(),
                    "Unit".to_owned(),
                    "Temporality".to_owned(),
                ]
            });
        self.snapshot.table = Some(TableData { header, rows });
        if let Some((first, last)) = self.selectable_bounds() {
            self.selected = self.selected.clamp(first, last);
        }
    }

    /// The tree node the cursor is on (catalog view only).
    pub(crate) fn selected_tree_row(&self) -> Option<TreeRowRef> {
        if !self.on_catalog() {
            return None;
        }
        let (first, last) = self.selectable_bounds()?;
        self.tree_rows
            .get(self.selected.clamp(first, last))
            .copied()
    }

    /// Handle Space on the selected node: expand/collapse a metric or dimension, or toggle a value's
    /// checkbox (exclusive within its dimension). Returns the metric name when a metric was expanded
    /// for the first time and its dimensions must be fetched.
    pub(crate) fn toggle_node(&mut self) -> Option<String> {
        let mut to_load = None;
        match self.selected_tree_row()? {
            TreeRowRef::Metric(mi) => {
                let node = &mut self.metric_tree[mi];
                if !node.expanded && node.dims.is_none() && !node.loading {
                    node.loading = true;
                    node.expanded = true;
                    to_load = Some(node.name.clone());
                } else {
                    node.expanded = !node.expanded;
                }
            }
            TreeRowRef::Dim(mi, di) => {
                if let Some(dims) = self.metric_tree[mi].dims.as_mut() {
                    dims[di].expanded = !dims[di].expanded;
                }
            }
            TreeRowRef::Value(mi, di, vi) => {
                if let Some(dims) = self.metric_tree[mi].dims.as_mut() {
                    // Exclusive within the axis: checking a value replaces any other; checking the
                    // already-checked value clears it.
                    dims[di].selected = if dims[di].selected == Some(vi) {
                        None
                    } else {
                        Some(vi)
                    };
                }
            }
            TreeRowRef::NoDims(mi) => {
                self.metric_tree[mi].whole_selected = !self.metric_tree[mi].whole_selected;
            }
        }
        self.rebuild_catalog_table();
        to_load
    }

    /// Store freshly-discovered dimensions on the matching metric node.
    pub(crate) fn apply_metric_dims(&mut self, metric: &str, dims: Vec<DimNode>) {
        if let Some(node) = self.metric_tree.iter_mut().find(|n| n.name == metric) {
            node.dims = Some(dims);
            node.loading = false;
        }
        if self.on_catalog() {
            self.rebuild_catalog_table();
        }
    }

    /// The PromQL for metric `mi`, filtered by its checked values (one per axis) and optionally grouped
    /// by `group_by` (skipping that axis's matcher as redundant).
    pub(crate) fn metric_node_query(&self, mi: usize, group_by: Option<&str>) -> String {
        let node = &self.metric_tree[mi];
        let dims = node.dims.as_deref().unwrap_or(&[]);
        let matchers = dims
            .iter()
            .filter(|dim| Some(dim.label.as_str()) != group_by)
            .filter_map(|dim| {
                dim.selected
                    .and_then(|vi| dim.values.get(vi))
                    .map(|value| (dim.label.as_str(), value.as_str()))
            })
            .collect::<Vec<_>>();
        // The metric's own axes, which a histogram's aggregation has to name explicitly to keep the
        // series split the other kinds get for free (see `build_metric_query`).
        let labels = dims
            .iter()
            .map(|dim| dim.label.as_str())
            .collect::<Vec<_>>();
        build_metric_query(&node.name, &node.kind, &matchers, group_by, &labels)
    }

    /// The queries to run when visualizing from the catalog. Checking any series (a dimension value)
    /// under a metric is itself the selection: every metric with at least one checked value is
    /// visualized together, each filtered by its own checked values. When nothing is checked anywhere,
    /// fall back to the single node under the cursor — grouped by the dimension on a `by …` row. Empty
    /// only when nothing is selectable.
    pub(crate) fn visualize_queries(&self) -> Vec<String> {
        let selected = self
            .metric_tree
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                node.whole_selected
                    || node
                        .dims
                        .as_deref()
                        .is_some_and(|dims| dims.iter().any(|dim| dim.selected.is_some()))
            })
            .map(|(mi, _)| self.metric_node_query(mi, None))
            .collect::<Vec<_>>();
        if !selected.is_empty() {
            return selected;
        }
        let Some(row) = self.selected_tree_row() else {
            return Vec::new();
        };
        let mi = match row {
            TreeRowRef::Metric(mi)
            | TreeRowRef::Dim(mi, _)
            | TreeRowRef::Value(mi, _, _)
            | TreeRowRef::NoDims(mi) => mi,
        };
        let group_by = match row {
            TreeRowRef::Dim(_, di) => self.metric_tree[mi]
                .dims
                .as_deref()
                .and_then(|dims| dims.get(di))
                .map(|dim| dim.label.clone()),
            _ => None,
        };
        vec![self.metric_node_query(mi, group_by.as_deref())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::catalog_app;

    #[test]
    fn catalog_tree_expands_metrics_and_dimensions() {
        let mut app = catalog_app();
        assert_eq!(app.snapshot.table.as_ref().unwrap().rows.len(), 2);
        assert_eq!(
            app.tree_rows,
            vec![TreeRowRef::Metric(0), TreeRowRef::Metric(1)]
        );

        // Expand the second metric (reqs); dims not loaded yet -> a placeholder row appears.
        app.selected = 1;
        let load = app.toggle_node();
        assert_eq!(load, Some("reqs".to_owned()));
        assert_eq!(app.snapshot.table.as_ref().unwrap().rows.len(), 3);

        // Dimensions arrive; the placeholder becomes a dimension row.
        app.apply_metric_dims(
            "reqs",
            vec![DimNode {
                label: "service".into(),
                values: vec!["cart".into(), "checkout".into()],
                expanded: false,
                selected: None,
            }],
        );
        assert_eq!(
            app.tree_rows,
            vec![
                TreeRowRef::Metric(0),
                TreeRowRef::Metric(1),
                TreeRowRef::Dim(1, 0),
            ]
        );

        // Expand the dimension to reveal its two values.
        app.selected = 2;
        assert_eq!(app.toggle_node(), None); // dim expand needs no fetch
        assert_eq!(
            app.tree_rows,
            vec![
                TreeRowRef::Metric(0),
                TreeRowRef::Metric(1),
                TreeRowRef::Dim(1, 0),
                TreeRowRef::Value(1, 0, 0),
                TreeRowRef::Value(1, 0, 1),
            ]
        );
    }

    #[test]
    fn value_checkbox_is_exclusive_and_drives_the_query() {
        let mut app = catalog_app();
        app.apply_metric_dims(
            "reqs",
            vec![DimNode {
                label: "service".into(),
                values: vec!["cart".into(), "checkout".into()],
                expanded: true,
                selected: None,
            }],
        );
        app.metric_tree[1].expanded = true;
        app.rebuild_catalog_table();
        // rows: [cpu, reqs, by service, service=cart, service=checkout]

        // Nothing checked: the metric row visualizes the whole metric.
        app.selected = 1;
        assert_eq!(app.visualize_queries(), vec!["rate(reqs[5m])".to_owned()]);

        // Nothing checked, cursor on the "by service" row: group by service.
        app.selected = 2;
        assert_eq!(
            app.visualize_queries(),
            vec!["sum by (service) (rate(reqs[5m]))".to_owned()]
        );

        // Check service=cart (Space on that value row).
        app.selected = 3;
        assert_eq!(app.toggle_node(), None);
        assert_eq!(
            app.metric_tree[1].dims.as_ref().unwrap()[0].selected,
            Some(0)
        );
        // A checked series *is* the selection: it drives the visualization regardless of where the
        // cursor sits (here the "by service" row), filtered by the checked value.
        app.selected = 2;
        assert_eq!(
            app.visualize_queries(),
            vec!["rate(reqs{service=\"cart\"}[5m])".to_owned()]
        );

        // Checking a different value in the same axis replaces the first (exclusive).
        app.selected = 4;
        app.toggle_node();
        assert_eq!(
            app.metric_tree[1].dims.as_ref().unwrap()[0].selected,
            Some(1)
        );

        // Toggling the checked value off clears it, so the cursor drives it again.
        app.toggle_node();
        assert_eq!(app.metric_tree[1].dims.as_ref().unwrap()[0].selected, None);
        app.selected = 1;
        assert_eq!(app.visualize_queries(), vec!["rate(reqs[5m])".to_owned()]);
    }

    #[test]
    fn checked_series_visualize_their_metrics_together() {
        let mut app = catalog_app(); // cpu (gauge), reqs (sum)

        // Nothing checked: visualize only the node under the cursor.
        app.selected = 0;
        assert_eq!(app.visualize_queries(), vec!["cpu".to_owned()]);

        // Mark a series under each metric (a checked value = an implicit metric selection).
        app.metric_tree[0].dims = Some(vec![DimNode {
            label: "host".into(),
            values: vec!["node-a".into()],
            expanded: false,
            selected: Some(0),
        }]);
        app.metric_tree[1].dims = Some(vec![DimNode {
            label: "service".into(),
            values: vec!["cart".into()],
            expanded: false,
            selected: Some(0),
        }]);

        // Both metrics are visualized together, each filtered by its own checked series and using its
        // own kind-appropriate transform (gauge as-is, sum as a rate). Cursor position is irrelevant.
        app.selected = 0;
        assert_eq!(
            app.visualize_queries(),
            vec![
                "cpu{host=\"node-a\"}".to_owned(),
                "rate(reqs{service=\"cart\"}[5m])".to_owned(),
            ]
        );

        // Clearing cpu's series drops it from the selection.
        app.metric_tree[0].dims.as_mut().unwrap()[0].selected = None;
        assert_eq!(
            app.visualize_queries(),
            vec!["rate(reqs{service=\"cart\"}[5m])".to_owned()]
        );
    }

    #[test]
    fn no_dimensions_checkbox_selects_the_whole_metric() {
        let mut app = catalog_app(); // cpu (gauge), reqs (sum)
        // cpu has no dimensions; expanding it shows the "(no dimensions)" checkbox row.
        app.metric_tree[0].dims = Some(vec![]);
        app.metric_tree[0].expanded = true;
        app.rebuild_catalog_table();
        assert_eq!(
            app.tree_rows,
            vec![
                TreeRowRef::Metric(0),
                TreeRowRef::NoDims(0),
                TreeRowRef::Metric(1),
            ]
        );
        assert!(
            app.snapshot.table.as_ref().unwrap().rows[1][0].contains("[ ] (no dimensions)"),
            "unchecked box shown alongside the message"
        );

        // Space on that row checks the whole metric.
        app.selected = 1;
        assert_eq!(app.toggle_node(), None);
        assert!(app.metric_tree[0].whole_selected);
        assert!(app.snapshot.table.as_ref().unwrap().rows[1][0].contains("[x]"));

        // The whole-metric checkbox drives the visualization regardless of the cursor.
        app.selected = 2; // cursor on reqs
        assert_eq!(app.visualize_queries(), vec!["cpu".to_owned()]);

        // Unchecking it returns to the node under the cursor.
        app.selected = 1;
        app.toggle_node();
        assert!(!app.metric_tree[0].whole_selected);
        app.selected = 2;
        assert_eq!(app.visualize_queries(), vec!["rate(reqs[5m])".to_owned()]);
    }

    #[test]
    fn catalog_selection_survives_navigating_away_and_back() {
        let mut app = catalog_app(); // cpu (gauge), reqs (sum)
        // Discover reqs' dimensions, check a series, and expand it (the state before visualizing).
        app.apply_metric_dims(
            "reqs",
            vec![DimNode {
                label: "service".into(),
                values: vec!["cart".into(), "checkout".into()],
                expanded: true,
                selected: Some(0),
            }],
        );
        app.metric_tree[1].expanded = true;
        assert_eq!(
            app.visualize_queries(),
            vec!["rate(reqs{service=\"cart\"}[5m])".to_owned()]
        );

        // Navigate away to the series list and back: a fresh catalog snapshot arrives (raw rows) and
        // the tree is rebuilt. Expansion, discovered dims, and the checked series must all survive.
        app.snapshot.table = Some(TableData {
            header: vec![
                "Metric".into(),
                "Kind".into(),
                "Unit".into(),
                "Temporality".into(),
            ],
            rows: vec![
                vec!["cpu".into(), "gauge".into(), "1".into(), "-".into()],
                vec!["reqs".into(), "sum".into(), "1".into(), "cumulative".into()],
            ],
        });
        app.build_metric_tree();

        assert!(app.metric_tree[1].expanded, "expansion preserved");
        let dims = app.metric_tree[1].dims.as_ref().expect("dims preserved");
        assert_eq!(dims[0].selected, Some(0), "checked series preserved");
        assert_eq!(
            app.visualize_queries(),
            vec!["rate(reqs{service=\"cart\"}[5m])".to_owned()]
        );
    }
}
