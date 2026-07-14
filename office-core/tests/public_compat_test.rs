//! Compile-level guards for Rust interfaces that predate Phase 2.

use office_core::Result;
use office_core::layout::WordDoc;
use office_core::layout::numbering_layout::ListCounters;
use office_core::layout::table::{RowLayout, layout_table};
use office_core::oxml::wml::{AnchorOrigin, FloatingTableProps, Table};
use office_core::profile::Warning;
use std::collections::HashMap;

type LegacyLayoutTable = fn(
    &WordDoc,
    &mut Vec<Warning>,
    &Table,
    &HashMap<String, String>,
    f64,
    f64,
    &mut ListCounters,
) -> Result<Vec<RowLayout>>;

#[test]
fn layout_table_keeps_legacy_public_return_type() {
    let _: LegacyLayoutTable = layout_table;
}

#[test]
fn floating_table_props_keep_legacy_wrap_padding_field() {
    let legacy = FloatingTableProps {
        x_pt: 1.0,
        y_pt: 2.0,
        horizontal: AnchorOrigin::Margin,
        vertical: AnchorOrigin::Paragraph,
        wrap_padding_pt: 9.35,
    };
    let FloatingTableProps {
        x_pt,
        y_pt,
        horizontal,
        vertical,
        wrap_padding_pt,
    } = legacy;
    assert_eq!((x_pt, y_pt, wrap_padding_pt), (1.0, 2.0, 9.35));
    assert_eq!(horizontal, AnchorOrigin::Margin);
    assert_eq!(vertical, AnchorOrigin::Paragraph);
}
