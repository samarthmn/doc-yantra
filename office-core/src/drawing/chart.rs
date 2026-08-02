//! Cached-data DrawingML chart parsing and layout primitives.

use crate::fonts::{FontDb, matching};
use crate::layout::{GlyphRun, Item, PathCommand, Positioned, Stroke};
use crate::oxml::pml::ColorSpec;
use crate::oxml::theme::Theme;
use crate::oxml::{Rgb, Rgba};
use crate::pptx::geometry::resolve_colorspec;
use crate::text::shape;
use crate::{OfficeError, Result, UnsupportedCode};
use roxmltree::Node;
use std::collections::BTreeMap;

const C: &str = "http://schemas.openxmlformats.org/drawingml/2006/chart";

/// Supported two-dimensional chart families.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChartKind {
    /// Vertical or horizontal bars.
    Bar {
        /// Whether bars run horizontally (`bar`) rather than vertically (`col`).
        horizontal: bool,
        /// Series grouping.
        stack: StackMode,
        /// Gap between category clusters, as a percentage of one bar width.
        gap_percent: f64,
    },
    /// Connected values, optionally with markers.
    Line {
        /// Whether marker symbols are requested.
        markers: bool,
    },
    /// Pie or doughnut sectors.
    Pie {
        /// Inner-radius fraction; absent for a pie.
        hole_ratio: Option<f64>,
    },
    /// Filled area series.
    Area {
        /// Series grouping.
        stack: StackMode,
    },
    /// Numeric x/y points.
    Scatter,
}

/// Bar/area series stacking mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackMode {
    /// Independent series.
    None,
    /// Values accumulate by category.
    Stacked,
    /// Values normalize to 100% by category.
    Percent,
}

/// One cached chart series.
#[derive(Debug, Clone)]
pub struct Series {
    /// Legend label.
    pub name: String,
    /// Category labels.
    pub categories: Vec<String>,
    /// Numeric y values.
    pub values: Vec<f64>,
    /// Numeric x values for scatter charts.
    pub x_values: Vec<f64>,
    /// Explicit series color.
    pub color: Option<ColorSpec>,
    /// Explicit per-point colors keyed by point index.
    pub point_colors: BTreeMap<usize, ColorSpec>,
    /// Whether cached values should be drawn as labels.
    pub show_values: bool,
    /// Optional data-label number format.
    pub label_format: Option<String>,
}

/// A parsed chart backed entirely by embedded caches.
#[derive(Debug, Clone)]
pub struct Chart {
    /// Chart family and grouping.
    pub kind: ChartKind,
    /// Cached series.
    pub series: Vec<Series>,
    /// Chart title.
    pub title: Option<String>,
    /// Category-axis title.
    pub category_axis_title: Option<String>,
    /// Value-axis title.
    pub value_axis_title: Option<String>,
    /// Legend position (`l`, `r`, `t`, `b`) or absent.
    pub legend_position: Option<String>,
    /// Value-axis number format.
    pub number_format: String,
    /// Whether major gridlines are requested.
    pub major_gridlines: bool,
}

impl Chart {
    /// Parses a `chart*.xml` part, rejecting missing caches and named unsupported families.
    pub fn parse(xml: &str) -> Result<Self> {
        let doc = crate::oxml::parse_xml(xml, "chart")?;
        validate_visible_chart_options(doc.root_element())?;
        for node in doc.descendants().filter(|node| node.is_element()) {
            let name = node.tag_name().name();
            if name.ends_with("3DChart")
                || matches!(
                    name,
                    "surfaceChart" | "radarChart" | "stockChart" | "bubbleChart"
                )
            {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::Chart,
                    format!("unsupported chart type {name}"),
                ));
            }
        }
        let plot = find(&doc.root_element(), "plotArea").ok_or_else(|| {
            OfficeError::unsupported(UnsupportedCode::Chart, "chart missing plotArea")
        })?;
        let families: Vec<_> = plot
            .children()
            .filter(|node| {
                node.is_element()
                    && node.tag_name().namespace() == Some(C)
                    && node.tag_name().name().ends_with("Chart")
            })
            .collect();
        if families.len() != 1 {
            let names = families
                .iter()
                .map(|node| node.tag_name().name())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(OfficeError::unsupported(
                UnsupportedCode::Chart,
                if families.is_empty() {
                    "chart has no supported plot family".to_string()
                } else {
                    format!("unsupported combo chart families: {names}")
                },
            ));
        }
        let family = *families.first().ok_or_else(|| {
            OfficeError::unsupported(UnsupportedCode::Chart, "chart has no supported plot family")
        })?;
        let kind = match family.tag_name().name() {
            "barChart" => {
                let grouping = val_child(family, "grouping");
                let stack = match grouping.as_deref() {
                    Some("stacked") => StackMode::Stacked,
                    Some("percentStacked") => StackMode::Percent,
                    None | Some("clustered" | "standard") => StackMode::None,
                    Some(other) => {
                        return Err(OfficeError::unsupported(
                            UnsupportedCode::Chart,
                            format!("unsupported barChart grouping {other}"),
                        ));
                    }
                };
                let gap_percent = match val_child(family, "gapWidth") {
                    Some(value) => value
                        .parse::<f64>()
                        .ok()
                        .filter(|gap| gap.is_finite() && (0.0..=500.0).contains(gap))
                        .ok_or_else(|| {
                            OfficeError::unsupported(
                                UnsupportedCode::Chart,
                                "barChart gapWidth must be between 0 and 500",
                            )
                        })?,
                    None => 150.0,
                };
                if let Some(overlap) = val_child(family, "overlap") {
                    let overlap = overlap.parse::<i32>().map_err(|_| {
                        OfficeError::unsupported(
                            UnsupportedCode::Chart,
                            "barChart overlap is invalid",
                        )
                    })?;
                    let expected = if stack == StackMode::None { 0 } else { 100 };
                    if overlap != expected {
                        return Err(OfficeError::unsupported(
                            UnsupportedCode::Chart,
                            format!("unsupported barChart overlap {overlap}; expected {expected}"),
                        ));
                    }
                }
                let horizontal = match val_child(family, "barDir").as_deref() {
                    None | Some("col") => false,
                    Some("bar") => true,
                    Some(other) => {
                        return Err(OfficeError::unsupported(
                            UnsupportedCode::Chart,
                            format!("unsupported barDir {other}"),
                        ));
                    }
                };
                ChartKind::Bar {
                    horizontal,
                    stack,
                    gap_percent,
                }
            }
            "lineChart" => {
                if val_child(family, "grouping")
                    .as_deref()
                    .is_some_and(|grouping| grouping != "standard")
                {
                    return Err(OfficeError::unsupported(
                        UnsupportedCode::Chart,
                        format!(
                            "unsupported lineChart grouping {}",
                            val_child(family, "grouping").unwrap_or_default()
                        ),
                    ));
                }
                ChartKind::Line {
                    markers: family.descendants().any(|node| {
                        is(node, "marker")
                            && child(node, "symbol").and_then(|symbol| symbol.attribute("val"))
                                != Some("none")
                    }),
                }
            }
            "pieChart" => ChartKind::Pie { hole_ratio: None },
            "doughnutChart" => {
                let hole = match val_child(family, "holeSize") {
                    Some(value) => value
                        .parse::<f64>()
                        .ok()
                        .filter(|hole| hole.is_finite() && (10.0..=90.0).contains(hole)),
                    None => Some(50.0),
                }
                .ok_or_else(|| {
                    OfficeError::unsupported(
                        UnsupportedCode::Chart,
                        "doughnut holeSize must be between 10 and 90",
                    )
                })? / 100.0;
                ChartKind::Pie {
                    hole_ratio: Some(hole),
                }
            }
            "areaChart" => ChartKind::Area {
                stack: match val_child(family, "grouping").as_deref() {
                    Some("stacked") => StackMode::Stacked,
                    Some("percentStacked") => StackMode::Percent,
                    None | Some("standard") => StackMode::None,
                    Some(other) => {
                        return Err(OfficeError::unsupported(
                            UnsupportedCode::Chart,
                            format!("unsupported areaChart grouping {other}"),
                        ));
                    }
                },
            },
            "scatterChart" => match val_child(family, "scatterStyle").as_deref() {
                None | Some("marker") => ChartKind::Scatter,
                Some(style) => {
                    return Err(OfficeError::unsupported(
                        UnsupportedCode::Chart,
                        format!("unsupported scatterStyle {style}"),
                    ));
                }
            },
            other => {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::Chart,
                    format!("unsupported chart type {other}"),
                ));
            }
        };
        if let Some(vary_colors) = child(family, "varyColors") {
            let requested = parse_on_off(vary_colors)?;
            let rendered = matches!(kind, ChartKind::Pie { .. });
            if requested != rendered {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::Chart,
                    format!("unsupported varyColors={requested}"),
                ));
            }
        }
        for smooth in family.descendants().filter(|node| is(*node, "smooth")) {
            if parse_on_off(smooth)? {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::Chart,
                    "smoothed chart lines are unsupported",
                ));
            }
        }
        if let Some(shape) = val_child(family, "shape")
            && shape != "box"
        {
            return Err(OfficeError::unsupported(
                UnsupportedCode::Chart,
                format!("unsupported bar shape {shape}"),
            ));
        }
        let mut series = Vec::new();
        for (series_index, ser) in family
            .children()
            .filter(|node| is(*node, "ser"))
            .enumerate()
        {
            let category_points = cache_strings(child(ser, "cat"))?;
            let value_points = cache_numbers(child(
                ser,
                if matches!(kind, ChartKind::Scatter) {
                    "yVal"
                } else {
                    "val"
                },
            ))?;
            let x_points = if matches!(kind, ChartKind::Scatter) {
                cache_numbers(child(ser, "xVal"))?
            } else {
                Vec::new()
            };
            let categories = category_points
                .iter()
                .map(|(_, value)| value.clone())
                .collect::<Vec<_>>();
            let values = value_points
                .iter()
                .map(|(_, value)| *value)
                .collect::<Vec<_>>();
            let x_values = x_points.iter().map(|(_, value)| *value).collect::<Vec<_>>();
            if !matches!(kind, ChartKind::Scatter) && categories.is_empty() {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::Chart,
                    format!(
                        "{} series {series_index} has no cached categories",
                        family.tag_name().name()
                    ),
                ));
            }
            if values.is_empty() || (matches!(kind, ChartKind::Scatter) && x_values.is_empty()) {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::Chart,
                    format!(
                        "{} series {series_index} has no cached numeric data",
                        family.tag_name().name()
                    ),
                ));
            }
            let aligned = if matches!(kind, ChartKind::Scatter) {
                x_points
                    .iter()
                    .map(|(index, _)| index)
                    .eq(value_points.iter().map(|(index, _)| index))
            } else {
                category_points
                    .iter()
                    .map(|(index, _)| index)
                    .eq(value_points.iter().map(|(index, _)| index))
            };
            if !aligned {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::Chart,
                    format!(
                        "{} series {series_index} cache point indexes differ",
                        family.tag_name().name()
                    ),
                ));
            }
            if values
                .iter()
                .chain(&x_values)
                .any(|value| !value.is_finite() || *value < 0.0)
            {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::Chart,
                    format!(
                        "{} series {series_index} contains a negative or non-finite cached value",
                        family.tag_name().name()
                    ),
                ));
            }
            let name = child(ser, "tx")
                .and_then(first_cache_value)
                .unwrap_or_else(|| format!("Series {}", series_index + 1));
            let color = explicit_solid_color(ser)?;
            let series_labels = child(ser, "dLbls");
            let family_labels = child(family, "dLbls");
            if let Some(labels) = series_labels {
                validate_data_labels(labels)?;
            }
            if let Some(labels) = family_labels {
                validate_data_labels(labels)?;
            }
            let show_values = series_labels
                .and_then(|labels| child(labels, "showVal"))
                .or_else(|| family_labels.and_then(|labels| child(labels, "showVal")))
                .map(parse_on_off)
                .transpose()?
                .unwrap_or(false);
            let label_format = series_labels
                .and_then(|labels| child(labels, "numFmt"))
                .or_else(|| family_labels.and_then(|labels| child(labels, "numFmt")))
                .and_then(|format| format.attribute("formatCode"))
                .map(str::to_string);
            let mut point_colors = BTreeMap::new();
            for dpt in ser.children().filter(|node| is(*node, "dPt")) {
                let index = child(dpt, "idx")
                    .and_then(|node| node.attribute("val"))
                    .and_then(|value| value.parse().ok())
                    .ok_or_else(|| {
                        OfficeError::unsupported(
                            UnsupportedCode::Chart,
                            "chart dPt has no valid point index",
                        )
                    })?;
                let color = explicit_solid_color(dpt)?.ok_or_else(|| {
                    OfficeError::unsupported(
                        UnsupportedCode::Chart,
                        format!("chart dPt {index} has no supported solidFill"),
                    )
                })?;
                point_colors.insert(index, color);
            }
            series.push(Series {
                name,
                categories,
                values,
                x_values,
                color,
                point_colors,
                show_values,
                label_format,
            });
        }
        if series.is_empty() {
            return Err(OfficeError::unsupported(
                UnsupportedCode::Chart,
                "chart has no cached series",
            ));
        }
        if !matches!(kind, ChartKind::Scatter)
            && series
                .iter()
                .any(|item| item.values.len() != item.categories.len())
        {
            return Err(OfficeError::unsupported(
                UnsupportedCode::Chart,
                "chart category/value cache lengths differ",
            ));
        }
        if !matches!(kind, ChartKind::Scatter)
            && series
                .iter()
                .skip(1)
                .any(|item| item.categories != series[0].categories)
        {
            return Err(OfficeError::unsupported(
                UnsupportedCode::Chart,
                "chart series use different cached category sets",
            ));
        }
        if matches!(kind, ChartKind::Pie { .. }) && series.len() != 1 {
            return Err(OfficeError::unsupported(
                UnsupportedCode::Chart,
                "pie/doughnut charts with multiple cached series are unsupported",
            ));
        }
        if matches!(kind, ChartKind::Pie { .. }) && series[0].values.iter().sum::<f64>() <= 0.0 {
            return Err(OfficeError::unsupported(
                UnsupportedCode::Chart,
                "pie/doughnut values total zero",
            ));
        }
        let chart = find(&doc.root_element(), "chart").unwrap_or(doc.root_element());
        let title = child(chart, "title").and_then(text_content);
        let category_axis_title = find(&plot, "catAx")
            .and_then(|axis| child(axis, "title"))
            .and_then(text_content);
        let value_axis = find(&plot, "valAx");
        let value_axis_title = value_axis
            .and_then(|axis| child(axis, "title"))
            .and_then(text_content);
        let number_format = value_axis
            .and_then(|axis| child(axis, "numFmt"))
            .and_then(|node| node.attribute("formatCode"))
            .unwrap_or("General")
            .to_string();
        let major_gridlines =
            value_axis.is_some_and(|axis| child(axis, "majorGridlines").is_some());
        let legend = child(chart, "legend");
        if legend
            .and_then(|legend| child(legend, "overlay"))
            .map(parse_on_off)
            .transpose()?
            .unwrap_or(false)
        {
            return Err(OfficeError::unsupported(
                UnsupportedCode::Chart,
                "overlay chart legends are unsupported",
            ));
        }
        let legend_deleted = legend
            .and_then(|legend| child(legend, "delete"))
            .map(parse_on_off)
            .transpose()?
            .unwrap_or(false);
        let legend_position = legend
            .filter(|_| !legend_deleted)
            .map(|legend| val_child(legend, "legendPos").unwrap_or_else(|| "r".to_string()));
        if legend_position
            .as_deref()
            .is_some_and(|position| !matches!(position, "l" | "r" | "t" | "b"))
        {
            return Err(OfficeError::unsupported(
                UnsupportedCode::Chart,
                format!(
                    "unsupported chart legend position {}",
                    legend_position.as_deref().unwrap_or_default()
                ),
            ));
        }
        Ok(Chart {
            kind,
            series,
            title,
            category_axis_title,
            value_axis_title,
            legend_position,
            number_format,
            major_gridlines,
        })
    }

    /// Renders plot, axes, cached labels, title, and legend into layout primitives.
    pub fn render(
        &self,
        fonts: &FontDb,
        theme: &Theme,
        width: f64,
        height: f64,
    ) -> Result<Vec<Positioned>> {
        let mut items = vec![Positioned {
            x_pt: 0.0,
            y_pt: 0.0,
            item: Item::Rect {
                w_pt: width,
                h_pt: height,
                fill: Some(Rgba::opaque(Rgb::new(255, 255, 255))),
                stroke: None,
            },
        }];
        let legend_w = if self
            .legend_position
            .as_deref()
            .is_some_and(|p| matches!(p, "l" | "r"))
        {
            width * 0.2
        } else {
            0.0
        };
        let left = 42.0
            + if self.legend_position.as_deref() == Some("l") {
                legend_w
            } else {
                0.0
            };
        let right = width
            - 18.0
            - if self.legend_position.as_deref() == Some("r") {
                legend_w
            } else {
                0.0
            };
        let top = if self.title.is_some() { 30.0 } else { 16.0 }
            + if self.legend_position.as_deref() == Some("t") {
                18.0
            } else {
                0.0
            };
        let bottom = height
            - 34.0
            - if self.legend_position.as_deref() == Some("b") {
                18.0
            } else {
                0.0
            };
        let plot_w = (right - left).max(10.0);
        let plot_h = (bottom - top).max(10.0);
        let stack_mode = match self.kind {
            ChartKind::Bar { stack, .. } | ChartKind::Area { stack } => stack,
            _ => StackMode::None,
        };
        let max = if stack_mode == StackMode::Percent {
            1.0
        } else if stack_mode == StackMode::Stacked {
            let categories = self.series.first().map_or(0, |series| series.values.len());
            (0..categories)
                .map(|index| {
                    self.series
                        .iter()
                        .map(|series| series.values[index].max(0.0))
                        .sum::<f64>()
                })
                .fold(0.0, f64::max)
                .max(1.0)
        } else {
            self.series
                .iter()
                .flat_map(|series| series.values.iter())
                .copied()
                .fold(0.0_f64, f64::max)
                .max(1.0)
        };
        let axis = Stroke {
            width_pt: 0.7,
            color: Rgba::opaque(Rgb::new(80, 80, 80)),
            dash: None,
        };
        let horizontal_bar = matches!(
            self.kind,
            ChartKind::Bar {
                horizontal: true,
                ..
            }
        );
        let scatter_x_max = matches!(self.kind, ChartKind::Scatter).then(|| {
            self.series
                .iter()
                .flat_map(|series| &series.x_values)
                .copied()
                .fold(0.0_f64, f64::max)
                .max(1.0)
        });
        if !matches!(self.kind, ChartKind::Pie { .. }) {
            items.push(line(left, bottom, plot_w, 0.0, axis.clone()));
            items.push(line(left, top, 0.0, plot_h, axis.clone()));
            for tick in 0..=4 {
                let axis_format =
                    if stack_mode == StackMode::Percent && self.number_format == "General" {
                        "0%"
                    } else {
                        &self.number_format
                    };
                let label =
                    crate::xlsx::numfmt::format_number(max * tick as f64 / 4.0, axis_format, false)
                        .text;
                if horizontal_bar {
                    let x = left + plot_w * tick as f64 / 4.0;
                    if self.major_gridlines && tick > 0 {
                        items.push(line(
                            x,
                            top,
                            0.0,
                            plot_h,
                            Stroke {
                                width_pt: 0.3,
                                color: Rgba::opaque(Rgb::new(210, 210, 210)),
                                dash: None,
                            },
                        ));
                    }
                    push_text(&mut items, fonts, &label, 8.0, x, bottom + 12.0, true)?;
                } else {
                    let y = bottom - plot_h * tick as f64 / 4.0;
                    if self.major_gridlines && tick > 0 {
                        items.push(line(
                            left,
                            y,
                            plot_w,
                            0.0,
                            Stroke {
                                width_pt: 0.3,
                                color: Rgba::opaque(Rgb::new(210, 210, 210)),
                                dash: None,
                            },
                        ));
                    }
                    push_text(&mut items, fonts, &label, 8.0, left - 4.0, y + 3.0, true)?;
                }
            }
            if let Some(x_max) = scatter_x_max {
                for tick in 0..=4 {
                    let x = left + plot_w * tick as f64 / 4.0;
                    if self.major_gridlines && tick > 0 {
                        items.push(line(
                            x,
                            top,
                            0.0,
                            plot_h,
                            Stroke {
                                width_pt: 0.3,
                                color: Rgba::opaque(Rgb::new(210, 210, 210)),
                                dash: None,
                            },
                        ));
                    }
                    let label = crate::xlsx::numfmt::format_number(
                        x_max * tick as f64 / 4.0,
                        "General",
                        false,
                    )
                    .text;
                    push_text(&mut items, fonts, &label, 8.0, x, bottom + 12.0, true)?;
                }
            }
        }
        match self.kind {
            ChartKind::Bar {
                horizontal: false,
                stack,
                gap_percent,
            } => {
                let categories = self
                    .series
                    .iter()
                    .map(|s| s.values.len())
                    .max()
                    .unwrap_or(1)
                    .max(1);
                let group = plot_w / categories as f64;
                let slots = if stack == StackMode::None {
                    self.series.len()
                } else {
                    1
                };
                let bar_w = group / (slots as f64 + gap_percent / 100.0);
                let group_start = (group - bar_w * slots as f64) / 2.0;
                let totals: Vec<f64> = (0..categories)
                    .map(|index| {
                        self.series
                            .iter()
                            .map(|series| series.values[index].max(0.0))
                            .sum()
                    })
                    .collect();
                let mut cumulative = vec![0.0; categories];
                for (si, series) in self.series.iter().enumerate() {
                    for (i, value) in series.values.iter().enumerate() {
                        let normalized = if stack == StackMode::Percent {
                            value.max(0.0) / totals[i].max(f64::EPSILON)
                        } else {
                            value.max(0.0)
                        };
                        let h = plot_h * normalized / max;
                        let base = cumulative[i];
                        if stack != StackMode::None {
                            cumulative[i] += h;
                        }
                        let x = left
                            + i as f64 * group
                            + group_start
                            + if stack == StackMode::None {
                                si as f64 * bar_w
                            } else {
                                0.0
                            };
                        let y = bottom - base - h;
                        items.push(Positioned {
                            x_pt: x,
                            y_pt: y,
                            item: Item::Rect {
                                w_pt: bar_w,
                                h_pt: h,
                                fill: Some(point_color(series, i, theme, si)),
                                stroke: None,
                            },
                        });
                        if series.show_values {
                            push_value_label(
                                &mut items,
                                fonts,
                                series,
                                *value,
                                x + bar_w / 2.0,
                                if stack == StackMode::None {
                                    y - 2.0
                                } else {
                                    y + h / 2.0 + 3.0
                                },
                                &self.number_format,
                            )?;
                        }
                    }
                }
                if let Some(categories) = self.series.first().map(|series| &series.categories) {
                    for (i, label) in categories.iter().enumerate() {
                        push_text(
                            &mut items,
                            fonts,
                            label,
                            8.0,
                            left + (i as f64 + 0.5) * group,
                            bottom + 12.0,
                            true,
                        )?;
                    }
                }
            }
            ChartKind::Bar {
                horizontal: true,
                stack,
                gap_percent,
            } => {
                let count = self
                    .series
                    .first()
                    .map(|series| series.values.len())
                    .unwrap_or(1)
                    .max(1);
                let row = plot_h / count as f64;
                let totals: Vec<f64> = (0..count)
                    .map(|index| {
                        self.series
                            .iter()
                            .map(|series| series.values[index].max(0.0))
                            .sum()
                    })
                    .collect();
                let mut cumulative = vec![0.0; count];
                let slots = if stack == StackMode::None {
                    self.series.len()
                } else {
                    1
                };
                let bar_h = row / (slots as f64 + gap_percent / 100.0);
                let row_start = (row - bar_h * slots as f64) / 2.0;
                for (si, series) in self.series.iter().enumerate() {
                    for (i, value) in series.values.iter().enumerate() {
                        let normalized = if stack == StackMode::Percent {
                            value.max(0.0) / totals[i].max(f64::EPSILON)
                        } else {
                            value.max(0.0)
                        };
                        let w = plot_w * normalized / max;
                        let x = left + cumulative[i];
                        if stack != StackMode::None {
                            cumulative[i] += w;
                        }
                        let h = bar_h;
                        let y = top
                            + i as f64 * row
                            + row_start
                            + if stack == StackMode::None {
                                si as f64 * h
                            } else {
                                0.0
                            };
                        items.push(Positioned {
                            x_pt: x,
                            y_pt: y,
                            item: Item::Rect {
                                w_pt: w,
                                h_pt: h,
                                fill: Some(point_color(series, i, theme, si)),
                                stroke: None,
                            },
                        });
                        if series.show_values {
                            push_value_label(
                                &mut items,
                                fonts,
                                series,
                                *value,
                                x + w / 2.0,
                                y + h / 2.0 + 3.0,
                                &self.number_format,
                            )?;
                        }
                    }
                }
                if let Some(categories) = self.series.first().map(|series| &series.categories) {
                    for (index, label) in categories.iter().enumerate() {
                        push_text(
                            &mut items,
                            fonts,
                            label,
                            8.0,
                            left / 2.0,
                            top + (index as f64 + 0.35) * row + 4.0,
                            true,
                        )?;
                    }
                }
            }
            ChartKind::Line { markers } => {
                for (si, series) in self.series.iter().enumerate() {
                    let color = point_color(series, 0, theme, si);
                    let count = series.values.len().max(2);
                    let mut commands = Vec::new();
                    for (i, value) in series.values.iter().enumerate() {
                        let x = left + plot_w * i as f64 / (count - 1) as f64;
                        let y = bottom - plot_h * value.max(0.0) / max;
                        commands.push(if i == 0 {
                            PathCommand::MoveTo { x, y }
                        } else {
                            PathCommand::LineTo { x, y }
                        });
                        if markers {
                            items.push(Positioned {
                                x_pt: x - 2.0,
                                y_pt: y - 2.0,
                                item: Item::Rect {
                                    w_pt: 4.0,
                                    h_pt: 4.0,
                                    fill: Some(color),
                                    stroke: None,
                                },
                            });
                        }
                        if series.show_values {
                            push_value_label(
                                &mut items,
                                fonts,
                                series,
                                *value,
                                x,
                                y - 3.0,
                                &self.number_format,
                            )?;
                        }
                    }
                    items.push(Positioned {
                        x_pt: 0.0,
                        y_pt: 0.0,
                        item: Item::Path {
                            commands,
                            paint: crate::layout::PathPaint {
                                fill: None,
                                stroke: Some(Stroke {
                                    width_pt: 1.4,
                                    color,
                                    dash: None,
                                }),
                                fill_rule: crate::layout::FillRule::NonZero,
                            },
                        },
                    });
                }
            }
            ChartKind::Area { stack } => {
                render_area(
                    &mut items, self, theme, fonts, left, bottom, plot_w, plot_h, max, stack,
                )?;
            }
            ChartKind::Scatter => {
                let xmax = self
                    .series
                    .iter()
                    .flat_map(|series| &series.x_values)
                    .copied()
                    .fold(0.0_f64, f64::max)
                    .max(1.0);
                for (si, series) in self.series.iter().enumerate() {
                    for (i, (&xv, &yv)) in series.x_values.iter().zip(&series.values).enumerate() {
                        let x = left + plot_w * xv / xmax;
                        let y = bottom - plot_h * yv / max;
                        items.push(Positioned {
                            x_pt: x - 2.5,
                            y_pt: y - 2.5,
                            item: Item::Rect {
                                w_pt: 5.0,
                                h_pt: 5.0,
                                fill: Some(point_color(series, i, theme, si)),
                                stroke: None,
                            },
                        });
                        if series.show_values {
                            push_value_label(
                                &mut items,
                                fonts,
                                series,
                                yv,
                                x,
                                y - 3.0,
                                &self.number_format,
                            )?;
                        }
                    }
                }
            }
            ChartKind::Pie { hole_ratio } => render_pie(
                &mut items,
                self,
                fonts,
                theme,
                left + plot_w / 2.0,
                top + plot_h / 2.0,
                plot_w.min(plot_h) * 0.42,
                hole_ratio,
            )?,
        }
        if matches!(self.kind, ChartKind::Line { .. } | ChartKind::Area { .. })
            && let Some(categories) = self.series.first().map(|series| &series.categories)
        {
            let denominator = categories.len().saturating_sub(1).max(1) as f64;
            for (index, label) in categories.iter().enumerate() {
                push_text(
                    &mut items,
                    fonts,
                    label,
                    8.0,
                    left + plot_w * index as f64 / denominator,
                    bottom + 12.0,
                    true,
                )?;
            }
        }
        if let Some(title) = &self.title {
            push_text(&mut items, fonts, title, 12.0, width / 2.0, 16.0, true)?;
        }
        if let Some(title) = &self.category_axis_title {
            if horizontal_bar {
                push_text(
                    &mut items,
                    fonts,
                    title,
                    9.0,
                    2.0,
                    top + plot_h / 2.0,
                    false,
                )?;
            } else {
                push_text(
                    &mut items,
                    fonts,
                    title,
                    9.0,
                    left + plot_w / 2.0,
                    height - 5.0,
                    true,
                )?;
            }
        }
        if let Some(title) = &self.value_axis_title {
            if horizontal_bar {
                push_text(
                    &mut items,
                    fonts,
                    title,
                    9.0,
                    left + plot_w / 2.0,
                    height - 5.0,
                    true,
                )?;
            } else {
                push_text(
                    &mut items,
                    fonts,
                    title,
                    9.0,
                    2.0,
                    top + plot_h / 2.0,
                    false,
                )?;
            }
        }
        render_legend(&mut items, self, fonts, theme, width, height)?;
        Ok(items)
    }
}

#[expect(clippy::too_many_arguments, reason = "chart plot geometry is explicit")]
fn render_area(
    items: &mut Vec<Positioned>,
    chart: &Chart,
    theme: &Theme,
    fonts: &FontDb,
    left: f64,
    bottom: f64,
    plot_w: f64,
    plot_h: f64,
    max: f64,
    stack: StackMode,
) -> Result<()> {
    let count = chart.series[0].values.len();
    let totals: Vec<f64> = (0..count)
        .map(|index| {
            chart
                .series
                .iter()
                .map(|series| series.values[index].max(0.0))
                .sum()
        })
        .collect();
    let mut lower = vec![0.0; count];
    for (series_index, series) in chart.series.iter().enumerate() {
        let upper: Vec<f64> = series
            .values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let value = if stack == StackMode::Percent {
                    value.max(0.0) / totals[index].max(f64::EPSILON)
                } else {
                    value.max(0.0)
                };
                if stack == StackMode::None {
                    value
                } else {
                    lower[index] + value
                }
            })
            .collect();
        let point = |index: usize, value: f64| {
            let x = left + plot_w * index as f64 / count.saturating_sub(1).max(1) as f64;
            let y = bottom - plot_h * value / max;
            (x, y)
        };
        let mut commands = Vec::new();
        for (index, value) in upper.iter().enumerate() {
            let (x, y) = point(index, *value);
            commands.push(if index == 0 {
                PathCommand::MoveTo { x, y }
            } else {
                PathCommand::LineTo { x, y }
            });
        }
        for index in (0..count).rev() {
            let base = if stack == StackMode::None {
                0.0
            } else {
                lower[index]
            };
            let (x, y) = point(index, base);
            commands.push(PathCommand::LineTo { x, y });
        }
        commands.push(PathCommand::Close);
        let color = point_color(series, 0, theme, series_index);
        items.push(Positioned {
            x_pt: 0.0,
            y_pt: 0.0,
            item: Item::Path {
                commands,
                paint: crate::layout::PathPaint {
                    fill: Some(color),
                    stroke: Some(Stroke {
                        width_pt: 0.8,
                        color,
                        dash: None,
                    }),
                    fill_rule: crate::layout::FillRule::NonZero,
                },
            },
        });
        if series.show_values {
            for (index, value) in series.values.iter().enumerate() {
                let (x, y) = point(index, upper[index]);
                push_value_label(
                    items,
                    fonts,
                    series,
                    *value,
                    x,
                    y - 3.0,
                    &chart.number_format,
                )?;
            }
        }
        if stack != StackMode::None {
            lower = upper;
        }
    }
    Ok(())
}

#[expect(clippy::too_many_arguments, reason = "pie plot geometry is explicit")]
fn render_pie(
    items: &mut Vec<Positioned>,
    chart: &Chart,
    fonts: &FontDb,
    theme: &Theme,
    cx: f64,
    cy: f64,
    radius: f64,
    hole_ratio: Option<f64>,
) -> Result<()> {
    let values = &chart.series[0].values;
    let total: f64 = values.iter().map(|value| value.max(0.0)).sum();
    if total <= 0.0 {
        return Err(OfficeError::unsupported(
            UnsupportedCode::Chart,
            "pie/doughnut values total zero",
        ));
    }
    let mut angle = -std::f64::consts::FRAC_PI_2;
    for (index, value) in values.iter().enumerate() {
        let sweep = std::f64::consts::TAU * value.max(0.0) / total;
        let steps = (sweep.abs() / 0.25).ceil() as usize;
        let start = angle;
        let mut commands = if hole_ratio.is_some() {
            vec![PathCommand::MoveTo {
                x: cx + radius * angle.cos(),
                y: cy + radius * angle.sin(),
            }]
        } else {
            vec![
                PathCommand::MoveTo { x: cx, y: cy },
                PathCommand::LineTo {
                    x: cx + radius * angle.cos(),
                    y: cy + radius * angle.sin(),
                },
            ]
        };
        for step in 1..=steps.max(1) {
            let a = angle + sweep * step as f64 / steps.max(1) as f64;
            commands.push(PathCommand::LineTo {
                x: cx + radius * a.cos(),
                y: cy + radius * a.sin(),
            });
        }
        if let Some(hole_ratio) = hole_ratio {
            let inner = radius * hole_ratio;
            let end = angle + sweep;
            commands.push(PathCommand::LineTo {
                x: cx + inner * end.cos(),
                y: cy + inner * end.sin(),
            });
            for step in 1..=steps.max(1) {
                let a = end - sweep * step as f64 / steps.max(1) as f64;
                commands.push(PathCommand::LineTo {
                    x: cx + inner * a.cos(),
                    y: cy + inner * a.sin(),
                });
            }
        }
        commands.push(PathCommand::Close);
        items.push(Positioned {
            x_pt: 0.0,
            y_pt: 0.0,
            item: Item::Path {
                commands,
                paint: crate::layout::PathPaint {
                    fill: Some(point_color(&chart.series[0], index, theme, index)),
                    stroke: Some(Stroke {
                        width_pt: 0.5,
                        color: Rgba::opaque(Rgb::new(255, 255, 255)),
                        dash: None,
                    }),
                    fill_rule: if hole_ratio.is_some() {
                        crate::layout::FillRule::EvenOdd
                    } else {
                        crate::layout::FillRule::NonZero
                    },
                },
            },
        });
        if chart.series[0].show_values {
            let label_radius = radius * hole_ratio.map_or(0.62, |hole| (1.0 + hole) / 2.0);
            let middle = start + sweep / 2.0;
            push_value_label(
                items,
                fonts,
                &chart.series[0],
                *value,
                cx + label_radius * middle.cos(),
                cy + label_radius * middle.sin(),
                &chart.number_format,
            )?;
        }
        angle += sweep;
    }
    Ok(())
}

fn render_legend(
    items: &mut Vec<Positioned>,
    chart: &Chart,
    fonts: &FontDb,
    theme: &Theme,
    width: f64,
    height: f64,
) -> Result<()> {
    let Some(position) = chart.legend_position.as_deref() else {
        return Ok(());
    };
    let entries: Vec<(&str, Rgba)> = if matches!(chart.kind, ChartKind::Pie { .. }) {
        chart.series[0]
            .categories
            .iter()
            .enumerate()
            .map(|(index, category)| {
                (
                    category.as_str(),
                    point_color(&chart.series[0], index, theme, index),
                )
            })
            .collect()
    } else {
        chart
            .series
            .iter()
            .enumerate()
            .map(|(index, series)| (series.name.as_str(), point_color(series, 0, theme, index)))
            .collect()
    };
    for (index, (label, color)) in entries.into_iter().enumerate() {
        let (x, y) = match position {
            "l" => (4.0, 30.0 + index as f64 * 14.0),
            "t" => (30.0 + index as f64 * 90.0, 22.0),
            "b" => (30.0 + index as f64 * 90.0, height - 12.0),
            _ => (width * 0.82, 30.0 + index as f64 * 14.0),
        };
        items.push(Positioned {
            x_pt: x,
            y_pt: y - 7.0,
            item: Item::Rect {
                w_pt: 8.0,
                h_pt: 8.0,
                fill: Some(color),
                stroke: None,
            },
        });
        push_text(items, fonts, label, 8.0, x + 11.0, y, false)?;
    }
    Ok(())
}

fn push_value_label(
    items: &mut Vec<Positioned>,
    fonts: &FontDb,
    series: &Series,
    value: f64,
    x: f64,
    baseline: f64,
    chart_format: &str,
) -> Result<()> {
    let format = series.label_format.as_deref().unwrap_or(chart_format);
    let label = crate::xlsx::numfmt::format_number(value, format, false).text;
    push_text(items, fonts, &label, 8.0, x, baseline, true)
}

fn push_text(
    items: &mut Vec<Positioned>,
    fonts: &FontDb,
    text: &str,
    size: f64,
    x: f64,
    baseline: f64,
    centered: bool,
) -> Result<()> {
    let face = matching::resolve(fonts, "Calibri", 400, false)
        .face
        .ok_or_else(|| {
            OfficeError::unsupported(UnsupportedCode::MissingGlyph, "chart text has no font")
        })?;
    let fallback_ids = matching::coverage_fallbacks(fonts, face, 400, false);
    let fallback_faces: Vec<_> = fallback_ids
        .iter()
        .filter_map(|face_id| fonts.face(*face_id))
        .collect();
    let shaped = shape::require_glyphs(shape::shape_with_fallback(
        text,
        &fallback_faces,
        size,
        0.0,
        false,
    ))?;
    let start = if centered {
        x - shaped.width_pt / 2.0
    } else {
        x
    };
    let mapped = crate::text::semantic::map_shaped_run(text, &shaped);
    let mut semantics = Some(mapped.semantic);
    let mut cursor = start;
    for (index, (glyph, visible)) in shaped.glyphs.iter().zip(mapped.visible).enumerate() {
        let face = *fallback_ids.get(glyph.face_index).ok_or_else(|| {
            OfficeError::unsupported(
                UnsupportedCode::MissingGlyph,
                "chart text fallback face is unavailable",
            )
        })?;
        let mut glyphs = vec![visible];
        if index == 0 {
            glyphs.extend(semantics.take().unwrap_or_default());
        }
        items.push(Positioned {
            x_pt: cursor,
            y_pt: baseline,
            item: Item::Glyphs(GlyphRun {
                face,
                size_pt: size,
                color: Rgba::opaque(Rgb::new(30, 30, 30)),
                synthetic_bold: false,
                base_direction: None,
                glyphs,
                underline: None,
                strike: None,
            }),
        });
        cursor += glyph.advance_pt;
    }
    Ok(())
}
fn line(x: f64, y: f64, dx: f64, dy: f64, stroke: Stroke) -> Positioned {
    Positioned {
        x_pt: x,
        y_pt: y,
        item: Item::Line {
            dx_pt: dx,
            dy_pt: dy,
            stroke,
        },
    }
}
fn point_color(series: &Series, point: usize, theme: &Theme, series_index: usize) -> Rgba {
    series
        .point_colors
        .get(&point)
        .map(|color| resolve_colorspec(theme, color))
        .or_else(|| {
            series
                .color
                .as_ref()
                .map(|color| resolve_colorspec(theme, color))
        })
        .or_else(|| {
            theme
                .color(&format!("accent{}", series_index % 6 + 1))
                .map(Rgba::opaque)
        })
        .unwrap_or_else(|| {
            const COLORS: [Rgb; 6] = [
                Rgb::new(0x44, 0x72, 0xC4),
                Rgb::new(0xED, 0x7D, 0x31),
                Rgb::new(0xA5, 0xA5, 0xA5),
                Rgb::new(0xFF, 0xC0, 0x00),
                Rgb::new(0x5B, 0x9B, 0xD5),
                Rgb::new(0x70, 0xAD, 0x47),
            ];
            Rgba::opaque(COLORS[series_index % COLORS.len()])
        })
}
fn is(node: Node, name: &str) -> bool {
    node.is_element() && node.tag_name().namespace() == Some(C) && node.tag_name().name() == name
}
fn child<'a>(node: Node<'a, 'a>, name: &str) -> Option<Node<'a, 'a>> {
    node.children().find(|candidate| is(*candidate, name))
}
fn find<'a>(node: &Node<'a, 'a>, name: &str) -> Option<Node<'a, 'a>> {
    node.descendants().find(|candidate| is(*candidate, name))
}
fn val_child(node: Node, name: &str) -> Option<String> {
    child(node, name)
        .and_then(|child| child.attribute("val"))
        .map(str::to_string)
}
fn first_cache_value(node: Node) -> Option<String> {
    node.descendants()
        .find(|candidate| is(*candidate, "v"))
        .and_then(|value| value.text())
        .map(str::to_string)
}
fn cache_strings(node: Option<Node>) -> Result<Vec<(usize, String)>> {
    let Some(node) = node else {
        return Ok(Vec::new());
    };
    let mut values = Vec::new();
    for point in node.descendants().filter(|candidate| is(*candidate, "pt")) {
        let index = point
            .attribute("idx")
            .and_then(|index| index.parse().ok())
            .ok_or_else(|| {
                OfficeError::unsupported(
                    UnsupportedCode::Chart,
                    "cached chart point has no valid index",
                )
            })?;
        let value = child(point, "v")
            .and_then(|value| value.text())
            .unwrap_or_default()
            .to_string();
        values.push((index, value));
    }
    values.sort_by_key(|pair| pair.0);
    if values.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(OfficeError::unsupported(
            UnsupportedCode::Chart,
            "cached chart points contain duplicate indexes",
        ));
    }
    if values
        .iter()
        .enumerate()
        .any(|(expected, (index, _))| *index != expected)
    {
        return Err(OfficeError::unsupported(
            UnsupportedCode::Chart,
            "cached chart points are sparse or do not start at zero",
        ));
    }
    if let Some(point_count) = node
        .descendants()
        .find(|candidate| is(*candidate, "ptCount"))
    {
        let declared = point_count
            .attribute("val")
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or_else(|| {
                OfficeError::unsupported(UnsupportedCode::Chart, "cached chart ptCount is invalid")
            })?;
        if declared != values.len() {
            return Err(OfficeError::unsupported(
                UnsupportedCode::Chart,
                format!(
                    "cached chart ptCount {declared} differs from {} points",
                    values.len()
                ),
            ));
        }
    }
    Ok(values)
}
fn cache_numbers(node: Option<Node>) -> Result<Vec<(usize, f64)>> {
    cache_strings(node)?
        .into_iter()
        .map(|(index, value)| {
            value
                .parse::<f64>()
                .map(|value| (index, value))
                .map_err(|_| {
                    OfficeError::unsupported(
                        UnsupportedCode::Chart,
                        format!("cached chart point {index} is not numeric"),
                    )
                })
        })
        .collect()
}
fn parse_on_off(node: Node) -> Result<bool> {
    match node.attribute("val").unwrap_or("true") {
        "1" | "true" | "on" => Ok(true),
        "0" | "false" | "off" => Ok(false),
        value => Err(OfficeError::unsupported(
            UnsupportedCode::Chart,
            format!("invalid chart boolean value {value}"),
        )),
    }
}
fn validate_data_labels(labels: Node) -> Result<()> {
    if let Some(unsupported) = labels.children().find(|child| {
        child.is_element()
            && child.tag_name().namespace() == Some(C)
            && !matches!(child.tag_name().name(), "showVal" | "numFmt")
    }) {
        return Err(OfficeError::unsupported(
            UnsupportedCode::Chart,
            format!(
                "unsupported chart data-label option {}",
                unsupported.tag_name().name()
            ),
        ));
    }
    Ok(())
}
fn validate_visible_chart_options(root: Node) -> Result<()> {
    for node in root.descendants().filter(|node| node.is_element()) {
        if node.tag_name().namespace() == Some(crate::oxml::A)
            && matches!(node.tag_name().name(), "rPr" | "defRPr" | "endParaRPr")
        {
            return Err(OfficeError::unsupported(
                UnsupportedCode::Chart,
                format!(
                    "unsupported formatted chart text a:{}",
                    node.tag_name().name()
                ),
            ));
        }
        if node.tag_name().namespace() != Some(C) {
            continue;
        }
        let name = node.tag_name().name();
        if matches!(
            name,
            "trendline"
                | "errBars"
                | "upDownBars"
                | "dropLines"
                | "hiLowLines"
                | "serLines"
                | "explosion"
                | "firstSliceAng"
                | "secondPieSize"
                | "splitType"
                | "splitPos"
                | "custSplit"
        ) {
            return Err(OfficeError::unsupported(
                UnsupportedCode::Chart,
                format!("unsupported visible chart option {name}"),
            ));
        }
        if name == "spPr"
            && !node.parent().is_some_and(|parent| {
                parent.tag_name().namespace() == Some(C)
                    && matches!(parent.tag_name().name(), "ser" | "dPt")
            })
        {
            return Err(OfficeError::unsupported(
                UnsupportedCode::Chart,
                "chart/plot/axis formatting is unsupported",
            ));
        }
        if matches!(name, "majorUnit" | "minorUnit") {
            return Err(OfficeError::unsupported(
                UnsupportedCode::Chart,
                format!("unsupported axis option {name}"),
            ));
        }
        if matches!(name, "min" | "max" | "logBase")
            && node.parent().is_some_and(|parent| is(parent, "scaling"))
        {
            return Err(OfficeError::unsupported(
                UnsupportedCode::Chart,
                format!("unsupported axis scaling {name}"),
            ));
        }
        if name == "orientation" && node.attribute("val") != Some("minMax") {
            return Err(OfficeError::unsupported(
                UnsupportedCode::Chart,
                "reversed chart axes are unsupported",
            ));
        }
        if name == "crossesAt" || (name == "crosses" && node.attribute("val") != Some("autoZero")) {
            return Err(OfficeError::unsupported(
                UnsupportedCode::Chart,
                format!("unsupported axis crossing option {name}"),
            ));
        }
        if name == "tickLblPos"
            && !matches!(node.attribute("val"), None | Some("nextTo" | "nextToAxis"))
        {
            return Err(OfficeError::unsupported(
                UnsupportedCode::Chart,
                "unsupported tick-label position",
            ));
        }
    }
    Ok(())
}
fn text_content(node: Node) -> Option<String> {
    let text = node
        .descendants()
        .filter(|candidate| {
            candidate.is_element() && matches!(candidate.tag_name().name(), "t" | "v")
        })
        .filter_map(|candidate| candidate.text())
        .collect::<Vec<_>>()
        .join("");
    (!text.is_empty()).then_some(text)
}
fn explicit_solid_color(node: Node) -> Result<Option<ColorSpec>> {
    let Some(properties) = child(node, "spPr") else {
        return Ok(None);
    };
    let solid = properties.children().find(|candidate| {
        candidate.is_element()
            && candidate.tag_name().namespace() == Some(crate::oxml::A)
            && candidate.tag_name().name() == "solidFill"
    });
    if let Some(solid) = solid {
        return crate::oxml::pml::color_from_solid(solid)
            .map(Some)
            .ok_or_else(|| {
                OfficeError::unsupported(
                    UnsupportedCode::Chart,
                    "chart solidFill has no supported color",
                )
            });
    }
    if let Some(visible) = properties.children().find(|candidate| {
        candidate.is_element()
            && candidate.tag_name().namespace() == Some(crate::oxml::A)
            && matches!(
                candidate.tag_name().name(),
                "gradFill" | "pattFill" | "blipFill" | "noFill" | "ln" | "effectLst"
            )
    }) {
        return Err(OfficeError::unsupported(
            UnsupportedCode::Chart,
            format!(
                "unsupported chart series formatting a:{}",
                visible.tag_name().name()
            ),
        ));
    }
    Ok(None)
}
