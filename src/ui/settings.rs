use gpui_kit::component::button::*;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::tab::*;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{CategoriesView, PaymentMethodsView, field};
use crate::import::API_KEY_ENV;
use crate::state::AppState;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Categories,
    PaymentMethods,
    Import,
    Appearance,
}

impl Section {
    const ALL: [Section; 4] = [
        Section::Categories,
        Section::PaymentMethods,
        Section::Import,
        Section::Appearance,
    ];

    fn label(self) -> &'static str {
        match self {
            Section::Categories => "Categories",
            Section::PaymentMethods => "Payment methods",
            Section::Import => "Import",
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
    api_key: Entity<InputState>,
    /// Confirms the last save of the API key.
    api_key_notice: Option<SharedString>,
}

impl SettingsView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            categories: cx.new(|cx| CategoriesView::new(state.clone(), window, cx)),
            payment_methods: cx.new(|cx| PaymentMethodsView::new(state.clone(), window, cx)),
            api_key: cx.new(|cx| {
                let mut input = InputState::new(window, cx)
                    .placeholder("sk-ant-…")
                    .masked(true);
                if let Some(key) = state.read(cx).saved_api_key() {
                    input.set_value(key, window, cx);
                }
                input
            }),
            api_key_notice: None,
            state,
            section: Section::Categories,
        }
    }

    fn save_api_key(&mut self, cx: &mut Context<Self>) {
        let key = self.api_key.read(cx).value().trim().to_string();
        self.state.update(cx, |s, cx| s.set_api_key(&key, cx));
        self.api_key_notice = Some(if key.is_empty() {
            "API key removed.".into()
        } else {
            "API key saved.".into()
        });
        cx.notify();
    }

    fn render_import(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let env_note = (self.state.read(cx).saved_api_key().is_none()
            && std::env::var(API_KEY_ENV).is_ok_and(|k| !k.trim().is_empty()))
        .then(|| format!("Using the key from the {API_KEY_ENV} environment variable."));
        v_flex()
            .p_6()
            .gap_3()
            .max_w(px(560.))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Anthropic API key"),
            )
            .child(div().text_sm().text_color(muted).child(
                "Importing reads screenshots of transactions with Claude, so each \
                 screenshot is sent to Anthropic's API. Create a key at \
                 console.anthropic.com. It is saved in this app's database on this computer.",
            ))
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .child(field("API key", Input::new(&self.api_key).mask_toggle(), cx).flex_1())
                    .child(
                        Button::new("save-api-key")
                            .primary()
                            .label("Save")
                            .on_click(cx.listener(|this, _, _, cx| this.save_api_key(cx))),
                    ),
            )
            .children(env_note.map(|note| div().text_sm().text_color(muted).child(note)))
            .children(
                self.api_key_notice
                    .clone()
                    .map(|notice| div().text_sm().text_color(cx.theme().success).child(notice)),
            )
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
            Section::Import => self.render_import(cx).into_any_element(),
            Section::Appearance => self.render_appearance(cx).into_any_element(),
        };

        v_flex()
            .size_full()
            .child(tabs)
            .child(div().flex_1().min_h_0().child(body))
    }
}
