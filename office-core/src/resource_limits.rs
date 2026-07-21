//! Checked budgets for work derived from valid, document-controlled structure.

use crate::{OfficeError, Result, UnsupportedCode};

/// Maximum number of output pages created by one conversion.
pub const MAX_PAGES: usize = 2_000;

fn refusal(detail: impl Into<String>) -> OfficeError {
    OfficeError::unsupported(UnsupportedCode::ResourceLimit, detail)
}

/// Checks a document-controlled collection length before work or allocation.
pub fn ensure_cardinality(label: &str, value: usize, limit: usize) -> Result<()> {
    if value > limit {
        return Err(refusal(format!(
            "{label} count {value} exceeds limit {limit}"
        )));
    }
    Ok(())
}

/// Adds two document-controlled byte or item counts without wrapping.
pub fn checked_count_add(label: &str, left: usize, right: usize) -> Result<usize> {
    left.checked_add(right)
        .ok_or_else(|| refusal(format!("{label} count overflows usize")))
}

/// Adds two document-controlled byte counts without wrapping.
pub fn checked_bytes_add(label: &str, left: usize, right: usize) -> Result<usize> {
    left.checked_add(right)
        .ok_or_else(|| refusal(format!("{label} byte count overflows usize")))
}

/// Multiplies two document-controlled byte counts without wrapping.
pub fn checked_bytes_mul(label: &str, left: usize, right: usize) -> Result<usize> {
    left.checked_mul(right)
        .ok_or_else(|| refusal(format!("{label} byte count overflows usize")))
}

/// Reserves document-controlled vector capacity without panicking on capacity
/// overflow or allocator refusal.
pub fn try_reserve<T>(values: &mut Vec<T>, additional: usize, label: &str) -> Result<()> {
    values.try_reserve(additional).map_err(|_| {
        refusal(format!(
            "{label} allocation for {additional} additional items was refused"
        ))
    })
}

/// Counts pages before each output-page allocation.
#[derive(Debug, Clone)]
pub struct PageBudget {
    limit: usize,
    used: usize,
}

impl PageBudget {
    /// Creates a budget with an injectable limit, primarily for small tests.
    pub const fn new(limit: usize) -> Self {
        Self { limit, used: 0 }
    }

    /// Creates the production 2,000-page budget.
    pub const fn production() -> Self {
        Self::new(MAX_PAGES)
    }

    /// Charges one page before its storage is created.
    pub fn consume(&mut self, context: &str) -> Result<()> {
        if self.used >= self.limit {
            return Err(refusal(format!(
                "{context}: page count would exceed limit {}",
                self.limit
            )));
        }
        self.used += 1;
        Ok(())
    }

    /// Returns the configured page maximum.
    pub const fn limit(&self) -> usize {
        self.limit
    }

    /// Returns the number of page allocations charged so far.
    pub const fn used(&self) -> usize {
        self.used
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OfficeError, UnsupportedCode};

    #[test]
    fn checked_helpers_return_resource_limit_on_overflow_or_excess() {
        let too_many = ensure_cardinality("tabs", 5, 4).expect_err("limit");
        let overflow = checked_bytes_mul("pixels", usize::MAX, 2).expect_err("overflow");
        let byte_sum = checked_bytes_add("images", usize::MAX, 1).expect_err("byte sum overflow");
        for error in [too_many, overflow, byte_sum] {
            assert!(matches!(
                error,
                OfficeError::Unsupported {
                    code: UnsupportedCode::ResourceLimit,
                    ..
                }
            ));
        }
    }

    #[test]
    fn page_budget_refuses_before_exceeding_configured_limit() {
        let mut budget = PageBudget::new(2);
        budget.consume("first page").unwrap();
        budget.consume("second page").unwrap();
        let error = budget.consume("third page").expect_err("limit");
        assert_eq!(budget.used(), 2);
        assert!(matches!(
            error,
            OfficeError::Unsupported {
                code: UnsupportedCode::ResourceLimit,
                ..
            }
        ));
    }

    #[test]
    fn production_page_budget_is_two_thousand_pages() {
        assert_eq!(PageBudget::production().limit(), 2_000);
    }
}
