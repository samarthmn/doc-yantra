//! A deliberately bounded legacy VML Note envelope. Other VML is rejected.
use super::paginate::{DrawingAnchor, DrawingMarker, XlsxDrawing};
use crate::opc::{Package, TargetMode};
use crate::oxml::Rgb;
use crate::oxml::sml::{Worksheet, parse_required_a1};
use crate::{OfficeError, Result, UnsupportedCode};
use std::collections::{HashMap, HashSet};
const V: &str = "urn:schemas-microsoft-com:vml";
const X: &str = "urn:schemas-microsoft-com:office:excel";
const S: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
fn unsupported(detail: &str) -> OfficeError {
    OfficeError::unsupported(
        UnsupportedCode::UnknownElement,
        format!("legacy Note: {detail}"),
    )
}
fn xml<'a>(pkg: &'a Package, part: &str) -> Result<&'a str> {
    pkg.part(part)
        .and_then(|b| std::str::from_utf8(b).ok())
        .ok_or_else(|| unsupported("missing or invalid related part"))
}
pub(super) fn resolve(pkg: &Package, part: &str, sheet: &Worksheet) -> Result<Vec<XlsxDrawing>> {
    let Some(id) = &sheet.legacy_drawing_rel_id else {
        return Ok(Vec::new());
    };
    let relationships = pkg.relationships(part)?;
    let vml = relationships
        .iter()
        .find(|rel| &rel.id == id)
        .ok_or_else(|| unsupported("missing VML relationship"))?;
    if vml.mode != TargetMode::Internal || !vml.rel_type.ends_with("/vmlDrawing") {
        return Err(unsupported("external or invalid VML relationship"));
    }
    let comments = relationships
        .iter()
        .filter(|r| r.rel_type.ends_with("/comments"))
        .collect::<Vec<_>>();
    if comments.len() != 1 || comments[0].mode != TargetMode::Internal {
        return Err(unsupported(
            "missing, external, or ambiguous comments relationship",
        ));
    }
    let comments_doc = roxmltree::Document::parse(xml(pkg, &comments[0].target)?)
        .map_err(|_| unsupported("malformed comments"))?;
    if !comments_doc.root_element().has_tag_name((S, "comments")) {
        return Err(unsupported("invalid comments root"));
    }
    let mut texts = HashMap::new();
    for node in comments_doc
        .descendants()
        .filter(|n| n.has_tag_name((S, "comment")))
    {
        let cell = parse_required_a1(
            node.attribute("ref")
                .ok_or_else(|| unsupported("missing comment reference"))?,
            "legacy Note reference",
        )?;
        let mut text = String::new();
        for child in node.descendants().filter(|n| n.is_element()) {
            if child.tag_name().namespace() != Some(S) {
                return Err(unsupported("unknown comment namespace"));
            }
            match child.tag_name().name() {
                "comment" | "text" | "r" => {}
                "t" => text.push_str(child.text().unwrap_or("")),
                // Rich-text styling requires a separate run model; never flatten it.
                _ => return Err(unsupported("unsupported comment text formatting")),
            }
        }
        if text.is_empty() || text.len() > 32767 || texts.insert(cell, text).is_some() {
            return Err(unsupported("empty, oversized, or duplicate comment"));
        }
    }
    let drawing = roxmltree::Document::parse(xml(pkg, &vml.target)?)
        .map_err(|_| unsupported("malformed VML"))?;
    let definitions = drawing
        .root_element()
        .children()
        .filter(|n| n.has_tag_name((V, "shapetype")))
        .collect::<Vec<_>>();
    if definitions.len() != 1 {
        return Err(unsupported(
            "expected exactly one canonical Note shape definition",
        ));
    }
    let definition = definitions[0];
    if definition.attribute("id") != Some("_x0000_t202")
        || definition.attribute("coordsize") != Some("21600,21600")
        || definition.attribute(("urn:schemas-microsoft-com:office:office", "spt")) != Some("202")
        || definition.attribute("path") != Some("m,l,21600r21600,l21600,xe")
    {
        return Err(unsupported(
            "incomplete or noncanonical Note shape definition",
        ));
    }
    let mut notes = Vec::new();
    let mut seen = HashSet::new();
    for node in drawing.root_element().children().filter(|n| n.is_element()) {
        if node.has_tag_name((V, "shapetype")) {
            // Only Excel's canonical Note shape type. Shape geometry is rectangular.
            for a in node.attributes() {
                let valid = match a.name() {
                    "id" => a.value() == "_x0000_t202",
                    "coordsize" => a.value() == "21600,21600",
                    "spt" => a.value() == "202",
                    "path" => a.value() == "m,l,21600r21600,l21600,xe",
                    _ => false,
                };
                if !valid {
                    return Err(unsupported("custom Note shape type"));
                }
            }
            if node.attribute("id") != Some("_x0000_t202") {
                return Err(unsupported("unknown shape type"));
            }
            for child in node.children().filter(|n| n.is_element()) {
                let valid = (child.has_tag_name((V, "stroke"))
                    && child
                        .attributes()
                        .all(|a| a.name() == "joinstyle" && a.value() == "miter"))
                    || (child.has_tag_name((V, "path"))
                        && child.attributes().all(|a| {
                            matches!(
                                (a.name(), a.value()),
                                ("gradientshapeok", "t") | ("connecttype", "rect")
                            )
                        }));
                if !valid || child.children().any(|n| n.is_element()) {
                    return Err(unsupported("custom Note shape geometry"));
                }
            }
            continue;
        }
        if !node.has_tag_name((V, "shape")) {
            return Err(unsupported("unknown VML element"));
        }
        if node.attribute("type") != Some("#_x0000_t202") {
            return Err(unsupported("unknown shape"));
        }
        for a in node.attributes() {
            if !matches!(
                a.name(),
                "id" | "type" | "style" | "fillcolor" | "strokecolor" | "strokeweight"
            ) {
                return Err(unsupported("unknown shape attribute"));
            }
        }
        if node
            .attribute("strokecolor")
            .is_some_and(|v| !matches!(v, "black" | "#000000"))
            || node.attribute("strokeweight").is_some_and(|v| v != "1pt")
        {
            return Err(unsupported("unsupported Note border"));
        }
        let mut style = HashMap::new();
        for pair in node
            .attribute("style")
            .unwrap_or("")
            .split(';')
            .filter(|p| !p.trim().is_empty())
        {
            let (key, value) = pair
                .split_once(':')
                .ok_or_else(|| unsupported("malformed style"))?;
            let key = key.trim();
            let value = value.trim();
            if !matches!(
                key,
                "position"
                    | "margin-left"
                    | "margin-top"
                    | "width"
                    | "height"
                    | "visibility"
                    | "z-index"
            ) || style.insert(key, value).is_some()
            {
                return Err(unsupported("unknown or duplicate Note style"));
            }
        }
        if style.get("z-index").is_some_and(|value| *value != "1") {
            return Err(unsupported("unsupported Note stacking order"));
        }
        if style.get("position") != Some(&"absolute") {
            return Err(unsupported("non-absolute Note"));
        }
        let length = |key| -> Result<f64> {
            let n = style
                .get(key)
                .and_then(|s| s.strip_suffix("pt"))
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 10000.0)
                .ok_or_else(|| unsupported("invalid Note geometry"))?;
            Ok(n)
        };
        let width = length("width")?;
        let height = length("height")?;
        length("margin-left")?;
        length("margin-top")?;
        if width <= 4.0 || height <= 4.0 {
            return Err(unsupported("empty Note geometry"));
        }
        let visible = match style.get("visibility").copied().unwrap_or("visible") {
            "visible" => true,
            "hidden" => false,
            _ => return Err(unsupported("unknown visibility")),
        };
        let fill = match node.attribute("fillcolor").unwrap_or("#ffffe1") {
            "#ffffe1" => Rgb::new(255, 255, 225),
            "#ffffff" | "white" => Rgb::new(255, 255, 255),
            _ => return Err(unsupported("unsupported Note fill")),
        };
        let mut clients = Vec::new();
        for child in node.children().filter(|n| n.is_element()) {
            if child.has_tag_name((X, "ClientData")) {
                clients.push(child);
            } else if child.has_tag_name((V, "textbox")) {
                if child
                    .attributes()
                    .any(|a| a.name() != "style" || a.value() != "mso-direction-alt:auto")
                    || child
                        .descendants()
                        .any(|n| n.is_text() && !n.text().unwrap_or("").trim().is_empty())
                {
                    return Err(unsupported("unsupported textbox content/style"));
                }
                for inner in child
                    .descendants()
                    .filter(|n| n.is_element() && *n != child)
                {
                    if inner.tag_name().name() != "div"
                        || inner.children().any(|n| n.is_element())
                        || inner
                            .attributes()
                            .any(|a| a.name() != "style" || a.value() != "text-align:left")
                    {
                        return Err(unsupported("unknown textbox element"));
                    }
                }
            } else {
                return Err(unsupported("unsupported Note decoration"));
            }
        }
        if clients.len() != 1 || clients[0].attribute("ObjectType") != Some("Note") {
            return Err(OfficeError::unsupported(
                UnsupportedCode::OleObject,
                "legacy VML contains a control or non-Note object",
            ));
        }
        let client = clients[0];
        if client.attributes().any(|a| a.name() != "ObjectType") {
            return Err(unsupported("unknown client-data attribute"));
        }
        let mut fields = HashSet::new();
        if client
            .children()
            .filter(|n| n.is_element())
            .any(|n| !fields.insert(n.tag_name().name()))
        {
            return Err(unsupported("duplicate client-data field"));
        }
        let value = |name| {
            client
                .children()
                .find(|n| n.has_tag_name((X, name)))
                .and_then(|n| n.text())
        };
        for child in client.children().filter(|n| n.is_element()) {
            if child.tag_name().namespace() != Some(X)
                || !matches!(
                    child.tag_name().name(),
                    "Row"
                        | "Column"
                        | "Anchor"
                        | "MoveWithCells"
                        | "SizeWithCells"
                        | "AutoFill"
                        | "Visible"
                        | "PrintObject"
                )
            {
                return Err(unsupported("unknown client-data field"));
            }
        }
        let client_visible = match client.children().find(|n| n.has_tag_name((X, "Visible"))) {
            None => false,
            Some(node) => match node.text().unwrap_or("").trim() {
                "" | "True" | "true" | "t" | "1" => true,
                "False" | "false" | "f" | "0" => false,
                _ => return Err(unsupported("unknown client visibility")),
            },
        };
        if client_visible != visible {
            return Err(unsupported("conflicting Note visibility"));
        }
        let print_object = match value("PrintObject") {
            None | Some("True") | Some("true") | Some("1") => true,
            Some("False") | Some("false") | Some("0") => false,
            _ => return Err(unsupported("unknown print visibility")),
        };
        let row: u32 = value("Row")
            .and_then(|v| v.trim().parse().ok())
            .ok_or_else(|| unsupported("missing row"))?;
        let col: u32 = value("Column")
            .and_then(|v| v.trim().parse().ok())
            .ok_or_else(|| unsupported("missing column"))?;
        let text = texts
            .get(&(col, row))
            .ok_or_else(|| unsupported("Note has no matching comment"))?;
        if !seen.insert((row, col)) {
            return Err(unsupported("duplicate Note shape"));
        }
        let anchor = value("Anchor")
            .ok_or_else(|| unsupported("missing anchor"))?
            .split(',')
            .map(|v| v.trim().parse::<u32>())
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| unsupported("malformed anchor"))?;
        if anchor.len() != 8
            || anchor[0] >= 16384
            || anchor[4] >= 16384
            || anchor[2] >= 1048576
            || anchor[6] >= 1048576
            || anchor[1] > 1024
            || anchor[3] > 1024
            || anchor[5] > 1024
            || anchor[7] > 1024
        {
            return Err(unsupported("out-of-range anchor"));
        }
        if (anchor[4], anchor[5]) <= (anchor[0], anchor[1])
            || (anchor[6], anchor[7]) <= (anchor[2], anchor[3])
        {
            return Err(unsupported("reversed anchor"));
        }
        if visible && sheet.print_comments && print_object {
            notes.push(XlsxDrawing::Note {
                anchor: DrawingAnchor::OneCell {
                    from: DrawingMarker {
                        col: anchor[0],
                        col_off_pt: f64::from(anchor[1]) * 0.75,
                        row: anchor[2],
                        row_off_pt: f64::from(anchor[3]) * 0.75,
                    },
                    width_pt: width,
                    height_pt: height,
                },
                text: text.clone(),
                fill,
            });
        }
    }
    if seen.len() != texts.len() {
        return Err(unsupported("unmatched comments or VML shapes"));
    }
    Ok(notes)
}
