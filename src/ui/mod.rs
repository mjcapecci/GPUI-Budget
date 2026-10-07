mod app_view;
mod budgets;
mod categories;
mod payment_methods;
mod settings;
mod summary;
mod transactions;

pub use app_view::AppView;
pub use budgets::BudgetsView;
pub use categories::CategoriesView;
pub use payment_methods::PaymentMethodsView;
pub use settings::SettingsView;
pub use summary::SummaryView;
pub use transactions::TransactionsView;

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::base::ElementExt as _;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::*;

/// A text input that takes focus wherever its box is clicked.
///
/// `Input` only focuses when the click lands on the line of text itself, so
/// clicks on its padding are ignored. Size the returned wrapper, not the input.
pub(super) fn text_input(state: &Entity<InputState>) -> Div {
    let focus = state.clone();
    div()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            focus.update(cx, |input, cx| input.focus(window, cx));
        })
        .child(Input::new(state))
}

/// A small muted caption above a form field.
pub(super) fn field(label: &'static str, input: impl IntoElement, cx: &App) -> Div {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(input)
}

/// One line of text that ends in "…" when it doesn't fit, and only then shows
/// the full text in a tooltip on hover.
pub(super) fn truncated_text(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
) -> Stateful<Div> {
    let text: SharedString = text.into();
    // Measured each frame once the cell's width is known.
    let truncated = Rc::new(Cell::new(false));
    div()
        .id(id)
        .min_w_0()
        .truncate()
        .child(text.clone())
        .on_prepaint({
            let truncated = truncated.clone();
            let text = text.clone();
            move |bounds, window, _| {
                let style = window.text_style();
                let font_size = style.font_size.to_pixels(window.rem_size());
                let run = style.to_run(text.len());
                let line = window
                    .text_system()
                    .shape_line(text, font_size, &[run], None);
                truncated.set(line.width() > bounds.size.width);
            }
        })
        .tooltip(move |window, cx| {
            if truncated.get() {
                Tooltip::new(text.clone()).build(window, cx)
            } else {
                cx.new(|_| EmptyView).into()
            }
        })
}
