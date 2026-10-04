use std::fmt;

use chrono::{Datelike, NaiveDate};

/// A money amount stored as integer cents to avoid floating point rounding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Money(pub i64);

impl Money {
    /// Parse user input like `12`, `12.5`, `-3.07` or `$1,234.56` into cents.
    pub fn parse(input: &str) -> Option<Money> {
        let s: String = input
            .trim()
            .chars()
            .filter(|c| !matches!(c, '$' | ',' | ' '))
            .collect();
        let (negative, s) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s.as_str()),
        };
        if s.is_empty() {
            return None;
        }
        let (whole, frac) = match s.split_once('.') {
            Some((w, f)) => (w, f),
            None => (s, ""),
        };
        if frac.len() > 2
            || !whole.chars().all(|c| c.is_ascii_digit())
            || !frac.chars().all(|c| c.is_ascii_digit())
            || (whole.is_empty() && frac.is_empty())
        {
            return None;
        }
        let whole: i64 = if whole.is_empty() { 0 } else { whole.parse().ok()? };
        let frac: i64 = match frac.len() {
            0 => 0,
            1 => frac.parse::<i64>().ok()? * 10,
            _ => frac.parse().ok()?,
        };
        let cents = whole.checked_mul(100)?.checked_add(frac)?;
        Some(Money(if negative { -cents } else { cents }))
    }

    /// Format without the currency symbol, e.g. `1234.50`, for editing.
    pub fn to_plain(self) -> String {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        format!("{sign}{}.{:02}", abs / 100, abs % 100)
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        let whole = (abs / 100).to_string();
        let mut grouped = String::new();
        for (i, c) in whole.chars().enumerate() {
            if i > 0 && (whole.len() - i).is_multiple_of(3) {
                grouped.push(',');
            }
            grouped.push(c);
        }
        write!(f, "{sign}${grouped}.{:02}", abs % 100)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryKind {
    Income,
    Expense,
}

impl CategoryKind {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "income" => Some(CategoryKind::Income),
            "expense" => Some(CategoryKind::Expense),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CategoryKind::Income => "income",
            CategoryKind::Expense => "expense",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Category {
    pub id: i64,
    pub name: String,
    pub kind: CategoryKind,
}

/// Name shown for transactions whose category was deleted.
pub const UNASSIGNED: &str = "Unassigned";

/// Check a new category name against the existing ones. Returns the trimmed
/// name, or a message explaining why it can't be used.
pub fn validate_category_name(name: &str, existing: &[Category]) -> Result<String, String> {
    validate_name(name, existing.iter().map(|c| c.name.as_str()), "category")
}

/// Check a new payment method name against the existing ones, like
/// [`validate_category_name`].
pub fn validate_payment_method_name(
    name: &str,
    existing: &[PaymentMethod],
) -> Result<String, String> {
    validate_name(name, existing.iter().map(|p| p.name.as_str()), "payment method")
}

fn validate_name<'a>(
    name: &str,
    mut existing: impl Iterator<Item = &'a str>,
    noun: &str,
) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("Enter a {noun} name."));
    }
    if name.eq_ignore_ascii_case(UNASSIGNED) {
        return Err(format!("\"{UNASSIGNED}\" is reserved for transactions whose {noun} was deleted."));
    }
    if let Some(n) = existing.find(|n| n.eq_ignore_ascii_case(name)) {
        return Err(format!("A {noun} named \"{n}\" already exists."));
    }
    Ok(name.to_string())
}

/// An account used to pay for expenses, such as a credit card.
#[derive(Debug, Clone, PartialEq)]
pub struct PaymentMethod {
    pub id: i64,
    pub name: String,
}

/// The category a transaction belonged to before that category was deleted.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviousCategory {
    pub name: String,
    pub kind: CategoryKind,
}

/// A transaction. `amount` is always positive; the category kind decides
/// whether it counts as income or expense.
#[derive(Debug, Clone, PartialEq)]
pub struct Transaction {
    pub id: i64,
    pub date: NaiveDate,
    pub amount: Money,
    /// `None` when the transaction is unassigned because its category was deleted.
    pub category_id: Option<i64>,
    pub note: String,
    /// Set while the transaction is unassigned, so it can be re-filed later.
    pub previous_category: Option<PreviousCategory>,
    /// How an expense was paid. `None` if not recorded, or if the payment
    /// method was deleted.
    pub payment_method_id: Option<i64>,
    /// The name of the deleted payment method, so re-adding a payment method
    /// with that name moves the transaction back to it.
    pub previous_payment_method: Option<String>,
}

/// Fields for creating or updating a transaction.
#[derive(Debug, Clone, PartialEq)]
pub struct TransactionInput {
    pub date: NaiveDate,
    pub amount: Money,
    pub category_id: i64,
    /// Only kept for expenses; ignored for income.
    pub payment_method_id: Option<i64>,
    pub note: String,
}

/// A calendar month, used for filtering and budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Month {
    pub year: i32,
    pub month: u32,
}

impl Month {
    pub fn containing(date: NaiveDate) -> Self {
        Month {
            year: date.year(),
            month: date.month(),
        }
    }

    pub fn current() -> Self {
        Self::containing(chrono::Local::now().date_naive())
    }

    pub fn next(self) -> Self {
        if self.month == 12 {
            Month { year: self.year + 1, month: 1 }
        } else {
            Month { year: self.year, month: self.month + 1 }
        }
    }

    pub fn prev(self) -> Self {
        if self.month == 1 {
            Month { year: self.year - 1, month: 12 }
        } else {
            Month { year: self.year, month: self.month - 1 }
        }
    }

    /// The `YYYY-MM` key used in the database.
    pub fn key(self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }

    pub fn first_day(self) -> NaiveDate {
        NaiveDate::from_ymd_opt(self.year, self.month, 1).expect("valid month")
    }

    /// Human label like "October 2026".
    pub fn label(self) -> String {
        self.first_day().format("%B %Y").to_string()
    }

    /// A sensible default date for new entries in this month: today if it
    /// falls inside the month, otherwise the first day.
    pub fn default_entry_date(self) -> NaiveDate {
        let today = chrono::Local::now().date_naive();
        if Month::containing(today) == self {
            today
        } else {
            self.first_day()
        }
    }
}

/// Spending and budget status for one category in a month.
#[derive(Debug, Clone, PartialEq)]
pub struct CategoryStatus {
    pub category: Category,
    pub spent: Money,
    pub budget: Option<Money>,
}

impl CategoryStatus {
    pub fn remaining(&self) -> Option<Money> {
        self.budget.map(|b| Money(b.0 - self.spent.0))
    }

    pub fn is_over_budget(&self) -> bool {
        self.remaining().is_some_and(|r| r.0 < 0)
    }

    /// Fraction of budget used, 0.0..=1.0 (clamped).
    pub fn used_fraction(&self) -> f32 {
        match self.budget {
            Some(b) if b.0 > 0 => (self.spent.0 as f32 / b.0 as f32).clamp(0.0, 1.0),
            Some(_) if self.spent.0 > 0 => 1.0,
            _ => 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MonthSummary {
    pub income: Money,
    pub expenses: Money,
    /// The part of `expenses` with no category (its category was deleted).
    pub unassigned_expenses: Money,
}

/// A month's expenses grouped by how they were paid.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PaymentMethodSpending {
    /// Every payment method, including those with nothing spent.
    pub by_method: Vec<(PaymentMethod, Money)>,
    /// Expenses whose payment method was deleted.
    pub unassigned: Money,
    /// Expenses with no payment method recorded.
    pub unspecified: Money,
}

impl MonthSummary {
    pub fn net(&self) -> Money {
        Money(self.income.0 - self.expenses.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_money() {
        assert_eq!(Money::parse("12"), Some(Money(1200)));
        assert_eq!(Money::parse("12.5"), Some(Money(1250)));
        assert_eq!(Money::parse("12.05"), Some(Money(1205)));
        assert_eq!(Money::parse(".5"), Some(Money(50)));
        assert_eq!(Money::parse("$1,234.56"), Some(Money(123456)));
        assert_eq!(Money::parse("-3.07"), Some(Money(-307)));
        assert_eq!(Money::parse(""), None);
        assert_eq!(Money::parse("abc"), None);
        assert_eq!(Money::parse("1.234"), None);
        assert_eq!(Money::parse("."), None);
    }

    #[test]
    fn format_money() {
        assert_eq!(Money(123456).to_string(), "$1,234.56");
        assert_eq!(Money(5).to_string(), "$0.05");
        assert_eq!(Money(-100000).to_string(), "-$1,000.00");
        assert_eq!(Money(123456).to_plain(), "1234.56");
    }

    #[test]
    fn month_navigation() {
        let m = Month { year: 2026, month: 12 };
        assert_eq!(m.next(), Month { year: 2027, month: 1 });
        assert_eq!(m.next().prev(), m);
        assert_eq!(Month { year: 2026, month: 1 }.prev().key(), "2025-12");
        assert_eq!(Month { year: 2026, month: 10 }.label(), "October 2026");
    }

    #[test]
    fn category_status() {
        let category = Category { id: 1, name: "Food".into(), kind: CategoryKind::Expense };
        let s = CategoryStatus { category, spent: Money(15000), budget: Some(Money(10000)) };
        assert!(s.is_over_budget());
        assert_eq!(s.remaining(), Some(Money(-5000)));
        assert_eq!(s.used_fraction(), 1.0);
    }

    #[test]
    fn category_name_validation() {
        let existing = [Category { id: 1, name: "Food".into(), kind: CategoryKind::Expense }];
        assert_eq!(validate_category_name("  Pets ", &existing), Ok("Pets".into()));
        assert!(validate_category_name("   ", &existing).is_err());
        assert!(validate_category_name("food", &existing).is_err());
        assert!(validate_category_name("unassigned", &existing).is_err());
    }

    #[test]
    fn payment_method_name_validation() {
        let existing = [PaymentMethod { id: 1, name: "Chase Visa".into() }];
        assert_eq!(validate_payment_method_name(" Amex ", &existing), Ok("Amex".into()));
        assert!(validate_payment_method_name("", &existing).is_err());
        assert_eq!(
            validate_payment_method_name("chase visa", &existing),
            Err("A payment method named \"Chase Visa\" already exists.".into())
        );
        assert!(validate_payment_method_name("Unassigned", &existing).is_err());
    }
}
