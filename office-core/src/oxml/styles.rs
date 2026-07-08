//! `word/styles.xml`: `docDefaults` plus the style table, and the cascade that
//! resolves effective run/paragraph properties.
//!
//! Cascade order (least → most specific), matching Word:
//! `docDefaults` → default paragraph style → paragraph style chain (`basedOn`)
//! → character style chain → direct properties.

use super::wml::{ParaProps, RunProps, parse_para_props, parse_run_props};
use super::{W, attr, child, children};
use crate::{OfficeError, Result};
use std::collections::HashMap;

/// Maximum `basedOn` chain depth (cycle guard).
const MAX_CHAIN: usize = 16;

/// Word's default font size when `docDefaults` omits `w:sz` (points).
pub const DEFAULT_SIZE_PT: f64 = 11.0;

/// The category of a `w:style`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StyleType {
    /// A paragraph style.
    Paragraph,
    /// A character style.
    Character,
    /// A table style.
    Table,
    /// A numbering style.
    Numbering,
}

impl StyleType {
    fn parse(v: Option<&str>) -> StyleType {
        match v {
            Some("character") => StyleType::Character,
            Some("table") => StyleType::Table,
            Some("numbering") => StyleType::Numbering,
            _ => StyleType::Paragraph,
        }
    }
}

/// A single parsed style.
#[derive(Debug, Clone)]
pub struct Style {
    /// Style category.
    pub style_type: StyleType,
    /// The style id.
    pub style_id: String,
    /// The parent style id (`w:basedOn`).
    pub based_on: Option<String>,
    /// The style's paragraph properties.
    pub ppr: ParaProps,
    /// The style's run properties.
    pub rpr: RunProps,
    /// Whether this style is the default for its type.
    pub is_default: bool,
}

/// Resolves effective properties through the style cascade.
#[derive(Debug, Clone, Default)]
pub struct StyleResolver {
    styles: HashMap<String, Style>,
    default_ppr: ParaProps,
    default_rpr: RunProps,
    default_para_style: Option<String>,
}

impl StyleResolver {
    /// Parses `word/styles.xml`. An absent styles part yields an empty resolver
    /// (everything falls back to hardcoded Word defaults).
    pub fn parse(xml: &str) -> Result<StyleResolver> {
        let doc = roxmltree::Document::parse(xml)
            .map_err(|e| OfficeError::Invalid(format!("styles.xml XML: {e}")))?;
        let root = doc.root_element();
        let mut resolver = StyleResolver::default();

        if let Some(defaults) = child(root, W, "docDefaults") {
            if let Some(rpr) = child(defaults, W, "rPrDefault").and_then(|d| child(d, W, "rPr")) {
                resolver.default_rpr = parse_run_props(rpr);
            }
            if let Some(ppr) = child(defaults, W, "pPrDefault").and_then(|d| child(d, W, "pPr")) {
                resolver.default_ppr = parse_para_props(ppr);
            }
        }

        for node in children(root) {
            if !super::is(node, W, "style") {
                continue;
            }
            let style_type = StyleType::parse(attr(node, W, "type"));
            let Some(style_id) = attr(node, W, "styleId") else {
                continue;
            };
            let based_on = child(node, W, "basedOn")
                .and_then(|b| attr(b, W, "val"))
                .map(str::to_string);
            let ppr = child(node, W, "pPr")
                .map(parse_para_props)
                .unwrap_or_default();
            let rpr = child(node, W, "rPr")
                .map(parse_run_props)
                .unwrap_or_default();
            let is_default =
                super::on_off(attr(node, W, "default")) && attr(node, W, "default").is_some();

            if is_default && style_type == StyleType::Paragraph {
                resolver.default_para_style = Some(style_id.to_string());
            }
            resolver.styles.insert(
                style_id.to_string(),
                Style {
                    style_type,
                    style_id: style_id.to_string(),
                    based_on,
                    ppr,
                    rpr,
                    is_default,
                },
            );
        }
        Ok(resolver)
    }

    /// Looks up a style by id.
    pub fn get(&self, id: &str) -> Option<&Style> {
        self.styles.get(id)
    }

    /// The `docDefaults` run properties.
    pub fn default_run_props(&self) -> &RunProps {
        &self.default_rpr
    }

    /// Resolves a style's run properties through its `basedOn` chain (root-first,
    /// so more-derived styles win). Cycle-safe to [`MAX_CHAIN`].
    fn resolved_style_rpr(&self, id: &str) -> RunProps {
        let mut chain = self.chain(id);
        // chain is leaf → root; fold root → leaf so the leaf overlays last.
        let mut acc = RunProps::default();
        while let Some(style) = chain.pop() {
            acc = style.rpr.merge_over(&acc);
        }
        acc
    }

    /// Resolves a style's paragraph properties through its `basedOn` chain.
    fn resolved_style_ppr(&self, id: &str) -> ParaProps {
        let mut chain = self.chain(id);
        let mut acc = ParaProps::default();
        while let Some(style) = chain.pop() {
            acc = style.ppr.merge_over(&acc);
        }
        acc
    }

    /// Returns the `basedOn` chain for `id`, leaf-first, cycle-safe.
    fn chain(&self, id: &str) -> Vec<&Style> {
        let mut out = Vec::new();
        let mut cur = Some(id.to_string());
        let mut seen = std::collections::HashSet::new();
        while let Some(cid) = cur {
            if out.len() >= MAX_CHAIN || !seen.insert(cid.clone()) {
                break;
            }
            match self.styles.get(&cid) {
                Some(style) => {
                    out.push(style);
                    cur = style.based_on.clone();
                }
                None => break,
            }
        }
        out
    }

    /// Effective run properties for a run in a paragraph.
    ///
    /// `docDefaults` → default paragraph style → paragraph style chain → run
    /// character style chain → direct run properties. An absent effective size
    /// defaults to [`DEFAULT_SIZE_PT`] (font defaulting needs the theme and is
    /// applied by the layout engine).
    pub fn run_props(&self, para: &ParaProps, run: &RunProps) -> RunProps {
        let mut acc = self.default_rpr.clone();
        if let Some(dp) = &self.default_para_style {
            acc = self.resolved_style_rpr(dp).merge_over(&acc);
        }
        if let Some(ps) = &para.style_id {
            acc = self.resolved_style_rpr(ps).merge_over(&acc);
        }
        if let Some(cs) = &run.style_id {
            acc = self.resolved_style_rpr(cs).merge_over(&acc);
        }
        acc = run.merge_over(&acc);
        if acc.size_pt.is_none() {
            acc.size_pt = Some(DEFAULT_SIZE_PT);
        }
        acc
    }

    /// Effective paragraph properties.
    ///
    /// `docDefaults` → default paragraph style → paragraph style chain → direct
    /// paragraph properties. (Numbering-level `pPr` merges between the style
    /// chain and direct properties; the numbering layer applies that overlay.)
    pub fn para_props(&self, para: &ParaProps) -> ParaProps {
        let mut acc = self.default_ppr.clone();
        if let Some(dp) = &self.default_para_style {
            acc = self.resolved_style_ppr(dp).merge_over(&acc);
        }
        if let Some(ps) = &para.style_id {
            acc = self.resolved_style_ppr(ps).merge_over(&acc);
        }
        acc = para.merge_over(&acc);
        acc
    }
}
