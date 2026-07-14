//! Compile-level guards for Rust interfaces that predate Phase 2.

use office_core::Result;
use office_core::layout::WordDoc;
use office_core::layout::numbering_layout::ListCounters;
use office_core::layout::table::{RowLayout, layout_table};
use office_core::oxml::wml::{FloatingTableProps, Table};
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
    fn legacy_field_access(props: &FloatingTableProps) -> f64 {
        props.wrap_padding_pt
    }
    let _ = legacy_field_access;
}
