use std::path::PathBuf;

use anyhow::{Context as _, Result};
use chrono::Local;
use gpui_kit::component::button::*;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::date_picker::{DatePicker, DatePickerState};
use gpui_kit::component::input::InputState;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::*;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::transactions::{OptionItem, category_items, payment_method_items, set_options};
use super::{field, text_input};
use crate::import::{self, Extracted};
use crate::model::{Category, CategoryKind, Money, PaymentMethod, TransactionInput};
use crate::state::AppState;

actions!(import, [PasteScreenshot]);

const CONTEXT: &str = "ImportView";

/// ⌘V on the Import page adds the screenshot on the clipboard. Text inputs
/// on the page keep their own paste.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "secondary-v",
        PasteScreenshot,
        Some(CONTEXT),
    )]);
}

/// A screenshot being read, or one that couldn't be.
struct Job {
    id: usize,
    name: SharedString,
    /// `None` while reading.
    error: Option<SharedString>,
    _task: Option<Task<()>>,
}

/// One transaction found in a screenshot, editable before it is imported.
struct Row {
    id: usize,
    include: bool,
    date: Entity<DatePickerState>,
    amount: Entity<InputState>,
    note: Entity<InputState>,
    category: Entity<SelectState<SearchableVec<OptionItem>>>,
    pending: bool,
    credit: bool,
    /// Why this transaction may already be recorded.
    duplicate: Option<SharedString>,
    /// Set when it was checked but couldn't be imported as entered.
    invalid: bool,
}

impl Row {
    fn read(&self, cx: &App) -> Option<(chrono::NaiveDate, Money, String)> {
        let date = self.date.read(cx).date().start()?;
        let amount = Money::parse(&self.amount.read(cx).value()).filter(|m| m.0 > 0)?;
        let note = self.note.read(cx).value().trim().to_string();
        Some((date, amount, note))
    }
}

/// Add transactions by reading screenshots of them with Claude, then
/// reviewing what was found before importing it.
pub struct ImportView {
    state: Entity<AppState>,
    focus_handle: FocusHandle,
    /// Applied to every expense in the import.
    payment_method: Entity<SelectState<SearchableVec<OptionItem>>>,
    jobs: Vec<Job>,
    rows: Vec<Row>,
    next_id: usize,
    /// The result of the last import.
    notice: Option<SharedString>,
    form_error: Option<SharedString>,
    /// The categories the dropdowns were last built from.
    categories: Vec<Category>,
    /// The payment methods the dropdown was last built from.
    payment_methods: Vec<PaymentMethod>,
    _subscriptions: Vec<Subscription>,
}

impl ImportView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let categories = state.read(cx).categories.clone();
        let payment_methods = state.read(cx).payment_methods.clone();
        let payment_method = cx.new(|cx| {
            SelectState::new(payment_method_items(&payment_methods), None, window, cx)
                .searchable(true)
        });

        let subscriptions = vec![cx.observe_in(&state, window, |this, state, window, cx| {
            if state.read(cx).categories != this.categories {
                this.categories = state.read(cx).categories.clone();
                for row in &this.rows {
                    set_options(&row.category, category_items(&this.categories), window, cx);
                }
            }
            if state.read(cx).payment_methods != this.payment_methods {
                this.payment_methods = state.read(cx).payment_methods.clone();
                let items = payment_method_items(&this.payment_methods);
                set_options(&this.payment_method, items, window, cx);
            }
            cx.notify();
        })];

        Self {
            state,
            focus_handle: cx.focus_handle(),
            payment_method,
            jobs: Vec::new(),
            rows: Vec::new(),
            next_id: 0,
            notice: None,
            form_error: None,
            categories,
            payment_methods,
            _subscriptions: subscriptions,
        }
    }

    /// Focus the page so ⌘V pastes a screenshot.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus_handle, cx);
    }

    fn next_id(&mut self) -> usize {
        self.next_id += 1;
        self.next_id
    }

    fn paste(&mut self, _: &PasteScreenshot, window: &mut Window, cx: &mut Context<Self>) {
        self.paste_from_clipboard(window, cx);
    }

    fn paste_from_clipboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = cx
            .read_from_clipboard()
            .map(|item| item.entries().to_vec())
            .unwrap_or_default();
        let mut found = false;
        for entry in entries {
            match entry {
                ClipboardEntry::Image(image) => {
                    found = true;
                    let bytes = image.bytes;
                    self.read_screenshot("Pasted screenshot".into(), window, cx, move || Ok(bytes));
                    // The clipboard can hold the same image in several formats.
                    break;
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    found = true;
                    self.read_files(paths.paths().to_vec(), window, cx);
                }
                ClipboardEntry::String(_) => {}
            }
        }
        if !found {
            self.notice = None;
            self.form_error = Some("The clipboard doesn't contain an image.".into());
            cx.notify();
        }
    }

    fn choose_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Import".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                this.update_in(cx, |this, window, cx| this.read_files(paths, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn read_files(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        for path in paths {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            self.read_screenshot(name.into(), window, cx, move || {
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))
            });
        }
    }

    /// Load a screenshot and have Claude read it in the background, adding
    /// the transactions found to the review list.
    fn read_screenshot(
        &mut self,
        name: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
        load: impl FnOnce() -> Result<Vec<u8>> + Send + 'static,
    ) {
        let id = self.next_id();
        self.notice = None;
        self.form_error = None;
        let Some(api_key) = self.state.read(cx).api_key() else {
            self.jobs.push(Job {
                id,
                name,
                error: Some(
                    "Add your Anthropic API key in Settings › Import to read screenshots.".into(),
                ),
                _task: None,
            });
            cx.notify();
            return;
        };
        let categories = self.state.read(cx).categories.clone();
        let today = Local::now().date_naive();
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { import::extract(&api_key, &load()?, &categories, today) },
                )
                .await;
            this.update_in(cx, |this, window, cx| {
                this.finish_reading(id, result, window, cx)
            })
            .ok();
        });
        self.jobs.push(Job {
            id,
            name,
            error: None,
            _task: Some(task),
        });
        cx.notify();
    }

    fn finish_reading(
        &mut self,
        id: usize,
        result: Result<Vec<Extracted>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) else {
            return;
        };
        job._task = None;
        match result {
            Ok(found) if found.is_empty() => {
                job.error = Some("No transactions found in this image.".into());
            }
            Ok(found) => {
                self.jobs.retain(|j| j.id != id);
                for extracted in found {
                    let row = self.new_row(extracted, window, cx);
                    self.rows.push(row);
                }
            }
            Err(e) => job.error = Some(format!("{e:#}").into()),
        }
        cx.notify();
    }

    fn new_row(&mut self, e: Extracted, window: &mut Window, cx: &mut Context<Self>) -> Row {
        let state = self.state.read(cx);
        // A category picked for this merchant before wins over Claude's
        // guess, as long as it fits the direction the money moved.
        let wanted = if e.credit {
            CategoryKind::Income
        } else {
            CategoryKind::Expense
        };
        let category_id = state
            .merchant_category(&e.merchant)
            .filter(|id| state.category(*id).is_some_and(|c| c.kind == wanted))
            .or(e.category_id);
        let is_income = category_id
            .and_then(|id| state.category(id))
            .is_some_and(|c| c.kind == CategoryKind::Income);

        let duplicate = match state.similar_transaction(e.date, e.amount) {
            Some(t) => {
                let category = t
                    .category_id
                    .and_then(|id| state.category(id))
                    .map(|c| c.name.as_str())
                    .unwrap_or(crate::model::UNASSIGNED);
                let mut text = format!(
                    "Possibly already added: {} · {} · {category}",
                    t.date.format("%b %-d"),
                    t.amount
                );
                if !t.note.is_empty() {
                    text.push_str(&format!(" · {}", t.note));
                }
                Some(text.into())
            }
            None => {
                let merchant = e.merchant.to_lowercase();
                self.rows
                    .iter()
                    .any(|row| {
                        row.read(cx).is_some_and(|(date, amount, note)| {
                            date == e.date && amount == e.amount && note.to_lowercase() == merchant
                        })
                    })
                    .then(|| "Also found in another screenshot".into())
            }
        };

        let date = cx.new(|cx| {
            let mut picker = DatePickerState::new(window, cx).date_format("%b %-d, %Y");
            picker.set_date(e.date, window, cx);
            picker
        });
        let amount = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("0.00");
            input.set_value(e.amount.to_plain(), window, cx);
            input
        });
        let note = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Note");
            input.set_value(e.merchant.clone(), window, cx);
            input
        });
        let categories = self.categories.clone();
        let category = cx.new(|cx| {
            let mut select =
                SelectState::new(category_items(&categories), None, window, cx).searchable(true);
            if let Some(id) = category_id {
                select.set_selected_value(&id, window, cx);
            }
            select
        });

        Row {
            id: self.next_id(),
            // Money in is usually a refund or card payment, which this app
            // doesn't track, unless it was filed as income.
            include: duplicate.is_none() && (!e.credit || is_income),
            date,
            amount,
            note,
            category,
            pending: e.pending,
            credit: e.credit,
            duplicate,
            invalid: false,
        }
    }

    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let payment_method_id = self.payment_method.read(cx).selected_value().copied();
        let mut inputs = Vec::new();
        for row in &mut self.rows {
            row.invalid = false;
            if !row.include {
                continue;
            }
            let category_id = row.category.read(cx).selected_value().copied();
            match (row.read(cx), category_id) {
                (Some((date, amount, note)), Some(category_id)) => inputs.push(TransactionInput {
                    date,
                    amount,
                    category_id,
                    payment_method_id,
                    note,
                }),
                _ => row.invalid = true,
            }
        }
        self.notice = None;
        if self.rows.iter().any(|r| r.invalid) {
            self.form_error = Some(
                "Each selected transaction needs a date, a category and a positive amount.".into(),
            );
        } else if inputs.is_empty() {
            self.form_error = Some("Select at least one transaction to import.".into());
        } else {
            let count = inputs.len();
            if self
                .state
                .update(cx, |s, cx| s.import_transactions(inputs, cx))
            {
                self.rows.clear();
                self.form_error = None;
                self.notice = Some(match count {
                    1 => "Imported 1 transaction.".into(),
                    n => format!("Imported {n} transactions.").into(),
                });
                self.focus(window, cx);
            }
        }
        cx.notify();
    }

    fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rows.clear();
        self.jobs.retain(|j| j.error.is_none());
        self.form_error = None;
        self.notice = None;
        self.focus(window, cx);
        cx.notify();
    }

    fn render_drop_zone(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let has_key = self.state.read(cx).api_key().is_some();
        v_flex()
            .id("drop-zone")
            .gap_3()
            .p_6()
            .items_center()
            .rounded_lg()
            .border_2()
            .border_dashed()
            .border_color(theme.border)
            .drag_over::<ExternalPaths>(|style, _, _, cx| {
                style
                    .border_color(cx.theme().primary)
                    .bg(cx.theme().primary.opacity(0.06))
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.read_files(paths.paths().to_vec(), window, cx);
                this.focus(window, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus(window, cx)),
            )
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Drop screenshots of transactions here, or paste one with ⌘V"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(if has_key {
                        "Screenshots are sent to Anthropic's API to be read. Nothing is \
                         added until you review it below."
                    } else {
                        "Add your Anthropic API key in Settings › Import to read screenshots."
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("paste")
                            .outline()
                            .label("Paste screenshot")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.paste_from_clipboard(window, cx)
                            })),
                    )
                    .child(
                        Button::new("choose")
                            .outline()
                            .label("Choose files…")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.choose_files(window, cx)),
                            ),
                    ),
            )
    }

    fn render_jobs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex().gap_1().children(self.jobs.iter().map(|job| {
            let id = job.id;
            h_flex()
                .gap_2()
                .items_center()
                .text_sm()
                .map(|this| match &job.error {
                    None => this
                        .text_color(theme.muted_foreground)
                        .child(Spinner::new())
                        .child(format!("Reading {}…", job.name)),
                    Some(error) => this
                        .text_color(theme.danger)
                        .child(div().flex_1().child(format!("{}: {error}", job.name)))
                        .child(
                            Button::new(SharedString::from(format!("dismiss-{id}")))
                                .ghost()
                                .compact()
                                .label("Dismiss")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.jobs.retain(|j| j.id != id);
                                    cx.notify();
                                })),
                        ),
                })
        }))
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_3()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .text_color(cx.theme().muted_foreground)
            .child(div().w(px(20.)))
            .child(div().w(px(170.)).child("Date"))
            .child(div().flex_1().child("Note"))
            .child(div().w(px(190.)).child("Category"))
            .child(div().w(px(110.)).child("Amount"))
    }

    fn render_row(&self, row: &Row, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let id = row.id;
        let mut flags: Vec<SharedString> = Vec::new();
        if row.pending {
            flags.push("Pending".into());
        }
        if row.credit {
            flags.push("Money in".into());
        }
        v_flex()
            .gap_1()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .when(row.invalid, |this| this.bg(theme.danger.opacity(0.08)))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        Checkbox::new(SharedString::from(format!("include-{id}")))
                            .checked(row.include)
                            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                if let Some(row) = this.rows.iter_mut().find(|r| r.id == id) {
                                    row.include = *checked;
                                    row.invalid = false;
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .gap_3()
                            .items_center()
                            .when(!row.include, |this| this.opacity(0.5))
                            .child(div().w(px(170.)).child(DatePicker::new(&row.date)))
                            .child(text_input(&row.note).flex_1().min_w_0())
                            .child(
                                div().w(px(190.)).child(
                                    Select::new(&row.category)
                                        .placeholder("Choose a category")
                                        .search_placeholder("Search categories"),
                                ),
                            )
                            .child(text_input(&row.amount).w(px(110.))),
                    ),
            )
            .when(!flags.is_empty() || row.duplicate.is_some(), |this| {
                this.child(
                    h_flex()
                        .pl(px(32.))
                        .gap_3()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .children(flags)
                        .children(
                            row.duplicate
                                .clone()
                                .map(|d| div().text_color(theme.warning).child(d)),
                        ),
                )
            })
    }

    fn render_review(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.rows.iter().filter(|r| r.include).count();
        let rows: Vec<AnyElement> = self
            .rows
            .iter()
            .map(|row| self.render_row(row, cx).into_any_element())
            .collect();
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_3()
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_header(cx))
                    .child(
                        v_flex()
                            .id("import-rows")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scrollbar()
                            .children(rows),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_end()
                    .child(
                        field(
                            "Paid with (expenses)",
                            Select::new(&self.payment_method)
                                .placeholder("None")
                                .search_placeholder("Search payment methods")
                                .cleanable(true),
                            cx,
                        )
                        .w(px(220.)),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{selected} of {} selected", self.rows.len())),
                    )
                    .child(
                        Button::new("clear")
                            .ghost()
                            .label("Clear")
                            .on_click(cx.listener(|this, _, window, cx| this.clear(window, cx))),
                    )
                    .child(
                        Button::new("import")
                            .primary()
                            .label(match selected {
                                1 => "Import 1 transaction".to_string(),
                                n => format!("Import {n} transactions"),
                            })
                            .disabled(selected == 0)
                            .on_click(cx.listener(|this, _, window, cx| this.import(window, cx))),
                    ),
            )
    }
}

impl Render for ImportView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .p_6()
            .gap_4()
            .track_focus(&self.focus_handle)
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::paste))
            .child(self.render_drop_zone(cx))
            .when(!self.jobs.is_empty(), |this| {
                this.child(self.render_jobs(cx))
            })
            .when_some(self.form_error.clone(), |this, error| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .when_some(self.notice.clone(), |this, notice| {
                this.child(div().text_sm().text_color(cx.theme().success).child(notice))
            })
            .when(!self.rows.is_empty(), |this| {
                this.child(self.render_review(cx))
            })
    }
}
