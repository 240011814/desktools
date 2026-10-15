use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt;

mod config;
mod history;
mod logger;
mod mqtt;

use config::MqttConfig;
use history::{HistoryItem, HistoryState};
use logger::{LogEntry, LogState};
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

#[tauri::command]
fn get_history(state: State<HistoryState>) -> Vec<HistoryItem> {
    state.list()
}

#[tauri::command]
fn delete_history(app: tauri::AppHandle, id: String) -> Result<(), String> {
    app.state::<HistoryState>().delete(&app, &id)
}

#[tauri::command]
fn clear_history(app: tauri::AppHandle) {
    app.state::<HistoryState>().clear(&app);
}

/// 将文本写入系统剪贴板（前端复制统一走后端，绕开插件 IPC 权限限制，跨平台一致）
#[tauri::command]
fn copy_to_clipboard(app: tauri::AppHandle, text: String) -> Result<(), String> {
    #[cfg(desktop)]
    {
        use tauri_plugin_clipboard_manager::ClipboardExt;
        app.clipboard().write_text(text.clone()).map_err(|e| {
            let msg = format!("写入剪贴板失败：{e}");
            logger::log(&app, logger::ERROR, &msg);
            msg
        })?;
        logger::log(&app, logger::INFO, format!("已复制到剪贴板（{} 字符）", text.chars().count()));
        Ok(())
    }
    #[cfg(not(desktop))]
    {
        let _ = (app, text);
        Err("当前平台不支持剪贴板操作".into())
    }
}

#[tauri::command]
fn get_logs(state: State<LogState>) -> Vec<LogEntry> {
    state.list()
}

#[tauri::command]
fn clear_logs(app: tauri::AppHandle) {
    app.state::<LogState>().clear(&app);
    logger::log(&app, logger::INFO, "日志已清空");
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
            save_mqtt_config,
            get_history,
            delete_history,
            clear_history,
            copy_to_clipboard,
            get_logs,
            clear_logs
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

            // 加载持久化配置（MQTT + Group 号）、剪贴板历史与运行日志
            let cfg = MqttConfig::load(&config::config_path(app.handle()));
            app.manage(SyncState::new(cfg.clone()));
            app.manage(HistoryState::load(config::history_path(app.handle())));
            let log_path = app
                .path()
                .app_log_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join("desktools.jsonl");
            app.manage(LogState::load(log_path));
            logger::log(
                app.handle(),
                logger::INFO,
                format!(
                    "应用启动（{}），同步配置：{}",
                    app.package_info().version,
                    if cfg.host.trim().is_empty() {
                        "未配置".to_string()
                    } else {
                        format!(
                            "{}:{}，group={}，tls={}",
                            cfg.host,
                            cfg.port,
                            cfg.group,
                            if cfg.tls { "开" } else { "关" }
                        )
                    }
                ),
            );

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
