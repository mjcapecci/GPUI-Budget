use chrono::NaiveDate;
use gpui_kit::component::button::*;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::*;
use gpui_kit::component::*;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::model::{
    Category, CategoryKind, Money, Month, PaymentMethod, TransactionInput, UNASSIGNED,
};
use crate::state::AppState;

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

fn category_items(categories: &[Category]) -> Vec<OptionItem> {
    categories
        .iter()
        .map(|c| OptionItem {
            id: c.id,
            title: match c.kind {
                CategoryKind::Income => format!("{} (income)", c.name),
                CategoryKind::Expense => c.name.clone(),
            }
            .into(),
        })
        .collect()
}

fn payment_method_items(payment_methods: &[PaymentMethod]) -> Vec<OptionItem> {
    payment_methods
        .iter()
        .map(|p| OptionItem { id: p.id, title: p.name.clone().into() })
        .collect()
}

/// Replace a dropdown's options, keeping the selection if it still exists.
fn set_options(
    select: &Entity<SelectState<Vec<OptionItem>>>,
    items: Vec<OptionItem>,
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

/// The month's transactions with an add/edit form above the list.
pub struct TransactionsView {
    state: Entity<AppState>,
    date: Entity<InputState>,
    amount: Entity<InputState>,
    note: Entity<InputState>,
    category: Entity<SelectState<Vec<OptionItem>>>,
    /// Optional, and only used for expenses.
    payment_method: Entity<SelectState<Vec<OptionItem>>>,
    /// The transaction being edited, or `None` when adding a new one.
    editing: Option<i64>,
    form_error: Option<SharedString>,
    month: Month,
    /// The categories the dropdown was last built from.
    categories: Vec<Category>,
    /// The payment methods the dropdown was last built from.
    payment_methods: Vec<PaymentMethod>,
    _subscriptions: Vec<Subscription>,
}

impl TransactionsView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let month = state.read(cx).month;
        let categories = state.read(cx).categories.clone();
        let payment_methods = state.read(cx).payment_methods.clone();

        let date = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("YYYY-MM-DD")
                .default_value(month.default_entry_date().to_string())
        });
        let amount = cx.new(|cx| InputState::new(window, cx).placeholder("0.00"));
        let note = cx.new(|cx| InputState::new(window, cx).placeholder("Note (optional)"));
        let category =
            cx.new(|cx| SelectState::new(category_items(&categories), None, window, cx));
        let payment_method = cx.new(|cx| {
            SelectState::new(payment_method_items(&payment_methods), None, window, cx)
        });

        let mut subscriptions = Vec::new();
        for input in [&date, &amount, &note] {
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
            |_, _, _: &SelectEvent<Vec<OptionItem>>, cx| cx.notify(),
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
                    this.date.update(cx, |input, cx| {
                        input.set_value(month.default_entry_date().to_string(), window, cx)
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
        let date = self.date.read(cx).value();
        let date = NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d")
            .map_err(|_| "Enter the date as YYYY-MM-DD.")?;
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
        let Some(t) = self.state.read(cx).transactions.iter().find(|t| t.id == id).cloned() else {
            return;
        };
        self.editing = Some(id);
        self.form_error = None;
        self.date
            .update(cx, |i, cx| i.set_value(t.date.to_string(), window, cx));
        self.amount
            .update(cx, |i, cx| i.set_value(t.amount.to_plain(), window, cx));
        self.note.update(cx, |i, cx| i.set_value(t.note, window, cx));
        self.category.update(cx, |s, cx| match t.category_id {
            Some(id) => s.set_selected_value(&id, window, cx),
            None => s.set_selected_index(None, window, cx),
        });
        self.payment_method.update(cx, |s, cx| match t.payment_method_id {
            Some(id) => s.set_selected_value(&id, window, cx),
            None => s.set_selected_index(None, window, cx),
        });
        cx.notify();
    }

    /// Clear the form for the next entry. Date, category and payment method
    /// are kept when adding, since consecutive entries often share them.
    fn reset_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            let date = self.month.default_entry_date().to_string();
            self.date.update(cx, |i, cx| i.set_value(date, window, cx));
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
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(if editing { "Edit transaction" } else { "Add transaction" }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(Input::new(&self.date).w(px(120.)))
                    .child(
                        div()
                            .w(px(200.))
                            .child(Select::new(&self.category).placeholder("Category")),
                    )
                    .child(
                        div().w(px(200.)).child(
                            Select::new(&self.payment_method)
                                .placeholder(if income {
                                    "No payment method for income"
                                } else {
                                    "Payment method"
                                })
                                .cleanable(true)
                                .disabled(income),
                        ),
                    )
                    .child(Input::new(&self.amount).w(px(110.))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(Input::new(&self.note).flex_1())
                    .child(
                        Button::new("submit")
                            .primary()
                            .label(if editing { "Save" } else { "Add" })
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    )
                    .when(editing, |this| {
                        this.child(Button::new("cancel").ghost().label("Cancel").on_click(
                            cx.listener(|this, _, window, cx| this.reset_form(window, cx)),
                        ))
                    }),
            )
            .when_some(self.form_error.clone(), |this, error| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let theme = cx.theme();
        let rows: Vec<AnyElement> = state
            .transactions
            .iter()
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
                    .when(selected, |this| this.bg(theme.list_active))
                    .child(
                        div()
                            .w(px(100.))
                            .text_color(theme.muted_foreground)
                            .child(t.date.format("%b %-d, %Y").to_string()),
                    )
                    .child(div().w(px(160.)).map(|this| match category {
                        Some(c) => this.child(c.name.clone()),
                        None => this
                            .italic()
                            .text_color(theme.muted_foreground)
                            .child(UNASSIGNED),
                    }))
                    .child(
                        div()
                            .w(px(140.))
                            .min_w_0()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .map(|this| {
                                match t.payment_method_id.and_then(|id| state.payment_method(id)) {
                                    Some(p) => this.child(p.name.clone()),
                                    None if t.previous_payment_method.is_some() => {
                                        this.italic().child(UNASSIGNED)
                                    }
                                    None => this,
                                }
                            }),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(t.note.clone()))
                    .child(
                        div()
                            .w(px(110.))
                            .text_right()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if is_income { theme.success } else { theme.foreground })
                            .child(if is_income {
                                format!("+{}", t.amount)
                            } else {
                                format!("-{}", t.amount)
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new(SharedString::from(format!("edit-{id}")))
                                    .ghost()
                                    .compact()
                                    .label("Edit")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.start_edit(id, window, cx)
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("delete-{id}")))
                                    .ghost()
                                    .compact()
                                    .label("Delete")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if this.editing == Some(id) {
                                            this.reset_form(window, cx);
                                        }
                                        this.state
                                            .update(cx, |s, cx| s.delete_transaction(id, cx));
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
    }
}

impl Render for TransactionsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .p_6()
            .gap_4()
            .child(self.render_form(cx))
            .child(self.render_list(cx))
    }
}
