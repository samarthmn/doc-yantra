//! Structural subset of Office Math Markup Language (OMML).

use super::{M, Node, attr, children, is};

const MAX_MATH_NESTING: usize = 64;

/// One parsed inline or display equation.
#[derive(Debug, Clone)]
pub struct MathContent {
    /// Root expression.
    pub expression: MathNode,
    /// Whether Word requested display layout.
    pub display: bool,
}

/// Supported OMML expression tree.
#[derive(Debug, Clone)]
pub enum MathNode {
    /// Horizontally concatenated expressions.
    Sequence(Vec<MathNode>),
    /// Math run text.
    Run(String),
    /// Stacked fraction.
    Fraction {
        /// Numerator.
        numerator: Box<MathNode>,
        /// Denominator.
        denominator: Box<MathNode>,
    },
    /// Base with optional lower and upper scripts.
    Script {
        /// Base expression.
        base: Box<MathNode>,
        /// Lower script.
        subscript: Option<Box<MathNode>>,
        /// Upper script.
        superscript: Option<Box<MathNode>>,
    },
    /// Radical with optional degree.
    Radical {
        /// Optional radical degree.
        degree: Option<Box<MathNode>>,
        /// Radicand.
        radicand: Box<MathNode>,
    },
    /// N-ary operator with limits and operand.
    Nary {
        /// Operator glyph.
        operator: char,
        /// Lower limit.
        lower: Option<Box<MathNode>>,
        /// Upper limit.
        upper: Option<Box<MathNode>>,
        /// Operand.
        operand: Box<MathNode>,
    },
    /// Delimited expression.
    Delimiter {
        /// Opening delimiter.
        open: String,
        /// Closing delimiter.
        close: String,
        /// Delimited body.
        body: Box<MathNode>,
    },
    /// Function name and argument.
    Function {
        /// Function name.
        name: Box<MathNode>,
        /// Function argument.
        argument: Box<MathNode>,
    },
    /// Recognized but deliberately unsupported structure.
    Unsupported(String),
}

impl MathContent {
    /// Parses `m:oMath` or `m:oMathPara`.
    pub fn parse(node: Node, display: bool) -> Self {
        MathContent {
            expression: parse_node(node, 0),
            display,
        }
    }

    /// Returns the first unsupported structural detail, if present.
    pub fn unsupported(&self) -> Option<&str> {
        unsupported(&self.expression)
    }

    /// Logical text used for profile coverage and PDF semantics.
    pub fn source_text(&self) -> String {
        source_text(&self.expression)
    }
}

fn parse_node(node: Node, depth: usize) -> MathNode {
    if depth > MAX_MATH_NESTING {
        return MathNode::Unsupported(format!("equation nesting exceeds limit {MAX_MATH_NESTING}"));
    }
    let name = node.tag_name().name();
    match name {
        "m" if node.tag_name().namespace() == Some(M) => {
            MathNode::Unsupported("matrix".to_string())
        }
        "eqArr" => MathNode::Unsupported("equation array".to_string()),
        "acc" => MathNode::Unsupported("accent".to_string()),
        "r" => {
            if child_m(node, "rPr").is_some_and(has_element_children)
                || children(node).any(|child| {
                    child.tag_name().namespace() != Some(M)
                        && child.tag_name().name().ends_with("Pr")
                })
            {
                return MathNode::Unsupported("OMML run formatting".to_string());
            }
            MathNode::Run(
                node.descendants()
                    .filter(|candidate| is(*candidate, M, "t"))
                    .filter_map(|text| text.text())
                    .collect(),
            )
        }
        "f" => {
            if let Some(properties) = child_m(node, "fPr") {
                for property in children(properties) {
                    if !is(property, M, "type")
                        || attr(property, M, "val").unwrap_or("bar") != "bar"
                    {
                        return MathNode::Unsupported("non-bar fraction properties".to_string());
                    }
                }
            }
            MathNode::Fraction {
                numerator: boxed_child(node, "num", depth),
                denominator: boxed_child(node, "den", depth),
            }
        }
        "sSup" => {
            if property_has_children(node, "sSupPr") {
                return MathNode::Unsupported("superscript properties".to_string());
            }
            MathNode::Script {
                base: boxed_child(node, "e", depth),
                subscript: None,
                superscript: optional_boxed_child(node, "sup", depth),
            }
        }
        "sSub" => {
            if property_has_children(node, "sSubPr") {
                return MathNode::Unsupported("subscript properties".to_string());
            }
            MathNode::Script {
                base: boxed_child(node, "e", depth),
                subscript: optional_boxed_child(node, "sub", depth),
                superscript: None,
            }
        }
        "sSubSup" => {
            if property_has_children(node, "sSubSupPr") {
                return MathNode::Unsupported("subscript/superscript properties".to_string());
            }
            MathNode::Script {
                base: boxed_child(node, "e", depth),
                subscript: optional_boxed_child(node, "sub", depth),
                superscript: optional_boxed_child(node, "sup", depth),
            }
        }
        "rad" => {
            if property_has_children(node, "radPr") {
                return MathNode::Unsupported("radical properties".to_string());
            }
            MathNode::Radical {
                degree: optional_boxed_child(node, "deg", depth)
                    .filter(|degree| !source_text(degree).is_empty()),
                radicand: boxed_child(node, "e", depth),
            }
        }
        "nary" => {
            let properties = child_m(node, "naryPr");
            if properties.is_some_and(|properties| {
                children(properties).any(|property| !is(property, M, "chr"))
            }) {
                return MathNode::Unsupported("n-ary operator properties".to_string());
            }
            let operator_value = properties
                .and_then(|properties| child_m(properties, "chr"))
                .and_then(|character| attr(character, M, "val"))
                .unwrap_or("∑");
            let mut operator_chars = operator_value.chars();
            let operator = operator_chars.next().unwrap_or('∑');
            if operator_chars.next().is_some() {
                return MathNode::Unsupported("multi-character n-ary operator".to_string());
            }
            MathNode::Nary {
                operator,
                lower: optional_boxed_child(node, "sub", depth),
                upper: optional_boxed_child(node, "sup", depth),
                operand: boxed_child(node, "e", depth),
            }
        }
        "d" => {
            if children(node).filter(|child| is(*child, M, "e")).count() > 1 {
                return MathNode::Unsupported("delimiter with multiple expressions".to_string());
            }
            let properties = child_m(node, "dPr");
            if properties.is_some_and(|properties| {
                children(properties)
                    .any(|property| !matches!(property.tag_name().name(), "begChr" | "endChr"))
            }) {
                return MathNode::Unsupported("delimiter properties".to_string());
            }
            let delimiter = |name: &str, fallback: char| {
                properties
                    .and_then(|properties| child_m(properties, name))
                    .and_then(|node| attr(node, M, "val"))
                    .map(str::to_string)
                    .unwrap_or_else(|| fallback.to_string())
            };
            MathNode::Delimiter {
                open: delimiter("begChr", '('),
                close: delimiter("endChr", ')'),
                body: boxed_child(node, "e", depth),
            }
        }
        "func" => {
            if property_has_children(node, "funcPr") {
                return MathNode::Unsupported("function properties".to_string());
            }
            MathNode::Function {
                name: boxed_child(node, "fName", depth),
                argument: boxed_child(node, "e", depth),
            }
        }
        "oMath" | "oMathPara" | "e" | "num" | "den" | "sub" | "sup" | "deg" | "fName" => {
            sequence(node, depth)
        }
        "t" => MathNode::Sequence(Vec::new()),
        other if other.ends_with("Pr") || other == "ctrlPr" => {
            if has_element_children(node) {
                MathNode::Unsupported(format!("OMML {other}"))
            } else {
                MathNode::Sequence(Vec::new())
            }
        }
        other => MathNode::Unsupported(format!("OMML {other}")),
    }
}

fn sequence(node: Node, depth: usize) -> MathNode {
    MathNode::Sequence(
        children(node)
            .filter(|child| child.tag_name().namespace() == Some(M))
            .map(|child| parse_node(child, depth + 1))
            .filter(|node| !matches!(node, MathNode::Sequence(nodes) if nodes.is_empty()))
            .collect(),
    )
}

fn child_m<'a>(node: Node<'a, 'a>, name: &str) -> Option<Node<'a, 'a>> {
    children(node).find(|child| is(*child, M, name))
}

fn has_element_children(node: Node) -> bool {
    children(node).next().is_some()
}

fn property_has_children(node: Node, name: &str) -> bool {
    child_m(node, name).is_some_and(has_element_children)
}

fn boxed_child(node: Node, name: &str, depth: usize) -> Box<MathNode> {
    optional_boxed_child(node, name, depth)
        .unwrap_or_else(|| Box::new(MathNode::Sequence(Vec::new())))
}

fn optional_boxed_child(node: Node, name: &str, depth: usize) -> Option<Box<MathNode>> {
    child_m(node, name)
        .map(|child| parse_node(child, depth + 1))
        .map(Box::new)
}

fn unsupported(node: &MathNode) -> Option<&str> {
    match node {
        MathNode::Unsupported(detail) => Some(detail),
        MathNode::Sequence(nodes) => nodes.iter().find_map(unsupported),
        MathNode::Fraction {
            numerator,
            denominator,
        } => unsupported(numerator).or_else(|| unsupported(denominator)),
        MathNode::Script {
            base,
            subscript,
            superscript,
        } => unsupported(base)
            .or_else(|| subscript.as_deref().and_then(unsupported))
            .or_else(|| superscript.as_deref().and_then(unsupported)),
        MathNode::Radical { degree, radicand } => degree
            .as_deref()
            .and_then(unsupported)
            .or_else(|| unsupported(radicand)),
        MathNode::Nary {
            lower,
            upper,
            operand,
            ..
        } => lower
            .as_deref()
            .and_then(unsupported)
            .or_else(|| upper.as_deref().and_then(unsupported))
            .or_else(|| unsupported(operand)),
        MathNode::Delimiter { body, .. } => unsupported(body),
        MathNode::Function { name, argument } => {
            unsupported(name).or_else(|| unsupported(argument))
        }
        MathNode::Run(_) => None,
    }
}

fn source_text(node: &MathNode) -> String {
    match node {
        MathNode::Sequence(nodes) => nodes.iter().map(source_text).collect(),
        MathNode::Run(text) => text.clone(),
        MathNode::Fraction {
            numerator,
            denominator,
        } => {
            format!("{}/{}", source_text(numerator), source_text(denominator))
        }
        MathNode::Script {
            base,
            subscript,
            superscript,
        } => format!(
            "{}{}{}",
            source_text(base),
            subscript.as_deref().map(source_text).unwrap_or_default(),
            superscript.as_deref().map(source_text).unwrap_or_default()
        ),
        MathNode::Radical { degree, radicand } => format!(
            "{}√{}",
            degree.as_deref().map(source_text).unwrap_or_default(),
            source_text(radicand)
        ),
        MathNode::Nary {
            operator,
            lower,
            upper,
            operand,
        } => format!(
            "{operator}{}{}{}",
            lower.as_deref().map(source_text).unwrap_or_default(),
            upper.as_deref().map(source_text).unwrap_or_default(),
            source_text(operand)
        ),
        MathNode::Delimiter { open, close, body } => {
            format!("{open}{}{close}", source_text(body))
        }
        MathNode::Function { name, argument } => {
            format!("{}{}", source_text(name), source_text(argument))
        }
        MathNode::Unsupported(_) => String::new(),
    }
}
