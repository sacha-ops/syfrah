/// How a resource is displayed to the user.
#[derive(Debug, Clone)]
pub struct PresentationDef {
    pub table: Option<TableDef>,
    pub detail: Option<DetailDef>,
}

/// Table layout for `list` output.
#[derive(Debug, Clone)]
pub struct TableDef {
    pub columns: Vec<ColumnDef>,
    pub default_sort: Option<&'static str>,
    pub empty_message: Option<&'static str>,
}

/// A single column in the table.
#[derive(Debug, Clone)]
pub struct ColumnDef {
    /// Header text
    pub header: &'static str,
    /// Field name to extract from the resource JSON
    pub field: &'static str,
    /// Column width behavior
    pub width: ColumnWidth,
    /// Text alignment
    pub align: Align,
    /// How to format the value
    pub format: DisplayFormat,
}

/// Column width strategy.
#[derive(Debug, Clone)]
pub enum ColumnWidth {
    Auto,
    Fixed(usize),
    Min(usize),
    Max(usize),
}

/// Text alignment.
#[derive(Debug, Clone)]
pub enum Align {
    Left,
    Right,
}

/// Detail view layout for `get` output.
#[derive(Debug, Clone)]
pub struct DetailDef {
    pub sections: Vec<DetailSection>,
}

/// A group of fields in the detail view.
#[derive(Debug, Clone)]
pub struct DetailSection {
    pub title: Option<&'static str>,
    pub fields: Vec<DetailField>,
}

/// A single field in the detail view.
#[derive(Debug, Clone)]
pub struct DetailField {
    pub label: &'static str,
    pub field: &'static str,
    pub format: DisplayFormat,
}

/// How to format a value for display.
#[derive(Debug, Clone)]
pub enum DisplayFormat {
    /// Display as-is
    Plain,
    /// Format bytes: 1024 → "1 KiB"
    Bytes,
    /// Format duration: 3600 → "1h"
    Duration,
    /// Format unix timestamp: 1775393207 → "2026-04-05 15:33 UTC"
    Timestamp,
    /// Color based on value (e.g. "Available" = green, "NotReady" = yellow)
    Status,
    /// Mask sensitive values: "abc123" → "****...23"
    Masked,
    /// Boolean: true → "yes", false → "no"
    YesNo,
}

// ── Convenience constructors ───────────────────────────────

impl ColumnDef {
    pub fn new(header: &'static str, field: &'static str) -> Self {
        Self {
            header,
            field,
            width: ColumnWidth::Auto,
            align: Align::Left,
            format: DisplayFormat::Plain,
        }
    }

    pub fn fixed(mut self, width: usize) -> Self {
        self.width = ColumnWidth::Fixed(width);
        self
    }

    pub fn right(mut self) -> Self {
        self.align = Align::Right;
        self
    }

    pub fn with_format(mut self, format: DisplayFormat) -> Self {
        self.format = format;
        self
    }
}

impl DetailField {
    pub fn new(label: &'static str, field: &'static str) -> Self {
        Self {
            label,
            field,
            format: DisplayFormat::Plain,
        }
    }

    pub fn with_format(mut self, format: DisplayFormat) -> Self {
        self.format = format;
        self
    }
}

impl PresentationDef {
    pub fn none() -> Self {
        Self {
            table: None,
            detail: None,
        }
    }
}
