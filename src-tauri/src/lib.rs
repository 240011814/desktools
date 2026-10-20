use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, State, WindowEvent};

mod auto_lock;
mod autostart;
mod clipboard;
mod config;
mod file_hub;
mod fs_atomic;
mod history;
mod key_sync;
mod lock_sync;
mod logger;
mod mqtt;
mod secret;
mod shell_integration;

use config::{MinioConfig, MqttConfig};
use history::{HistoryItem, HistoryState};
use logger::{LogEntry, LogState};
use mqtt::{MqttStatus, SyncState};

/// `set_key_sync` 的返回：保存后的配置 + 警告（部分成功场景，如 macOS 缺权限回退关闭）
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct KeySyncSaveResult {
    config: MqttConfig,
    /// 非空时前端弹 toast（例：macOS 缺监听权限，总开关已回退关闭）
    warning: String,
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// 弹出独立的剪贴板历史小窗口（桌面右键 / 托盘菜单调用）
fn show_history_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("history") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        logger::log(app, logger::INFO, "已打开剪贴板历史窗口");
        // 打开时让前端自动刷新一次（窗口已显示/已聚焦时 show 不触发窗口事件，这里兜底）
        let _ = app.emit(history::HISTORY_REFRESH_EVENT, ());
    }
}

/// 弹出独立的文件中转站小窗口（桌面右键 / 托盘菜单调用）
fn show_files_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("files") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        logger::log(app, logger::INFO, "已打开文件中转站窗口");
        // 打开时让前端自动刷新一次（窗口已显示/已聚焦时 show 不触发窗口事件，这里兜底）
        let _ = app.emit(file_hub::REFRESH_EVENT, ());
    }
}

/// 显示主窗口（文件中转站小窗口在未配置 MinIO 时引导用户回主窗口设置）
#[tauri::command]
fn open_main_window(app: tauri::AppHandle) {
    show_main_window(&app);
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
    // 开机自启由 set_autostart、MinIO 由 save_minio_config 各自专管：
    // 整份表单保存时保留已持久化值，避免被前端旧值覆盖。
    // 按键同理：其规则与开关专由 set_key_sync 维护，不随整份表单覆写。
    {
        let state = app.state::<SyncState>();
        let persisted = state.config.lock().unwrap().clone();
        cfg.auto_start = persisted.auto_start;
        cfg.minio = persisted.minio;
        cfg.key_sync_enabled = persisted.key_sync_enabled;
        cfg.key_sync_rules = persisted.key_sync_rules;
    }
    cfg.validate()?;

    let path = config::config_path(&app);
    cfg.save(&path)?;

    {
        let state = app.state::<SyncState>();
        *state.config.lock().unwrap() = cfg.clone();
    }
    // 关闭离人自动锁屏时，取消挂起中的延迟锁屏
    if !cfg.auto_lock {
        app.state::<auto_lock::AutoLockState>().cancel_all(&app);
    }
    // 应用新配置并重连（自动订阅 texthub/<group>）
    mqtt::start(app, cfg.clone());
    Ok(cfg)
}

/// 开关类设置（离人自动锁屏 / 锁屏同步）依赖 MQTT；未配置时给出可执行的引导
fn require_mqtt_configured(cfg: &MqttConfig) -> Result<(), String> {
    if cfg.host.trim().is_empty() || cfg.group.trim().is_empty() {
        return Err("需要先配置 MQTT 连接（服务器地址与 Group 号，见「剪贴板同步」卡片）".into());
    }
    Ok(())
}

/// 开启开关：先确认 MQTT 已配置，再全量校验
fn validate_switch_on(cfg: &MqttConfig) -> Result<(), String> {
    require_mqtt_configured(cfg)?;
    cfg.validate()
}

/// 关闭开关：从未配置过同步的机器也应能关掉开关（仅保存状态，不做 MQTT 相关校验）
fn validate_switch_off(cfg: &MqttConfig) -> Result<(), String> {
    if cfg.host.trim().is_empty() {
        return Ok(());
    }
    cfg.validate()
}

/// 更新单个开关类配置：以持久化配置为基，只改目标字段后保存，再重启同步。
///
/// 与 `save_mqtt_config`（整份表单保存）相对：设置页的开关调用本路径，
/// 避免把表单里「已修改但未提交」的其他字段（host / 密码等）一并写入并重连。
fn apply_switch_update(
    app: &tauri::AppHandle,
    update: impl FnOnce(&mut MqttConfig),
    validate: fn(&MqttConfig) -> Result<(), String>,
) -> Result<MqttConfig, String> {
    let cfg = {
        let state = app.state::<SyncState>();
        let mut cfg = state.config.lock().unwrap().clone();
        update(&mut cfg);
        cfg
    };
    validate(&cfg)?;

    cfg.save(&config::config_path(app))?;
    {
        let state = app.state::<SyncState>();
        *state.config.lock().unwrap() = cfg.clone();
    }
    // 关闭离人自动锁屏时，取消挂起中的延迟锁屏
    if !cfg.auto_lock {
        app.state::<auto_lock::AutoLockState>().cancel_all(app);
    }
    // 重启同步：开启时建立 csi/notify 与 locksync 订阅；关闭时旧订阅随重连断开
    mqtt::start(app.clone(), cfg.clone());
    Ok(cfg)
}

/// 切换「离人自动锁屏」开关（窄接口：只改开关与延迟，不保存设置页其他未提交字段）
#[tauri::command]
fn set_auto_lock(
    app: tauri::AppHandle,
    enabled: bool,
    delay_secs: u64,
) -> Result<MqttConfig, String> {
    apply_switch_update(
        &app,
        |cfg| {
            cfg.auto_lock = enabled;
            cfg.auto_lock_delay_secs = delay_secs.clamp(1, 300);
        },
        if enabled {
            validate_switch_on
        } else {
            validate_switch_off
        },
    )
}

/// 更新「锁屏同步」（窄接口，同上）：广播 / 跟随两个开关 + 活跃守卫阈值。
/// 任一端开启时要求 MQTT 已配置；阈值 0 = 关闭守卫（总是跟随）
#[tauri::command]
fn set_lock_sync(
    app: tauri::AppHandle,
    publish: bool,
    follow: bool,
    idle_guard_secs: u64,
) -> Result<MqttConfig, String> {
    apply_switch_update(
        &app,
        |cfg| {
            cfg.lock_sync_publish = publish;
            cfg.lock_sync_follow = follow;
            cfg.lock_sync_idle_guard_secs = idle_guard_secs.clamp(0, 3600);
        },
        if publish || follow {
            validate_switch_on
        } else {
            validate_switch_off
        },
    )
}

/// 保存「按键同步」配置（窄接口：只改 keySync 开关与规则，不触碰设置页其他未提交字段）。
///
/// 流程：校验 → 先启停引擎（启用失败不落盘，避免「开关亮着却不工作」）→ 保存 → 仅在
/// 总开关变化时重连 MQTT（keysync 订阅只与开关有关；编辑规则不打断剪贴板同步）。
#[tauri::command]
fn set_key_sync(
    app: tauri::AppHandle,
    enabled: bool,
    rules: Vec<key_sync::KeySyncRule>,
) -> Result<KeySyncSaveResult, String> {
    let mut cfg = {
        let state = app.state::<SyncState>();
        let mut cfg = state.config.lock().unwrap().clone();
        cfg.key_sync_enabled = enabled;
        cfg.key_sync_rules = rules;
        cfg
    };
    let was_enabled = app
        .state::<SyncState>()
        .config
        .lock()
        .unwrap()
        .key_sync_enabled;
    key_sync::validate_rules(&cfg.key_sync_rules)?;
    if enabled {
        validate_switch_on(&cfg)?;
    } else {
        validate_switch_off(&cfg)?;
    }

    // 先启停引擎：macOS 缺监听权限时总开关回退关闭（规则仍然保存），由前端弹提示
    let mut warning = String::new();
    match key_sync::start_engine(&app, &cfg) {
        Ok(_) => {}
        Err(key_sync::EngineError::Permission(message)) => {
            cfg.key_sync_enabled = false;
            warning = message;
        }
        Err(key_sync::EngineError::Failed(message)) => {
            key_sync::stop_engine();
            return Err(message);
        }
    }

    cfg.save(&config::config_path(&app))?;
    {
        let state = app.state::<SyncState>();
        *state.config.lock().unwrap() = cfg.clone();
    }
    if was_enabled != cfg.key_sync_enabled {
        // 重连：开启时订阅 keysync/<group>，关闭时旧订阅随重连断开
        mqtt::start(app.clone(), cfg.clone());
    }
    if !warning.is_empty() {
        logger::log(&app, logger::WARN, format!("按键同步：{warning}"));
    }
    Ok(KeySyncSaveResult {
        config: cfg,
        warning,
    })
}

#[tauri::command]
fn get_key_sync_permissions() -> key_sync::KeySyncPermissions {
    key_sync::key_sync_permissions()
}

/// 打开 macOS 隐私设置对应面板（仅 macOS；kind = listen / post）
#[tauri::command]
fn open_key_sync_permission_settings(kind: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let pane = match kind.as_str() {
            "listen" => "Privacy_ListenEvent",
            "post" => "Privacy_Accessibility",
            _ => return Err("未知权限类型".into()),
        };
        std::process::Command::new("/usr/bin/open")
            .arg(format!(
                "x-apple.systempreferences:com.apple.preference.security?{pane}"
            ))
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("打开系统设置失败：{e}"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = kind;
        Err("当前平台无需此权限".into())
    }
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
        // 写入 + 读回校验 + 竞争重试，并记入防回环基准（监听线程不再重复发布/记录）
        clipboard::copy(&app, &text)?;
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

/// 同步命令默认跑在主线程；status()/set_enabled() 需拉起 reg.exe（Windows）、pbs（macOS）
/// 等子进程并等待退出，会挡住窗口消息循环导致设置页整窗冻结几秒，故标 async 挪到后台线程
#[tauri::command(async)]
fn get_desktop_integration() -> shell_integration::IntegrationStatus {
    shell_integration::status()
}

#[tauri::command(async)]
fn set_desktop_integration(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<shell_integration::IntegrationStatus, String> {
    shell_integration::set_enabled(enabled)?;
    logger::log(
        &app,
        logger::INFO,
        if enabled {
            "已启用桌面右键菜单集成（剪贴板历史、文件中转站）"
        } else {
            "已关闭桌面右键菜单集成"
        },
    );
    Ok(shell_integration::status())
}

#[tauri::command]
fn get_autostart_state(app: tauri::AppHandle) -> Result<autostart::AutostartState, String> {
    autostart::state(&app)
}

#[tauri::command]
fn set_autostart(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<autostart::AutostartState, String> {
    autostart::set_enabled(&app, enabled)
}

/// 锁屏能力自检（macOS：SAC 可用性 / 辅助功能 / 自动化 / 唤醒密码），设置页展示
#[tauri::command]
fn get_lock_capability() -> auto_lock::LockReport {
    auto_lock::self_check()
}

/// 从当前内存配置中取 MinIO 配置
fn current_minio(app: &tauri::AppHandle) -> MinioConfig {
    app.state::<SyncState>()
        .config
        .lock()
        .unwrap()
        .minio
        .clone()
}

/// 保存 MinIO 配置（host 留空 = 清除配置；凭据写盘时机器绑定加密）
#[tauri::command]
fn save_minio_config(app: tauri::AppHandle, minio: MinioConfig) -> Result<MinioConfig, String> {
    let saved = if minio.configured() {
        let normalized = minio.normalized();
        normalized.validate()?;
        normalized
    } else {
        MinioConfig::default()
    };

    {
        let state = app.state::<SyncState>();
        let mut cfg = state.config.lock().unwrap();
        cfg.minio = saved.clone();
        cfg.save(&config::config_path(&app))?;
    }

    logger::log(
        &app,
        logger::INFO,
        if saved.configured() {
            format!(
                "文件中转站：MinIO 配置已保存（{}:{}，bucket「{}」，TLS {}）",
                saved.host,
                saved.port,
                saved.bucket,
                if saved.tls { "开" } else { "关" }
            )
        } else {
            "文件中转站：MinIO 配置已清除".to_string()
        },
    );
    Ok(saved)
}

/// 测试 MinIO 连接（直接用表单里的值测，保存前即可验证）：连接成功且 bucket 可用时返回一句话结果
#[tauri::command]
async fn minio_test_connection(
    app: tauri::AppHandle,
    minio: MinioConfig,
) -> Result<String, String> {
    let cfg = minio.normalized();
    if !cfg.configured() {
        return Err("请先填写 MinIO 服务器地址".into());
    }
    cfg.validate()?;
    let started = std::time::Instant::now();
    match file_hub::test_connection(&app, &cfg).await {
        Ok(message) => {
            logger::log(
                &app,
                logger::INFO,
                format!(
                    "文件中转站：{message}（{} ms）",
                    started.elapsed().as_millis()
                ),
            );
            Ok(message)
        }
        Err(err) => {
            logger::log(
                &app,
                logger::WARN,
                format!("文件中转站：测试连接失败：{err}"),
            );
            Err(err)
        }
    }
}

#[tauri::command]
async fn file_hub_list(app: tauri::AppHandle) -> Result<Vec<file_hub::HubFile>, String> {
    let cfg = current_minio(&app);
    let started = std::time::Instant::now();
    match file_hub::list(&app, &cfg).await {
        Ok(files) => {
            logger::log(
                &app,
                logger::INFO,
                format!(
                    "文件中转站：已加载 {} 个文件（{} ms）",
                    files.len(),
                    started.elapsed().as_millis()
                ),
            );
            Ok(files)
        }
        Err(err) => {
            logger::log(
                &app,
                logger::WARN,
                format!("文件中转站：加载文件列表失败：{err}"),
            );
            Err(err)
        }
    }
}

#[tauri::command]
async fn file_hub_upload(
    app: tauri::AppHandle,
    path: String,
    transfer_id: String,
) -> Result<String, String> {
    let cfg = current_minio(&app);
    let started = std::time::Instant::now();
    match file_hub::upload(&app, &cfg, &path, &transfer_id).await {
        Ok(key) => {
            logger::log(
                &app,
                logger::INFO,
                format!(
                    "文件中转站：上传完成 {key}（{} ms）",
                    started.elapsed().as_millis()
                ),
            );
            Ok(key)
        }
        Err(err) => {
            logger::log(
                &app,
                logger::ERROR,
                format!("文件中转站：上传失败（{path}）：{err}"),
            );
            Err(err)
        }
    }
}

#[tauri::command]
async fn file_hub_upload_image(app: tauri::AppHandle, name: String) -> Result<String, String> {
    let cfg = current_minio(&app);
    let started = std::time::Instant::now();
    match file_hub::upload_image(&app, &cfg, &name).await {
        Ok(key) => {
            logger::log(
                &app,
                logger::INFO,
                format!(
                    "文件中转站：上传剪贴板图片完成 {key}（{} ms）",
                    started.elapsed().as_millis()
                ),
            );
            Ok(key)
        }
        Err(err) => {
            // 「剪贴板中没有图片」属于常见的用户操作提示，用 WARN 避免刷错误日志
            logger::log(
                &app,
                logger::WARN,
                format!("文件中转站：上传剪贴板图片失败：{err}"),
            );
            Err(err)
        }
    }
}

#[tauri::command]
async fn file_hub_download(
    app: tauri::AppHandle,
    key: String,
    target_path: String,
    transfer_id: String,
) -> Result<(), String> {
    let cfg = current_minio(&app);
    let started = std::time::Instant::now();
    match file_hub::download(&app, &cfg, &key, &target_path, &transfer_id).await {
        Ok(()) => {
            logger::log(
                &app,
                logger::INFO,
                format!(
                    "文件中转站：下载完成 {key} → {target_path}（{} ms）",
                    started.elapsed().as_millis()
                ),
            );
            Ok(())
        }
        Err(err) => {
            logger::log(
                &app,
                logger::ERROR,
                format!("文件中转站：下载失败（{key}）：{err}"),
            );
            Err(err)
        }
    }
}

#[tauri::command]
async fn file_hub_delete(app: tauri::AppHandle, key: String) -> Result<(), String> {
    let cfg = current_minio(&app);
    match file_hub::delete(&app, &cfg, &key).await {
        Ok(()) => {
            logger::log(&app, logger::INFO, format!("文件中转站：已删除 {key}"));
            Ok(())
        }
        Err(err) => {
            logger::log(
                &app,
                logger::ERROR,
                format!("文件中转站：删除失败（{key}）：{err}"),
            );
            Err(err)
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 单实例：桌面右键/快速操作再次启动时，转发给已运行实例打开对应窗口
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if argv.iter().any(|arg| arg == "--history") {
                show_history_window(app);
            } else if argv.iter().any(|arg| arg == "--files") {
                show_files_window(app);
            }
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_os::init())
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .args(["--hidden"])
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .invoke_handler(tauri::generate_handler![
            get_mqtt_config,
            get_mqtt_status,
            save_mqtt_config,
            set_auto_lock,
            set_lock_sync,
            set_key_sync,
            get_key_sync_permissions,
            open_key_sync_permission_settings,
            get_history,
            delete_history,
            clear_history,
            copy_to_clipboard,
            get_logs,
            clear_logs,
            get_desktop_integration,
            set_desktop_integration,
            get_autostart_state,
            set_autostart,
            get_lock_capability,
            open_main_window,
            save_minio_config,
            minio_test_connection,
            file_hub_list,
            file_hub_upload,
            file_hub_upload_image,
            file_hub_download,
            file_hub_delete
        ])
        .setup(|app| {
            // 托盘图标与菜单（右键菜单可直接弹出剪贴板历史窗口）
            let history_item = MenuItem::with_id(app, "history", "剪贴板历史", true, None::<&str>)?;
            let files_item = MenuItem::with_id(app, "files", "文件中转站", true, None::<&str>)?;
            let show_item = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu =
                Menu::with_items(app, &[&history_item, &files_item, &show_item, &quit_item])?;

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
                    "history" => show_history_window(app),
                    "files" => show_files_window(app),
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
            app.manage(auto_lock::AutoLockState::default());
            app.manage(lock_sync::LockSyncState::default());
            app.manage(key_sync::KeySyncState::default());
            let log_path = app
                .path()
                .app_log_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join("desktools.jsonl");
            app.manage(LogState::load(log_path));
            // 启动自检：macOS「模拟按键」权限自修复（签名异常时自动重签并重启）
            // 放在 LogState 之后的目的是：自检发生在日志通道可用的情况下。
            #[cfg(target_os = "macos")]
            if key_sync::startup_self_heal(app.handle()) == key_sync::StartupHeal::Relaunching {
                app.handle().exit(0);
                return Ok(());
            }
            // 按键同步：按持久化开关启动引擎（macOS 缺权限时回退关闭并记 WARN）。
            // 必须在 mqtt::start 之前：回退修正过后的 config 决定是否订阅 keysync/<group>。
            let cfg = key_sync::startup_enable(app.handle(), cfg);
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

            // 收到的 MQTT 消息进入内存队列，由后台单线程顺序处理（记历史、写剪贴板）
            mqtt::spawn_message_worker(app.handle().clone());

            // 启动后自动连接 MQTT 并订阅 texthub/<group号>
            mqtt::start(app.handle().clone(), cfg);

            // 监听本机复制的文本，自动发布到同步 topic
            #[cfg(desktop)]
            mqtt::spawn_clipboard_watcher(app.handle().clone());

            // 监听本机锁屏 / 解锁状态，发布到 locksync/<group>（开关开启时生效）
            #[cfg(desktop)]
            lock_sync::spawn_watcher(app.handle().clone());

            // 开机自启：按 config.json 的 autoStart 意图与系统实际状态对齐（仅打包构建管理，
            // debug 构建跳过，避免把自启条目指向 target/debug 二进制）
            autostart::sync_on_startup(app.handle());

            // 桌面右键集成：旧版只装过「剪贴板历史」，用户已启用时补齐新增条目（仅打包构建）
            match shell_integration::sync_on_startup() {
                Ok(added) if !added.is_empty() => logger::log(
                    app.handle(),
                    logger::INFO,
                    format!("桌面右键菜单集成：已补装条目 {}", added.join("、")),
                ),
                Ok(_) => {}
                Err(err) => logger::log(
                    app.handle(),
                    logger::WARN,
                    format!("桌面右键菜单集成：补装条目失败：{err}"),
                ),
            }

            // 锁屏能力自检：启动时探测一次（非破坏性，不锁屏不弹窗），异常时记 WARN 并在设置页展示
            let lock_report = auto_lock::self_check();
            if lock_report.degraded {
                let advice = if lock_report.advice.is_empty() {
                    String::new()
                } else {
                    format!("；建议：{}", lock_report.advice)
                };
                logger::log(
                    app.handle(),
                    logger::WARN,
                    format!("锁屏能力自检：{}{advice}", lock_report.summary),
                );
            } else {
                logger::log(
                    app.handle(),
                    logger::INFO,
                    format!("锁屏能力自检：{}", lock_report.summary),
                );
            }

            // 启动参数决定初始窗口：--history / --files 弹对应小窗口，--hidden 仅驻留托盘
            if std::env::args().any(|arg| arg == "--history") {
                show_history_window(app.app_handle());
            } else if std::env::args().any(|arg| arg == "--files") {
                show_files_window(app.app_handle());
            } else if !std::env::args().any(|arg| arg == "--hidden") {
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
            // 小窗口每次获得焦点时，通知前端自动刷新一次
            //（show_xxx_window 在 show 后也会显式触发，覆盖已显示/已聚焦的场景）
            if matches!(event, WindowEvent::Focused(true)) {
                match window.label() {
                    "history" => {
                        let _ = window.emit(history::HISTORY_REFRESH_EVENT, ());
                    }
                    "files" => {
                        let _ = window.emit(file_hub::REFRESH_EVENT, ());
                    }
                    _ => {}
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
