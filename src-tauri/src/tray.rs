use tauri::{
    AppHandle, Manager,
    menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_shell::ShellExt;

const TRAY_ID: &str = "orca-tray";

/// Build the tray menu: running worktree instances (with their URLs) on top,
/// then the window controls.
fn build_menu(app: &AppHandle, instances: &[(String, Vec<String>)]) -> tauri::Result<Menu<tauri::Wry>> {
    let mut items: Vec<Box<dyn IsMenuItem<tauri::Wry>>> = Vec::new();
    if !instances.is_empty() {
        items.push(Box::new(MenuItem::with_id(
            app,
            "instances-title",
            "Running instances",
            false,
            None::<&str>,
        )?));
        for (label, urls) in instances {
            if urls.is_empty() {
                items.push(Box::new(MenuItem::with_id(
                    app,
                    format!("inst:{label}"),
                    label,
                    false,
                    None::<&str>,
                )?));
                continue;
            }
            let entries: Vec<MenuItem<tauri::Wry>> = urls
                .iter()
                .map(|u| MenuItem::with_id(app, format!("open:{u}"), format!("Open {u}"), true, None::<&str>))
                .collect::<tauri::Result<_>>()?;
            let refs: Vec<&dyn IsMenuItem<tauri::Wry>> =
                entries.iter().map(|e| e as &dyn IsMenuItem<tauri::Wry>).collect();
            items.push(Box::new(Submenu::with_items(app, label, true, &refs)?));
        }
        items.push(Box::new(PredefinedMenuItem::separator(app)?));
    }
    items.push(Box::new(MenuItem::with_id(
        app,
        "show",
        "Show Orca",
        true,
        None::<&str>,
    )?));
    items.push(Box::new(MenuItem::with_id(
        app,
        "hide",
        "Hide Window",
        true,
        None::<&str>,
    )?));
    items.push(Box::new(PredefinedMenuItem::separator(app)?));
    items.push(Box::new(MenuItem::with_id(
        app,
        "quit",
        "Quit Orca",
        true,
        None::<&str>,
    )?));
    let refs: Vec<&dyn IsMenuItem<tauri::Wry>> = items.iter().map(|i| i.as_ref()).collect();
    Menu::with_items(app, &refs)
}

/// Keep the tray's instance list current.
fn spawn_refresh(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut last: Option<Vec<(String, Vec<String>)>> = None;
        loop {
            let now = crate::commands::running_instances_for_tray().await;
            if last.as_ref() != Some(&now) {
                if let (Some(tray), Ok(menu)) = (app.tray_by_id(TRAY_ID), build_menu(&app, &now)) {
                    let _ = tray.set_menu(Some(menu));
                }
                last = Some(now);
            }
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
        }
    });
}

pub fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(app, &[])?;

    let _tray = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Orca — Container Desktop")
        .icon(tauri::image::Image::from_bytes(include_bytes!("../icons/tray-32x32.png")).expect("tray icon"))
        .icon_as_template(cfg!(target_os = "macos"))
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "hide" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }
            "quit" => {
                app.exit(0);
            }
            id => {
                if let Some(url) = id.strip_prefix("open:") {
                    #[allow(deprecated)]
                    let _ = app.shell().open(url, None);
                }
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        })
        .build(app)?;

    spawn_refresh(app.clone());
    Ok(())
}
