mod db;
mod model;
mod state;
mod ui;

use anyhow::{Context as _, Result};
use gpui_kit::*;

use crate::db::Db;
use crate::state::AppState;
use crate::ui::AppView;

/// Opens `~/Library/Application Support/budget/budget.db` (or the platform
/// equivalent). Set `BUDGET_DB` to use a different file.
fn open_db() -> Result<Db> {
    if let Some(path) = std::env::var_os("BUDGET_DB") {
        return Db::open(std::path::Path::new(&path));
    }
    let dirs = directories::BaseDirs::new().context("could not locate the home directory")?;
    Db::open(&dirs.data_dir().join("budget").join("budget.db"))
}

fn main() -> Result<()> {
    let db = open_db()?;

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);

            let state = cx.new(|_| AppState::new(db));
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1040.), px(720.)),
                    cx,
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Budget".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| AppView::new(state, window, cx))
            })
            .expect("failed to open window");
            cx.activate(true);
        });
    Ok(())
}
