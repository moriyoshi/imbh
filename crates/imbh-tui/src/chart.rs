//! Metric-chart geometry and scaling, shared by the renderer and the mascot's chart ride.
//!
//! [`chart_graph_area`] and [`chart_point_cell`] reproduce ratatui-widgets 0.3.2's `Chart` layout
//! (see `ratatui-widgets/src/{chart.rs,canvas.rs}`) so the mascot can walk the *actual* rendered
//! line, and [`ascii_chart`] is the `--ascii` fallback plot.

use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

/// The rendered geometry of the metric time-series chart, published by
/// [`draw_metric_detail`](crate::ui::metrics::draw_metric_detail) through
/// [`App::chart_geom`](crate::app::App::chart_geom) so [`ChartRide`](crate::mascot::ChartRide) can
/// walk the *actual* on-screen datapoints. `cells` holds the terminal cell of each finite datapoint,
/// left-to-right; `graph` is the plotting rectangle.
#[derive(Debug, Clone)]
pub(crate) struct ChartGeometry {
    pub(crate) graph: Rect,
    pub(crate) cells: Vec<(u16, u16)>,
}

/// The plotting rectangle ratatui reserves inside a `Chart` widget area: the block border is removed
/// first, then space is taken on the left for y-axis labels (+1 for the axis line) and two rows at the
/// bottom for the x-axis labels + line. `block_inner` is `block.inner(plot_area)`.
pub(crate) fn chart_graph_area(
    block_inner: Rect,
    y_labels: &[String],
    x_first_label: &str,
) -> Option<Rect> {
    if block_inner.width == 0 || block_inner.height == 0 {
        return None;
    }
    let has_y_axis = !y_labels.is_empty();
    let y_max_w = y_labels.iter().map(|s| s.width() as u16).max().unwrap_or(0);
    // x-axis labels default to Left alignment, so the first one overhangs the y-axis gutter by width-1.
    let x_overhang = (x_first_label.width() as u16).saturating_sub(u16::from(has_y_axis));
    let label_left = y_max_w.max(x_overhang).min(block_inner.width / 3);

    let mut x = block_inner.left();
    let mut y = block_inner.bottom() - 1;
    if y > block_inner.top() {
        y -= 1; // x-axis labels row
    }
    x += label_left;
    if y > block_inner.top() {
        y -= 1; // x-axis line row
    }
    if has_y_axis && x + 1 < block_inner.right() {
        x += 1; // y-axis line column
    }
    let graph_w = block_inner.right().saturating_sub(x);
    let graph_h = y.saturating_sub(block_inner.top()).saturating_add(1);
    if graph_w == 0 || graph_h == 0 {
        return None;
    }
    Some(Rect::new(x, block_inner.top(), graph_w, graph_h))
}

/// The terminal cell a datapoint `(x, y)` renders into within `graph`, using ratatui's Braille canvas
/// mapping (2×4 dots per cell). `None` if the point is out of the axis bounds (ratatui would not plot it).
pub(crate) fn chart_point_cell(
    graph: Rect,
    x_min: f64,
    x_max: f64,
    y_min: f64,
    y_max: f64,
    x: f64,
    y: f64,
) -> Option<(u16, u16)> {
    if x < x_min || x > x_max || y < y_min || y > y_max {
        return None;
    }
    let (w, h) = (x_max - x_min, y_max - y_min);
    if w <= 0.0 || h <= 0.0 || graph.width == 0 || graph.height == 0 {
        return None;
    }
    let res_x = graph.width as f64 * 2.0;
    let res_y = graph.height as f64 * 4.0;
    let dot_x = ((x - x_min) * (res_x - 1.0) / w).round() as u32;
    let dot_y = ((y_max - y) * (res_y - 1.0) / h).round() as u32;
    let col = graph.left() + (dot_x / 2) as u16;
    let row = graph.top() + (dot_y / 4) as u16;
    Some((col, row))
}

pub(crate) fn chart_values(values: impl Iterator<Item = f64>) -> Vec<u64> {
    let values = values.filter(|value| value.is_finite()).collect::<Vec<_>>();
    let maximum = values
        .iter()
        .copied()
        .fold(0.0_f64, f64::max)
        .max(f64::EPSILON);
    values
        .into_iter()
        .map(|value| ((value.max(0.0) / maximum) * 1_000.0) as u64)
        .collect()
}

pub(crate) fn ascii_chart(values: &[u64], width: usize, height: usize) -> String {
    if values.is_empty() || width == 0 || height == 0 {
        return String::new();
    }
    let columns = (0..width)
        .map(|column| {
            let index = column.saturating_mul(values.len()) / width;
            values[index.min(values.len() - 1)].min(1_000)
        })
        .collect::<Vec<_>>();
    (0..height)
        .rev()
        .map(|row| {
            let threshold = ((row + 1) as u64 * 1_000).div_ceil(height as u64);
            columns
                .iter()
                .map(|value| if *value >= threshold { '#' } else { ' ' })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_chart_uses_only_ascii_and_requested_dimensions() {
        let rendered = ascii_chart(&[0, 500, 1_000], 6, 3);
        assert!(rendered.is_ascii());
        let rows = rendered.lines().collect::<Vec<_>>();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.len() == 6));
    }

    #[test]
    fn chart_point_cell_matches_ratatui_corners() {
        // A block-inner region; `chart_graph_area` reserves the axis gutters inside it.
        let y_labels = ["0".to_owned(), "5".to_owned(), "10".to_owned()];
        let graph = chart_graph_area(Rect::new(0, 0, 40, 12), &y_labels, "12:00:00").unwrap();
        assert!(graph.width > 0 && graph.height > 0);
        assert!(graph.left() >= 1 && graph.bottom() <= 12);

        // Top-left of the data (x_min, y_max) maps to the graph origin; bottom-right (x_max, y_min) to
        // the far corner cell — the Braille 2×4 mapping ratatui uses.
        let tl = chart_point_cell(graph, 0.0, 10.0, 0.0, 10.0, 0.0, 10.0).unwrap();
        assert_eq!(tl, (graph.left(), graph.top()));
        let br = chart_point_cell(graph, 0.0, 10.0, 0.0, 10.0, 10.0, 0.0).unwrap();
        assert_eq!(br, (graph.right() - 1, graph.bottom() - 1));
        // Out of the axis bounds → not plotted.
        assert!(chart_point_cell(graph, 0.0, 10.0, 0.0, 10.0, 20.0, 5.0).is_none());
    }

    #[test]
    fn chart_scaling_is_bounded() {
        assert_eq!(
            chart_values([0.0, 5.0, 10.0].into_iter()),
            vec![0, 500, 1000]
        );
    }
}
