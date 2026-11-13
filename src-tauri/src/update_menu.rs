use tauri::menu::{Menu, MenuItem};
use tauri::{App, AppHandle, Manager};
use tokio::sync::Mutex;

use crate::services::process::SystemRunner;
use crate::services::update_check::{self, UpdateResult};

const CHECK_UPDATES: &str = "check-updates";

struct UpdateState {
    item: MenuItem<tauri::Wry>,
    url: Mutex<Option<String>>,
}

pub fn setup(app: &App) -> tauri::Result<()> {
    // D4: menu integration is optional so platform menu failures cannot block launch.
    // Costs losing update discovery on failure; retry on the next launch.
    if let Err(error) = install(app) {
        eprintln!("[ClusterDeck] could not install update menu: {error}");
    }
    Ok(())
}

fn install(app: &App) -> tauri::Result<()> {
    let menu = Menu::default(app.handle())?;
    let items = menu.items()?;
    let Some(app_menu) = items.first().and_then(|item| item.as_submenu()) else {
        eprintln!("[ClusterDeck] default menu has no first application submenu");
        return Ok(());
    };
    let item = MenuItem::with_id(app, CHECK_UPDATES, "Check for Updates…", true, None::<&str>)?;
    if app_menu.items()?.is_empty() {
        app_menu.append(&item)?;
    } else {
        app_menu.insert(&item, 1)?;
    }
    app.set_menu(menu)?;
    app.manage(UpdateState {
        item,
        url: Mutex::new(None),
    });
    app.on_menu_event(|app, event| {
        if event.id().as_ref() == CHECK_UPDATES {
            let app = app.clone();
            tauri::async_runtime::spawn(async move { run_check(app, true).await });
        }
    });
    let app = app.handle().clone();
    tauri::async_runtime::spawn(async move { run_check(app, false).await });
    Ok(())
}

async fn run_check(app: AppHandle, interactive: bool) {
    let state = app.state::<UpdateState>();
    // D5: lock only the cached value so I/O never blocks other clicks. Costs possible
    // duplicate checks/dialogs; add a separate operation guard if coalescing is needed.
    if interactive {
        let cached = state.url.lock().await.clone();
        if let Some(url) = cached {
            open_or_alert(&url).await;
            return;
        }
    }
    match update_check::check(&SystemRunner).await {
        UpdateResult::Available {
            version,
            url: found,
        } => {
            *state.url.lock().await = Some(found.clone());
            if let Err(error) = state
                .item
                .set_text(format!("Update Available (v{version})…"))
            {
                eprintln!("[ClusterDeck] could not update menu label: {error}");
            }
            if interactive {
                open_or_alert(&found).await;
            }
        }
        UpdateResult::UpToDate { current } if interactive => {
            let _ = update_check::show_alert(
                &SystemRunner,
                "ClusterDeck is up to date",
                &format!("Version {current} is the latest version."),
            )
            .await;
        }
        UpdateResult::Failed { reason } if interactive => {
            let _ = update_check::show_alert(&SystemRunner, "Couldn't check for updates", &reason)
                .await;
        }
        _ => {}
    }
}

async fn open_or_alert(url: &str) {
    if let Err(reason) = update_check::open_release(&SystemRunner, url).await {
        let _ = update_check::show_alert(&SystemRunner, "Couldn't open update page", &reason).await;
    }
}
