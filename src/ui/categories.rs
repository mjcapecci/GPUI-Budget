use gpui_kit::component::button::*;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::text_input;
use crate::model::{CategoryKind, UNASSIGNED, validate_category_name};
use crate::state::AppState;

/// Add and delete categories, listed in separate Expense and Income sections
/// that each have their own add box.
///
/// Deleting asks for confirmation first. Transactions in the deleted category
/// (in every month) become unassigned; the database remembers which category
/// they came from.
pub struct CategoriesView {
    state: Entity<AppState>,
    expense_name: Entity<InputState>,
    income_name: Entity<InputState>,
    /// A validation error from the section it was added in.
    form_error: Option<(CategoryKind, SharedString)>,
    /// Confirmation shown after adding a category that restored transactions.
    notice: Option<(CategoryKind, SharedString)>,
    /// The category whose Delete button was clicked and awaits confirmation.
    confirming: Option<i64>,
    _subscriptions: Vec<Subscription>,
}

pub(super) fn transactions_label(count: usize) -> String {
    match count {
        1 => "1 transaction".into(),
        n => format!("{n} transactions"),
    }
}

impl CategoriesView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let expense_name = cx.new(|cx| {
            InputState::new(window, cx).placeholder("New expense category, like \"Groceries\"")
        });
        let income_name = cx.new(|cx| {
            InputState::new(window, cx).placeholder("New income category, like \"Salary\"")
        });
        let mut subscriptions = vec![cx.observe(&state, |_, _, cx| cx.notify())];
        for (input, kind) in [
            (&expense_name, CategoryKind::Expense),
            (&income_name, CategoryKind::Income),
        ] {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                move |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.submit(kind, window, cx);
                    }
                },
            ));
        }
        Self {
            state,
            expense_name,
            income_name,
            form_error: None,
            notice: None,
            confirming: None,
            _subscriptions: subscriptions,
        }
    }

    fn name_input(&self, kind: CategoryKind) -> &Entity<InputState> {
        match kind {
            CategoryKind::Expense => &self.expense_name,
            CategoryKind::Income => &self.income_name,
        }
    }

    fn submit(&mut self, kind: CategoryKind, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.name_input(kind).clone();
        let text = input.read(cx).value();
        match validate_category_name(&text, &self.state.read(cx).categories) {
            Ok(name) => {
                let unassigned =
                    |s: &AppState| s.transaction_counts.get(&None).copied().unwrap_or(0);
                let before = unassigned(self.state.read(cx));
                self.state
                    .update(cx, |s, cx| s.add_category(name.clone(), kind, cx));
                let restored = before.saturating_sub(unassigned(self.state.read(cx)));
                self.notice = (restored > 0).then(|| {
                    let message = format!(
                        "Moved {} back into \"{name}\".",
                        transactions_label(restored)
                    );
                    (kind, message.into())
                });
                self.form_error = None;
                input.update(cx, |i, cx| i.set_value("", window, cx));
            }
            Err(message) => {
                self.form_error = Some((kind, message.into()));
                self.notice = None;
            }
        }
        cx.notify();
    }

    /// The add box at the top of a section.
    fn render_add(&self, kind: CategoryKind, cx: &mut Context<Self>) -> impl IntoElement {
        let message = |m: &Option<(CategoryKind, SharedString)>| {
            m.as_ref()
                .filter(|(k, _)| *k == kind)
                .map(|(_, m)| m.clone())
        };
        let id = match kind {
            CategoryKind::Expense => "add-expense-category",
            CategoryKind::Income => "add-income-category",
        };
        v_flex()
            .gap_1()
            .pb_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(text_input(self.name_input(kind)).flex_1())
                    .child(Button::new(id).primary().label("Add").on_click(
                        cx.listener(move |this, _, window, cx| this.submit(kind, window, cx)),
                    )),
            )
            .when_some(message(&self.form_error), |this, error| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .when_some(message(&self.notice), |this, notice| {
                this.child(div().text_sm().text_color(cx.theme().success).child(notice))
            })
    }

    /// A heading, add box and the categories of one kind.
    fn render_section(&self, kind: CategoryKind, cx: &mut Context<Self>) -> impl IntoElement {
        let add = self.render_add(kind, cx).into_any_element();
        let state = self.state.read(cx);
        let theme = cx.theme();
        let rows: Vec<AnyElement> = state
            .categories
            .iter()
            .filter(|c| c.kind == kind)
            .map(|category| {
                let id = category.id;
                let count = state
                    .transaction_counts
                    .get(&Some(id))
                    .copied()
                    .unwrap_or(0);
                let confirming = self.confirming == Some(id);
                let actions = if confirming {
                    let prompt = if count == 0 {
                        format!("Delete \"{}\"?", category.name)
                    } else {
                        format!(
                            "Delete \"{}\" and move its {} to {UNASSIGNED}?",
                            category.name,
                            transactions_label(count)
                        )
                    };
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().text_sm().text_color(theme.danger).child(prompt))
                        .child(
                            Button::new(SharedString::from(format!("confirm-delete-{id}")))
                                .danger()
                                .compact()
                                .label("Delete")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.confirming = None;
                                    this.state.update(cx, |s, cx| s.delete_category(id, cx));
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("cancel-delete-{id}")))
                                .ghost()
                                .compact()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirming = None;
                                    cx.notify();
                                })),
                        )
                } else {
                    h_flex().child(
                        Button::new(SharedString::from(format!("delete-category-{id}")))
                            .ghost()
                            .compact()
                            .label("Delete")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.confirming = Some(id);
                                cx.notify();
                            })),
                    )
                };

                h_flex()
                    .gap_4()
                    .px_3()
                    .py_2()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .when(confirming, |this| this.bg(theme.list_active))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(category.name.clone()),
                    )
                    .child(
                        div()
                            .w(px(120.))
                            .text_right()
                            .text_color(theme.muted_foreground)
                            .child(transactions_label(count)),
                    )
                    .child(actions)
                    .into_any_element()
            })
            .collect();

        let (title, empty) = match kind {
            CategoryKind::Expense => ("Expense categories", "No expense categories yet."),
            CategoryKind::Income => ("Income categories", "No income categories yet."),
        };
        let none = rows.is_empty();
        v_flex()
            .gap_2()
            .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
            .child(add)
            .child(
                v_flex()
                    .border_t_1()
                    .border_color(theme.border)
                    .children(rows)
                    .when(none, |this| {
                        this.child(
                            div()
                                .p_3()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(empty),
                        )
                    }),
            )
    }
}

impl Render for CategoriesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let unassigned = self
            .state
            .read(cx)
            .transaction_counts
            .get(&None)
            .copied()
            .unwrap_or(0);
        v_flex()
            .id("categories")
            .size_full()
            .p_6()
            .gap_8()
            .overflow_y_scrollbar()
            .child(self.render_section(CategoryKind::Expense, cx))
            .child(self.render_section(CategoryKind::Income, cx))
            .when(unassigned > 0, |this| {
                this.child(
                    div().text_sm().text_color(cx.theme().muted_foreground).child(format!(
                        "{} {} from deleted categories. Re-add a deleted category with the same name and \
                         kind to move its transactions back, or edit them on the Transactions page.",
                        transactions_label(unassigned),
                        if unassigned == 1 { "is unassigned" } else { "are unassigned" },
                    )),
                )
            })
    }
}
