use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt;

mod config;
mod mqtt;

use config::MqttConfig;
use mqtt::{MqttStatus, SyncState};

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[tauri::command]
fn get_mqtt_config(state: State<SyncState>) -> MqttConfig {
    state.config.lock().unwrap().clone()
}

#[tauri::command]
fn get_mqtt_status(state: State<SyncState>) -> MqttStatus {
    state.status.lock().unwrap().clone()
}

#[tauri::command]
fn save_mqtt_config(app: tauri::AppHandle, mut cfg: MqttConfig) -> Result<MqttConfig, String> {
    cfg.host = cfg.host.trim().to_string();
    cfg.group = cfg.group.trim().to_string();
    cfg.username = cfg.username.trim().to_string();
    cfg.validate()?;

    let path = config::config_path(&app);
    cfg.save(&path)?;

    {
        let state = app.state::<SyncState>();
        *state.config.lock().unwrap() = cfg.clone();
    }
    // 应用新配置并重连（自动订阅 texthub/<group>）
    mqtt::start(app, cfg.clone());
    Ok(cfg)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .args(["--hidden"])
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .invoke_handler(tauri::generate_handler![
            get_mqtt_config,
            get_mqtt_status,
            save_mqtt_config
        ])
        .setup(|app| {
            // 托盘图标与菜单
            let show_item = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

            TrayIconBuilder::with_id("main-tray")
                .icon(
                    app.default_window_icon()
                        .cloned()
                        .expect("missing default window icon"),
                )
                .tooltip("desktools")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
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
                            if window.is_visible().unwrap_or(false) {
                                let _ = window.hide();
                            } else {
                                show_main_window(app);
                            }
                        }
                    }
                })
                .build(app)?;

            // 加载持久化配置（MQTT + Group 号）
            let cfg = MqttConfig::load(&config::config_path(app.handle()));
            app.manage(SyncState::new(cfg.clone()));

            // 启动后自动连接 MQTT 并订阅 texthub/<group号>
            mqtt::start(app.handle().clone(), cfg);

            // 监听本机复制的文本，自动发布到同步 topic
            #[cfg(desktop)]
            mqtt::spawn_clipboard_watcher(app.handle().clone());

            // 开机自动启动（登录时以 --hidden 启动，仅驻留托盘）
            let _ = app.autolaunch().enable();

            // 手动启动时显示主窗口
            if !std::env::args().any(|arg| arg == "--hidden") {
                show_main_window(app.app_handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭窗口 = 最小化到托盘，不退出程序
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
