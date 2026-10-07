use gpui_kit::component::button::*;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::model::{CategoryKind, UNASSIGNED, validate_category_name};
use crate::state::AppState;

/// Add and delete categories.
///
/// Deleting asks for confirmation first. Transactions in the deleted category
/// (in every month) become unassigned; the database remembers which category
/// they came from.
pub struct CategoriesView {
    state: Entity<AppState>,
    name: Entity<InputState>,
    kind: CategoryKind,
    form_error: Option<SharedString>,
    /// Confirmation shown after adding a category that restored transactions.
    notice: Option<SharedString>,
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
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("New category name"));
        let subscriptions = vec![
            cx.subscribe_in(&name, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit(window, cx);
                }
            }),
            cx.observe(&state, |_, _, cx| cx.notify()),
        ];
        Self {
            state,
            name,
            kind: CategoryKind::Expense,
            form_error: None,
            notice: None,
            confirming: None,
            _subscriptions: subscriptions,
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.name.read(cx).value();
        match validate_category_name(&text, &self.state.read(cx).categories) {
            Ok(name) => {
                let kind = self.kind;
                let unassigned =
                    |s: &AppState| s.transaction_counts.get(&None).copied().unwrap_or(0);
                let before = unassigned(self.state.read(cx));
                self.state
                    .update(cx, |s, cx| s.add_category(name.clone(), kind, cx));
                let restored = before.saturating_sub(unassigned(self.state.read(cx)));
                self.notice = (restored > 0).then(|| {
                    format!(
                        "Moved {} back into \"{name}\".",
                        transactions_label(restored)
                    )
                    .into()
                });
                self.form_error = None;
                self.name.update(cx, |i, cx| i.set_value("", window, cx));
            }
            Err(message) => {
                self.form_error = Some(message.into());
                self.notice = None;
            }
        }
        cx.notify();
    }

    fn render_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let kind_button = |kind: CategoryKind, label: &'static str| {
            Button::new(label)
                .label(label)
                .map(|b| {
                    if self.kind == kind {
                        b.secondary()
                    } else {
                        b.ghost()
                    }
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.kind = kind;
                    cx.notify();
                }))
        };
        v_flex()
            .gap_2()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .child(div().font_weight(FontWeight::MEDIUM).child("Add category"))
            .child(
                h_flex()
                    .gap_2()
                    .child(Input::new(&self.name).flex_1())
                    .child(kind_button(CategoryKind::Expense, "Expense"))
                    .child(kind_button(CategoryKind::Income, "Income"))
                    .child(
                        Button::new("add-category")
                            .primary()
                            .label("Add")
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
            .when_some(self.form_error.clone(), |this, error| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .when_some(self.notice.clone(), |this, notice| {
                this.child(div().text_sm().text_color(cx.theme().success).child(notice))
            })
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let theme = cx.theme();
        let rows: Vec<AnyElement> = state
            .categories
            .iter()
            .map(|category| {
                let id = category.id;
                let count = state
                    .transaction_counts
                    .get(&Some(id))
                    .copied()
                    .unwrap_or(0);
                let confirming = self.confirming == Some(id);
                let kind = match category.kind {
                    CategoryKind::Income => "Income",
                    CategoryKind::Expense => "Expense",
                };

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
                            .w(px(80.))
                            .text_color(theme.muted_foreground)
                            .child(kind),
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

        let unassigned = state.transaction_counts.get(&None).copied().unwrap_or(0);
        v_flex()
            .id("categories-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .children(rows)
            .when(unassigned > 0, |this| {
                this.child(
                    div().p_3().text_sm().text_color(theme.muted_foreground).child(format!(
                        "{} {} from deleted categories. Re-add a deleted category with the same name and \
                         kind to move its transactions back, or edit them on the Transactions page.",
                        transactions_label(unassigned),
                        if unassigned == 1 { "is unassigned" } else { "are unassigned" },
                    )),
                )
            })
    }
}

impl Render for CategoriesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .p_6()
            .gap_4()
            .child(self.render_form(cx))
            .child(self.render_list(cx))
    }
}
