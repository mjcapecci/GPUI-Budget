use gpui_kit::component::button::*;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::categories::transactions_label;
use super::text_input;
use crate::model::{UNASSIGNED, validate_payment_method_name};
use crate::state::AppState;

/// Add and delete payment methods, such as credit cards.
///
/// Works like the Categories settings: deleting asks for confirmation, and its
/// transactions (in every month) lose their payment method until one with
/// the same name is added again.
pub struct PaymentMethodsView {
    state: Entity<AppState>,
    name: Entity<InputState>,
    form_error: Option<SharedString>,
    /// Confirmation shown after adding a payment method that restored transactions.
    notice: Option<SharedString>,
    /// The payment method whose Delete button was clicked and awaits confirmation.
    confirming: Option<i64>,
    _subscriptions: Vec<Subscription>,
}

impl PaymentMethodsView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| {
            InputState::new(window, cx).placeholder("New payment method, like \"Chase Visa\"")
        });
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
            form_error: None,
            notice: None,
            confirming: None,
            _subscriptions: subscriptions,
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.name.read(cx).value();
        match validate_payment_method_name(&text, &self.state.read(cx).payment_methods) {
            Ok(name) => {
                let unassigned =
                    |s: &AppState| s.payment_method_counts.get(&None).copied().unwrap_or(0);
                let before = unassigned(self.state.read(cx));
                self.state
                    .update(cx, |s, cx| s.add_payment_method(name.clone(), cx));
                let restored = before.saturating_sub(unassigned(self.state.read(cx)));
                self.notice = (restored > 0).then(|| {
                    format!("Moved {} back to \"{name}\".", transactions_label(restored)).into()
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
        v_flex()
            .gap_2()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Payment methods"),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(text_input(&self.name).flex_1())
                    .child(
                        Button::new("add-payment-method")
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
            .payment_methods
            .iter()
            .map(|method| {
                let id = method.id;
                let count = state
                    .payment_method_counts
                    .get(&Some(id))
                    .copied()
                    .unwrap_or(0);
                let confirming = self.confirming == Some(id);

                let actions = if confirming {
                    let prompt = if count == 0 {
                        format!("Delete \"{}\"?", method.name)
                    } else {
                        format!(
                            "Delete \"{}\" and move its {} to {UNASSIGNED}?",
                            method.name,
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
                                    this.state
                                        .update(cx, |s, cx| s.delete_payment_method(id, cx));
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
                        Button::new(SharedString::from(format!("delete-payment-method-{id}")))
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
                            .child(method.name.clone()),
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

        let empty = rows.is_empty();
        let unassigned = state.payment_method_counts.get(&None).copied().unwrap_or(0);
        v_flex()
            .id("payment-methods-list")
            .flex_1()
            .min_h_0()
            .border_t_1()
            .border_color(theme.border)
            .overflow_y_scrollbar()
            .children(rows)
            .when(empty, |this| {
                this.child(
                    div()
                        .p_6()
                        .text_center()
                        .text_color(theme.muted_foreground)
                        .child("Add the cards and accounts you pay with to track what each one owes."),
                )
            })
            .when(unassigned > 0, |this| {
                this.child(
                    div().p_3().text_sm().text_color(theme.muted_foreground).child(format!(
                        "{} {} from deleted payment methods. Re-add a deleted payment method with the \
                         same name to move its transactions back, or edit them on the Transactions page.",
                        transactions_label(unassigned),
                        if unassigned == 1 { "is unassigned" } else { "are unassigned" },
                    )),
                )
            })
    }
}

impl Render for PaymentMethodsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .p_6()
            .gap_4()
            .child(self.render_form(cx))
            .child(self.render_list(cx))
    }
}
