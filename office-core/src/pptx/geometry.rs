//! DrawingML shape geometry, guide formulas, and fill-color helpers.

use crate::layout::PathCommand;
use crate::oxml::Rgba;
use crate::oxml::pml::{
    ColorSpec, CustomGeometry, Fill, GeometryCommand, GeometryGuide, GeometryPoint, PresetGeometry,
    ShapeProps, Xfrm,
};
use crate::oxml::theme::{Theme, resolve_color};
use crate::{OfficeError, Result, UnsupportedCode};
use std::collections::HashMap;

/// One point in page-relative shape coordinates.
#[derive(Debug, Clone, Copy)]
pub struct Point {
    /// X coordinate in points.
    pub x: f64,
    /// Y coordinate in points.
    pub y: f64,
}

/// One vector contour with native cubic segments and representative points.
#[derive(Debug, Clone)]
pub struct Contour {
    /// Native path commands.
    pub commands: Vec<PathCommand>,
    /// Ordered endpoints/control points used for bounds and arrow directions.
    pub points: Vec<Point>,
    /// Whether last point connects to first.
    pub closed: bool,
}

/// Resolves a [`ColorSpec`] against theme.
pub fn resolve_colorspec(theme: &Theme, spec: &ColorSpec) -> Rgba {
    resolve_color(spec.base.clone(), &spec.transforms, theme)
}

/// Returns representative solid color for fills that have one.
pub fn solid_fill_color(theme: &Theme, fill: &Fill) -> Option<Rgba> {
    match fill {
        Fill::Solid(c) => Some(resolve_colorspec(theme, c)),
        Fill::LinearGradient { stops, .. } => {
            stops.first().map(|(_, c)| resolve_colorspec(theme, c))
        }
        Fill::Pattern { background, .. } => Some(resolve_colorspec(theme, background)),
        Fill::None | Fill::Picture(_) | Fill::Unsupported(_) => None,
    }
}

/// Evaluates shape geometry. Absence means default rectangle.
pub fn evaluate_shape(props: &ShapeProps, xfrm: Xfrm) -> Result<Vec<Contour>> {
    if let Some(custom) = &props.custom_geom {
        return evaluate_custom(custom, xfrm.w_pt, xfrm.h_pt);
    }
    match &props.preset {
        Some(preset) => evaluate_preset(preset, xfrm.w_pt, xfrm.h_pt),
        None => Ok(vec![polygon(&[
            (0.0, 0.0),
            (xfrm.w_pt, 0.0),
            (xfrm.w_pt, xfrm.h_pt),
            (0.0, xfrm.h_pt),
        ])]),
    }
}

/// Reports whether preset has an implemented renderer.
pub fn supports_preset(name: &str) -> bool {
    matches!(
        name,
        "rect"
            | "roundRect"
            | "ellipse"
            | "triangle"
            | "rtTriangle"
            | "diamond"
            | "parallelogram"
            | "trapezoid"
            | "pentagon"
            | "hexagon"
            | "octagon"
            | "rightArrow"
            | "leftArrow"
            | "upArrow"
            | "downArrow"
            | "leftRightArrow"
            | "chevron"
            | "plus"
            | "star4"
            | "star5"
            | "pie"
            | "arc"
            | "chord"
            | "line"
            | "straightConnector1"
            | "bentConnector2"
            | "bentConnector3"
            | "bentConnector4"
            | "bentConnector5"
    ) || name.to_ascii_lowercase().contains("callout")
}

fn evaluate_preset(preset: &PresetGeometry, w: f64, h: f64) -> Result<Vec<Contour>> {
    let adj = preset_adjustments(preset, w, h)?;
    let ratio = |name: &str, default: f64| {
        adj.get(name)
            .copied()
            .unwrap_or(default)
            .clamp(0.0, 100_000.0)
            / 100_000.0
    };
    let poly = |points: &[(f64, f64)]| {
        vec![polygon(
            &points
                .iter()
                .map(|(x, y)| (x * w, y * h))
                .collect::<Vec<_>>(),
        )]
    };
    let contours = match preset.name.as_str() {
        "rect" => poly(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]),
        "roundRect" => vec![rounded_rect(w, h, ratio("adj", 16667.0))],
        "ellipse" => vec![ellipse(w / 2.0, h / 2.0, w / 2.0, h / 2.0, true)],
        "triangle" => poly(&[(0.5, 0.0), (1.0, 1.0), (0.0, 1.0)]),
        "rtTriangle" => poly(&[(0.0, 0.0), (1.0, 1.0), (0.0, 1.0)]),
        "diamond" => poly(&[(0.5, 0.0), (1.0, 0.5), (0.5, 1.0), (0.0, 0.5)]),
        "parallelogram" => {
            let a = ratio("adj", 25000.0).min(0.49);
            poly(&[(a, 0.0), (1.0, 0.0), (1.0 - a, 1.0), (0.0, 1.0)])
        }
        "trapezoid" => {
            let a = ratio("adj", 25000.0).min(0.49);
            poly(&[(a, 0.0), (1.0 - a, 0.0), (1.0, 1.0), (0.0, 1.0)])
        }
        "pentagon" => vec![regular_polygon(5, w, h, -90.0)],
        "hexagon" => vec![regular_polygon(6, w, h, 0.0)],
        "octagon" => vec![regular_polygon(8, w, h, 22.5)],
        "rightArrow" => {
            let shaft = ratio("adj1", 50000.0) / 2.0;
            let head = ratio("adj2", 50000.0);
            poly(&[
                (0.0, 0.5 - shaft),
                (1.0 - head, 0.5 - shaft),
                (1.0 - head, 0.0),
                (1.0, 0.5),
                (1.0 - head, 1.0),
                (1.0 - head, 0.5 + shaft),
                (0.0, 0.5 + shaft),
            ])
        }
        "leftArrow" => mirror_x(evaluate_preset(
            &PresetGeometry {
                name: "rightArrow".into(),
                adjustments: preset.adjustments.clone(),
            },
            w,
            h,
        )?),
        "upArrow" => rotate_quarter(
            evaluate_preset(
                &PresetGeometry {
                    name: "rightArrow".into(),
                    adjustments: preset.adjustments.clone(),
                },
                h,
                w,
            )?,
            w,
        ),
        "downArrow" => mirror_y(
            rotate_quarter(
                evaluate_preset(
                    &PresetGeometry {
                        name: "rightArrow".into(),
                        adjustments: preset.adjustments.clone(),
                    },
                    h,
                    w,
                )?,
                w,
            ),
            h,
        ),
        "leftRightArrow" => {
            let s = ratio("adj1", 50000.0) / 2.0;
            let head = ratio("adj2", 25000.0).min(0.45);
            poly(&[
                (0.0, 0.5),
                (head, 0.0),
                (head, 0.5 - s),
                (1.0 - head, 0.5 - s),
                (1.0 - head, 0.0),
                (1.0, 0.5),
                (1.0 - head, 1.0),
                (1.0 - head, 0.5 + s),
                (head, 0.5 + s),
                (head, 1.0),
            ])
        }
        "chevron" => {
            let a = ratio("adj", 25000.0).min(0.49);
            poly(&[
                (0.0, 0.0),
                (1.0 - a, 0.0),
                (1.0, 0.5),
                (1.0 - a, 1.0),
                (0.0, 1.0),
                (a, 0.5),
            ])
        }
        "plus" => {
            let a = ratio("adj", 25000.0).min(0.49);
            poly(&[
                (a, 0.0),
                (1.0 - a, 0.0),
                (1.0 - a, a),
                (1.0, a),
                (1.0, 1.0 - a),
                (1.0 - a, 1.0 - a),
                (1.0 - a, 1.0),
                (a, 1.0),
                (a, 1.0 - a),
                (0.0, 1.0 - a),
                (0.0, a),
                (a, a),
            ])
        }
        "star4" => vec![star(4, w, h, ratio("adj", 25000.0), -90.0)],
        "star5" => vec![star(5, w, h, ratio("adj", 38197.0), -90.0)],
        "pie" | "arc" | "chord" => arc_preset(&preset.name, w, h, &adj),
        "line" | "straightConnector1" => vec![Contour {
            commands: vec![
                PathCommand::MoveTo { x: 0.0, y: 0.0 },
                PathCommand::LineTo { x: w, y: h },
            ],
            points: vec![Point { x: 0.0, y: 0.0 }, Point { x: w, y: h }],
            closed: false,
        }],
        name if name.starts_with("bentConnector") => vec![Contour {
            commands: vec![
                PathCommand::MoveTo { x: 0.0, y: 0.0 },
                PathCommand::LineTo { x: w / 2.0, y: 0.0 },
                PathCommand::LineTo { x: w / 2.0, y: h },
                PathCommand::LineTo { x: w, y: h },
            ],
            points: vec![
                Point { x: 0.0, y: 0.0 },
                Point { x: w / 2.0, y: 0.0 },
                Point { x: w / 2.0, y: h },
                Point { x: w, y: h },
            ],
            closed: false,
        }],
        name if name.to_ascii_lowercase().contains("callout") => vec![polygon(&[
            (0.0, 0.0),
            (w, 0.0),
            (w, h * 0.75),
            (w * 0.45, h * 0.75),
            (w * 0.25, h),
            (w * 0.3, h * 0.75),
            (0.0, h * 0.75),
        ])],
        other => {
            return Err(OfficeError::unsupported(
                UnsupportedCode::ShapeGeometry,
                format!("preset:{other}"),
            ));
        }
    };
    Ok(contours)
}

fn preset_adjustments(preset: &PresetGeometry, w: f64, h: f64) -> Result<HashMap<String, f64>> {
    let mut env = builtin_env(w, h);
    evaluate_guides(&preset.adjustments, &mut env)?;
    Ok(env)
}

fn evaluate_custom(custom: &CustomGeometry, w: f64, h: f64) -> Result<Vec<Contour>> {
    let mut env = builtin_env(w, h);
    evaluate_guides(&custom.guides, &mut env)?;
    let mut contours = Vec::new();
    for path in &custom.paths {
        if let Some(detail) = path.unsupported.first() {
            return Err(OfficeError::unsupported(
                UnsupportedCode::ShapeGeometry,
                detail.clone(),
            ));
        }
        let pw = path.width.unwrap_or(w).max(1.0);
        let ph = path.height.unwrap_or(h).max(1.0);
        let sx = w / pw;
        let sy = h / ph;
        let mut points = Vec::new();
        let mut commands = Vec::new();
        let mut start = Point { x: 0.0, y: 0.0 };
        let mut current = start;
        let mut closed = false;
        for command in &path.commands {
            match command {
                GeometryCommand::Move(point) => {
                    if commands.len() > 1 {
                        contours.push(Contour {
                            commands: std::mem::take(&mut commands),
                            points: std::mem::take(&mut points),
                            closed,
                        });
                    }
                    current = eval_point(point, &env, sx, sy)?;
                    start = current;
                    points.push(current);
                    commands.push(PathCommand::MoveTo {
                        x: current.x,
                        y: current.y,
                    });
                    closed = false;
                }
                GeometryCommand::Line(point) => {
                    current = eval_point(point, &env, sx, sy)?;
                    points.push(current);
                    commands.push(PathCommand::LineTo {
                        x: current.x,
                        y: current.y,
                    });
                }
                GeometryCommand::Quad(control, end) => {
                    let control = eval_point(control, &env, sx, sy)?;
                    let end = eval_point(end, &env, sx, sy)?;
                    let c1 = Point {
                        x: current.x + (control.x - current.x) * 2.0 / 3.0,
                        y: current.y + (control.y - current.y) * 2.0 / 3.0,
                    };
                    let c2 = Point {
                        x: end.x + (control.x - end.x) * 2.0 / 3.0,
                        y: end.y + (control.y - end.y) * 2.0 / 3.0,
                    };
                    commands.push(cubic_command(c1, c2, end));
                    points.extend([c1, c2, end]);
                    current = end;
                }
                GeometryCommand::Cubic(c1, c2, end) => {
                    let c1 = eval_point(c1, &env, sx, sy)?;
                    let c2 = eval_point(c2, &env, sx, sy)?;
                    let end = eval_point(end, &env, sx, sy)?;
                    commands.push(cubic_command(c1, c2, end));
                    points.extend([c1, c2, end]);
                    current = end;
                }
                GeometryCommand::Arc {
                    width_radius,
                    height_radius,
                    start_angle,
                    sweep_angle,
                } => {
                    let rx = eval_token(width_radius, &env)? * sx;
                    let ry = eval_token(height_radius, &env)? * sy;
                    let start_angle = eval_token(start_angle, &env)? / 60_000.0;
                    let sweep = eval_token(sweep_angle, &env)? / 60_000.0;
                    let start_rad = start_angle.to_radians();
                    let center = Point {
                        x: current.x - rx * start_rad.cos(),
                        y: current.y - ry * start_rad.sin(),
                    };
                    let segments = ((sweep.abs() / 90.0).ceil() as usize).clamp(1, 4);
                    for segment in 0..segments {
                        let a0 =
                            (start_angle + sweep * segment as f64 / segments as f64).to_radians();
                        let a1 = (start_angle + sweep * (segment + 1) as f64 / segments as f64)
                            .to_radians();
                        let (c1, c2, end) = ellipse_cubic(center, rx, ry, a0, a1);
                        commands.push(cubic_command(c1, c2, end));
                        points.extend([c1, c2, end]);
                    }
                    current = *points.last().unwrap_or(&current);
                }
                GeometryCommand::Close => {
                    current = start;
                    closed = true;
                    commands.push(PathCommand::Close);
                }
            }
        }
        if commands.len() > 1 {
            contours.push(Contour {
                commands,
                points,
                closed,
            });
        }
    }
    if contours.is_empty() {
        return Err(OfficeError::unsupported(
            UnsupportedCode::ShapeGeometry,
            "custGeom:pathLst empty",
        ));
    }
    Ok(contours)
}

fn builtin_env(w: f64, h: f64) -> HashMap<String, f64> {
    let mut env = HashMap::new();
    for (name, value) in [
        ("l", 0.0),
        ("t", 0.0),
        ("r", w),
        ("b", h),
        ("w", w),
        ("h", h),
        ("hc", w / 2.0),
        ("vc", h / 2.0),
        ("wd2", w / 2.0),
        ("hd2", h / 2.0),
        ("wd4", w / 4.0),
        ("hd4", h / 4.0),
        ("ss", w.min(h)),
        ("ls", w.max(h)),
        ("cd2", 10_800_000.0),
        ("cd4", 5_400_000.0),
        ("cd8", 2_700_000.0),
    ] {
        env.insert(name.to_string(), value);
    }
    env
}

fn evaluate_guides(guides: &[GeometryGuide], env: &mut HashMap<String, f64>) -> Result<()> {
    for guide in guides {
        let value = eval_formula(&guide.formula, env).map_err(|detail| {
            OfficeError::unsupported(
                UnsupportedCode::ShapeGeometry,
                format!("guide {}: {detail}", guide.name),
            )
        })?;
        env.insert(guide.name.clone(), value);
    }
    Ok(())
}

fn eval_formula(formula: &str, env: &HashMap<String, f64>) -> std::result::Result<f64, String> {
    let parts: Vec<&str> = formula.split_whitespace().collect();
    let get = |index: usize| {
        parts
            .get(index)
            .ok_or_else(|| format!("bad formula {formula}"))
            .and_then(|token| eval_token(token, env).map_err(|error| error.to_string()))
    };
    let op = parts.first().copied().unwrap_or("val");
    let value = match op {
        "val" => get(1)?,
        "*/" => get(1)? * get(2)? / get(3)?.max(f64::MIN_POSITIVE),
        "+-" => get(1)? + get(2)? - get(3)?,
        "+/" => (get(1)? + get(2)?) / get(3)?.max(f64::MIN_POSITIVE),
        "?:" => {
            if get(1)? > 0.0 {
                get(2)?
            } else {
                get(3)?
            }
        }
        "abs" => get(1)?.abs(),
        "min" => get(1)?.min(get(2)?),
        "max" => get(1)?.max(get(2)?),
        "sqrt" => get(1)?.max(0.0).sqrt(),
        "mod" => (get(1)?.powi(2) + get(2)?.powi(2) + get(3)?.powi(2)).sqrt(),
        "pin" => get(2)?.clamp(get(1)?, get(3)?),
        "at2" => get(2)?.atan2(get(1)?).to_degrees() * 60_000.0,
        "sin" => get(1)? * (get(2)? / 60_000.0).to_radians().sin(),
        "cos" => get(1)? * (get(2)? / 60_000.0).to_radians().cos(),
        "tan" => get(1)? * (get(2)? / 60_000.0).to_radians().tan(),
        "cat2" => {
            let x = get(2)?;
            let y = get(3)?;
            get(1)? * x / (x * x + y * y).sqrt().max(f64::MIN_POSITIVE)
        }
        "sat2" => {
            let x = get(2)?;
            let y = get(3)?;
            get(1)? * y / (x * x + y * y).sqrt().max(f64::MIN_POSITIVE)
        }
        _ => return Err(format!("unknown formula op {op}")),
    };
    Ok(value)
}

fn eval_token(token: &str, env: &HashMap<String, f64>) -> Result<f64> {
    token
        .parse::<f64>()
        .ok()
        .or_else(|| env.get(token).copied())
        .ok_or_else(|| {
            OfficeError::unsupported(
                UnsupportedCode::ShapeGeometry,
                format!("unknown guide token {token}"),
            )
        })
}

fn eval_point(
    point: &GeometryPoint,
    env: &HashMap<String, f64>,
    sx: f64,
    sy: f64,
) -> Result<Point> {
    Ok(Point {
        x: eval_token(&point.x, env)? * sx,
        y: eval_token(&point.y, env)? * sy,
    })
}

fn polygon(points: &[(f64, f64)]) -> Contour {
    let points: Vec<Point> = points.iter().map(|&(x, y)| Point { x, y }).collect();
    let mut commands = Vec::with_capacity(points.len() + 1);
    if let Some(first) = points.first() {
        commands.push(PathCommand::MoveTo {
            x: first.x,
            y: first.y,
        });
        commands.extend(points.iter().skip(1).map(|point| PathCommand::LineTo {
            x: point.x,
            y: point.y,
        }));
        commands.push(PathCommand::Close);
    }
    Contour {
        commands,
        points,
        closed: true,
    }
}

fn ellipse(cx: f64, cy: f64, rx: f64, ry: f64, closed: bool) -> Contour {
    let start = Point { x: cx + rx, y: cy };
    let center = Point { x: cx, y: cy };
    let mut commands = vec![PathCommand::MoveTo {
        x: start.x,
        y: start.y,
    }];
    let mut points = vec![start];
    for quarter in 0..4 {
        let a0 = quarter as f64 * std::f64::consts::FRAC_PI_2;
        let a1 = (quarter + 1) as f64 * std::f64::consts::FRAC_PI_2;
        let (c1, c2, end) = ellipse_cubic(center, rx, ry, a0, a1);
        commands.push(cubic_command(c1, c2, end));
        points.extend([c1, c2, end]);
    }
    if closed {
        commands.push(PathCommand::Close);
    }
    Contour {
        commands,
        points,
        closed,
    }
}

fn rounded_rect(w: f64, h: f64, ratio: f64) -> Contour {
    let radius = w.min(h) * ratio.min(0.5);
    let k = 0.552_284_749_830_793_6 * radius;
    let points = vec![
        (radius, 0.0),
        (w - radius, 0.0),
        (w - radius + k, 0.0),
        (w, radius - k),
        (w, radius),
        (w, h - radius),
        (w, h - radius + k),
        (w - radius + k, h),
        (w - radius, h),
        (radius, h),
        (radius - k, h),
        (0.0, h - radius + k),
        (0.0, h - radius),
        (0.0, radius),
        (0.0, radius - k),
        (radius - k, 0.0),
        (radius, 0.0),
    ];
    let points: Vec<Point> = points.into_iter().map(|(x, y)| Point { x, y }).collect();
    let commands = vec![
        PathCommand::MoveTo { x: radius, y: 0.0 },
        PathCommand::LineTo {
            x: w - radius,
            y: 0.0,
        },
        cubic_command(points[2], points[3], points[4]),
        PathCommand::LineTo {
            x: w,
            y: h - radius,
        },
        cubic_command(points[6], points[7], points[8]),
        PathCommand::LineTo { x: radius, y: h },
        cubic_command(points[10], points[11], points[12]),
        PathCommand::LineTo { x: 0.0, y: radius },
        cubic_command(points[14], points[15], points[16]),
        PathCommand::Close,
    ];
    Contour {
        commands,
        points,
        closed: true,
    }
}

fn regular_polygon(sides: usize, w: f64, h: f64, start_deg: f64) -> Contour {
    let points: Vec<(f64, f64)> = (0..sides)
        .map(|i| {
            let a = (start_deg + 360.0 * i as f64 / sides as f64).to_radians();
            (w / 2.0 + w / 2.0 * a.cos(), h / 2.0 + h / 2.0 * a.sin())
        })
        .collect();
    polygon(&points)
}

fn star(points: usize, w: f64, h: f64, inner: f64, start_deg: f64) -> Contour {
    let vertices: Vec<(f64, f64)> = (0..points * 2)
        .map(|i| {
            let radius = if i % 2 == 0 { 1.0 } else { inner };
            let a = (start_deg + 180.0 * i as f64 / points as f64).to_radians();
            (
                w / 2.0 + w / 2.0 * radius * a.cos(),
                h / 2.0 + h / 2.0 * radius * a.sin(),
            )
        })
        .collect();
    polygon(&vertices)
}

fn mirror_x(mut contours: Vec<Contour>) -> Vec<Contour> {
    let max = contours
        .iter()
        .flat_map(|c| c.points.iter())
        .map(|p| p.x)
        .fold(0.0, f64::max);
    for contour in &mut contours {
        transform_contour(contour, |point| Point {
            x: max - point.x,
            y: point.y,
        });
    }
    contours
}
fn mirror_y(mut contours: Vec<Contour>, h: f64) -> Vec<Contour> {
    for contour in &mut contours {
        transform_contour(contour, |point| Point {
            x: point.x,
            y: h - point.y,
        });
    }
    contours
}
fn rotate_quarter(mut contours: Vec<Contour>, w: f64) -> Vec<Contour> {
    for contour in &mut contours {
        transform_contour(contour, |point| Point {
            x: w - point.y,
            y: point.x,
        });
    }
    contours
}

fn arc_preset(kind: &str, w: f64, h: f64, adj: &HashMap<String, f64>) -> Vec<Contour> {
    let start = adj.get("adj1").copied().unwrap_or(0.0) / 60_000.0;
    let end = adj.get("adj2").copied().unwrap_or(5_400_000.0) / 60_000.0;
    let sweep = if end >= start {
        end - start
    } else {
        end + 360.0 - start
    };
    let center = Point {
        x: w / 2.0,
        y: h / 2.0,
    };
    let start_rad = start.to_radians();
    let start_point = Point {
        x: center.x + w / 2.0 * start_rad.cos(),
        y: center.y + h / 2.0 * start_rad.sin(),
    };
    let mut points = Vec::new();
    let mut commands = Vec::new();
    if kind == "pie" {
        points.push(center);
        commands.push(PathCommand::MoveTo {
            x: center.x,
            y: center.y,
        });
        points.push(start_point);
        commands.push(PathCommand::LineTo {
            x: start_point.x,
            y: start_point.y,
        });
    } else {
        points.push(start_point);
        commands.push(PathCommand::MoveTo {
            x: start_point.x,
            y: start_point.y,
        });
    }
    let segments = ((sweep.abs() / 90.0).ceil() as usize).clamp(1, 4);
    for segment in 0..segments {
        let a0 = (start + sweep * segment as f64 / segments as f64).to_radians();
        let a1 = (start + sweep * (segment + 1) as f64 / segments as f64).to_radians();
        let (c1, c2, end) = ellipse_cubic(center, w / 2.0, h / 2.0, a0, a1);
        commands.push(cubic_command(c1, c2, end));
        points.extend([c1, c2, end]);
    }
    if kind == "chord" || kind == "pie" {
        commands.push(PathCommand::Close);
    }
    vec![Contour {
        commands,
        points,
        closed: kind != "arc",
    }]
}

fn cubic_command(c1: Point, c2: Point, end: Point) -> PathCommand {
    PathCommand::CubicTo {
        x1: c1.x,
        y1: c1.y,
        x2: c2.x,
        y2: c2.y,
        x: end.x,
        y: end.y,
    }
}

fn ellipse_cubic(center: Point, rx: f64, ry: f64, start: f64, end: f64) -> (Point, Point, Point) {
    let k = 4.0 / 3.0 * ((end - start) / 4.0).tan();
    let c1 = Point {
        x: center.x + rx * (start.cos() - k * start.sin()),
        y: center.y + ry * (start.sin() + k * start.cos()),
    };
    let c2 = Point {
        x: center.x + rx * (end.cos() + k * end.sin()),
        y: center.y + ry * (end.sin() - k * end.cos()),
    };
    let point = Point {
        x: center.x + rx * end.cos(),
        y: center.y + ry * end.sin(),
    };
    (c1, c2, point)
}

fn transform_contour(contour: &mut Contour, transform: impl Fn(Point) -> Point + Copy) {
    for point in &mut contour.points {
        *point = transform(*point);
    }
    for command in &mut contour.commands {
        *command = match *command {
            PathCommand::MoveTo { x, y } => {
                let point = transform(Point { x, y });
                PathCommand::MoveTo {
                    x: point.x,
                    y: point.y,
                }
            }
            PathCommand::LineTo { x, y } => {
                let point = transform(Point { x, y });
                PathCommand::LineTo {
                    x: point.x,
                    y: point.y,
                }
            }
            PathCommand::CubicTo {
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => {
                let c1 = transform(Point { x: x1, y: y1 });
                let c2 = transform(Point { x: x2, y: y2 });
                let end = transform(Point { x, y });
                cubic_command(c1, c2, end)
            }
            PathCommand::Close => PathCommand::Close,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawingml_formula_subset_evaluates() {
        let mut env = builtin_env(200.0, 100.0);
        env.insert("x".to_string(), 40.0);
        for (formula, expected) in [
            ("*/ w 1 2", 100.0),
            ("+- w h x", 260.0),
            ("min w h", 100.0),
            ("max w h", 200.0),
            ("?: x w h", 200.0),
            ("abs -12", 12.0),
            ("sqrt 81", 9.0),
            ("mod 3 4 0", 5.0),
            ("pin 10 50 30", 30.0),
        ] {
            let actual = eval_formula(formula, &env).unwrap();
            assert!((actual - expected).abs() < 0.001, "{formula}: {actual}");
        }
        assert!((eval_formula("sin 10 5400000", &env).unwrap() - 10.0).abs() < 0.001);
        assert!((eval_formula("cos 10 0", &env).unwrap() - 10.0).abs() < 0.001);
        assert!((eval_formula("tan 10 2700000", &env).unwrap() - 10.0).abs() < 0.001);
    }
}
