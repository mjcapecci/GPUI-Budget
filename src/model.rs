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
        let whole: i64 = if whole.is_empty() {
            0
        } else {
            whole.parse().ok()?
        };
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
    validate_name(
        name,
        existing.iter().map(|p| p.name.as_str()),
        "payment method",
    )
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
        return Err(format!(
            "\"{UNASSIGNED}\" is reserved for transactions whose {noun} was deleted."
        ));
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
    /// The recurring series this transaction was added from, if any.
    pub recurring_id: Option<i64>,
    /// Which of the series' months this transaction pays for. Usually the
    /// month of `date`, but it stays the same if the date is moved, so that
    /// month's payment isn't offered again.
    pub occurrence: Option<Month>,
    /// Set for a payment of a recurring transaction whose amount varies,
    /// such as a utility bill. `amount` is then an estimate until the user
    /// enters the actual amount.
    pub amount_pending: bool,
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
            Month {
                year: self.year + 1,
                month: 1,
            }
        } else {
            Month {
                year: self.year,
                month: self.month + 1,
            }
        }
    }

    pub fn prev(self) -> Self {
        if self.month == 1 {
            Month {
                year: self.year - 1,
                month: 12,
            }
        } else {
            Month {
                year: self.year,
                month: self.month - 1,
            }
        }
    }

    /// The `YYYY-MM` key used in the database.
    pub fn key(self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }

    /// Parse a `YYYY-MM` database key.
    pub fn from_key(key: &str) -> Option<Self> {
        let (year, month) = key.split_once('-')?;
        let month = Month {
            year: year.parse().ok()?,
            month: month.parse().ok()?,
        };
        (1..=12).contains(&month.month).then_some(month)
    }

    pub fn first_day(self) -> NaiveDate {
        NaiveDate::from_ymd_opt(self.year, self.month, 1).expect("valid month")
    }

    /// `day` of this month, or the last day if the month is shorter, so a
    /// payment on the 31st falls on February 28th.
    pub fn day(self, day: u32) -> NaiveDate {
        let last = self
            .next()
            .first_day()
            .pred_opt()
            .expect("valid date")
            .day();
        NaiveDate::from_ymd_opt(self.year, self.month, day.clamp(1, last)).expect("valid date")
    }

    /// The month `n` months later (or earlier, if negative).
    pub fn plus(self, n: i32) -> Self {
        let index = self.year * 12 + self.month as i32 - 1 + n;
        Month {
            year: index.div_euclid(12),
            month: index.rem_euclid(12) as u32 + 1,
        }
    }

    /// Months from `self` to `later`; negative if `later` is earlier.
    pub fn months_until(self, later: Month) -> i32 {
        (later.year - self.year) * 12 + later.month as i32 - self.month as i32
    }

    /// Short label like "Oct 2026".
    pub fn short_label(self) -> String {
        self.first_day().format("%b %Y").to_string()
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

/// How often a recurring transaction repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frequency {
    Monthly,
    Quarterly,
    Yearly,
}

impl Frequency {
    pub const ALL: [Frequency; 3] = [Frequency::Monthly, Frequency::Quarterly, Frequency::Yearly];

    /// Months between payments, as stored in the database.
    pub fn months(self) -> i32 {
        match self {
            Frequency::Monthly => 1,
            Frequency::Quarterly => 3,
            Frequency::Yearly => 12,
        }
    }

    pub fn from_months(months: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.months() == months)
    }

    pub fn label(self) -> &'static str {
        match self {
            Frequency::Monthly => "Monthly",
            Frequency::Quarterly => "Every 3 months",
            Frequency::Yearly => "Yearly",
        }
    }
}

/// A transaction that repeats, such as rent or a subscription. Each payment
/// is added as an ordinary [`Transaction`] when the user accepts it, so
/// editing one payment, or the series, never changes payments already added.
#[derive(Debug, Clone, PartialEq)]
pub struct Recurring {
    pub id: i64,
    /// The month of the first payment.
    pub start: Month,
    /// The month of the last payment, or `None` to repeat until stopped.
    pub end: Option<Month>,
    pub frequency: Frequency,
    /// Day of the month payments fall on, 1–31. Shorter months use their
    /// last day.
    pub day: u32,
    /// `None` when its category was deleted, like [`Transaction::category_id`].
    pub category_id: Option<i64>,
    pub previous_category: Option<PreviousCategory>,
    pub payment_method_id: Option<i64>,
    pub previous_payment_method: Option<String>,
    pub note: String,
    /// The amount and each later change to it, oldest first. Each applies
    /// from its month until the next, so rent can go up in July without
    /// changing January's. `None` means the amount varies from that month
    /// on, like a utility bill, so it's entered once the bill arrives.
    pub amounts: Vec<(Month, Option<Money>)>,
}

impl Recurring {
    /// Whether a payment falls in `month`.
    pub fn occurs_in(&self, month: Month) -> bool {
        let n = self.start.months_until(month);
        n >= 0 && n % self.frequency.months() == 0 && self.end.is_none_or(|end| month <= end)
    }

    /// The amount of the payment in `month`, or `None` if it varies.
    pub fn amount_in(&self, month: Month) -> Option<Money> {
        self.amounts
            .iter()
            .rev()
            .find(|(from, _)| *from <= month)
            .or(self.amounts.first())
            .and_then(|(_, amount)| *amount)
    }

    pub fn date_in(&self, month: Month) -> NaiveDate {
        month.day(self.day)
    }

    /// The first month at or after `from` with a payment, if any.
    pub fn next_occurrence(&self, from: Month) -> Option<Month> {
        let every = self.frequency.months();
        let n = self.start.months_until(from).max(0);
        let month = self.start.plus((n + every - 1) / every * every);
        self.occurs_in(month).then_some(month)
    }

    /// The months with a payment from `from` to `to`, inclusive.
    pub fn occurrences(&self, from: Month, to: Month) -> Vec<Month> {
        let mut months = Vec::new();
        let mut next = self.next_occurrence(from);
        while let Some(month) = next.filter(|m| *m <= to) {
            months.push(month);
            next = self.next_occurrence(month.next());
        }
        months
    }

    /// Like "Monthly on the 1st" or "Yearly on Oct 18".
    pub fn schedule_label(&self) -> String {
        match self.frequency {
            Frequency::Yearly => format!("Yearly on {}", self.date_in(self.start).format("%b %-d")),
            f => format!("{} on the {}", f.label(), ordinal(self.day)),
        }
    }

    /// Like "Jan 2026 – Dec 2026" or "Since Jan 2026".
    pub fn range_label(&self) -> String {
        match self.end {
            Some(end) if end == self.start => self.start.short_label(),
            Some(end) => format!("{} – {}", self.start.short_label(), end.short_label()),
            None => format!("Since {}", self.start.short_label()),
        }
    }
}

/// "1st", "2nd", "23rd", "31st".
pub fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// Fields for creating or updating a recurring transaction. The amount is
/// set separately, since changing it takes effect from a chosen month.
#[derive(Debug, Clone, PartialEq)]
pub struct RecurringInput {
    pub start: Month,
    pub end: Option<Month>,
    pub frequency: Frequency,
    pub day: u32,
    pub category_id: i64,
    /// Only kept for expenses; ignored for income.
    pub payment_method_id: Option<i64>,
    pub note: String,
}

/// A recurring payment in a month that hasn't been added or skipped yet.
#[derive(Debug, Clone, PartialEq)]
pub struct DueTransaction {
    pub recurring_id: i64,
    pub month: Month,
    pub date: NaiveDate,
    /// The amount, or an estimate if `estimated`.
    pub amount: Money,
    /// The series' amount varies this month, so `amount` is the last amount
    /// entered for it (or zero) and the payment is added needing its amount.
    pub estimated: bool,
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
        let m = Month {
            year: 2026,
            month: 12,
        };
        assert_eq!(
            m.next(),
            Month {
                year: 2027,
                month: 1
            }
        );
        assert_eq!(m.next().prev(), m);
        assert_eq!(
            Month {
                year: 2026,
                month: 1
            }
            .prev()
            .key(),
            "2025-12"
        );
        assert_eq!(
            Month {
                year: 2026,
                month: 10
            }
            .label(),
            "October 2026"
        );
    }

    fn month(year: i32, month: u32) -> Month {
        Month { year, month }
    }

    #[test]
    fn month_arithmetic() {
        assert_eq!(month(2026, 11).plus(2), month(2027, 1));
        assert_eq!(month(2026, 1).plus(-1), month(2025, 12));
        assert_eq!(month(2026, 1).plus(-13), month(2024, 12));
        assert_eq!(month(2025, 11).months_until(month(2026, 2)), 3);
        assert_eq!(month(2026, 2).months_until(month(2025, 11)), -3);
        assert_eq!(Month::from_key("2026-07"), Some(month(2026, 7)));
        assert_eq!(Month::from_key("2026-13"), None);
        assert_eq!(month(2026, 2).day(31).to_string(), "2026-02-28");
        assert_eq!(month(2028, 2).day(30).to_string(), "2028-02-29");
        assert_eq!(month(2026, 3).day(31).to_string(), "2026-03-31");
    }

    fn rent() -> Recurring {
        Recurring {
            id: 1,
            start: month(2026, 1),
            end: None,
            frequency: Frequency::Monthly,
            day: 31,
            category_id: Some(1),
            previous_category: None,
            payment_method_id: None,
            previous_payment_method: None,
            note: "Rent".into(),
            amounts: vec![
                (month(2026, 1), Some(Money(150000))),
                (month(2026, 7), Some(Money(160000))),
                (month(2027, 1), None),
                (month(2027, 4), Some(Money(170000))),
            ],
        }
    }

    #[test]
    fn recurring_schedule() {
        let mut r = rent();
        assert!(!r.occurs_in(month(2025, 12)));
        assert!(r.occurs_in(month(2026, 1)));
        assert!(r.occurs_in(month(2030, 5)));
        assert_eq!(r.date_in(month(2026, 4)).to_string(), "2026-04-30");
        assert_eq!(r.amount_in(month(2026, 6)), Some(Money(150000)));
        assert_eq!(r.amount_in(month(2026, 7)), Some(Money(160000)));
        assert_eq!(r.amount_in(month(2027, 2)), None, "varies from January");
        assert_eq!(
            r.amount_in(month(2027, 4)),
            Some(Money(170000)),
            "fixed again"
        );
        assert_eq!(r.schedule_label(), "Monthly on the 31st");
        assert_eq!(r.range_label(), "Since Jan 2026");

        r.end = Some(month(2026, 3));
        assert!(r.occurs_in(month(2026, 3)));
        assert!(!r.occurs_in(month(2026, 4)));
        assert_eq!(r.next_occurrence(month(2026, 4)), None);
        assert_eq!(r.range_label(), "Jan 2026 – Mar 2026");

        r.end = None;
        r.frequency = Frequency::Quarterly;
        assert!(!r.occurs_in(month(2026, 2)));
        assert!(r.occurs_in(month(2026, 4)));
        assert_eq!(r.next_occurrence(month(2026, 2)), Some(month(2026, 4)));
        assert_eq!(r.next_occurrence(month(2025, 6)), Some(month(2026, 1)));
        assert_eq!(
            r.occurrences(month(2026, 1), month(2026, 12)),
            [
                month(2026, 1),
                month(2026, 4),
                month(2026, 7),
                month(2026, 10)
            ]
        );

        r.frequency = Frequency::Yearly;
        r.end = Some(month(2027, 6));
        assert_eq!(r.next_occurrence(month(2026, 2)), Some(month(2027, 1)));
        assert_eq!(r.next_occurrence(month(2027, 2)), None);
        assert_eq!(r.schedule_label(), "Yearly on Jan 31");
    }

    #[test]
    fn ordinals() {
        let got: Vec<String> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 23, 31]
            .into_iter()
            .map(ordinal)
            .collect();
        assert_eq!(
            got,
            [
                "1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "23rd", "31st"
            ]
        );
    }

    #[test]
    fn category_status() {
        let category = Category {
            id: 1,
            name: "Food".into(),
            kind: CategoryKind::Expense,
        };
        let s = CategoryStatus {
            category,
            spent: Money(15000),
            budget: Some(Money(10000)),
        };
        assert!(s.is_over_budget());
        assert_eq!(s.remaining(), Some(Money(-5000)));
        assert_eq!(s.used_fraction(), 1.0);
    }

    #[test]
    fn category_name_validation() {
        let existing = [Category {
            id: 1,
            name: "Food".into(),
            kind: CategoryKind::Expense,
        }];
        assert_eq!(
            validate_category_name("  Pets ", &existing),
            Ok("Pets".into())
        );
        assert!(validate_category_name("   ", &existing).is_err());
        assert!(validate_category_name("food", &existing).is_err());
        assert!(validate_category_name("unassigned", &existing).is_err());
    }

    #[test]
    fn payment_method_name_validation() {
        let existing = [PaymentMethod {
            id: 1,
            name: "Chase Visa".into(),
        }];
        assert_eq!(
            validate_payment_method_name(" Amex ", &existing),
            Ok("Amex".into())
        );
        assert!(validate_payment_method_name("", &existing).is_err());
        assert_eq!(
            validate_payment_method_name("chase visa", &existing),
            Err("A payment method named \"Chase Visa\" already exists.".into())
        );
        assert!(validate_payment_method_name("Unassigned", &existing).is_err());
    }
}
