use chrono::Datelike as _;
use gpui_kit::component::button::*;
use gpui_kit::component::calendar::Date;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::date_picker::{DatePicker, DatePickerState};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::*;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::transactions::{
    OptionItem, category_items, end_date_picker, frequency_select, payment_method_items,
    selected_frequency, set_options,
};
use super::{field, text_input, truncated_text};
use crate::model::{
    Category, CategoryKind, Money, Month, PaymentMethod, Recurring, RecurringInput, UNASSIGNED,
};
use crate::state::AppState;

/// How far past the current month the "Starting" dropdown for an amount
/// change reaches.
const AMOUNT_CHANGE_MONTHS: i32 = 24;

/// A month in the "Starting" dropdown, valued by `year * 100 + month`.
fn month_value(month: Month) -> i64 {
    month.year as i64 * 100 + month.month as i64
}

fn value_month(value: i64) -> Month {
    Month {
        year: (value / 100) as i32,
        month: (value % 100) as u32,
    }
}

/// Every recurring transaction, with a form to add one or edit, stop or
/// delete it. Changes only affect payments not added yet: each payment
/// already added is an ordinary transaction that keeps its own amount.
pub struct RecurringView {
    state: Entity<AppState>,
    amount: Entity<InputState>,
    category: Entity<SelectState<SearchableVec<OptionItem>>>,
    payment_method: Entity<SelectState<SearchableVec<OptionItem>>>,
    start: Entity<DatePickerState>,
    frequency: Entity<SelectState<SearchableVec<OptionItem>>>,
    end: Entity<DatePickerState>,
    note: Entity<InputState>,
    /// Whether "Amount varies" is checked, for bills like utilities whose
    /// amount is entered once each one arrives.
    varies: bool,
    /// When editing, the month a new amount takes effect from.
    amount_from: Entity<SelectState<SearchableVec<OptionItem>>>,
    /// The series being edited, or `None` when adding one.
    editing: Option<i64>,
    form_error: Option<SharedString>,
    /// The series whose Delete button was clicked and awaits confirmation.
    confirming: Option<i64>,
    categories: Vec<Category>,
    payment_methods: Vec<PaymentMethod>,
    _subscriptions: Vec<Subscription>,
}

impl RecurringView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let categories = state.read(cx).categories.clone();
        let payment_methods = state.read(cx).payment_methods.clone();
        let amount = cx.new(|cx| InputState::new(window, cx).placeholder("0.00"));
        let note =
            cx.new(|cx| InputState::new(window, cx).placeholder("Like \"Rent\" or \"Netflix\""));
        let category = cx.new(|cx| {
            SelectState::new(category_items(&categories), None, window, cx).searchable(true)
        });
        let payment_method = cx.new(|cx| {
            SelectState::new(payment_method_items(&payment_methods), None, window, cx)
                .searchable(true)
        });
        let start = cx.new(|cx| {
            let mut picker = DatePickerState::new(window, cx).date_format("%b %-d, %Y");
            picker.set_date(Month::current().default_entry_date(), window, cx);
            picker
        });
        let amount_from = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(Vec::<OptionItem>::new()),
                None,
                window,
                cx,
            )
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
        // Choosing when a new amount applies shows the amount at that point.
        subscriptions.push(cx.subscribe_in(
            &amount_from,
            window,
            |this, _, _: &SelectEvent<SearchableVec<OptionItem>>, window, cx| {
                this.show_amount_from(window, cx);
            },
        ));
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
            if let Some(id) = this.editing
                && state.read(cx).recurring(id).is_none()
            {
                this.reset_form(window, cx);
            }
            cx.notify();
        }));

        Self {
            state,
            amount,
            category,
            payment_method,
            start,
            frequency: frequency_select(window, cx),
            end: end_date_picker(window, cx),
            note,
            varies: false,
            amount_from,
            editing: None,
            form_error: None,
            confirming: None,
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

    /// The form's fields, and its amount or `None` if it varies.
    fn read_form(&self, cx: &App) -> Result<(RecurringInput, Option<Money>), &'static str> {
        let amount = if self.varies {
            None
        } else {
            Some(
                Money::parse(&self.amount.read(cx).value())
                    .filter(|m| m.0 > 0)
                    .ok_or("Enter a positive amount, like 12.50, or check \"Amount varies\".")?,
            )
        };
        let category_id = *self
            .category
            .read(cx)
            .selected_value()
            .ok_or("Choose a category.")?;
        let first = self
            .start
            .read(cx)
            .date()
            .start()
            .ok_or("Choose the date of the first payment.")?;
        let start = Month::containing(first);
        let end = self.end.read(cx).date().start().map(Month::containing);
        if end.is_some_and(|end| end < start) {
            return Err("The last payment can't be before the first.");
        }
        let payment_method_id = if self.income_selected(cx) {
            None
        } else {
            self.payment_method.read(cx).selected_value().copied()
        };
        Ok((
            RecurringInput {
                start,
                end,
                frequency: selected_frequency(&self.frequency, cx),
                day: first.day(),
                category_id,
                payment_method_id,
                note: self.note.read(cx).value().trim().to_string(),
            },
            amount,
        ))
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (input, amount) = match self.read_form(cx) {
            Ok(form) => form,
            Err(message) => {
                self.form_error = Some(message.into());
                cx.notify();
                return;
            }
        };
        match self
            .editing
            .and_then(|id| self.state.read(cx).recurring(id).cloned())
        {
            Some(series) => {
                // Only record an amount change if the amount (or whether it
                // varies) differs from what that month would already use.
                let from = self
                    .amount_from
                    .read(cx)
                    .selected_value()
                    .map(|v| value_month(*v))
                    .unwrap_or(input.start);
                let change = (series.amount_in(from) != amount).then_some((from, amount));
                self.state
                    .update(cx, |s, cx| s.update_recurring(series.id, input, change, cx));
            }
            None => self
                .state
                .update(cx, |s, cx| s.add_recurring(input, amount, cx)),
        }
        if self.state.read(cx).error.is_none() {
            self.reset_form(window, cx);
        }
    }

    fn start_edit(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.state.read(cx).recurring(id).cloned() else {
            return;
        };
        self.editing = Some(id);
        self.confirming = None;
        self.form_error = None;

        // A new amount can start in any month with a payment, defaulting to
        // the next one from this month on.
        let current = Month::current();
        let last = r.end.unwrap_or(current.plus(AMOUNT_CHANGE_MONTHS));
        let months = r.occurrences(r.start, last);
        let from = r
            .next_occurrence(current)
            .or(months.last().copied())
            .unwrap_or(r.start);
        let items: Vec<OptionItem> = months
            .iter()
            .map(|m| OptionItem::new(month_value(*m), format!("From {}", m.short_label())))
            .collect();
        self.amount_from.update(cx, |s, cx| {
            s.set_items(SearchableVec::new(items), window, cx);
            s.set_selected_value(&month_value(from), window, cx);
        });
        self.show_amount_from(window, cx);
        self.note
            .update(cx, |i, cx| i.set_value(r.note.clone(), window, cx));
        self.category.update(cx, |s, cx| match r.category_id {
            Some(id) => s.set_selected_value(&id, window, cx),
            None => s.set_selected_index(None, window, cx),
        });
        self.payment_method
            .update(cx, |s, cx| match r.payment_method_id {
                Some(id) => s.set_selected_value(&id, window, cx),
                None => s.set_selected_index(None, window, cx),
            });
        let first = r.date_in(r.start);
        self.start.update(cx, |p, cx| p.set_date(first, window, cx));
        self.frequency.update(cx, |s, cx| {
            s.set_selected_value(&(r.frequency.months() as i64), window, cx)
        });
        self.end.update(cx, |p, cx| match r.end {
            Some(end) => p.set_date(r.date_in(end), window, cx),
            None => p.set_date(Date::Single(None), window, cx),
        });
        cx.notify();
    }

    /// Fill in the amount, or "Amount varies", that the edited series uses in
    /// the month chosen under "New amount applies".
    fn show_amount_from(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(r) = self.editing.and_then(|id| state.recurring(id)) else {
            return;
        };
        let from = self
            .amount_from
            .read(cx)
            .selected_value()
            .map_or(r.start, |v| value_month(*v));
        let amount = r.amount_in(from);
        self.varies = amount.is_none();
        let text = amount.map(Money::to_plain).unwrap_or_default();
        self.amount
            .update(cx, |i, cx| i.set_value(text, window, cx));
        cx.notify();
    }

    fn reset_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = None;
        self.varies = false;
        self.form_error = None;
        self.amount.update(cx, |i, cx| i.set_value("", window, cx));
        self.note.update(cx, |i, cx| i.set_value("", window, cx));
        let today = Month::current().default_entry_date();
        self.start.update(cx, |p, cx| p.set_date(today, window, cx));
        self.end
            .update(cx, |p, cx| p.set_date(Date::Single(None), window, cx));
        cx.notify();
    }

    fn render_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editing = self
            .editing
            .and_then(|id| self.state.read(cx).recurring(id).cloned());
        let income = self.income_selected(cx);
        let muted = cx.theme().muted_foreground;
        let history = editing.as_ref().filter(|r| r.amounts.len() > 1).map(|r| {
            let changes: Vec<String> = r
                .amounts
                .iter()
                .map(|(month, amount)| match amount {
                    Some(amount) => format!("{amount} from {}", month.short_label()),
                    None => format!("varies from {}", month.short_label()),
                })
                .collect();
            format!("Amounts: {}", changes.join(" · "))
        });
        v_flex()
            .gap_2()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(if editing.is_some() {
                        "Edit recurring transaction"
                    } else {
                        "Add recurring transaction"
                    }),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        field(
                            "Amount",
                            if self.varies {
                                div()
                                    .h(px(32.))
                                    .flex()
                                    .items_center()
                                    .text_color(muted)
                                    .child("Varies")
                            } else {
                                text_input(&self.amount)
                            },
                            cx,
                        )
                        .w(px(120.)),
                    )
                    .when(editing.is_some(), |this| {
                        this.child(
                            field("New amount applies", Select::new(&self.amount_from), cx)
                                .w(px(170.)),
                        )
                    })
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
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(field("Repeats", Select::new(&self.frequency), cx).w(px(160.)))
                    .child(field("First payment", DatePicker::new(&self.start), cx).w(px(170.)))
                    .child(
                        field(
                            "Last payment",
                            DatePicker::new(&self.end)
                                .placeholder("Never")
                                .cleanable(true),
                            cx,
                        )
                        .w(px(170.)),
                    )
                    .child(field("Note", text_input(&self.note), cx).flex_1()),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        Checkbox::new("varies")
                            .label("Amount varies")
                            .tooltip(
                                "For bills like utilities: each payment is added with an \
                                 estimate, and you're reminded to enter the actual amount \
                                 once its date passes.",
                            )
                            .checked(self.varies)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.varies = *checked;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1().min_w_0().text_sm().text_color(muted).child(
                        history.unwrap_or_else(|| {
                            "Payments are offered in their month on the Transactions page. \
                             Changes here don't affect payments already added."
                                .into()
                        }),
                    ))
                    .when(editing.is_some(), |this| {
                        this.child(Button::new("cancel").ghost().label("Cancel").on_click(
                            cx.listener(|this, _, window, cx| this.reset_form(window, cx)),
                        ))
                    })
                    .child(
                        Button::new("submit")
                            .primary()
                            .label(if editing.is_some() { "Save" } else { "Add" })
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
            .when_some(self.form_error.clone(), |this, error| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
    }

    fn render_row(&self, r: &Recurring, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let theme = cx.theme();
        let id = r.id;
        let current = Month::current();
        let next = r.next_occurrence(current);
        let category = r
            .category_id
            .and_then(|id| state.category(id))
            .map_or(UNASSIGNED.to_string(), |c| c.name.clone());
        let is_income = match r.category_id {
            Some(id) => state.category(id).map(|c| c.kind),
            None => r.previous_category.as_ref().map(|p| p.kind),
        } == Some(CategoryKind::Income);
        let title = if r.note.trim().is_empty() {
            category.clone()
        } else {
            r.note.clone()
        };
        let mut details = vec![category];
        if let Some(p) = r.payment_method_id.and_then(|id| state.payment_method(id)) {
            details.push(p.name.clone());
        }
        let amount = r.amount_in(next.or(r.end).unwrap_or(current));
        let status = match next {
            Some(month) => format!("Next {}", r.date_in(month).format("%b %-d, %Y")),
            None if r.start > current => "Not started".into(),
            None => "Ended".into(),
        };
        let confirming = self.confirming == Some(id);
        let selected = self.editing == Some(id);

        let actions = if confirming {
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.danger)
                        .child("Delete? Payments already added are kept."),
                )
                .child(
                    Button::new(SharedString::from(format!("confirm-delete-{id}")))
                        .danger()
                        .compact()
                        .label("Delete")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.confirming = None;
                            this.state.update(cx, |s, cx| s.delete_recurring(id, cx));
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("cancel-delete-{id}")))
                        .ghost()
                        .compact()
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.confirming = None;
                            cx.notify();
                        })),
                )
        } else {
            h_flex()
                .gap_1()
                .when(next.is_some() && r.start <= current, |this| {
                    this.child(
                        Button::new(SharedString::from(format!("stop-{id}")))
                            .ghost()
                            .compact()
                            .label("Stop")
                            .tooltip("No more payments after the latest one added")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.state.update(cx, |s, cx| s.stop_recurring(id, cx));
                            })),
                    )
                })
                .child(
                    Button::new(SharedString::from(format!("delete-recurring-{id}")))
                        .ghost()
                        .compact()
                        .label("Delete")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.confirming = Some(id);
                            cx.notify();
                        })),
                )
        };

        h_flex()
            .id(SharedString::from(format!("recurring-{id}")))
            .gap_4()
            .px_3()
            .py_2()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .cursor_pointer()
            .hover(|this| this.bg(theme.list_hover))
            .when(selected || confirming, |this| this.bg(theme.list_active))
            .when(next.is_none(), |this| {
                this.text_color(theme.muted_foreground)
            })
            .on_click(cx.listener(move |this, _, window, cx| this.start_edit(id, window, cx)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(truncated_text("title", title))
                    .child(
                        truncated_text("details", details.join(" · "))
                            .text_sm()
                            .text_color(theme.muted_foreground),
                    ),
            )
            .child(
                v_flex()
                    .w(px(200.))
                    .child(truncated_text("schedule", r.schedule_label()))
                    .child(
                        truncated_text("range", r.range_label())
                            .text_sm()
                            .text_color(theme.muted_foreground),
                    ),
            )
            .child(
                div()
                    .w(px(130.))
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(status),
            )
            .child(
                div()
                    .w(px(110.))
                    .text_right()
                    .font_weight(FontWeight::MEDIUM)
                    .when(is_income && next.is_some(), |this| {
                        this.text_color(theme.success)
                    })
                    .child(match amount {
                        None => "Varies".to_string(),
                        Some(amount) if is_income => format!("+{amount}"),
                        Some(amount) => format!("-{amount}"),
                    }),
            )
            .child(actions)
            .into_any_element()
    }
}

impl Render for RecurringView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Upcoming payments first, soonest at the top, then ended series.
        let current = Month::current();
        let mut series = self.state.read(cx).recurring.clone();
        series.sort_by_key(|r| {
            let next = r.next_occurrence(current);
            (
                next.is_none(),
                next.map(|m| r.date_in(m)),
                r.note.to_lowercase(),
            )
        });
        let rows: Vec<AnyElement> = series.iter().map(|r| self.render_row(r, cx)).collect();
        let empty = rows.is_empty();
        v_flex()
            .size_full()
            .p_6()
            .gap_4()
            .child(self.render_form(cx))
            .child(
                v_flex()
                    .id("recurring-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .children(rows)
                    .child(
                        div()
                            .p_3()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(if empty {
                                "No recurring transactions yet. Add one here, or check \
                                 \"Repeat\" when adding a transaction."
                            } else {
                                "Click a recurring transaction to edit it."
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{month_value, value_month};
    use crate::model::Month;

    #[test]
    fn month_values_round_trip() {
        let m = Month {
            year: 2026,
            month: 11,
        };
        assert_eq!(month_value(m), 202611);
        assert_eq!(value_month(month_value(m)), m);
    }
}
