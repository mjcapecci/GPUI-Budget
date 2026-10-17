use std::collections::HashMap;

use anyhow::Result;
use chrono::NaiveDate;
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

use crate::db::Db;
use crate::import::{API_KEY_ENV, API_KEY_SETTING};
use crate::model::{
    Category, CategoryKind, CategoryStatus, Money, Month, MonthSummary, PaymentMethod,
    PaymentMethodSpending, Transaction, TransactionInput,
};

const THEME_KEY: &str = "theme";

/// Shared application state. Views hold an `Entity<AppState>` and re-render
/// when it notifies. Every mutation writes to SQLite and then reloads the
/// cached data for the selected month.
pub struct AppState {
    db: Db,
    pub month: Month,
    pub categories: Vec<Category>,
    /// Transactions per category across all months; `None` counts unassigned ones.
    pub transaction_counts: HashMap<Option<i64>, usize>,
    pub payment_methods: Vec<PaymentMethod>,
    /// Transactions per payment method across all months; `None` counts those
    /// whose payment method was deleted.
    pub payment_method_counts: HashMap<Option<i64>, usize>,
    pub transactions: Vec<Transaction>,
    pub statuses: Vec<CategoryStatus>,
    pub summary: MonthSummary,
    pub payment_method_spending: PaymentMethodSpending,
    /// The last database error, shown in the UI until the next success.
    pub error: Option<String>,
}

impl AppState {
    pub fn new(db: Db) -> Self {
        let mut state = AppState {
            db,
            month: Month::current(),
            categories: Vec::new(),
            transaction_counts: HashMap::new(),
            payment_methods: Vec::new(),
            payment_method_counts: HashMap::new(),
            transactions: Vec::new(),
            statuses: Vec::new(),
            summary: MonthSummary::default(),
            payment_method_spending: PaymentMethodSpending::default(),
            error: None,
        };
        let result = state.reload();
        state.record(result);
        state
    }

    pub fn category(&self, id: i64) -> Option<&Category> {
        self.categories.iter().find(|c| c.id == id)
    }

    /// Whether a transaction counts as income or expense. Unassigned
    /// transactions keep the kind of the category they were removed from.
    pub fn kind_of(&self, t: &Transaction) -> Option<CategoryKind> {
        match t.category_id {
            Some(id) => self.category(id).map(|c| c.kind),
            None => t.previous_category.as_ref().map(|p| p.kind),
        }
    }

    pub fn add_category(&mut self, name: String, kind: CategoryKind, cx: &mut Context<Self>) {
        self.apply(cx, |db| db.add_category(&name, kind).map(|_| ()));
    }

    pub fn delete_category(&mut self, id: i64, cx: &mut Context<Self>) {
        self.apply(cx, |db| db.delete_category(id).map(|_| ()));
    }

    pub fn payment_method(&self, id: i64) -> Option<&PaymentMethod> {
        self.payment_methods.iter().find(|p| p.id == id)
    }

    pub fn add_payment_method(&mut self, name: String, cx: &mut Context<Self>) {
        self.apply(cx, |db| db.add_payment_method(&name).map(|_| ()));
    }

    pub fn delete_payment_method(&mut self, id: i64, cx: &mut Context<Self>) {
        self.apply(cx, |db| db.delete_payment_method(id).map(|_| ()));
    }

    pub fn set_month(&mut self, month: Month, cx: &mut Context<Self>) {
        self.month = month;
        self.apply(cx, |_| Ok(()));
    }

    pub fn add_transaction(&mut self, input: TransactionInput, cx: &mut Context<Self>) {
        self.apply(cx, |db| db.add_transaction(&input).map(|_| ()));
    }

    pub fn update_transaction(&mut self, id: i64, input: TransactionInput, cx: &mut Context<Self>) {
        self.apply(cx, |db| db.update_transaction(id, &input));
    }

    pub fn delete_transaction(&mut self, id: i64, cx: &mut Context<Self>) {
        self.apply(cx, |db| db.delete_transaction(id));
    }

    /// Put back a transaction removed by [`Self::delete_transaction`].
    pub fn restore_transaction(&mut self, t: Transaction, cx: &mut Context<Self>) {
        self.apply(cx, |db| db.restore_transaction(&t));
    }

    pub fn set_budget(&mut self, category_id: i64, limit: Option<Money>, cx: &mut Context<Self>) {
        let month = self.month;
        self.apply(cx, |db| db.set_budget(category_id, month, limit));
    }

    /// Add the reviewed transactions from an import, all or none. Returns
    /// whether they were saved.
    pub fn import_transactions(
        &mut self,
        inputs: Vec<TransactionInput>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.apply(cx, |db| db.import_transactions(&inputs));
        self.error.is_none()
    }

    /// The category last imported for this merchant.
    pub fn merchant_category(&self, merchant: &str) -> Option<i64> {
        self.db.merchant_category(merchant).ok().flatten()
    }

    /// An existing transaction an imported one may duplicate: the same
    /// amount within a few days, since posting dates can lag.
    pub fn similar_transaction(&self, date: NaiveDate, amount: Money) -> Option<Transaction> {
        self.db.similar_transaction(date, amount, 3).ok().flatten()
    }

    /// The key saved in Settings, if any.
    pub fn saved_api_key(&self) -> Option<String> {
        self.db
            .setting(API_KEY_SETTING)
            .ok()
            .flatten()
            .filter(|k| !k.is_empty())
    }

    /// The Anthropic API key for reading screenshots: the saved one, or the
    /// `ANTHROPIC_API_KEY` environment variable.
    pub fn api_key(&self) -> Option<String> {
        self.saved_api_key().or_else(|| {
            std::env::var(API_KEY_ENV)
                .ok()
                .filter(|k| !k.trim().is_empty())
        })
    }

    /// Save the API key, or remove it when empty.
    pub fn set_api_key(&mut self, key: &str, cx: &mut Context<Self>) {
        let result = self.db.set_setting(API_KEY_SETTING, key.trim());
        self.record(result);
        cx.notify();
    }

    /// The light/dark choice saved by the user, or `None` to follow the system.
    pub fn saved_theme_mode(&self) -> Option<ThemeMode> {
        match self.db.setting(THEME_KEY).ok().flatten()?.as_str() {
            "light" => Some(ThemeMode::Light),
            "dark" => Some(ThemeMode::Dark),
            _ => None,
        }
    }

    /// Switch to light or dark mode and remember the choice.
    pub fn set_theme_mode(&mut self, mode: ThemeMode, cx: &mut Context<Self>) {
        Theme::change(mode, None, cx);
        let result = self.db.set_setting(THEME_KEY, mode.name());
        self.record(result);
        cx.notify();
    }

    fn apply(&mut self, cx: &mut Context<Self>, op: impl FnOnce(&Db) -> Result<()>) {
        let result = op(&self.db).and_then(|()| self.reload());
        self.record(result);
        cx.notify();
    }

    fn reload(&mut self) -> Result<()> {
        self.categories = self.db.categories()?;
        self.transaction_counts = self.db.transaction_counts()?;
        self.payment_methods = self.db.payment_methods()?;
        self.payment_method_counts = self.db.payment_method_counts()?;
        self.transactions = self.db.transactions_in(self.month)?;
        self.statuses = self.db.category_statuses(self.month)?;
        self.summary = self.db.month_summary(self.month)?;
        self.payment_method_spending = self.db.payment_method_spending(self.month)?;
        Ok(())
    }

    fn record(&mut self, result: Result<()>) {
        self.error = result.err().map(|e| format!("{e:#}"));
    }
}
