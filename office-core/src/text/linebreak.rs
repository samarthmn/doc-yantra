//! UAX-14 line breaking (wraps `unicode-linebreak`), yielding break
//! opportunities by byte index. Trailing spaces at a break do not count toward
//! line width (Word behaviour) — the layout engine handles that trimming.

/// A break opportunity within a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakOpportunity {
    /// Byte offset in the source string at the break point.
    pub byte_index: usize,
    /// Whether the break is mandatory (hard break) or merely allowed.
    pub mandatory: bool,
}

/// Returns the UAX-14 break opportunities in `text`, in order.
pub fn break_opportunities(text: &str) -> Vec<BreakOpportunity> {
    unicode_linebreak::linebreaks(text)
        .map(|(byte_index, op)| BreakOpportunity {
            byte_index,
            mandatory: matches!(op, unicode_linebreak::BreakOpportunity::Mandatory),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breaks_between_words() {
        let breaks = break_opportunities("hello world");
        // A break is allowed after the space (byte index 6) and at end (11).
        assert!(breaks.iter().any(|b| b.byte_index == 6 && !b.mandatory));
        assert!(breaks.iter().any(|b| b.byte_index == 11));
    }

    #[test]
    fn newline_is_mandatory() {
        let breaks = break_opportunities("a\nb");
        assert!(breaks.iter().any(|b| b.mandatory));
    }
}
