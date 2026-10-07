use std::collections::HashMap;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::progress::*;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::text_input;
use crate::model::{Money, Month};
use crate::state::AppState;

/// Per-category monthly budgets with spent-vs-limit progress bars.
///
/// Each limit input saves on Enter or when it loses focus. Clearing an input
/// removes this month's budget, so the category falls back to the most recent
/// earlier month's budget (if any).
pub struct BudgetsView {
    state: Entity<AppState>,
    inputs: HashMap<i64, Entity<InputState>>,
    input_subscriptions: HashMap<i64, Subscription>,
    month: Month,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl BudgetsView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![cx.observe_in(&state, window, |this, state, window, cx| {
            this.sync_categories(window, cx);
            let month = state.read(cx).month;
            if month != this.month {
                this.month = month;
                this.error = None;
                this.sync_inputs(window, cx);
            }
            cx.notify();
        })];

        let mut view = Self {
            month: state.read(cx).month,
            state,
            inputs: HashMap::new(),
            input_subscriptions: HashMap::new(),
            error: None,
            _subscriptions: subscriptions,
        };
        view.sync_categories(window, cx);
        view.sync_inputs(window, cx);
        view
    }

    /// Add an input for each new category and drop those of deleted ones.
    fn sync_categories(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let category_ids: Vec<i64> = self
            .state
            .read(cx)
            .statuses
            .iter()
            .map(|s| s.category.id)
            .collect();
        self.inputs.retain(|id, _| category_ids.contains(id));
        self.input_subscriptions
            .retain(|id, _| category_ids.contains(id));

        for category_id in category_ids {
            if self.inputs.contains_key(&category_id) {
                continue;
            }
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("No budget"));
            let subscription = cx.subscribe_in(
                &input,
                window,
                move |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        this.save(category_id, window, cx);
                    }
                },
            );
            self.inputs.insert(category_id, input);
            self.input_subscriptions.insert(category_id, subscription);
        }
    }

    /// Fill every input with the selected month's budget.
    fn sync_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let budgets: Vec<(i64, Option<Money>)> = self
            .state
            .read(cx)
            .statuses
            .iter()
            .map(|s| (s.category.id, s.budget))
            .collect();
        for (id, budget) in budgets {
            if let Some(input) = self.inputs.get(&id) {
                let text = budget.map(Money::to_plain).unwrap_or_default();
                input.update(cx, |i, cx| i.set_value(text, window, cx));
            }
        }
    }

    fn save(&mut self, category_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.inputs.get(&category_id).cloned() else {
            return;
        };
        let text = input.read(cx).value();
        let limit = if text.trim().is_empty() {
            None
        } else {
            match Money::parse(&text).filter(|m| m.0 >= 0) {
                Some(m) => Some(m),
                None => {
                    self.error = Some(format!("\"{text}\" isn't a valid amount.").into());
                    cx.notify();
                    return;
                }
            }
        };

        let current = self
            .state
            .read(cx)
            .statuses
            .iter()
            .find(|s| s.category.id == category_id)
            .and_then(|s| s.budget);
        self.error = None;
        if limit != current {
            self.state
                .update(cx, |s, cx| s.set_budget(category_id, limit, cx));
        }
        // Normalize the text (e.g. "50" -> "50.00", or show an inherited budget).
        let budget = self
            .state
            .read(cx)
            .statuses
            .iter()
            .find(|s| s.category.id == category_id)
            .and_then(|s| s.budget);
        let text = budget.map(Money::to_plain).unwrap_or_default();
        input.update(cx, |i, cx| i.set_value(text, window, cx));
        cx.notify();
    }
}

impl Render for BudgetsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let theme = cx.theme();

        let total_budget: i64 = state
            .statuses
            .iter()
            .filter_map(|s| s.budget)
            .map(|m| m.0)
            .sum();
        let total_spent: i64 = state.statuses.iter().map(|s| s.spent.0).sum();

        let rows = state.statuses.iter().map(|status| {
            let over = status.is_over_budget();
            let bar_color = if over {
                theme.danger
            } else if status.used_fraction() >= 0.85 {
                theme.warning
            } else {
                theme.success
            };
            let detail = match status.remaining() {
                Some(r) if r.0 < 0 => format!("{} spent · {} over", status.spent, Money(-r.0)),
                Some(r) => format!("{} spent · {} left", status.spent, r),
                None => format!("{} spent", status.spent),
            };

            h_flex()
                .gap_4()
                .py_3()
                .items_center()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    v_flex()
                        .flex_1()
                        .gap_1p5()
                        .child(
                            h_flex()
                                .justify_between()
                                .child(
                                    div()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(status.category.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(if over {
                                            theme.danger
                                        } else {
                                            theme.muted_foreground
                                        })
                                        .child(detail),
                                ),
                        )
                        .child(
                            Progress::new(SharedString::from(format!(
                                "progress-{}",
                                status.category.id
                            )))
                            .value(status.used_fraction() * 100.0)
                            .color(bar_color),
                        ),
                )
                .children(self.inputs.get(&status.category.id).map(|input| {
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(div().text_color(theme.muted_foreground).child("$"))
                        .child(text_input(input).w(px(110.)))
                }))
        });

        v_flex()
            .id("budgets")
            .size_full()
            .p_6()
            .gap_2()
            .overflow_y_scrollbar()
            .child(
                h_flex()
                    .justify_between()
                    .pb_2()
                    .text_color(theme.muted_foreground)
                    .child("Set a monthly limit for each category. Limits carry forward to later months.")
                    .child(format!("{} of {} budgeted", Money(total_spent), Money(total_budget))),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_sm().text_color(theme.danger).child(error))
            })
            .children(rows)
    }
}
