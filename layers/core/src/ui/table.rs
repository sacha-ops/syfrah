//! Table rendering — responsive, truncating, no-border.
//!
//! Tables auto-size columns to fit the terminal. When the terminal is too
//! narrow, rightmost columns are hidden and values are truncated with "…".

use super::color;
use unicode_width::UnicodeWidthStr;

/// A table to render.
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    /// Columns that contain status values (colored dots).
    status_columns: Vec<usize>,
    /// Minimum priority — columns with lower priority are hidden first.
    /// Lower number = hidden first when terminal is narrow.
    column_priority: Vec<u8>,
}

impl Table {
    pub fn new(headers: Vec<&str>) -> Self {
        let len = headers.len();
        Self {
            headers: headers.into_iter().map(|s| s.to_string()).collect(),
            rows: Vec::new(),
            status_columns: Vec::new(),
            column_priority: vec![50; len], // default priority 50
        }
    }

    /// Mark a column as containing status values (will be colored).
    pub fn status_column(mut self, index: usize) -> Self {
        self.status_columns.push(index);
        self
    }

    /// Set column priority (lower = hidden first when narrow).
    pub fn priority(mut self, index: usize, priority: u8) -> Self {
        if index < self.column_priority.len() {
            self.column_priority[index] = priority;
        }
        self
    }

    /// Add a row.
    pub fn row(mut self, cells: Vec<&str>) -> Self {
        self.rows
            .push(cells.into_iter().map(|s| s.to_string()).collect());
        self
    }

    /// Add a row from Strings.
    pub fn row_owned(mut self, cells: Vec<String>) -> Self {
        self.rows.push(cells);
        self
    }

    /// Render to stderr with colors.
    pub fn render(&self, term_width: u16, use_color: bool) {
        if self.rows.is_empty() {
            return;
        }

        let max_width = term_width as usize;
        let ncols = self.headers.len();

        // Calculate natural column widths
        let mut widths: Vec<usize> = (0..ncols)
            .map(|i| {
                let header_w = self.headers[i].width();
                let max_data_w = self
                    .rows
                    .iter()
                    .map(|row| row.get(i).map(|c| strip_ansi_width(c)).unwrap_or(0))
                    .max()
                    .unwrap_or(0);
                header_w.max(max_data_w)
            })
            .collect();

        // Determine which columns to show (responsive)
        let gap = 2usize; // space between columns
        let visible = visible_columns(&widths, &self.column_priority, max_width, gap);

        // Truncate columns that are too wide
        let available = max_width.saturating_sub(4); // 2 indent + some margin
        let total_gaps = visible.len().saturating_sub(1) * gap;
        let available_for_data = available.saturating_sub(total_gaps);
        let per_col_max = if visible.is_empty() {
            available_for_data
        } else {
            available_for_data / visible.len()
        };
        for &i in &visible {
            if widths[i] > per_col_max && per_col_max > 4 {
                widths[i] = per_col_max;
            }
        }

        // Print header
        let header_line: String = visible
            .iter()
            .map(|&i| {
                let h = &self.headers[i];
                if use_color {
                    format!(
                        "{:<width$}",
                        color::bold(h),
                        width = widths[i] + color::bold(h).len() - h.len()
                    )
                } else {
                    format!("{:<width$}", h, width = widths[i])
                }
            })
            .collect::<Vec<_>>()
            .join(&" ".repeat(gap));
        eprintln!("  {header_line}");

        // Separator
        let sep_len: usize = visible.iter().map(|&i| widths[i]).sum::<usize>()
            + (visible.len().saturating_sub(1)) * gap;
        eprintln!("  {}", "─".repeat(sep_len));

        // Rows
        for row in &self.rows {
            let line: String = visible
                .iter()
                .map(|&i| {
                    let cell = row.get(i).map(|s| s.as_str()).unwrap_or("—");
                    let is_status = self.status_columns.contains(&i);

                    if is_status && use_color {
                        let colored = color::status_dot(cell);
                        // Status dots add ANSI codes, so we need to pad based on visible width
                        let visible_len = strip_ansi_width(&colored);
                        let pad = widths[i].saturating_sub(visible_len);
                        format!("{colored}{}", " ".repeat(pad))
                    } else {
                        let truncated = truncate(cell, widths[i]);
                        format!("{:<width$}", truncated, width = widths[i])
                    }
                })
                .collect::<Vec<_>>()
                .join(&" ".repeat(gap));
            eprintln!("  {line}");
        }
    }

    /// Render as TSV (pipe/quiet mode).
    pub fn render_tsv(&self) {
        for row in &self.rows {
            let line: String = row
                .iter()
                .map(|c| strip_ansi(c))
                .collect::<Vec<_>>()
                .join("\t");
            println!("{line}");
        }
    }

    /// Render as JSON array.
    pub fn render_json(&self) {
        let items: Vec<serde_json::Value> = self
            .rows
            .iter()
            .map(|row| {
                let mut obj = serde_json::Map::new();
                for (i, header) in self.headers.iter().enumerate() {
                    let val = row.get(i).map(|s| strip_ansi(s)).unwrap_or_default();
                    obj.insert(
                        header.to_lowercase().replace(' ', "_"),
                        serde_json::Value::String(val),
                    );
                }
                serde_json::Value::Object(obj)
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".to_string())
        );
    }

    /// Row count.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Determine which columns to show given terminal width.
fn visible_columns(
    widths: &[usize],
    priorities: &[u8],
    max_width: usize,
    gap: usize,
) -> Vec<usize> {
    // Start with all columns
    let mut indices: Vec<usize> = (0..widths.len()).collect();

    loop {
        let total: usize = indices.iter().map(|&i| widths[i]).sum::<usize>()
            + indices.len().saturating_sub(1) * gap
            + 4; // indent

        if total <= max_width || indices.len() <= 1 {
            break;
        }

        // Remove the column with lowest priority (rightmost if tied)
        let min_priority = indices
            .iter()
            .map(|&i| priorities.get(i).copied().unwrap_or(50))
            .min()
            .unwrap_or(50);

        // Find the rightmost column with that priority
        if let Some(pos) = indices
            .iter()
            .rposition(|&i| priorities.get(i).copied().unwrap_or(50) == min_priority)
        {
            indices.remove(pos);
        } else {
            break;
        }
    }

    indices
}

/// Truncate a string to fit width, adding "…" if truncated.
fn truncate(s: &str, max_width: usize) -> String {
    let w = s.width();
    if w <= max_width {
        return s.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    // Take chars until we reach max_width - 1, then add "…"
    let mut result = String::new();
    let mut current_width = 0;
    for c in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if current_width + cw >= max_width {
            result.push('…');
            break;
        }
        result.push(c);
        current_width += cw;
    }
    result
}

/// Strip ANSI escape codes and return visible width.
fn strip_ansi_width(s: &str) -> usize {
    let stripped = strip_ansi(s);
    stripped.width()
}

/// Strip ANSI escape codes from a string.
fn strip_ansi(s: &str) -> String {
    console::strip_ansi_codes(s).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_renders_without_panic() {
        let t = Table::new(vec!["NAME", "STATUS", "CPU"])
            .status_column(1)
            .row(vec!["web-1", "running", "2"])
            .row(vec!["web-2", "creating", "1"]);
        t.render(80, false);
    }

    #[test]
    fn table_renders_with_color() {
        let t = Table::new(vec!["NAME", "STATUS"])
            .status_column(1)
            .row(vec!["web-1", "running"]);
        t.render(80, true);
    }

    #[test]
    fn table_tsv() {
        let t = Table::new(vec!["NAME", "STATUS"]).row(vec!["web-1", "running"]);
        t.render_tsv();
    }

    #[test]
    fn table_json() {
        let t = Table::new(vec!["NAME", "STATUS"]).row(vec!["web-1", "running"]);
        t.render_json();
    }

    #[test]
    fn table_empty() {
        let t = Table::new(vec!["NAME"]);
        assert!(t.is_empty());
        t.render(80, false); // no panic, no output
    }

    #[test]
    fn truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_long() {
        let t = truncate("hello world this is long", 10);
        assert!(t.width() <= 10, "visible width {} > 10: '{}'", t.width(), t);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn visible_columns_all_fit() {
        let widths = vec![10, 10, 10];
        let priorities = vec![50, 50, 50];
        let vis = visible_columns(&widths, &priorities, 80, 2);
        assert_eq!(vis, vec![0, 1, 2]);
    }

    #[test]
    fn visible_columns_hides_low_priority() {
        let widths = vec![20, 20, 20, 20];
        let priorities = vec![90, 50, 30, 10]; // col 3 hidden first
        let vis = visible_columns(&widths, &priorities, 50, 2);
        assert!(!vis.contains(&3)); // lowest priority hidden
    }

    #[test]
    fn strip_ansi_removes_codes() {
        let colored = console::style("hello").green().to_string();
        assert_eq!(strip_ansi(&colored), "hello");
    }

    #[test]
    fn table_row_count() {
        let t = Table::new(vec!["A"]).row(vec!["1"]).row(vec!["2"]);
        assert_eq!(t.len(), 2);
    }
}
