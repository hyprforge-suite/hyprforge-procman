//! Apps as the desktop names and draws them: the launcher entry's
//! `Name=` and its icon, found through the configured icon theme.
//!
//! Loaded once, off the window's thread — reading every launcher entry
//! and asking gsettings for the theme are both I/O, and the first frame
//! should not wait for either. Until they arrive a row shows the window
//! class and no icon, which is what it showed before there were any.

use hyprforge_procman::app_history;
use iced::widget::{container, image, svg, Space};
use iced::{Element, Length};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// The launcher entries and the icon theme, once loaded.
#[derive(Debug, Clone)]
pub struct Desktop {
    pub db: Arc<hyprforge_mime::MimeDb>,
    pub icons: Arc<hyprforge_icons::Icons>,
}

pub async fn load() -> Desktop {
    let loaded = tokio::task::spawn_blocking(|| (hyprforge_mime::MimeDb::load(), hyprforge_icons::Icons::load())).await;
    let (db, icons) = loaded.unwrap_or_else(|_| (hyprforge_mime::MimeDb::default(), hyprforge_icons::Icons::load_from("hicolor", &[], &[])));
    Desktop { db: Arc::new(db), icons: Arc::new(icons) }
}

/// An icon ready to draw: a theme's SVG or PNG.
#[derive(Debug, Clone)]
pub enum Icon {
    Svg(svg::Handle),
    Raster(image::Handle),
}

/// Drawn at 24 logical pixels; asked for at twice that, since this
/// machine's output is scaled 1.6 and a 24px raster would be blurred.
pub const SIDE: u32 = 24;

/// The icon for `class`, if its entry names one any theme has. `Icon=`
/// may also be a path, which is drawn as it is.
pub fn find(desktop: &Desktop, class: &str) -> Option<Icon> {
    let name = app_history::entry(&desktop.db, class)?.icon.clone()?;
    named(desktop, &name)
}

/// The icon an entry's `Icon=` names, through the theme — or the file,
/// when it names a path.
pub fn named(desktop: &Desktop, name: &str) -> Option<Icon> {
    if name.is_empty() {
        return None;
    }
    let path = if Path::new(name).is_absolute() { Some(name.into()) } else { desktop.icons.lookup(&[name], SIDE, 2) }?;
    Some(match path.extension().and_then(|e| e.to_str()) {
        Some("svg" | "svgz") => Icon::Svg(svg::Handle::from_path(path)),
        _ => Icon::Raster(image::Handle::from_path(path)),
    })
}

/// Every class asked about, found or not — a miss is remembered too, so
/// an app with no icon is not looked up again every second.
#[derive(Debug, Default)]
pub struct Cache {
    found: HashMap<String, Option<Icon>>,
}

impl Cache {
    pub fn get(&mut self, desktop: &Desktop, class: &str) -> Option<Icon> {
        self.found.entry(class.to_string()).or_insert_with(|| find(desktop, class)).clone()
    }

    /// By the icon's own name, as an autostart entry gives it — kept
    /// apart from window classes, which are a different namespace.
    pub fn get_named(&mut self, desktop: &Desktop, name: &str) -> Option<Icon> {
        self.found.entry(format!("icon:{name}")).or_insert_with(|| named(desktop, name)).clone()
    }

    pub fn peek_named(&self, name: &str) -> Option<&Icon> {
        self.found.get(&format!("icon:{name}")).and_then(Option::as_ref)
    }

    pub fn peek(&self, class: &str) -> Option<&Icon> {
        self.found.get(class).and_then(Option::as_ref)
    }
}

/// An icon at `side`, or the same space empty, so names line up whether
/// or not an app has one.
pub fn view<'a, Message: 'a>(icon: Option<&Icon>, side: f32) -> Element<'a, Message> {
    let side = Length::Fixed(side);
    match icon {
        Some(Icon::Svg(h)) => svg(h.clone()).width(side).height(side).into(),
        Some(Icon::Raster(h)) => image(h.clone()).width(side).height(side).into(),
        None => container(Space::new()).width(side).height(side).into(),
    }
}
