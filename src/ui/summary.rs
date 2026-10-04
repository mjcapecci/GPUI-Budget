use gpui_kit::component::chart::*;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::model::{Money, UNASSIGNED};
use crate::state::AppState;

#[derive(Clone)]
struct Bar {
    name: SharedString,
    dollars: f64,
    over: bool,
}

/// Income, expenses and net for the month plus spending by category and by
/// payment method.
pub struct SummaryView {
    state: Entity<AppState>,
}

impl SummaryView {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self { state }
    }
}

fn stat_card(label: &'static str, value: Money, color: Hsla, cx: &App) -> impl IntoElement {
    v_flex()
        .flex_1()
        .gap_1()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(
            div()
                .text_2xl()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(color)
                .child(value.to_string()),
        )
}

/// One line of the payment method table. `muted` marks the catch-all rows.
fn payment_row(name: SharedString, spent: Money, muted: bool, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    h_flex()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .when(muted, |this| {
                    this.italic().text_color(theme.muted_foreground)
                })
                .child(name),
        )
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .when(spent.0 == 0, |this| this.text_color(theme.muted_foreground))
                .child(spent.to_string()),
        )
}

impl Render for SummaryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let theme = cx.theme();
        let summary = state.summary.clone();
        let net = summary.net();

        let spending = &state.payment_method_spending;
        let mut payment_rows: Vec<AnyElement> = spending
            .by_method
            .iter()
            .map(|(p, spent)| {
                payment_row(p.name.clone().into(), *spent, false, cx).into_any_element()
            })
            .collect();
        if spending.unassigned.0 > 0 {
            payment_rows.push(
                payment_row(UNASSIGNED.into(), spending.unassigned, true, cx).into_any_element(),
            );
        }
        if spending.unspecified.0 > 0 && !payment_rows.is_empty() {
            payment_rows.push(
                payment_row("No payment method".into(), spending.unspecified, true, cx)
                    .into_any_element(),
            );
        }

        let mut bars: Vec<Bar> = state
            .statuses
            .iter()
            .filter(|s| s.spent.0 > 0)
            .map(|s| Bar {
                name: s.category.name.clone().into(),
                dollars: s.spent.0 as f64 / 100.0,
                over: s.is_over_budget(),
            })
            .collect();
        if summary.unassigned_expenses.0 > 0 {
            bars.push(Bar {
                name: UNASSIGNED.into(),
                dollars: summary.unassigned_expenses.0 as f64 / 100.0,
                over: false,
            });
        }
        bars.sort_by(|a, b| b.dollars.total_cmp(&a.dollars));

        let (normal, danger) = (theme.chart_2, theme.danger);
        let chart: AnyElement = if bars.is_empty() {
            div()
                .p_6()
                .text_center()
                .text_color(theme.muted_foreground)
                .child("No spending recorded this month.")
                .into_any_element()
        } else {
            div()
                .h(px(320.))
                .child(
                    BarChart::new(bars)
                        .band(|b: &Bar| b.name.clone())
                        .value(|b: &Bar| b.dollars)
                        .fill(move |b: &Bar, _, _, _| if b.over { danger } else { normal })
                        .label(|b: &Bar| Money((b.dollars * 100.0).round() as i64).to_string())
                        .tooltip_value(|_, v| Money((v * 100.0).round() as i64).to_string().into())
                        .corner_radii(px(4.)),
                )
                .into_any_element()
        };

        v_flex()
            .id("summary")
            .size_full()
            .p_6()
            .gap_6()
            .overflow_y_scrollbar()
            .child(
                h_flex()
                    .gap_4()
                    .child(stat_card("Income", summary.income, theme.success, cx))
                    .child(stat_card(
                        "Expenses",
                        summary.expenses,
                        theme.foreground,
                        cx,
                    ))
                    .child(stat_card(
                        "Net",
                        net,
                        if net.0 < 0 {
                            theme.danger
                        } else {
                            theme.success
                        },
                        cx,
                    )),
            )
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Spending by category"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child("Red bars are over budget"),
                            ),
                    )
                    .child(chart),
            )
            .when(!payment_rows.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .child("Spending by payment method"),
                        )
                        .child(
                            v_flex()
                                .rounded_lg()
                                .border_1()
                                .border_color(theme.border)
                                .children(payment_rows),
                        ),
                )
            })
    }
}
