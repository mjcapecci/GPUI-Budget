use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context as _, Result};
use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::model::{
    Category, CategoryKind, CategoryStatus, Money, Month, MonthSummary, PaymentMethod,
    PaymentMethodSpending, PreviousCategory, Transaction, TransactionInput,
};

const MIGRATIONS: &[&str] = &[
    // v1: initial schema
    "CREATE TABLE categories (
        id   INTEGER PRIMARY KEY,
        name TEXT NOT NULL UNIQUE,
        kind TEXT NOT NULL CHECK (kind IN ('income', 'expense'))
    );
    CREATE TABLE budgets (
        category_id INTEGER NOT NULL REFERENCES categories(id) ON DELETE CASCADE,
        month       TEXT NOT NULL,
        limit_cents INTEGER NOT NULL,
        PRIMARY KEY (category_id, month)
    );
    CREATE TABLE transactions (
        id           INTEGER PRIMARY KEY,
        date         TEXT NOT NULL,
        amount_cents INTEGER NOT NULL,
        category_id  INTEGER NOT NULL REFERENCES categories(id),
        note         TEXT NOT NULL DEFAULT ''
    );
    CREATE INDEX transactions_date ON transactions(date);
    INSERT INTO categories (name, kind) VALUES
        ('Salary', 'income'),
        ('Other Income', 'income'),
        ('Housing', 'expense'),
        ('Groceries', 'expense'),
        ('Dining Out', 'expense'),
        ('Transportation', 'expense'),
        ('Utilities', 'expense'),
        ('Entertainment', 'expense'),
        ('Health', 'expense'),
        ('Other', 'expense');",
    // v2: categories can be deleted. Their transactions become unassigned
    // (NULL category) and remember the old category's name and kind, so
    // income stays income and they can be re-filed later. SQLite can't drop
    // NOT NULL in place, so the table is rebuilt.
    "CREATE TABLE transactions_new (
        id                     INTEGER PRIMARY KEY,
        date                   TEXT NOT NULL,
        amount_cents           INTEGER NOT NULL,
        category_id            INTEGER REFERENCES categories(id),
        note                   TEXT NOT NULL DEFAULT '',
        previous_category_name TEXT,
        previous_category_kind TEXT CHECK (previous_category_kind IN ('income', 'expense')),
        CHECK (category_id IS NOT NULL OR previous_category_kind IS NOT NULL)
    );
    INSERT INTO transactions_new (id, date, amount_cents, category_id, note)
        SELECT id, date, amount_cents, category_id, note FROM transactions;
    DROP TABLE transactions;
    ALTER TABLE transactions_new RENAME TO transactions;
    CREATE INDEX transactions_date ON transactions(date);",
    // v3: app preferences, such as light or dark mode.
    "CREATE TABLE settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );",
    // v4: payment methods, such as credit cards, for expenses. Like
    // categories, a deleted payment method leaves its transactions with a
    // NULL payment method and remembers its name so they can be restored.
    "CREATE TABLE payment_methods (
        id   INTEGER PRIMARY KEY,
        name TEXT NOT NULL UNIQUE
    );
    ALTER TABLE transactions ADD COLUMN payment_method_id INTEGER REFERENCES payment_methods(id);
    ALTER TABLE transactions ADD COLUMN previous_payment_method_name TEXT;",
];

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating data directory {}", dir.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let db = Db { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(sql)
                .with_context(|| format!("running migration {}", i + 1))?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }

    // MARK: Settings

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    // MARK: Categories

    pub fn categories(&self) -> Result<Vec<Category>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, kind FROM categories ORDER BY kind DESC, name")?;
        let rows = stmt.query_map([], |r| {
            let kind: String = r.get(2)?;
            Ok(Category {
                id: r.get(0)?,
                name: r.get(1)?,
                kind: CategoryKind::from_str(&kind).unwrap_or(CategoryKind::Expense),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Add a category. If a deleted category with exactly this name and kind
    /// left unassigned transactions behind, they are moved into the new one.
    pub fn add_category(&self, name: &str, kind: CategoryKind) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO categories (name, kind) VALUES (?1, ?2)",
            params![name, kind.as_str()],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE transactions
             SET category_id = ?1, previous_category_name = NULL, previous_category_kind = NULL
             WHERE category_id IS NULL
               AND previous_category_name = ?2 AND previous_category_kind = ?3",
            params![id, name, kind.as_str()],
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Delete a category. Its transactions in every month become unassigned,
    /// remembering the old category, and its budgets are removed. Returns
    /// the number of transactions that were unassigned.
    pub fn delete_category(&self, id: i64) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let unassigned = tx.execute(
            "UPDATE transactions
             SET category_id = NULL,
                 previous_category_name = (SELECT name FROM categories WHERE id = ?1),
                 previous_category_kind = (SELECT kind FROM categories WHERE id = ?1)
             WHERE category_id = ?1",
            [id],
        )?;
        tx.execute("DELETE FROM categories WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(unassigned)
    }

    /// Number of transactions in each category across all months. Unassigned
    /// transactions are counted under `None`.
    pub fn transaction_counts(&self) -> Result<HashMap<Option<i64>, usize>> {
        let mut stmt = self
            .conn
            .prepare("SELECT category_id, COUNT(*) FROM transactions GROUP BY category_id")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as usize)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // MARK: Payment methods

    pub fn payment_methods(&self) -> Result<Vec<PaymentMethod>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name FROM payment_methods ORDER BY name")?;
        let rows = stmt.query_map([], |r| {
            Ok(PaymentMethod {
                id: r.get(0)?,
                name: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Add a payment method. If a deleted payment method with exactly this
    /// name left transactions behind, they are moved into the new one.
    pub fn add_payment_method(&self, name: &str) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("INSERT INTO payment_methods (name) VALUES (?1)", [name])?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE transactions
             SET payment_method_id = ?1, previous_payment_method_name = NULL
             WHERE payment_method_id IS NULL AND previous_payment_method_name = ?2",
            params![id, name],
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Delete a payment method. Its transactions in every month lose their
    /// payment method but remember its name. Returns how many were affected.
    pub fn delete_payment_method(&self, id: i64) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let unassigned = tx.execute(
            "UPDATE transactions
             SET payment_method_id = NULL,
                 previous_payment_method_name = (SELECT name FROM payment_methods WHERE id = ?1)
             WHERE payment_method_id = ?1",
            [id],
        )?;
        tx.execute("DELETE FROM payment_methods WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(unassigned)
    }

    /// Number of transactions paid with each payment method across all
    /// months. Those whose payment method was deleted are counted under
    /// `None`; those that never had one are not counted.
    pub fn payment_method_counts(&self) -> Result<HashMap<Option<i64>, usize>> {
        let mut stmt = self.conn.prepare(
            "SELECT payment_method_id, COUNT(*) FROM transactions
             WHERE payment_method_id IS NOT NULL OR previous_payment_method_name IS NOT NULL
             GROUP BY payment_method_id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as usize)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // MARK: Transactions

    pub fn transactions_in(&self, month: Month) -> Result<Vec<Transaction>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, date, amount_cents, category_id, note,
                    previous_category_name, previous_category_kind,
                    payment_method_id, previous_payment_method_name FROM transactions
             WHERE substr(date, 1, 7) = ?1
             ORDER BY date DESC, id DESC",
        )?;
        let rows = stmt.query_map([month.key()], row_to_transaction)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Income never has a payment method, so it is dropped for income
    /// categories.
    pub fn add_transaction(&self, t: &TransactionInput) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO transactions (date, amount_cents, category_id, note, payment_method_id)
             VALUES (?1, ?2, ?3, ?4,
                     CASE (SELECT kind FROM categories WHERE id = ?3)
                         WHEN 'expense' THEN ?5 END)",
            params![
                t.date.to_string(),
                t.amount.0,
                t.category_id,
                t.note,
                t.payment_method_id
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Choosing a payment method replaces any remembered deleted one. Leaving
    /// it empty keeps the deleted one remembered, so editing an expense
    /// doesn't stop it from being restored later.
    pub fn update_transaction(&self, id: i64, t: &TransactionInput) -> Result<()> {
        self.conn.execute(
            "UPDATE transactions SET date = ?1, amount_cents = ?2, category_id = ?3, note = ?4,
                    previous_category_name = NULL, previous_category_kind = NULL,
                    payment_method_id = CASE (SELECT kind FROM categories WHERE id = ?3)
                        WHEN 'expense' THEN ?5 END,
                    previous_payment_method_name = CASE
                        WHEN ?5 IS NULL AND (SELECT kind FROM categories WHERE id = ?3) = 'expense'
                        THEN previous_payment_method_name END
             WHERE id = ?6",
            params![
                t.date.to_string(),
                t.amount.0,
                t.category_id,
                t.note,
                t.payment_method_id,
                id
            ],
        )?;
        Ok(())
    }

    pub fn delete_transaction(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM transactions WHERE id = ?1", [id])?;
        Ok(())
    }

    // MARK: Budgets

    /// Set the monthly budget for a category. `None` removes it.
    pub fn set_budget(&self, category_id: i64, month: Month, limit: Option<Money>) -> Result<()> {
        match limit {
            Some(limit) => self.conn.execute(
                "INSERT INTO budgets (category_id, month, limit_cents) VALUES (?1, ?2, ?3)
                 ON CONFLICT (category_id, month) DO UPDATE SET limit_cents = excluded.limit_cents",
                params![category_id, month.key(), limit.0],
            )?,
            None => self.conn.execute(
                "DELETE FROM budgets WHERE category_id = ?1 AND month = ?2",
                params![category_id, month.key()],
            )?,
        };
        Ok(())
    }

    /// The budget for a month, falling back to the most recent earlier month
    /// that had one, so budgets carry forward until changed.
    pub fn budget_for(&self, category_id: i64, month: Month) -> Result<Option<Money>> {
        Ok(self
            .conn
            .query_row(
                "SELECT limit_cents FROM budgets WHERE category_id = ?1 AND month <= ?2
                 ORDER BY month DESC LIMIT 1",
                params![category_id, month.key()],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map(Money))
    }

    // MARK: Summaries

    /// Spending and budget for every expense category in the month.
    pub fn category_statuses(&self, month: Month) -> Result<Vec<CategoryStatus>> {
        let mut stmt = self.conn.prepare(
            "SELECT category_id, SUM(amount_cents) FROM transactions
             WHERE substr(date, 1, 7) = ?1 GROUP BY category_id",
        )?;
        let spent: HashMap<Option<i64>, i64> = stmt
            .query_map([month.key()], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;

        self.categories()?
            .into_iter()
            .filter(|c| c.kind == CategoryKind::Expense)
            .map(|category| {
                Ok(CategoryStatus {
                    spent: Money(spent.get(&Some(category.id)).copied().unwrap_or(0)),
                    budget: self.budget_for(category.id, month)?,
                    category,
                })
            })
            .collect()
    }

    pub fn month_summary(&self, month: Month) -> Result<MonthSummary> {
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(c.kind, t.previous_category_kind), t.category_id IS NULL,
                    SUM(t.amount_cents)
             FROM transactions t LEFT JOIN categories c ON c.id = t.category_id
             WHERE substr(t.date, 1, 7) = ?1 GROUP BY 1, 2",
        )?;
        let mut summary = MonthSummary::default();
        let rows = stmt.query_map([month.key()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, bool>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (kind, unassigned, total) = row?;
            match CategoryKind::from_str(&kind) {
                Some(CategoryKind::Income) => summary.income.0 += total,
                Some(CategoryKind::Expense) => {
                    summary.expenses.0 += total;
                    if unassigned {
                        summary.unassigned_expenses.0 += total;
                    }
                }
                None => {}
            }
        }
        Ok(summary)
    }

    /// The month's expenses grouped by payment method.
    pub fn payment_method_spending(&self, month: Month) -> Result<PaymentMethodSpending> {
        let mut stmt = self.conn.prepare(
            "SELECT t.payment_method_id, t.previous_payment_method_name IS NOT NULL,
                    SUM(t.amount_cents)
             FROM transactions t LEFT JOIN categories c ON c.id = t.category_id
             WHERE substr(t.date, 1, 7) = ?1
               AND COALESCE(c.kind, t.previous_category_kind) = 'expense'
             GROUP BY 1, 2",
        )?;
        let rows = stmt.query_map([month.key()], |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?,
                r.get::<_, bool>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        let mut spent = HashMap::new();
        let mut spending = PaymentMethodSpending::default();
        for row in rows {
            match row? {
                (Some(id), _, total) => {
                    spent.insert(id, total);
                }
                (None, true, total) => spending.unassigned.0 += total,
                (None, false, total) => spending.unspecified.0 += total,
            }
        }
        spending.by_method = self
            .payment_methods()?
            .into_iter()
            .map(|p| {
                let total = Money(spent.get(&p.id).copied().unwrap_or(0));
                (p, total)
            })
            .collect();
        Ok(spending)
    }
}

fn row_to_transaction(r: &Row) -> rusqlite::Result<Transaction> {
    let date: String = r.get(1)?;
    Ok(Transaction {
        id: r.get(0)?,
        date: NaiveDate::parse_from_str(&date, "%Y-%m-%d").map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?,
        amount: Money(r.get(2)?),
        category_id: r.get(3)?,
        note: r.get(4)?,
        previous_category: match (
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<String>>(6)?,
        ) {
            (Some(name), Some(kind)) => Some(PreviousCategory {
                name,
                kind: CategoryKind::from_str(&kind).unwrap_or(CategoryKind::Expense),
            }),
            _ => None,
        },
        payment_method_id: r.get(7)?,
        previous_payment_method: r.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn category(db: &Db, name: &str) -> i64 {
        db.categories()
            .unwrap()
            .into_iter()
            .find(|c| c.name == name)
            .unwrap()
            .id
    }

    fn add(db: &Db, d: &str, cents: i64, cat: &str) -> i64 {
        db.add_transaction(&TransactionInput {
            date: date(d),
            amount: Money(cents),
            category_id: category(db, cat),
            payment_method_id: None,
            note: String::new(),
        })
        .unwrap()
    }

    const OCT: Month = Month {
        year: 2026,
        month: 10,
    };

    #[test]
    fn seeds_categories_and_migrates_once() {
        let db = Db::open_in_memory().unwrap();
        let n = db.categories().unwrap().len();
        assert!(n >= 5);
        db.migrate().unwrap();
        assert_eq!(db.categories().unwrap().len(), n);
    }

    #[test]
    fn transaction_crud_filters_by_month() {
        let db = Db::open_in_memory().unwrap();
        let a = add(&db, "2026-10-01", 1000, "Groceries");
        add(&db, "2026-10-15", 2000, "Groceries");
        add(&db, "2026-09-30", 500, "Groceries");

        let oct = db.transactions_in(OCT).unwrap();
        assert_eq!(oct.len(), 2);
        assert_eq!(oct[0].date, date("2026-10-15"), "newest first");

        db.update_transaction(
            a,
            &TransactionInput {
                date: date("2026-10-02"),
                amount: Money(1234),
                category_id: category(&db, "Dining Out"),
                payment_method_id: None,
                note: "lunch".into(),
            },
        )
        .unwrap();
        let t = db
            .transactions_in(OCT)
            .unwrap()
            .into_iter()
            .find(|t| t.id == a)
            .unwrap();
        assert_eq!((t.amount, t.note.as_str()), (Money(1234), "lunch"));

        db.delete_transaction(a).unwrap();
        assert_eq!(db.transactions_in(OCT).unwrap().len(), 1);
    }

    #[test]
    fn summary_splits_income_and_expenses() {
        let db = Db::open_in_memory().unwrap();
        add(&db, "2026-10-01", 500000, "Salary");
        add(&db, "2026-10-03", 12000, "Groceries");
        add(&db, "2026-10-04", 3000, "Dining Out");
        let s = db.month_summary(OCT).unwrap();
        assert_eq!(s.income, Money(500000));
        assert_eq!(s.expenses, Money(15000));
        assert_eq!(s.net(), Money(485000));
    }

    #[test]
    fn budgets_carry_forward_and_track_spending() {
        let db = Db::open_in_memory().unwrap();
        let groceries = category(&db, "Groceries");
        db.set_budget(groceries, OCT.prev(), Some(Money(40000)))
            .unwrap();
        add(&db, "2026-10-03", 45000, "Groceries");

        let status = db
            .category_statuses(OCT)
            .unwrap()
            .into_iter()
            .find(|s| s.category.id == groceries)
            .unwrap();
        assert_eq!(
            status.budget,
            Some(Money(40000)),
            "inherited from September"
        );
        assert_eq!(status.spent, Money(45000));
        assert!(status.is_over_budget());

        db.set_budget(groceries, OCT, Some(Money(50000))).unwrap();
        assert_eq!(db.budget_for(groceries, OCT).unwrap(), Some(Money(50000)));
        assert_eq!(
            db.budget_for(groceries, OCT.prev()).unwrap(),
            Some(Money(40000))
        );

        db.set_budget(groceries, OCT, None).unwrap();
        assert_eq!(db.budget_for(groceries, OCT).unwrap(), Some(Money(40000)));
    }

    #[test]
    fn settings_round_trip() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.setting("theme").unwrap(), None);
        db.set_setting("theme", "dark").unwrap();
        db.set_setting("theme", "light").unwrap();
        assert_eq!(db.setting("theme").unwrap().as_deref(), Some("light"));
    }

    #[test]
    fn add_category() {
        let db = Db::open_in_memory().unwrap();
        let id = db.add_category("Pets", CategoryKind::Expense).unwrap();
        let pets = db
            .categories()
            .unwrap()
            .into_iter()
            .find(|c| c.id == id)
            .unwrap();
        assert_eq!(
            (pets.name.as_str(), pets.kind),
            ("Pets", CategoryKind::Expense)
        );
        assert!(
            db.add_category("Pets", CategoryKind::Income).is_err(),
            "names are unique"
        );
    }

    #[test]
    fn deleting_a_category_unassigns_its_transactions_in_every_month() {
        let db = Db::open_in_memory().unwrap();
        let groceries = category(&db, "Groceries");
        let salary = category(&db, "Salary");
        let sep = add(&db, "2026-09-12", 4000, "Groceries");
        let oct = add(&db, "2026-10-03", 6000, "Groceries");
        add(&db, "2026-10-01", 500000, "Salary");
        add(&db, "2026-10-04", 3000, "Dining Out");
        db.set_budget(groceries, OCT, Some(Money(40000))).unwrap();

        assert_eq!(db.delete_category(groceries).unwrap(), 2);
        assert_eq!(db.delete_category(salary).unwrap(), 1);
        assert!(
            db.categories()
                .unwrap()
                .iter()
                .all(|c| c.id != groceries && c.id != salary)
        );
        assert_eq!(db.budget_for(groceries, OCT).unwrap(), None);

        let all: Vec<Transaction> = [OCT.prev(), OCT]
            .into_iter()
            .flat_map(|m| db.transactions_in(m).unwrap())
            .collect();
        for id in [sep, oct] {
            let t = all.iter().find(|t| t.id == id).unwrap();
            assert_eq!(t.category_id, None);
            assert_eq!(
                t.previous_category,
                Some(PreviousCategory {
                    name: "Groceries".into(),
                    kind: CategoryKind::Expense
                })
            );
        }

        // Totals are unchanged: unassigned income is still income.
        let s = db.month_summary(OCT).unwrap();
        assert_eq!(s.income, Money(500000));
        assert_eq!(s.expenses, Money(9000));
        assert_eq!(s.unassigned_expenses, Money(6000));

        let counts = db.transaction_counts().unwrap();
        assert_eq!(counts.get(&None), Some(&3));

        // Re-filing a transaction clears its previous category.
        db.update_transaction(
            oct,
            &TransactionInput {
                date: date("2026-10-03"),
                amount: Money(6000),
                category_id: category(&db, "Other"),
                payment_method_id: None,
                note: String::new(),
            },
        )
        .unwrap();
        let t = db
            .transactions_in(OCT)
            .unwrap()
            .into_iter()
            .find(|t| t.id == oct)
            .unwrap();
        assert_eq!(t.category_id, Some(category(&db, "Other")));
        assert_eq!(t.previous_category, None);
        assert_eq!(db.month_summary(OCT).unwrap().unassigned_expenses, Money(0));
    }

    #[test]
    fn re_adding_a_deleted_category_restores_its_transactions() {
        let db = Db::open_in_memory().unwrap();
        let sep = add(&db, "2026-09-12", 4000, "Groceries");
        let oct = add(&db, "2026-10-03", 6000, "Groceries");
        let salary = add(&db, "2026-10-01", 500000, "Salary");
        db.delete_category(category(&db, "Groceries")).unwrap();
        db.delete_category(category(&db, "Salary")).unwrap();

        // Same name but a different kind, or a different case, is a different category.
        db.add_category("Salary", CategoryKind::Expense).unwrap();
        db.add_category("groceries", CategoryKind::Expense).unwrap();
        assert_eq!(db.transaction_counts().unwrap().get(&None), Some(&3));

        let groceries = db.add_category("Groceries", CategoryKind::Expense).unwrap();
        let all: Vec<Transaction> = [OCT.prev(), OCT]
            .into_iter()
            .flat_map(|m| db.transactions_in(m).unwrap())
            .collect();
        for id in [sep, oct] {
            let t = all.iter().find(|t| t.id == id).unwrap();
            assert_eq!(t.category_id, Some(groceries));
            assert_eq!(t.previous_category, None);
        }
        let t = all.iter().find(|t| t.id == salary).unwrap();
        assert_eq!(
            t.category_id, None,
            "still waiting for an income category named Salary"
        );
        assert_eq!(db.month_summary(OCT).unwrap().unassigned_expenses, Money(0));
    }

    fn pay(db: &Db, d: &str, cents: i64, cat: &str, method: Option<i64>) -> i64 {
        db.add_transaction(&TransactionInput {
            date: date(d),
            amount: Money(cents),
            category_id: category(db, cat),
            payment_method_id: method,
            note: String::new(),
        })
        .unwrap()
    }

    fn find(db: &Db, id: i64) -> Transaction {
        [OCT.prev(), OCT]
            .into_iter()
            .flat_map(|m| db.transactions_in(m).unwrap())
            .find(|t| t.id == id)
            .unwrap()
    }

    #[test]
    fn spending_is_grouped_by_payment_method() {
        let db = Db::open_in_memory().unwrap();
        let visa = db.add_payment_method("Visa").unwrap();
        let amex = db.add_payment_method("Amex").unwrap();
        assert!(db.add_payment_method("Visa").is_err(), "names are unique");
        let names: Vec<String> = db
            .payment_methods()
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, ["Amex", "Visa"]);

        pay(&db, "2026-10-03", 12000, "Groceries", Some(visa));
        pay(&db, "2026-10-04", 3000, "Dining Out", Some(visa));
        pay(&db, "2026-10-05", 800, "Dining Out", None);
        pay(&db, "2026-09-20", 9900, "Groceries", Some(visa));
        let salary = pay(&db, "2026-10-01", 500000, "Salary", Some(amex));
        assert_eq!(
            find(&db, salary).payment_method_id,
            None,
            "income has no payment method"
        );

        let s = db.payment_method_spending(OCT).unwrap();
        let totals: Vec<(&str, Money)> = s
            .by_method
            .iter()
            .map(|(p, m)| (p.name.as_str(), *m))
            .collect();
        assert_eq!(totals, [("Amex", Money(0)), ("Visa", Money(15000))]);
        assert_eq!((s.unassigned, s.unspecified), (Money(0), Money(800)));
    }

    #[test]
    fn deleting_and_re_adding_a_payment_method_restores_its_transactions() {
        let db = Db::open_in_memory().unwrap();
        let visa = db.add_payment_method("Visa").unwrap();
        let sep = pay(&db, "2026-09-12", 4000, "Groceries", Some(visa));
        let oct = pay(&db, "2026-10-03", 6000, "Groceries", Some(visa));
        let edited = pay(&db, "2026-10-04", 2500, "Dining Out", Some(visa));
        let refiled = pay(&db, "2026-10-05", 1000, "Dining Out", Some(visa));
        pay(&db, "2026-10-06", 700, "Dining Out", None);

        assert_eq!(db.delete_payment_method(visa).unwrap(), 4);
        assert!(db.payment_methods().unwrap().is_empty());
        let t = find(&db, oct);
        assert_eq!(
            (t.payment_method_id, t.previous_payment_method.as_deref()),
            (None, Some("Visa"))
        );
        assert_eq!(db.payment_method_counts().unwrap().get(&None), Some(&4));
        let s = db.payment_method_spending(OCT).unwrap();
        assert_eq!((s.unassigned, s.unspecified), (Money(9500), Money(700)));

        // Editing without choosing a payment method keeps it restorable;
        // choosing another one, or making it income, forgets it.
        let amex = db.add_payment_method("Amex").unwrap();
        let edit = |id, cat: &str, method| {
            db.update_transaction(
                id,
                &TransactionInput {
                    date: date("2026-10-04"),
                    amount: Money(2600),
                    category_id: category(&db, cat),
                    payment_method_id: method,
                    note: String::new(),
                },
            )
            .unwrap()
        };
        edit(edited, "Dining Out", None);
        edit(refiled, "Dining Out", Some(amex));
        edit(sep, "Other Income", None);
        assert_eq!(
            find(&db, edited).previous_payment_method.as_deref(),
            Some("Visa")
        );
        assert_eq!(find(&db, refiled).payment_method_id, Some(amex));
        assert_eq!(find(&db, refiled).previous_payment_method, None);
        assert_eq!(find(&db, sep).previous_payment_method, None);

        // Matching is exact, like categories.
        db.add_payment_method("visa").unwrap();
        assert_eq!(find(&db, oct).payment_method_id, None);

        let visa = db.add_payment_method("Visa").unwrap();
        for id in [oct, edited] {
            let t = find(&db, id);
            assert_eq!(
                (t.payment_method_id, t.previous_payment_method),
                (Some(visa), None)
            );
        }
        assert_eq!(find(&db, sep).payment_method_id, None);
        assert_eq!(db.payment_method_counts().unwrap().get(&None), None);
        assert_eq!(
            db.payment_method_counts().unwrap().get(&Some(visa)),
            Some(&2)
        );
    }

    #[test]
    fn migrating_a_v1_database_keeps_transactions() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute(
            "INSERT INTO transactions (date, amount_cents, category_id, note)
             SELECT '2026-10-05', 1234, id, 'milk' FROM categories WHERE name = 'Groceries'",
            [],
        )
        .unwrap();

        let db = Db::init(conn).unwrap();
        let t = &db.transactions_in(OCT).unwrap()[0];
        assert_eq!((t.amount, t.note.as_str()), (Money(1234), "milk"));
        assert_eq!(t.category_id, Some(category(&db, "Groceries")));
        assert_eq!(t.previous_category, None);
        assert_eq!(t.payment_method_id, None);
    }
}
