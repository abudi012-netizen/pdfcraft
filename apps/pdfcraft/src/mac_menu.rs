//! The menu bar at the top of the screen on macOS (PdfCraft, File, Edit, View, Pages, Window,
//! Help), as every Mac app has. The in-window Menu button and mode tabs stay as they are.
//!
//! Items come from the engine's command registry, so the menu bar lists the same commands as the
//! in-window menus and runs them the same way: a click becomes [`OsEvent::Command`]. Items carry
//! no keyboard shortcuts (bar Quit, Hide and Minimize): the window already handles every
//! shortcut, and a menu shortcut would take ⌘C or ⌘V away from a text field.

use std::sync::mpsc;

use muda::accelerator::Accelerator;
use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use pdfcraft_engine::commands::COMMANDS;
use pdfcraft_ui_egui::{OsEvent, OsEventsFn};

/// Menu ids that are not registry commands.
const QUIT: &str = "pdfcraft.menu.quit";
const FULL_SCREEN: &str = "view.full_screen";

/// Builds the menu bar, makes it the app's, and returns the poll that hands its clicks to the app.
/// Call on the main thread once the app has started.
pub fn install(ctx: &egui::Context) -> OsEventsFn {
    let (tx, rx) = mpsc::channel::<String>();
    let wake = ctx.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        // The app is gone if the receiver is: nothing to tell.
        let _ = tx.send(event.id.0);
        wake.request_repaint();
    }));
    let menu = build();
    if let Some(menu) = &menu {
        menu.init_for_nsapp();
    }
    Box::new(move || {
        // Owning the menu here keeps it alive as long as the app polls it.
        let _keep = &menu;
        rx.try_iter().map(|id| if id == QUIT { OsEvent::Quit } else { OsEvent::Command(id) }).collect()
    })
}

fn build() -> Option<Menu> {
    let accel = |s: &str| s.parse::<Accelerator>().ok();
    let menu = Menu::new();

    let app = Submenu::new("PdfCraft", true);
    let about = MenuItem::with_id("help.about", "About PdfCraft", true, None);
    let prefs = MenuItem::with_id("app.preferences", "Settings…", true, None);
    let quit = MenuItem::with_id(QUIT, "Quit PdfCraft", true, accel("Cmd+KeyQ"));
    app.append_items(&[
        &about,
        &PredefinedMenuItem::separator(),
        &prefs,
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::services(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::hide(None),
        &PredefinedMenuItem::hide_others(None),
        &PredefinedMenuItem::show_all(None),
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .ok()?;
    menu.append(&app).ok()?;

    // The registry's menus, in the order the in-window Menu shows them.
    let mut help = None;
    for (name, title) in [("File", "File"), ("Edit", "Edit"), ("View", "View"), ("Pages", "Pages"), ("Help", "Help")] {
        let sub = registry_submenu(name, title)?;
        if name == "View" {
            // Search across lectures sits with the other ways of finding things in Edit too, but
            // it is also a view of many files; and full screen belongs to View in every Mac app.
            sub.append(&PredefinedMenuItem::separator()).ok()?;
            sub.append(&MenuItem::with_id("edit.search_files", "Search in Multiple PDFs…", true, None)).ok()?;
        }
        menu.append(&sub).ok()?;
        if name == "Pages" {
            menu.append(&window_menu()?).ok()?;
        }
        if name == "Help" {
            help = Some(sub);
        }
    }
    if let Some(help) = help {
        help.set_as_help_menu_for_nsapp();
    }
    Some(menu)
}

/// A top-level menu of every registry command filed under `name`, except those the app menu or
/// the Window menu already shows.
fn registry_submenu(name: &str, title: &str) -> Option<Submenu> {
    let sub = Submenu::new(title, true);
    for command in COMMANDS.iter().filter(|c| c.menu == Some(name)) {
        if matches!(command.id, "help.about" | "app.preferences" | FULL_SCREEN) {
            continue;
        }
        sub.append(&MenuItem::with_id(command.id, command.label, true, None)).ok()?;
    }
    Some(sub)
}

fn window_menu() -> Option<Submenu> {
    let window = Submenu::new("Window", true);
    let pages_only = MenuItem::with_id(FULL_SCREEN, "Full Screen (Pages Only)", true, None);
    window
        .append_items(&[
            &PredefinedMenuItem::minimize(None),
            &PredefinedMenuItem::maximize(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::fullscreen(None),
            &pages_only,
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::bring_all_to_front(None),
        ])
        .ok()?;
    window.set_as_windows_menu_for_nsapp();
    Some(window)
}
