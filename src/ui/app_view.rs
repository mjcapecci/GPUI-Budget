use gpui_kit::component::button::*;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{BudgetsView, CategoriesView, PaymentMethodsView, SummaryView, TransactionsView};
use crate::state::AppState;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Transactions,
    Budgets,
    Summary,
    Categories,
    PaymentMethods,
}

impl Page {
    const ALL: [Page; 5] = [
        Page::Transactions,
        Page::Budgets,
        Page::Summary,
        Page::Categories,
        Page::PaymentMethods,
    ];

    fn label(self) -> &'static str {
        match self {
            Page::Transactions => "Transactions",
            Page::Budgets => "Budgets",
            Page::Summary => "Summary",
            Page::Categories => "Categories",
            Page::PaymentMethods => "Payment Methods",
        }
    }
}

/// Top-level window view: sidebar navigation, month switcher and the active page.
pub struct AppView {
    state: Entity<AppState>,
    page: Page,
    transactions: Entity<TransactionsView>,
    budgets: Entity<BudgetsView>,
    summary: Entity<SummaryView>,
    categories: Entity<CategoriesView>,
    payment_methods: Entity<PaymentMethodsView>,
}

impl AppView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        match state.read(cx).saved_theme_mode() {
            Some(mode) => Theme::change(mode, Some(window), cx),
            None => Theme::sync_system_appearance(Some(window), cx),
        }
        Self {
            transactions: cx.new(|cx| TransactionsView::new(state.clone(), window, cx)),
            budgets: cx.new(|cx| BudgetsView::new(state.clone(), window, cx)),
            summary: cx.new(|cx| SummaryView::new(state.clone(), cx)),
            categories: cx.new(|cx| CategoriesView::new(state.clone(), window, cx)),
            payment_methods: cx.new(|cx| PaymentMethodsView::new(state.clone(), window, cx)),
            state,
            page: Page::Transactions,
        }
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w(px(180.))
            .h_full()
            .p_3()
            .gap_1()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .px_2()
                    .pb_3()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Budget"),
            )
            .children(Page::ALL.map(|page| {
                // Button centers its label, so pass it as a full-width child instead.
                Button::new(page.label())
                    .accessibility_label(page.label())
                    .child(div().flex_1().min_w_0().truncate().child(page.label()))
                    .w_full()
                    .map(|b| {
                        if self.page == page {
                            b.secondary()
                        } else {
                            b.ghost()
                        }
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.page = page;
                        cx.notify();
                    }))
            }))
            .child(div().flex_1())
            .child(self.render_theme_toggle(cx))
    }

    fn render_theme_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = cx.theme().is_dark();
        let option = |mode: ThemeMode, label: &'static str| {
            Button::new(label)
                .label(label)
                .flex_1()
                .compact()
                .map(|b| {
                    if mode.is_dark() == dark {
                        b.secondary()
                    } else {
                        b.ghost()
                    }
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.state.update(cx, |s, cx| s.set_theme_mode(mode, cx));
                }))
        };
        h_flex()
            .gap_1()
            .child(option(ThemeMode::Light, "Light"))
            .child(option(ThemeMode::Dark, "Dark"))
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let month = self.state.read(cx).month;
        h_flex()
            .px_6()
            .py_3()
            .gap_3()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.page.label()),
            )
            .child(div().flex_1())
            .child(
                Button::new("prev-month")
                    .outline()
                    .label("‹")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state
                            .update(cx, |s, cx| s.set_month(s.month.prev(), cx));
                    })),
            )
            .child(
                div()
                    .min_w(px(140.))
                    .text_center()
                    .font_weight(FontWeight::MEDIUM)
                    .child(month.label()),
            )
            .child(
                Button::new("next-month")
                    .outline()
                    .label("›")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state
                            .update(cx, |s, cx| s.set_month(s.month.next(), cx));
                    })),
            )
    }
}

impl Render for AppView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let error = self.state.read(cx).error.clone();
        let page: AnyView = match self.page {
            Page::Transactions => self.transactions.clone().into(),
            Page::Budgets => self.budgets.clone().into(),
            Page::Summary => self.summary.clone().into(),
            Page::Categories => self.categories.clone().into(),
            Page::PaymentMethods => self.payment_methods.clone().into(),
        };

        h_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .child(self.render_header(cx))
                    .when_some(error, |this, error| {
                        this.child(
                            div()
                                .mx_6()
                                .mt_3()
                                .p_2()
                                .rounded_md()
                                .bg(cx.theme().danger.opacity(0.15))
                                .text_color(cx.theme().danger)
                                .child(format!("Database error: {error}")),
                        )
                    })
                    .child(div().flex_1().min_h_0().child(page)),
            )
    }
}
