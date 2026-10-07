use chrono::{Days, Local};
use gpui_kit::component::button::*;
use gpui_kit::component::date_picker::{DatePicker, DatePickerState, DateRangePreset};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::*;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use std::cmp::Ordering;

use super::{field, text_input, truncated_text};
use crate::model::{
    Category, CategoryKind, Money, Month, PaymentMethod, Transaction, TransactionInput, UNASSIGNED,
};
use crate::state::AppState;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortColumn {
    Date,
    Category,
    PaymentMethod,
    Note,
    Amount,
}

/// How the transaction list is ordered. Click a column header to sort by it,
/// click it again to reverse.
#[derive(Clone, Copy)]
struct Sort {
    column: SortColumn,
    descending: bool,
}

impl Default for Sort {
    /// Newest first, matching the order transactions are loaded in.
    fn default() -> Self {
        Self {
            column: SortColumn::Date,
            descending: true,
        }
    }
}

impl Sort {
    /// Sort by `column`, or reverse the direction if already sorted by it.
    /// Dates and amounts start out largest first, text columns A to Z.
    fn toggle(self, column: SortColumn) -> Self {
        if self.column == column {
            Self {
                column,
                descending: !self.descending,
            }
        } else {
            Self {
                column,
                descending: matches!(column, SortColumn::Date | SortColumn::Amount),
            }
        }
    }

    /// Order two transactions. Blank text values (no payment method, no note,
    /// unassigned category) always go last, whatever the direction.
    fn compare(self, state: &AppState, a: &Transaction, b: &Transaction) -> Ordering {
        let text = |a: Option<String>, b: Option<String>| match (a, b) {
            (Some(a), Some(b)) => self.directed(a.to_lowercase().cmp(&b.to_lowercase())),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        let category = |t: &Transaction| {
            t.category_id
                .and_then(|id| state.category(id))
                .map(|c| c.name.clone())
        };
        let payment_method = |t: &Transaction| {
            t.payment_method_id
                .and_then(|id| state.payment_method(id))
                .map(|p| p.name.clone())
        };
        let note = |t: &Transaction| Some(t.note.clone()).filter(|n| !n.trim().is_empty());
        // Income counts as positive and expenses as negative, as displayed.
        let signed = |t: &Transaction| {
            if state.kind_of(t) == Some(CategoryKind::Income) {
                t.amount.0
            } else {
                -t.amount.0
            }
        };

        match self.column {
            SortColumn::Date => self.directed(a.date.cmp(&b.date)),
            SortColumn::Category => text(category(a), category(b)),
            SortColumn::PaymentMethod => text(payment_method(a), payment_method(b)),
            SortColumn::Note => text(note(a), note(b)),
            SortColumn::Amount => self.directed(signed(a).cmp(&signed(b))),
        }
    }

    fn directed(self, ordering: Ordering) -> Ordering {
        if self.descending {
            ordering.reverse()
        } else {
            ordering
        }
    }
}

/// Width of the Delete button column, so the header lines up with rows.
const ACTIONS_WIDTH: Pixels = px(70.);

/// A category or payment method option in one of the form's dropdowns.
#[derive(Clone)]
pub struct OptionItem {
    id: i64,
    title: SharedString,
}

impl SelectItem for OptionItem {
    type Value = i64;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &i64 {
        &self.id
    }
}

fn category_items(categories: &[Category]) -> SearchableVec<OptionItem> {
    let items: Vec<OptionItem> = categories
        .iter()
        .map(|c| OptionItem {
            id: c.id,
            title: match c.kind {
                CategoryKind::Income => format!("{} (income)", c.name),
                CategoryKind::Expense => c.name.clone(),
            }
            .into(),
        })
        .collect();
    SearchableVec::new(items)
}

fn payment_method_items(payment_methods: &[PaymentMethod]) -> SearchableVec<OptionItem> {
    let items: Vec<OptionItem> = payment_methods
        .iter()
        .map(|p| OptionItem {
            id: p.id,
            title: p.name.clone().into(),
        })
        .collect();
    SearchableVec::new(items)
}

/// Replace a dropdown's options, keeping the selection if it still exists.
fn set_options(
    select: &Entity<SelectState<SearchableVec<OptionItem>>>,
    items: SearchableVec<OptionItem>,
    window: &mut Window,
    cx: &mut App,
) {
    select.update(cx, |select, cx| {
        let selected = select.selected_value().copied();
        select.set_items(items, window, cx);
        match selected {
            Some(id) => select.set_selected_value(&id, window, cx),
            None => select.set_selected_index(None, window, cx),
        }
    });
}

/// Quick picks shown beside the date picker's calendar.
fn date_presets() -> Vec<DateRangePreset> {
    let today = Local::now().date_naive();
    let mut presets = vec![DateRangePreset::single("Today", today)];
    if let Some(yesterday) = today.checked_sub_days(Days::new(1)) {
        presets.push(DateRangePreset::single("Yesterday", yesterday));
    }
    presets
}

/// The month's transactions with an add/edit form above the list.
pub struct TransactionsView {
    state: Entity<AppState>,
    date: Entity<DatePickerState>,
    amount: Entity<InputState>,
    note: Entity<InputState>,
    category: Entity<SelectState<SearchableVec<OptionItem>>>,
    /// Optional, and only used for expenses.
    payment_method: Entity<SelectState<SearchableVec<OptionItem>>>,
    /// The transaction being edited, or `None` when adding a new one.
    editing: Option<i64>,
    form_error: Option<SharedString>,
    month: Month,
    /// The categories the dropdown was last built from.
    categories: Vec<Category>,
    /// The payment methods the dropdown was last built from.
    payment_methods: Vec<PaymentMethod>,
    sort: Sort,
    _subscriptions: Vec<Subscription>,
}

impl TransactionsView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let month = state.read(cx).month;
        let categories = state.read(cx).categories.clone();
        let payment_methods = state.read(cx).payment_methods.clone();

        let date = cx.new(|cx| {
            let mut picker = DatePickerState::new(window, cx).date_format("%a, %b %-d, %Y");
            picker.set_date(month.default_entry_date(), window, cx);
            picker
        });
        let amount = cx.new(|cx| InputState::new(window, cx).placeholder("0.00"));
        let note = cx.new(|cx| InputState::new(window, cx).placeholder("Note (optional)"));
        let category = cx.new(|cx| {
            SelectState::new(category_items(&categories), None, window, cx).searchable(true)
        });
        let payment_method = cx.new(|cx| {
            SelectState::new(payment_method_items(&payment_methods), None, window, cx)
                .searchable(true)
        });

        let mut subscriptions = Vec::new();
        for input in [&amount, &note] {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.submit(window, cx);
                    }
                },
            ));
        }
        // Payment method is disabled while an income category is chosen.
        subscriptions.push(cx.subscribe(
            &category,
            |_, _, _: &SelectEvent<SearchableVec<OptionItem>>, cx| cx.notify(),
        ));
        // When the month changes, move the default date of a fresh form into it.
        subscriptions.push(cx.observe_in(&state, window, |this, state, window, cx| {
            if state.read(cx).categories != this.categories {
                this.categories = state.read(cx).categories.clone();
                set_options(&this.category, category_items(&this.categories), window, cx);
            }
            if state.read(cx).payment_methods != this.payment_methods {
                this.payment_methods = state.read(cx).payment_methods.clone();
                let items = payment_method_items(&this.payment_methods);
                set_options(&this.payment_method, items, window, cx);
            }
            let month = state.read(cx).month;
            if month != this.month {
                this.month = month;
                if this.editing.is_none() {
                    this.date.update(cx, |picker, cx| {
                        picker.set_date(month.default_entry_date(), window, cx)
                    });
                }
            }
            cx.notify();
        }));

        Self {
            state,
            date,
            amount,
            note,
            category,
            payment_method,
            editing: None,
            form_error: None,
            month,
            categories,
            payment_methods,
            sort: Sort::default(),
            _subscriptions: subscriptions,
        }
    }

    fn income_selected(&self, cx: &App) -> bool {
        let state = self.state.read(cx);
        self.category
            .read(cx)
            .selected_value()
            .and_then(|id| state.category(*id))
            .is_some_and(|c| c.kind == CategoryKind::Income)
    }

    fn read_form(&self, cx: &App) -> Result<TransactionInput, &'static str> {
        let date = self.date.read(cx).date().start().ok_or("Choose a date.")?;
        let category_id = *self
            .category
            .read(cx)
            .selected_value()
            .ok_or("Choose a category.")?;
        let amount = Money::parse(&self.amount.read(cx).value())
            .filter(|m| m.0 > 0)
            .ok_or("Enter a positive amount, like 12.50.")?;
        let payment_method_id = if self.income_selected(cx) {
            None
        } else {
            self.payment_method.read(cx).selected_value().copied()
        };
        Ok(TransactionInput {
            date,
            amount,
            category_id,
            payment_method_id,
            note: self.note.read(cx).value().trim().to_string(),
        })
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.read_form(cx) {
            Ok(input) => {
                let editing = self.editing;
                self.state.update(cx, |s, cx| match editing {
                    Some(id) => s.update_transaction(id, input, cx),
                    None => s.add_transaction(input, cx),
                });
                self.reset_form(window, cx);
            }
            Err(message) => {
                self.form_error = Some(message.into());
                cx.notify();
            }
        }
    }

    fn start_edit(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self
            .state
            .read(cx)
            .transactions
            .iter()
            .find(|t| t.id == id)
            .cloned()
        else {
            return;
        };
        self.editing = Some(id);
        self.form_error = None;
        self.date
            .update(cx, |picker, cx| picker.set_date(t.date, window, cx));
        self.amount
            .update(cx, |i, cx| i.set_value(t.amount.to_plain(), window, cx));
        self.note
            .update(cx, |i, cx| i.set_value(t.note, window, cx));
        self.category.update(cx, |s, cx| match t.category_id {
            Some(id) => s.set_selected_value(&id, window, cx),
            None => s.set_selected_index(None, window, cx),
        });
        self.payment_method
            .update(cx, |s, cx| match t.payment_method_id {
                Some(id) => s.set_selected_value(&id, window, cx),
                None => s.set_selected_index(None, window, cx),
            });
        cx.notify();
    }

    /// Clear the form for the next entry. Date, category and payment method
    /// are kept when adding, since consecutive entries often share them.
    fn reset_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            let date = self.month.default_entry_date();
            self.date
                .update(cx, |picker, cx| picker.set_date(date, window, cx));
        }
        self.form_error = None;
        self.amount.update(cx, |i, cx| i.set_value("", window, cx));
        self.note.update(cx, |i, cx| i.set_value("", window, cx));
        self.amount.update(cx, |i, cx| i.focus(window, cx));
        cx.notify();
    }

    fn render_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editing = self.editing.is_some();
        let income = self.income_selected(cx);
        v_flex()
            .gap_2()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .child(div().font_weight(FontWeight::MEDIUM).child(if editing {
                "Edit transaction"
            } else {
                "Add transaction"
            }))
            .child(
                h_flex()
                    .gap_3()
                    .child(field("Amount", text_input(&self.amount), cx).w(px(120.)))
                    .child(
                        field(
                            "Category",
                            Select::new(&self.category)
                                .placeholder("Choose a category")
                                .search_placeholder("Search categories"),
                            cx,
                        )
                        .flex_1()
                        .min_w_0(),
                    )
                    .child(
                        field(
                            "Payment method",
                            Select::new(&self.payment_method)
                                .placeholder(if income {
                                    "Not used for income"
                                } else {
                                    "None"
                                })
                                .search_placeholder("Search payment methods")
                                .cleanable(true)
                                .disabled(income),
                            cx,
                        )
                        .flex_1()
                        .min_w_0(),
                    )
                    .child(
                        field(
                            "Date",
                            DatePicker::new(&self.date).presets(date_presets()),
                            cx,
                        )
                        .w(px(190.)),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_end()
                    .child(field("Note", text_input(&self.note), cx).flex_1())
                    .when(editing, |this| {
                        this.child(Button::new("cancel").ghost().label("Cancel").on_click(
                            cx.listener(|this, _, window, cx| this.reset_form(window, cx)),
                        ))
                    })
                    .child(
                        Button::new("submit")
                            .primary()
                            .label(if editing { "Save" } else { "Add" })
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
            .when_some(self.form_error.clone(), |this, error| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
    }

    /// A clickable column header showing an arrow when the list is sorted by it.
    fn render_header_cell(
        &self,
        column: SortColumn,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let arrow = (self.sort.column == column).then_some(if self.sort.descending {
            "▼"
        } else {
            "▲"
        });
        h_flex()
            .id(label)
            .gap_1()
            .cursor_pointer()
            .hover(|this| this.text_color(cx.theme().foreground))
            .when(column == SortColumn::Amount, |this| this.justify_end())
            .child(label)
            .children(arrow.map(|a| div().text_xs().child(a)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.sort = this.sort.toggle(column);
                cx.notify();
            }))
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_4()
            .px_3()
            .py_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .text_color(cx.theme().muted_foreground)
            .child(
                self.render_header_cell(SortColumn::Date, "Date", cx)
                    .w(px(100.)),
            )
            .child(
                self.render_header_cell(SortColumn::Category, "Category", cx)
                    .w(px(160.)),
            )
            .child(
                self.render_header_cell(SortColumn::PaymentMethod, "Payment method", cx)
                    .w(px(140.)),
            )
            .child(
                self.render_header_cell(SortColumn::Note, "Note", cx)
                    .flex_1(),
            )
            .child(
                self.render_header_cell(SortColumn::Amount, "Amount", cx)
                    .w(px(110.)),
            )
            .child(div().w(ACTIONS_WIDTH))
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let theme = cx.theme();
        let mut transactions: Vec<&Transaction> = state.transactions.iter().collect();
        // Stable, so ties keep the loaded newest-first order.
        transactions.sort_by(|a, b| self.sort.compare(state, a, b));
        let rows: Vec<AnyElement> = transactions
            .into_iter()
            .map(|t| {
                let id = t.id;
                let category = t.category_id.and_then(|id| state.category(id));
                let is_income = state.kind_of(t) == Some(CategoryKind::Income);
                let selected = self.editing == Some(id);
                h_flex()
                    .id(SharedString::from(format!("row-{id}")))
                    .gap_4()
                    .px_3()
                    .py_2()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.list_hover))
                    .when(selected, |this| this.bg(theme.list_active))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.start_edit(id, window, cx)),
                    )
                    .child(
                        div()
                            .w(px(100.))
                            .text_color(theme.muted_foreground)
                            .child(t.date.format("%b %-d, %Y").to_string()),
                    )
                    .child(match category {
                        Some(c) => truncated_text("category", c.name.clone()).w(px(160.)),
                        None => truncated_text("category", UNASSIGNED)
                            .w(px(160.))
                            .italic()
                            .text_color(theme.muted_foreground),
                    })
                    .child(
                        match t.payment_method_id.and_then(|id| state.payment_method(id)) {
                            Some(p) => truncated_text("payment-method", p.name.clone()),
                            None if t.previous_payment_method.is_some() => {
                                truncated_text("payment-method", UNASSIGNED).italic()
                            }
                            None => truncated_text("payment-method", ""),
                        }
                        .w(px(140.))
                        .text_color(theme.muted_foreground),
                    )
                    .child(truncated_text("note", t.note.clone()).flex_1())
                    .child(
                        div()
                            .w(px(110.))
                            .text_right()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if is_income {
                                theme.success
                            } else {
                                theme.foreground
                            })
                            .child(if is_income {
                                format!("+{}", t.amount)
                            } else {
                                format!("-{}", t.amount)
                            }),
                    )
                    .child(
                        h_flex().w(ACTIONS_WIDTH).justify_end().gap_1().child(
                            Button::new(SharedString::from(format!("delete-{id}")))
                                .ghost()
                                .compact()
                                .label("Delete")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    // Don't also open the row for editing.
                                    cx.stop_propagation();
                                    if this.editing == Some(id) {
                                        this.reset_form(window, cx);
                                    }
                                    this.state.update(cx, |s, cx| s.delete_transaction(id, cx));
                                })),
                        ),
                    )
                    .into_any_element()
            })
            .collect();

        let empty = rows.is_empty();
        v_flex()
            .id("transactions-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .children(rows)
            .when(empty, |this| {
                this.child(
                    div()
                        .p_6()
                        .text_center()
                        .text_color(cx.theme().muted_foreground)
                        .child("No transactions this month yet."),
                )
            })
            .when(!empty, |this| {
                this.child(
                    div()
                        .p_3()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Click a transaction to edit it."),
                )
            })
    }
}

impl Render for TransactionsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .p_6()
            .gap_4()
            .child(self.render_form(cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_header(cx))
                    .child(self.render_list(cx)),
            )
    }
}
