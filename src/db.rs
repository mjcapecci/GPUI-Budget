use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::model::{
    Category, CategoryKind, CategoryStatus, DueTransaction, Frequency, Money, Month, MonthSummary,
    PaymentMethod, PaymentMethodSpending, PreviousCategory, Recurring, RecurringInput, Transaction,
    TransactionInput,
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
    // v5: the category last chosen for each merchant when importing
    // screenshots, keyed by the lowercased merchant name.
    "CREATE TABLE merchant_categories (
        merchant    TEXT PRIMARY KEY,
        category_id INTEGER NOT NULL REFERENCES categories(id) ON DELETE CASCADE
    );",
    // v6: recurring transactions. Each payment is added as an ordinary
    // transaction linked to its series and the month it pays for; the unique
    // index stops a month's payment being added twice. The amount can change
    // from a given month on, like budgets, without touching payments already
    // added. Skipped months aren't offered again. A deleted category or
    // payment method is remembered by name, as for transactions.
    "CREATE TABLE recurring (
        id                           INTEGER PRIMARY KEY,
        start_month                  TEXT NOT NULL,
        end_month                    TEXT,
        every_months                 INTEGER NOT NULL DEFAULT 1 CHECK (every_months >= 1),
        day                          INTEGER NOT NULL CHECK (day BETWEEN 1 AND 31),
        category_id                  INTEGER REFERENCES categories(id),
        note                         TEXT NOT NULL DEFAULT '',
        previous_category_name       TEXT,
        previous_category_kind       TEXT CHECK (previous_category_kind IN ('income', 'expense')),
        payment_method_id            INTEGER REFERENCES payment_methods(id),
        previous_payment_method_name TEXT,
        CHECK (category_id IS NOT NULL OR previous_category_kind IS NOT NULL),
        CHECK (end_month IS NULL OR end_month >= start_month)
    );
    CREATE TABLE recurring_amounts (
        recurring_id INTEGER NOT NULL REFERENCES recurring(id) ON DELETE CASCADE,
        month        TEXT NOT NULL,
        amount_cents INTEGER NOT NULL,
        PRIMARY KEY (recurring_id, month)
    );
    CREATE TABLE recurring_skips (
        recurring_id INTEGER NOT NULL REFERENCES recurring(id) ON DELETE CASCADE,
        month        TEXT NOT NULL,
        PRIMARY KEY (recurring_id, month)
    );
    ALTER TABLE transactions ADD COLUMN recurring_id INTEGER REFERENCES recurring(id);
    ALTER TABLE transactions ADD COLUMN occurrence TEXT;
    CREATE UNIQUE INDEX transactions_occurrence ON transactions(recurring_id, occurrence);",
    // v7: a recurring transaction's amount can vary (a NULL amount from a
    // month on), like a utility bill. Its payments are added with an
    // estimate and marked as needing their amount until the user enters it.
    // SQLite can't drop NOT NULL in place, so the amounts table is rebuilt.
    "CREATE TABLE recurring_amounts_new (
        recurring_id INTEGER NOT NULL REFERENCES recurring(id) ON DELETE CASCADE,
        month        TEXT NOT NULL,
        amount_cents INTEGER,
        PRIMARY KEY (recurring_id, month)
    );
    INSERT INTO recurring_amounts_new SELECT recurring_id, month, amount_cents
        FROM recurring_amounts;
    DROP TABLE recurring_amounts;
    ALTER TABLE recurring_amounts_new RENAME TO recurring_amounts;
    ALTER TABLE transactions ADD COLUMN amount_pending INTEGER NOT NULL DEFAULT 0;",
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
        let version = schema_version(&conn)?;
        if version > 0 && version < MIGRATIONS.len() as i64 {
            backup(&conn, path, version)?;
        }
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
        let version = schema_version(&self.conn)?;
        if version > MIGRATIONS.len() as i64 {
            bail!(
                "this database was created by a newer version of Budget \
                 (schema v{version}, this build supports up to v{}); please update the app",
                MIGRATIONS.len()
            );
        }
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
    /// left unassigned transactions or recurring transactions behind, they
    /// are moved into the new one.
    pub fn add_category(&self, name: &str, kind: CategoryKind) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO categories (name, kind) VALUES (?1, ?2)",
            params![name, kind.as_str()],
        )?;
        let id = tx.last_insert_rowid();
        for table in ["transactions", "recurring"] {
            tx.execute(
                &format!(
                    "UPDATE {table}
                     SET category_id = ?1, previous_category_name = NULL,
                         previous_category_kind = NULL
                     WHERE category_id IS NULL
                       AND previous_category_name = ?2 AND previous_category_kind = ?3"
                ),
                params![id, name, kind.as_str()],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    /// Delete a category. Its transactions in every month, and its recurring
    /// transactions, become unassigned, remembering the old category, and its
    /// budgets are removed. Returns the number of transactions that were
    /// unassigned.
    pub fn delete_category(&self, id: i64) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let unassign = |table: &str| {
            tx.execute(
                &format!(
                    "UPDATE {table}
                     SET category_id = NULL,
                         previous_category_name = (SELECT name FROM categories WHERE id = ?1),
                         previous_category_kind = (SELECT kind FROM categories WHERE id = ?1)
                     WHERE category_id = ?1"
                ),
                [id],
            )
        };
        let unassigned = unassign("transactions")?;
        unassign("recurring")?;
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
    /// name left transactions or recurring transactions behind, they are
    /// moved into the new one.
    pub fn add_payment_method(&self, name: &str) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("INSERT INTO payment_methods (name) VALUES (?1)", [name])?;
        let id = tx.last_insert_rowid();
        for table in ["transactions", "recurring"] {
            tx.execute(
                &format!(
                    "UPDATE {table}
                     SET payment_method_id = ?1, previous_payment_method_name = NULL
                     WHERE payment_method_id IS NULL AND previous_payment_method_name = ?2"
                ),
                params![id, name],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    /// Delete a payment method. Its transactions in every month, and its
    /// recurring transactions, lose their payment method but remember its
    /// name. Returns how many transactions were affected.
    pub fn delete_payment_method(&self, id: i64) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let unassign = |table: &str| {
            tx.execute(
                &format!(
                    "UPDATE {table}
                     SET payment_method_id = NULL,
                         previous_payment_method_name =
                             (SELECT name FROM payment_methods WHERE id = ?1)
                     WHERE payment_method_id = ?1"
                ),
                [id],
            )
        };
        let unassigned = unassign("transactions")?;
        unassign("recurring")?;
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
                    payment_method_id, previous_payment_method_name,
                    recurring_id, occurrence, amount_pending FROM transactions
             WHERE substr(date, 1, 7) = ?1
             ORDER BY date DESC, id DESC",
        )?;
        let rows = stmt.query_map([month.key()], row_to_transaction)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Income never has a payment method, so it is dropped for income
    /// categories.
    /// Every transaction in every month, newest first.
    pub fn all_transactions(&self) -> Result<Vec<Transaction>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, date, amount_cents, category_id, note,
                    previous_category_name, previous_category_kind,
                    payment_method_id, previous_payment_method_name,
                    recurring_id, occurrence, amount_pending FROM transactions
             ORDER BY date DESC, id DESC",
        )?;
        let rows = stmt.query_map([], row_to_transaction)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

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
    /// doesn't stop it from being restored later. Saving a payment that
    /// needed its amount confirms the amount.
    pub fn update_transaction(&self, id: i64, t: &TransactionInput) -> Result<()> {
        self.conn.execute(
            "UPDATE transactions SET date = ?1, amount_cents = ?2, category_id = ?3, note = ?4,
                    previous_category_name = NULL, previous_category_kind = NULL,
                    amount_pending = 0,
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

    /// Put back a deleted transaction exactly as it was, including any
    /// remembered deleted category or payment method. It keeps its id unless
    /// a transaction added since has taken it. It rejoins its recurring
    /// series unless the series was deleted or that month's payment was
    /// added again in the meantime.
    pub fn restore_transaction(&self, t: &Transaction) -> Result<()> {
        self.conn.execute(
            "INSERT INTO transactions (id, date, amount_cents, category_id, note,
                    previous_category_name, previous_category_kind,
                    payment_method_id, previous_payment_method_name,
                    recurring_id, occurrence, amount_pending)
             SELECT CASE WHEN EXISTS (SELECT 1 FROM transactions WHERE id = ?1)
                         THEN NULL ELSE ?1 END,
                    ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                    CASE WHEN rejoin THEN ?10 END, CASE WHEN rejoin THEN ?11 END, ?12
             FROM (SELECT EXISTS (SELECT 1 FROM recurring WHERE id = ?10)
                          AND NOT EXISTS (SELECT 1 FROM transactions
                                          WHERE recurring_id = ?10 AND occurrence = ?11)
                          AS rejoin)",
            params![
                t.id,
                t.date.to_string(),
                t.amount.0,
                t.category_id,
                t.note,
                t.previous_category.as_ref().map(|p| p.name.as_str()),
                t.previous_category.as_ref().map(|p| p.kind.as_str()),
                t.payment_method_id,
                t.previous_payment_method,
                t.recurring_id,
                t.occurrence.map(Month::key),
                t.amount_pending,
            ],
        )?;
        Ok(())
    }

    // MARK: Recurring

    /// Every recurring transaction, with its amount changes.
    pub fn recurring(&self) -> Result<Vec<Recurring>> {
        let mut stmt = self.conn.prepare(
            "SELECT recurring_id, month, amount_cents FROM recurring_amounts
             ORDER BY recurring_id, month",
        )?;
        let mut amounts: HashMap<i64, Vec<(Month, Option<Money>)>> = HashMap::new();
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                month_column(r, 1)?,
                r.get::<_, Option<i64>>(2)?.map(Money),
            ))
        })?;
        for row in rows {
            let (id, month, amount) = row?;
            amounts.entry(id).or_default().push((month, amount));
        }

        let mut stmt = self.conn.prepare(
            "SELECT id, start_month, end_month, every_months, day, category_id, note,
                    previous_category_name, previous_category_kind,
                    payment_method_id, previous_payment_method_name
             FROM recurring ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            let id = r.get(0)?;
            Ok(Recurring {
                id,
                start: month_column(r, 1)?,
                end: match r.get::<_, Option<String>>(2)? {
                    Some(_) => Some(month_column(r, 2)?),
                    None => None,
                },
                frequency: Frequency::from_months(r.get(3)?).unwrap_or(Frequency::Monthly),
                day: r.get(4)?,
                category_id: r.get(5)?,
                note: r.get(6)?,
                previous_category: previous_category(r, 7)?,
                payment_method_id: r.get(9)?,
                previous_payment_method: r.get(10)?,
                amounts: amounts.remove(&id).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Add a recurring transaction whose first payment is in `r.start`.
    /// `amount` is `None` if it varies.
    pub fn add_recurring(&self, r: &RecurringInput, amount: Option<Money>) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO recurring (start_month, end_month, every_months, day, category_id, note,
                                    payment_method_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6,
                     CASE (SELECT kind FROM categories WHERE id = ?5)
                         WHEN 'expense' THEN ?7 END)",
            params![
                r.start.key(),
                r.end.map(Month::key),
                r.frequency.months(),
                r.day,
                r.category_id,
                r.note,
                r.payment_method_id,
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO recurring_amounts (recurring_id, month, amount_cents) VALUES (?1, ?2, ?3)",
            params![id, r.start.key(), amount.map(|a| a.0)],
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Make an existing transaction repeat. It becomes the first payment of
    /// a new series, on the same day of the month, for the same amount
    /// unless the amount `varies`.
    pub fn repeat_transaction(
        &self,
        id: i64,
        frequency: Frequency,
        end: Option<Month>,
        varies: bool,
    ) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        let added = tx.execute(
            "INSERT INTO recurring (start_month, end_month, every_months, day, category_id, note,
                                    previous_category_name, previous_category_kind,
                                    payment_method_id, previous_payment_method_name)
             SELECT substr(date, 1, 7), ?2, ?3, CAST(substr(date, 9, 2) AS INTEGER),
                    category_id, note, previous_category_name, previous_category_kind,
                    payment_method_id, previous_payment_method_name
             FROM transactions WHERE id = ?1 AND recurring_id IS NULL",
            params![id, end.map(Month::key), frequency.months()],
        )?;
        if added == 0 {
            bail!("transaction {id} doesn't exist or already repeats");
        }
        let recurring_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO recurring_amounts (recurring_id, month, amount_cents)
             SELECT ?1, substr(date, 1, 7), CASE WHEN ?3 THEN NULL ELSE amount_cents END
             FROM transactions WHERE id = ?2",
            params![recurring_id, id, varies],
        )?;
        tx.execute(
            "UPDATE transactions SET recurring_id = ?1, occurrence = substr(date, 1, 7)
             WHERE id = ?2",
            params![recurring_id, id],
        )?;
        tx.commit()?;
        Ok(recurring_id)
    }

    /// Change a recurring transaction's schedule, category, payment method
    /// or note. Only payments added from now on are affected. Payment methods
    /// are kept as in [`Self::update_transaction`].
    pub fn update_recurring(&self, id: i64, r: &RecurringInput) -> Result<()> {
        self.conn.execute(
            "UPDATE recurring SET start_month = ?1, end_month = ?2, every_months = ?3, day = ?4,
                    category_id = ?5, note = ?6,
                    previous_category_name = NULL, previous_category_kind = NULL,
                    payment_method_id = CASE (SELECT kind FROM categories WHERE id = ?5)
                        WHEN 'expense' THEN ?7 END,
                    previous_payment_method_name = CASE
                        WHEN ?7 IS NULL AND (SELECT kind FROM categories WHERE id = ?5) = 'expense'
                        THEN previous_payment_method_name END
             WHERE id = ?8",
            params![
                r.start.key(),
                r.end.map(Month::key),
                r.frequency.months(),
                r.day,
                r.category_id,
                r.note,
                r.payment_method_id,
                id
            ],
        )?;
        Ok(())
    }

    /// Change the amount of a recurring transaction's payments in `from` and
    /// every later month, replacing any later changes. `None` makes the
    /// amount vary from then on; an amount makes it fixed again. Payments
    /// already added keep their amounts.
    pub fn set_recurring_amount(&self, id: i64, from: Month, amount: Option<Money>) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.replace_amounts(id, from, amount)?;
        tx.commit()?;
        Ok(())
    }

    /// [`Self::set_recurring_amount`] inside the caller's transaction.
    fn replace_amounts(&self, id: i64, from: Month, amount: Option<Money>) -> Result<()> {
        self.conn.execute(
            "DELETE FROM recurring_amounts WHERE recurring_id = ?1 AND month >= ?2",
            params![id, from.key()],
        )?;
        self.conn.execute(
            "INSERT INTO recurring_amounts (recurring_id, month, amount_cents) VALUES (?1, ?2, ?3)",
            params![id, from.key(), amount.map(|a| a.0)],
        )?;
        Ok(())
    }

    /// Save an edited payment and carry its amount, category, payment method
    /// and note over to the series' payments from that month on. Payments
    /// already added, earlier or later, are unchanged.
    pub fn update_transaction_and_series(&self, id: i64, t: &TransactionInput) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.update_transaction(id, t)?;
        let (recurring_id, occurrence): (Option<i64>, Option<String>) = tx.query_row(
            "SELECT recurring_id, occurrence, amount_pending FROM transactions WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let (Some(recurring_id), Some(occurrence)) = (
            recurring_id,
            occurrence.as_deref().and_then(Month::from_key),
        ) else {
            bail!("transaction {id} isn't part of a recurring series");
        };
        tx.execute(
            "UPDATE recurring SET category_id = ?1, note = ?2,
                    previous_category_name = NULL, previous_category_kind = NULL,
                    payment_method_id = CASE (SELECT kind FROM categories WHERE id = ?1)
                        WHEN 'expense' THEN ?3 END,
                    previous_payment_method_name = CASE
                        WHEN ?3 IS NULL AND (SELECT kind FROM categories WHERE id = ?1) = 'expense'
                        THEN previous_payment_method_name END
             WHERE id = ?4",
            params![t.category_id, t.note, t.payment_method_id, recurring_id],
        )?;
        self.replace_amounts(recurring_id, occurrence, Some(t.amount))?;
        tx.commit()?;
        Ok(())
    }

    /// Stop offering a recurring transaction's payments after `current`
    /// month: its last payment becomes the latest one already added, or the
    /// previous month's if that is later. A series that would then have no
    /// payments at all is deleted.
    pub fn stop_recurring(&self, id: i64, current: Month) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let (start, end, latest_added): (String, Option<String>, Option<String>) = tx.query_row(
            "SELECT start_month, end_month,
                    (SELECT MAX(occurrence) FROM transactions WHERE recurring_id = ?1)
             FROM recurring WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let mut new_end = current.prev().key();
        if let Some(latest) = latest_added {
            new_end = new_end.max(latest);
        }
        if let Some(end) = end {
            new_end = new_end.min(end);
        }
        if new_end < start {
            self.unlink_and_delete(id)?;
        } else {
            tx.execute(
                "UPDATE recurring SET end_month = ?1 WHERE id = ?2",
                params![new_end, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Delete a recurring transaction. Payments already added are kept as
    /// ordinary transactions.
    pub fn delete_recurring(&self, id: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.unlink_and_delete(id)?;
        tx.commit()?;
        Ok(())
    }

    /// [`Self::delete_recurring`] inside the caller's transaction.
    fn unlink_and_delete(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE transactions SET recurring_id = NULL, occurrence = NULL
             WHERE recurring_id = ?1",
            [id],
        )?;
        self.conn
            .execute("DELETE FROM recurring WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Recurring payments in `month` that haven't been added or skipped,
    /// by date.
    pub fn due_transactions(&self, month: Month) -> Result<Vec<DueTransaction>> {
        let done = |sql: &str| -> Result<Vec<i64>> {
            let mut stmt = self.conn.prepare(sql)?;
            let rows = stmt.query_map([month.key()], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        };
        let added = done("SELECT recurring_id FROM transactions WHERE occurrence = ?1")?;
        let skipped = done("SELECT recurring_id FROM recurring_skips WHERE month = ?1")?;
        let mut due = Vec::new();
        for r in self.recurring()? {
            if !r.occurs_in(month) || added.contains(&r.id) || skipped.contains(&r.id) {
                continue;
            }
            let (amount, estimated) = match r.amount_in(month) {
                Some(amount) => (amount, false),
                None => (self.last_amount(r.id)?.unwrap_or_default(), true),
            };
            due.push(DueTransaction {
                recurring_id: r.id,
                month,
                date: r.date_in(month),
                amount,
                estimated,
            });
        }
        due.sort_by_key(|d| (d.date, d.recurring_id));
        Ok(due)
    }

    /// The amount of a series' latest payment, preferring ones whose amount
    /// was entered over estimates. Used to estimate a varying amount.
    fn last_amount(&self, recurring_id: i64) -> Result<Option<Money>> {
        Ok(self
            .conn
            .query_row(
                "SELECT amount_cents FROM transactions WHERE recurring_id = ?1
                 ORDER BY amount_pending, occurrence DESC, date DESC LIMIT 1",
                [recurring_id],
                |r| r.get(0),
            )
            .optional()?
            .map(Money))
    }

    /// Payments whose amount varies and hasn't been entered yet, dated on or
    /// before `today`, oldest first.
    pub fn pending_amounts(&self, today: NaiveDate) -> Result<Vec<Transaction>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, date, amount_cents, category_id, note,
                    previous_category_name, previous_category_kind,
                    payment_method_id, previous_payment_method_name,
                    recurring_id, occurrence, amount_pending FROM transactions
             WHERE amount_pending AND date <= ?1
             ORDER BY date, id",
        )?;
        let rows = stmt.query_map([today.to_string()], row_to_transaction)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Add these due payments as transactions, all or none. A payment that
    /// was already added is not added again. Estimated ones are marked as
    /// needing their amount.
    pub fn add_due(&self, due: &[DueTransaction]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for d in due {
            tx.execute(
                "INSERT INTO transactions (date, amount_cents, category_id, note,
                        previous_category_name, previous_category_kind,
                        payment_method_id, previous_payment_method_name,
                        recurring_id, occurrence, amount_pending)
                 SELECT ?2, ?3, category_id, note, previous_category_name, previous_category_kind,
                        payment_method_id, previous_payment_method_name, id, ?4, ?5
                 FROM recurring WHERE id = ?1
                   AND NOT EXISTS (SELECT 1 FROM transactions
                                   WHERE recurring_id = ?1 AND occurrence = ?4)",
                params![
                    d.recurring_id,
                    d.date.to_string(),
                    d.amount.0,
                    d.month.key(),
                    d.estimated
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Don't offer a recurring transaction's payment in `month` again.
    pub fn skip_due(&self, recurring_id: i64, month: Month) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO recurring_skips (recurring_id, month) VALUES (?1, ?2)",
            params![recurring_id, month.key()],
        )?;
        Ok(())
    }

    /// How many recurring payments were skipped in `month`.
    pub fn skipped_count(&self, month: Month) -> Result<usize> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM recurring_skips WHERE month = ?1",
            [month.key()],
            |r| r.get::<_, i64>(0),
        )? as usize)
    }

    /// Offer the payments skipped in `month` again.
    pub fn unskip_all(&self, month: Month) -> Result<()> {
        self.conn.execute(
            "DELETE FROM recurring_skips WHERE month = ?1",
            [month.key()],
        )?;
        Ok(())
    }

    // MARK: Import

    /// Add several transactions at once, all or none, and remember the
    /// category chosen for each note so the next import can suggest it.
    pub fn import_transactions(&self, inputs: &[TransactionInput]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for t in inputs {
            self.add_transaction(t)?;
            let merchant = merchant_key(&t.note);
            if !merchant.is_empty() {
                tx.execute(
                    "INSERT INTO merchant_categories (merchant, category_id) VALUES (?1, ?2)
                     ON CONFLICT (merchant) DO UPDATE SET category_id = excluded.category_id",
                    params![merchant, t.category_id],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The category last imported for a merchant, if it still exists.
    pub fn merchant_category(&self, merchant: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT category_id FROM merchant_categories WHERE merchant = ?1",
                [merchant_key(merchant)],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// An existing transaction for the same amount within `days` of `date`,
    /// in any month, that an imported one may duplicate. The closest by date
    /// is returned.
    pub fn similar_transaction(
        &self,
        date: NaiveDate,
        amount: Money,
        days: u64,
    ) -> Result<Option<Transaction>> {
        let from = date - chrono::Days::new(days);
        let to = date + chrono::Days::new(days);
        Ok(self
            .conn
            .query_row(
                "SELECT id, date, amount_cents, category_id, note,
                        previous_category_name, previous_category_kind,
                        payment_method_id, previous_payment_method_name,
                    recurring_id, occurrence, amount_pending FROM transactions
                 WHERE amount_cents = ?1 AND date BETWEEN ?2 AND ?3
                 ORDER BY abs(julianday(date) - julianday(?4)), id DESC
                 LIMIT 1",
                params![amount.0, from.to_string(), to.to_string(), date.to_string()],
                row_to_transaction,
            )
            .optional()?)
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

fn schema_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// Snapshots the database to `budget.db.bak-v{version}` before migrating it,
/// replacing any earlier backup of the same version.
fn backup(conn: &Connection, path: &Path, version: i64) -> Result<()> {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(format!(".bak-v{version}"));
    let dest = path.with_file_name(name);
    if dest.exists() {
        std::fs::remove_file(&dest)
            .with_context(|| format!("removing old backup {}", dest.display()))?;
    }
    conn.execute("VACUUM INTO ?1", [dest.to_string_lossy()])
        .with_context(|| format!("backing up database to {}", dest.display()))?;
    Ok(())
}

/// How a merchant is looked up in `merchant_categories`: trimmed and
/// lowercased, so "Trader Joe's" and "TRADER JOE'S" match.
fn merchant_key(merchant: &str) -> String {
    merchant.trim().to_lowercase()
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
        previous_category: previous_category(r, 5)?,
        payment_method_id: r.get(7)?,
        previous_payment_method: r.get(8)?,
        recurring_id: r.get(9)?,
        occurrence: match r.get::<_, Option<String>>(10)? {
            Some(_) => Some(month_column(r, 10)?),
            None => None,
        },
        amount_pending: r.get(11)?,
    })
}

/// The previous category's name and kind, in columns `i` and `i + 1`.
fn previous_category(r: &Row, i: usize) -> rusqlite::Result<Option<PreviousCategory>> {
    Ok(
        match (
            r.get::<_, Option<String>>(i)?,
            r.get::<_, Option<String>>(i + 1)?,
        ) {
            (Some(name), Some(kind)) => Some(PreviousCategory {
                name,
                kind: CategoryKind::from_str(&kind).unwrap_or(CategoryKind::Expense),
            }),
            _ => None,
        },
    )
}

/// A `YYYY-MM` month key in column `i`.
fn month_column(r: &Row, i: usize) -> rusqlite::Result<Month> {
    let key: String = r.get(i)?;
    Month::from_key(&key).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            i,
            rusqlite::types::Type::Text,
            format!("invalid month {key:?}").into(),
        )
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
    fn all_transactions_spans_every_month_newest_first() {
        let db = Db::open_in_memory().unwrap();
        add(&db, "2026-09-30", 500, "Groceries");
        add(&db, "2026-10-15", 2000, "Groceries");
        add(&db, "2025-01-01", 100, "Groceries");

        let dates: Vec<NaiveDate> = db
            .all_transactions()
            .unwrap()
            .into_iter()
            .map(|t| t.date)
            .collect();
        assert_eq!(
            dates,
            [date("2026-10-15"), date("2026-09-30"), date("2025-01-01")]
        );
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

    #[test]
    fn restoring_a_deleted_transaction_puts_it_back_exactly() {
        let db = Db::open_in_memory().unwrap();
        let visa = db.add_payment_method("Visa").unwrap();
        let paid = pay(&db, "2026-10-03", 6000, "Groceries", Some(visa));
        let unassigned = pay(&db, "2026-10-04", 2500, "Dining Out", Some(visa));
        let last = add(&db, "2026-10-05", 100, "Other");
        db.delete_category(category(&db, "Dining Out")).unwrap();
        db.delete_payment_method(visa).unwrap();
        let visa = db.add_payment_method("Visa").unwrap();
        let before = db.transactions_in(OCT).unwrap();

        for id in [paid, unassigned] {
            let t = find(&db, id);
            db.delete_transaction(id).unwrap();
            db.restore_transaction(&t).unwrap();
        }
        assert_eq!(db.transactions_in(OCT).unwrap(), before);
        let t = find(&db, unassigned);
        assert_eq!(t.category_id, None);
        assert_eq!(t.payment_method_id, Some(visa));
        assert_eq!(t.previous_category.unwrap().name, "Dining Out");

        // If a new transaction took the id meanwhile, it gets another one.
        let t = find(&db, last);
        db.delete_transaction(last).unwrap();
        let taken = add(&db, "2026-10-06", 300, "Other");
        assert_eq!(taken, last, "SQLite reuses the highest id");
        db.restore_transaction(&t).unwrap();
        let restored = db
            .transactions_in(OCT)
            .unwrap()
            .into_iter()
            .find(|r| r.amount == t.amount && r.id != taken)
            .unwrap();
        assert_eq!(
            Transaction {
                id: t.id,
                ..restored
            },
            t
        );
        assert_eq!(find(&db, taken).amount, Money(300));
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

    fn import(db: &Db, d: &str, cents: i64, cat: &str, note: &str) -> TransactionInput {
        TransactionInput {
            date: date(d),
            amount: Money(cents),
            category_id: category(db, cat),
            payment_method_id: None,
            note: note.into(),
        }
    }

    #[test]
    fn importing_remembers_each_merchants_category() {
        let db = Db::open_in_memory().unwrap();
        db.import_transactions(&[
            import(&db, "2026-10-02", 4500, "Groceries", "Trader Joe's"),
            import(&db, "2026-10-03", 1200, "Dining Out", "Blue Bottle"),
            import(&db, "2026-10-04", 800, "Other", ""),
        ])
        .unwrap();
        assert_eq!(db.transactions_in(OCT).unwrap().len(), 3);
        assert_eq!(
            db.merchant_category(" TRADER JOE'S ").unwrap(),
            Some(category(&db, "Groceries"))
        );
        assert_eq!(db.merchant_category("").unwrap(), None);

        // The latest choice wins, and deleting the category forgets it.
        db.import_transactions(&[import(&db, "2026-10-05", 900, "Other", "Blue Bottle")])
            .unwrap();
        let other = category(&db, "Other");
        assert_eq!(db.merchant_category("blue bottle").unwrap(), Some(other));
        db.delete_category(other).unwrap();
        assert_eq!(db.merchant_category("blue bottle").unwrap(), None);
    }

    #[test]
    fn a_failed_import_adds_nothing() {
        let db = Db::open_in_memory().unwrap();
        let mut bad = import(&db, "2026-10-03", 100, "Groceries", "Bad");
        bad.category_id = 9999;
        let good = import(&db, "2026-10-02", 100, "Groceries", "Good");
        assert!(db.import_transactions(&[good, bad]).is_err());
        assert!(db.transactions_in(OCT).unwrap().is_empty());
        assert_eq!(db.merchant_category("good").unwrap(), None);
    }

    #[test]
    fn finds_the_closest_transaction_with_the_same_amount() {
        let db = Db::open_in_memory().unwrap();
        add(&db, "2026-09-29", 1500, "Groceries");
        let near = add(&db, "2026-10-02", 1500, "Dining Out");
        add(&db, "2026-10-01", 1499, "Groceries");

        let found = |d: &str, cents| {
            db.similar_transaction(date(d), Money(cents), 2)
                .unwrap()
                .map(|t| t.id)
        };
        assert_eq!(found("2026-10-01", 1500), Some(near));
        assert_eq!(found("2026-10-05", 1500), None);
        assert_eq!(found("2026-10-01", 1600), None);
        // Searches across months.
        assert!(found("2026-09-30", 1500).is_some());
    }

    fn month(key: &str) -> Month {
        Month::from_key(key).unwrap()
    }

    fn rent(db: &Db, start: &str, end: Option<&str>) -> i64 {
        db.add_recurring(
            &RecurringInput {
                start: month(start),
                end: end.map(month),
                frequency: Frequency::Monthly,
                day: 31,
                category_id: category(db, "Housing"),
                payment_method_id: None,
                note: "Rent".into(),
            },
            Some(Money(150000)),
        )
        .unwrap()
    }

    fn due(db: &Db, m: &str) -> Vec<(i64, NaiveDate, Money)> {
        db.due_transactions(month(m))
            .unwrap()
            .into_iter()
            .map(|d| (d.recurring_id, d.date, d.amount))
            .collect()
    }

    fn add_all_due(db: &Db, m: &str) {
        db.add_due(&db.due_transactions(month(m)).unwrap()).unwrap();
    }

    fn amounts(db: &Db, m: &str) -> Vec<Money> {
        db.transactions_in(month(m))
            .unwrap()
            .into_iter()
            .map(|t| t.amount)
            .collect()
    }

    #[test]
    fn recurring_payments_are_due_within_their_date_range() {
        let db = Db::open_in_memory().unwrap();
        let id = rent(&db, "2026-01", Some("2026-03"));
        assert!(due(&db, "2025-12").is_empty());
        assert_eq!(
            due(&db, "2026-02"),
            [(id, date("2026-02-28"), Money(150000))],
            "the 31st falls on the last day of shorter months"
        );
        assert_eq!(due(&db, "2026-03").len(), 1);
        assert!(due(&db, "2026-04").is_empty(), "ended in March");
    }

    #[test]
    fn due_payments_are_never_added_twice() {
        let db = Db::open_in_memory().unwrap();
        let id = rent(&db, "2026-10", None);
        let pending = db.due_transactions(OCT).unwrap();
        db.add_due(&pending).unwrap();
        db.add_due(&pending).unwrap();
        assert!(due(&db, "2026-10").is_empty());
        let t = &db.transactions_in(OCT).unwrap()[0];
        assert_eq!(db.transactions_in(OCT).unwrap().len(), 1);
        assert_eq!((t.recurring_id, t.occurrence), (Some(id), Some(OCT)));
        assert_eq!(
            (t.date, t.amount, t.note.as_str()),
            (date("2026-10-31"), Money(150000), "Rent")
        );
        assert_eq!(t.category_id, Some(category(&db, "Housing")));

        // Moving the payment to another month doesn't make October due again.
        db.update_transaction(
            t.id,
            &TransactionInput {
                date: date("2026-11-02"),
                amount: t.amount,
                category_id: category(&db, "Housing"),
                payment_method_id: None,
                note: "Rent".into(),
            },
        )
        .unwrap();
        assert!(due(&db, "2026-10").is_empty());
        assert_eq!(
            due(&db, "2026-11").len(),
            1,
            "November's payment is still due"
        );

        // Deleting it does, and undoing the delete takes it back.
        db.delete_transaction(t.id).unwrap();
        assert_eq!(due(&db, "2026-10").len(), 1);
        let moved = Transaction {
            date: date("2026-11-02"),
            ..t.clone()
        };
        db.restore_transaction(&moved).unwrap();
        assert!(due(&db, "2026-10").is_empty());
    }

    #[test]
    fn restoring_a_payment_added_again_leaves_the_new_one_in_the_series() {
        let db = Db::open_in_memory().unwrap();
        let id = rent(&db, "2026-10", None);
        add_all_due(&db, "2026-10");
        let t = db.transactions_in(OCT).unwrap()[0].clone();
        db.delete_transaction(t.id).unwrap();
        add_all_due(&db, "2026-10");
        db.restore_transaction(&t).unwrap();
        let linked: Vec<Option<i64>> = db
            .transactions_in(OCT)
            .unwrap()
            .into_iter()
            .map(|t| t.recurring_id)
            .collect();
        assert_eq!(linked.len(), 2);
        assert_eq!(linked.iter().filter(|r| **r == Some(id)).count(), 1);
    }

    #[test]
    fn changing_the_amount_only_affects_later_payments_not_yet_added() {
        let db = Db::open_in_memory().unwrap();
        let id = rent(&db, "2026-01", None);
        for m in ["2026-01", "2026-02", "2026-03", "2026-04"] {
            add_all_due(&db, m);
        }

        // Rent goes up from March. March and April were already added, so
        // they keep the old amount; May onwards uses the new one.
        db.set_recurring_amount(id, month("2026-03"), Some(Money(160000)))
            .unwrap();
        assert_eq!(amounts(&db, "2026-02"), [Money(150000)]);
        assert_eq!(amounts(&db, "2026-04"), [Money(150000)]);
        assert_eq!(due(&db, "2026-05")[0].2, Money(160000));

        // A later change replaces changes scheduled after it.
        db.set_recurring_amount(id, month("2026-08"), Some(Money(170000)))
            .unwrap();
        db.set_recurring_amount(id, month("2026-06"), Some(Money(165000)))
            .unwrap();
        assert_eq!(due(&db, "2026-05")[0].2, Money(160000));
        assert_eq!(due(&db, "2026-09")[0].2, Money(165000));
    }

    #[test]
    fn editing_one_payment_leaves_the_series_and_other_payments_alone() {
        let db = Db::open_in_memory().unwrap();
        rent(&db, "2026-09", None);
        add_all_due(&db, "2026-09");
        add_all_due(&db, "2026-10");
        let oct = db.transactions_in(OCT).unwrap()[0].clone();
        let edited = TransactionInput {
            date: oct.date,
            amount: Money(155000),
            category_id: category(&db, "Housing"),
            payment_method_id: None,
            note: "Rent + late fee".into(),
        };
        db.update_transaction(oct.id, &edited).unwrap();
        assert_eq!(amounts(&db, "2026-09"), [Money(150000)]);
        assert_eq!(amounts(&db, "2026-10"), [Money(155000)]);
        assert_eq!(due(&db, "2026-11")[0].2, Money(150000));
        assert_eq!(db.recurring().unwrap()[0].note, "Rent");
    }

    #[test]
    fn editing_a_payment_can_carry_over_to_later_months() {
        let db = Db::open_in_memory().unwrap();
        let visa = db.add_payment_method("Visa").unwrap();
        let id = rent(&db, "2026-09", None);
        for m in ["2026-09", "2026-10", "2026-11"] {
            add_all_due(&db, m);
        }
        let oct = db.transactions_in(OCT).unwrap()[0].clone();
        db.update_transaction_and_series(
            oct.id,
            &TransactionInput {
                date: oct.date,
                amount: Money(160000),
                category_id: category(&db, "Other"),
                payment_method_id: Some(visa),
                note: "New lease".into(),
            },
        )
        .unwrap();

        // September and the already-added November payment are untouched.
        assert_eq!(amounts(&db, "2026-09"), [Money(150000)]);
        assert_eq!(amounts(&db, "2026-11"), [Money(150000)]);
        assert_eq!(due(&db, "2026-12")[0].2, Money(160000));
        let r = db.recurring().unwrap().remove(0);
        assert_eq!(r.id, id);
        assert_eq!(r.note, "New lease");
        assert_eq!(r.category_id, Some(category(&db, "Other")));
        assert_eq!(r.payment_method_id, Some(visa));
        assert_eq!(r.amount_in(month("2026-09")), Some(Money(150000)));

        // Only payments in a series can carry over.
        let plain = add(&db, "2026-10-05", 100, "Other");
        assert!(
            db.update_transaction_and_series(
                plain,
                &TransactionInput {
                    date: date("2026-10-05"),
                    amount: Money(200),
                    category_id: category(&db, "Other"),
                    payment_method_id: None,
                    note: String::new(),
                },
            )
            .is_err()
        );
        assert_eq!(find(&db, plain).amount, Money(100), "rolled back");
    }

    #[test]
    fn a_transaction_can_start_repeating() {
        let db = Db::open_in_memory().unwrap();
        let visa = db.add_payment_method("Visa").unwrap();
        let t = pay(&db, "2026-10-18", 1599, "Entertainment", Some(visa));
        let id = db
            .repeat_transaction(t, Frequency::Yearly, Some(month("2028-12")), false)
            .unwrap();
        assert!(
            db.repeat_transaction(t, Frequency::Monthly, None, false)
                .is_err()
        );

        let r = db.recurring().unwrap().remove(0);
        assert_eq!(
            (r.start, r.end, r.frequency, r.day),
            (OCT, Some(month("2028-12")), Frequency::Yearly, 18)
        );
        assert_eq!(r.payment_method_id, Some(visa));
        assert_eq!(r.amounts, [(OCT, Some(Money(1599)))]);
        let t = find(&db, t);
        assert_eq!((t.recurring_id, t.occurrence), (Some(id), Some(OCT)));
        assert!(
            due(&db, "2026-10").is_empty(),
            "this month's is the transaction itself"
        );
        assert!(due(&db, "2027-09").is_empty());
        assert_eq!(due(&db, "2027-10"), [(id, date("2027-10-18"), Money(1599))]);
    }

    #[test]
    fn skipped_payments_are_not_offered_again_until_restored() {
        let db = Db::open_in_memory().unwrap();
        let id = rent(&db, "2026-09", None);
        db.skip_due(id, OCT).unwrap();
        db.skip_due(id, OCT).unwrap();
        assert!(due(&db, "2026-10").is_empty());
        assert_eq!(due(&db, "2026-11").len(), 1);
        assert_eq!(db.skipped_count(OCT).unwrap(), 1);
        db.unskip_all(OCT).unwrap();
        assert_eq!(due(&db, "2026-10").len(), 1);
    }

    #[test]
    fn stopping_keeps_payments_already_added() {
        let db = Db::open_in_memory().unwrap();
        let added = rent(&db, "2026-01", None);
        let not_added = rent(&db, "2026-01", None);
        let early = rent(&db, "2026-01", None);
        let future = rent(&db, "2027-01", None);
        db.add_due(&db.due_transactions(OCT).unwrap()[..1]).unwrap();
        db.add_due(&[DueTransaction {
            recurring_id: early,
            month: month("2026-12"),
            date: date("2026-12-31"),
            amount: Money(150000),
            estimated: false,
        }])
        .unwrap();
        for id in [added, not_added, early, future] {
            db.stop_recurring(id, OCT).unwrap();
        }
        let end = |id| {
            db.recurring()
                .unwrap()
                .into_iter()
                .find(|r| r.id == id)
                .map(|r| r.end)
        };
        assert_eq!(end(added), Some(Some(OCT)), "October was added");
        assert_eq!(end(not_added), Some(Some(month("2026-09"))));
        assert_eq!(
            end(early),
            Some(Some(month("2026-12"))),
            "December was added early"
        );
        assert_eq!(end(future), None, "never started, so deleted");
        let due_ids: Vec<i64> = due(&db, "2026-10").into_iter().map(|d| d.0).collect();
        assert_eq!(due_ids, [early]);
    }

    #[test]
    fn deleting_a_series_keeps_its_transactions() {
        let db = Db::open_in_memory().unwrap();
        let id = rent(&db, "2026-10", None);
        add_all_due(&db, "2026-10");
        db.skip_due(id, month("2026-11")).unwrap();
        db.delete_recurring(id).unwrap();
        assert!(db.recurring().unwrap().is_empty());
        let t = &db.transactions_in(OCT).unwrap()[0];
        assert_eq!((t.recurring_id, t.occurrence), (None, None));
        assert_eq!(t.amount, Money(150000));
    }

    #[test]
    fn deleted_categories_and_payment_methods_are_remembered_by_series() {
        let db = Db::open_in_memory().unwrap();
        let visa = db.add_payment_method("Visa").unwrap();
        let id = db
            .add_recurring(
                &RecurringInput {
                    start: OCT,
                    end: None,
                    frequency: Frequency::Monthly,
                    day: 1,
                    category_id: category(&db, "Utilities"),
                    payment_method_id: Some(visa),
                    note: "Power".into(),
                },
                Some(Money(9000)),
            )
            .unwrap();
        db.delete_category(category(&db, "Utilities")).unwrap();
        db.delete_payment_method(visa).unwrap();
        let r = db.recurring().unwrap().remove(0);
        assert_eq!(r.category_id, None);
        assert_eq!(r.previous_category.unwrap().name, "Utilities");
        assert_eq!(r.previous_payment_method.as_deref(), Some("Visa"));

        // Payments added meanwhile are unassigned but still count as expenses.
        add_all_due(&db, "2026-10");
        let t = &db.transactions_in(OCT).unwrap()[0];
        assert_eq!(t.category_id, None);
        assert_eq!(t.previous_payment_method.as_deref(), Some("Visa"));
        assert_eq!(
            db.month_summary(OCT).unwrap().unassigned_expenses,
            Money(9000)
        );

        let utilities = db.add_category("Utilities", CategoryKind::Expense).unwrap();
        let visa = db.add_payment_method("Visa").unwrap();
        let r = db.recurring().unwrap().remove(0);
        assert_eq!((r.id, r.category_id), (id, Some(utilities)));
        assert_eq!(r.payment_method_id, Some(visa));
    }

    #[test]
    fn income_series_have_no_payment_method() {
        let db = Db::open_in_memory().unwrap();
        let visa = db.add_payment_method("Visa").unwrap();
        db.add_recurring(
            &RecurringInput {
                start: OCT,
                end: None,
                frequency: Frequency::Monthly,
                day: 15,
                category_id: category(&db, "Salary"),
                payment_method_id: Some(visa),
                note: String::new(),
            },
            Some(Money(500000)),
        )
        .unwrap();
        assert_eq!(db.recurring().unwrap()[0].payment_method_id, None);
    }

    fn water(db: &Db, start: &str) -> i64 {
        db.add_recurring(
            &RecurringInput {
                start: month(start),
                end: None,
                frequency: Frequency::Monthly,
                day: 12,
                category_id: category(db, "Utilities"),
                payment_method_id: None,
                note: "Water".into(),
            },
            None,
        )
        .unwrap()
    }

    fn enter_amount(db: &Db, t: &Transaction, cents: i64) {
        db.update_transaction(
            t.id,
            &TransactionInput {
                date: t.date,
                amount: Money(cents),
                category_id: t.category_id.unwrap(),
                payment_method_id: t.payment_method_id,
                note: t.note.clone(),
            },
        )
        .unwrap();
    }

    #[test]
    fn varying_payments_are_added_with_an_estimate_and_need_their_amount() {
        let db = Db::open_in_memory().unwrap();
        let id = water(&db, "2026-09");

        // The first payment has nothing to estimate from.
        let sep = db.due_transactions(month("2026-09")).unwrap();
        assert_eq!((sep[0].amount, sep[0].estimated), (Money(0), true));
        db.add_due(&sep).unwrap();
        let t = db.transactions_in(month("2026-09")).unwrap().remove(0);
        assert!(t.amount_pending);
        assert_eq!(t.recurring_id, Some(id));

        // Reminders only start once the payment's date has passed.
        assert!(db.pending_amounts(date("2026-09-11")).unwrap().is_empty());
        let pending = db.pending_amounts(date("2026-09-12")).unwrap();
        assert_eq!(pending.iter().map(|t| t.id).collect::<Vec<_>>(), [t.id]);

        // Entering the amount confirms it, and later payments estimate from it.
        enter_amount(&db, &t, 8412);
        assert!(db.pending_amounts(date("2026-12-31")).unwrap().is_empty());
        assert!(!find(&db, t.id).amount_pending);
        let oct = db.due_transactions(OCT).unwrap();
        assert_eq!((oct[0].amount, oct[0].estimated), (Money(8412), true));

        // An unconfirmed estimate doesn't become the next estimate.
        db.add_due(&oct).unwrap();
        let oct_t = db.transactions_in(OCT).unwrap().remove(0);
        assert!(oct_t.amount_pending);
        db.update_transaction(
            oct_t.id,
            &TransactionInput {
                date: oct_t.date,
                amount: Money(9000),
                category_id: category(&db, "Utilities"),
                payment_method_id: None,
                note: "Water".into(),
            },
        )
        .unwrap();
        db.conn
            .execute(
                "UPDATE transactions SET amount_pending = 1 WHERE id = ?1",
                [oct_t.id],
            )
            .unwrap();
        assert_eq!(due(&db, "2026-11")[0].2, Money(8412));

        // Undoing a delete keeps the payment's pending flag.
        let pending = find(&db, oct_t.id);
        db.delete_transaction(pending.id).unwrap();
        db.restore_transaction(&pending).unwrap();
        assert!(find(&db, pending.id).amount_pending);
    }

    #[test]
    fn a_series_can_switch_between_fixed_and_varying_amounts() {
        let db = Db::open_in_memory().unwrap();
        let id = rent(&db, "2026-08", None);
        add_all_due(&db, "2026-08");
        add_all_due(&db, "2026-09");

        // Varies from October on; payments already added are untouched.
        db.set_recurring_amount(id, OCT, None).unwrap();
        assert_eq!(amounts(&db, "2026-09"), [Money(150000)]);
        let oct = db.due_transactions(OCT).unwrap();
        assert_eq!((oct[0].amount, oct[0].estimated), (Money(150000), true));
        assert!(
            !db.due_transactions(month("2026-09"))
                .unwrap()
                .iter()
                .any(|d| d.estimated)
        );

        // Fixed again from January, same series.
        db.set_recurring_amount(id, month("2027-01"), Some(Money(155000)))
            .unwrap();
        let r = db.recurring().unwrap().remove(0);
        assert_eq!(r.id, id);
        assert_eq!(r.amount_in(month("2026-12")), None);
        let jan = db.due_transactions(month("2027-01")).unwrap();
        assert_eq!((jan[0].amount, jan[0].estimated), (Money(155000), false));
        assert_eq!(db.recurring().unwrap().len(), 1);
    }

    #[test]
    fn confirming_a_payment_for_later_months_makes_the_amount_fixed() {
        let db = Db::open_in_memory().unwrap();
        water(&db, "2026-10");
        add_all_due(&db, "2026-10");
        let t = db.transactions_in(OCT).unwrap().remove(0);
        db.update_transaction_and_series(
            t.id,
            &TransactionInput {
                date: t.date,
                amount: Money(7500),
                category_id: category(&db, "Utilities"),
                payment_method_id: None,
                note: "Water".into(),
            },
        )
        .unwrap();
        assert!(!find(&db, t.id).amount_pending);
        let nov = db.due_transactions(month("2026-11")).unwrap();
        assert_eq!((nov[0].amount, nov[0].estimated), (Money(7500), false));
    }

    #[test]
    fn a_transaction_can_start_repeating_with_a_varying_amount() {
        let db = Db::open_in_memory().unwrap();
        let t = add(&db, "2026-10-12", 8412, "Utilities");
        db.repeat_transaction(t, Frequency::Monthly, None, true)
            .unwrap();
        let r = db.recurring().unwrap().remove(0);
        assert_eq!(r.amounts, [(OCT, None)]);
        assert!(!find(&db, t).amount_pending, "its own amount is known");
        let nov = db.due_transactions(month("2026-11")).unwrap();
        assert_eq!((nov[0].amount, nov[0].estimated), (Money(8412), true));
    }

    #[test]
    fn migrating_a_v6_database_keeps_recurring_amounts() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..6] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 6).unwrap();
        conn.execute_batch(
            "INSERT INTO recurring (start_month, day, category_id, note)
                 SELECT '2026-01', 1, id, 'Rent' FROM categories WHERE name = 'Housing';
             INSERT INTO recurring_amounts VALUES (1, '2026-01', 150000);
             INSERT INTO transactions (date, amount_cents, category_id, recurring_id, occurrence)
                 SELECT '2026-10-01', 150000, id, 1, '2026-10' FROM categories
                 WHERE name = 'Housing';",
        )
        .unwrap();

        let db = Db::init(conn).unwrap();
        let r = db.recurring().unwrap().remove(0);
        assert_eq!(r.amounts, [(month("2026-01"), Some(Money(150000)))]);
        let t = &db.transactions_in(OCT).unwrap()[0];
        assert_eq!((t.recurring_id, t.amount_pending), (Some(1), false));
        db.set_recurring_amount(1, OCT, None).unwrap();
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

    #[test]
    fn refuses_a_database_from_a_newer_version() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", MIGRATIONS.len() as i64 + 1)
            .unwrap();
        let err = Db::init(conn).err().unwrap();
        assert!(err.to_string().contains("newer version"));
    }

    #[test]
    fn backs_up_before_migrating() {
        let dir = std::env::temp_dir().join(format!("budget-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("budget.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        drop(conn);

        Db::open(&path).unwrap();
        let backup = Connection::open(dir.join("budget.db.bak-v1")).unwrap();
        assert_eq!(schema_version(&backup).unwrap(), 1);

        // Already up to date: no new backup.
        std::fs::remove_file(dir.join("budget.db.bak-v1")).unwrap();
        Db::open(&path).unwrap();
        assert!(!dir.join("budget.db.bak-v1").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
