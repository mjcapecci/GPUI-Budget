use gpui_kit::component::button::*;
use gpui_kit::component::tab::*;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{CategoriesView, PaymentMethodsView};
use crate::state::AppState;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Categories,
    PaymentMethods,
    Appearance,
}

impl Section {
    const ALL: [Section; 3] = [
        Section::Categories,
        Section::PaymentMethods,
        Section::Appearance,
    ];

    fn label(self) -> &'static str {
        match self {
            Section::Categories => "Categories",
            Section::PaymentMethods => "Payment methods",
            Section::Appearance => "Appearance",
        }
    }
}

/// Everything that configures the app rather than recording money, one tab
/// per section. Add new sections to [`Section`].
pub struct SettingsView {
    state: Entity<AppState>,
    section: Section,
    categories: Entity<CategoriesView>,
    payment_methods: Entity<PaymentMethodsView>,
}

impl SettingsView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            categories: cx.new(|cx| CategoriesView::new(state.clone(), window, cx)),
            payment_methods: cx.new(|cx| PaymentMethodsView::new(state.clone(), window, cx)),
            state,
            section: Section::Categories,
        }
    }

    fn render_appearance(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = cx.theme().is_dark();
        let option = |mode: ThemeMode, label: &'static str| {
            Button::new(label)
                .label(label)
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
        v_flex()
            .p_6()
            .gap_2()
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Theme"))
            .child(
                h_flex()
                    .gap_1()
                    .child(option(ThemeMode::Light, "Light"))
                    .child(option(ThemeMode::Dark, "Dark")),
            )
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = Section::ALL
            .iter()
            .position(|s| *s == self.section)
            .unwrap_or(0);
        let view = cx.entity().downgrade();
        let tabs = TabBar::new("settings-tabs")
            .underline()
            .px_6()
            .selected_index(selected)
            .children(Section::ALL.map(|s| s.label()))
            .on_click(move |ix, _, cx| {
                if let Some(section) = Section::ALL.get(*ix).copied() {
                    view.update(cx, |this, cx| {
                        this.section = section;
                        cx.notify();
                    })
                    .ok();
                }
            });

        let body = match self.section {
            Section::Categories => self.categories.clone().into_any_element(),
            Section::PaymentMethods => self.payment_methods.clone().into_any_element(),
            Section::Appearance => self.render_appearance(cx).into_any_element(),
        };

        v_flex()
            .size_full()
            .child(tabs)
            .child(div().flex_1().min_h_0().child(body))
    }
}
