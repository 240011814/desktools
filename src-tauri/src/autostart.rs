use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_autostart::ManagerExt;

use crate::config;
use crate::logger::{self, ERROR, INFO, WARN};
use crate::mqtt::SyncState;

/// 当前构建是否允许管理系统自启：
/// debug 构建（pnpm tauri dev / cargo run）不碰系统自启，避免把
/// ~/Library/LaunchAgents/desktools.plist 指向 target/debug 二进制，
/// 与打包版本来回覆盖（也会触发 macOS BTM 反复注册）。
fn managed_build() -> bool {
    !cfg!(debug_assertions)
}

/// 开机自启状态（返回给前端）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutostartState {
    /// 系统实际是否已注册开机自启
    pub enabled: bool,
    /// 当前构建是否管理开机自启（debug 构建为 false，前端据此禁用开关）
    pub managed: bool,
}

/// 读取系统实际状态（debug 构建也可安全读取，不写入）
pub fn state(app: &AppHandle) -> Result<AutostartState, String> {
    let enabled = app
        .autolaunch()
        .is_enabled()
        .map_err(|e| format!("读取开机自启状态失败：{e}"))?;
    Ok(AutostartState {
        enabled,
        managed: managed_build(),
    })
}

/// 按持久化意图（config.json 的 autoStart）与系统实际状态对齐。
/// 只在不一致时写系统：一致时不动 plist，避免 macOS 每次启动都重新注册后台项（BTM 通知配额被打爆）。
pub fn sync_on_startup(app: &AppHandle) {
    if !managed_build() {
        logger::log(
            app,
            INFO,
            "开机自启：开发构建跳过同步（避免指向 target/debug 二进制）",
        );
        return;
    }
    let want = app.state::<SyncState>().config.lock().unwrap().auto_start;
    match app.autolaunch().is_enabled() {
        Ok(actual) if actual == want => {
            logger::log(
                app,
                INFO,
                format!(
                    "开机自启：已{}（与配置一致）",
                    if want { "启用" } else { "关闭" }
                ),
            );
        }
        Ok(_) => apply(app, want),
        Err(err) => {
            logger::log(
                app,
                WARN,
                format!("开机自启：读取系统状态失败，跳过同步：{err}"),
            );
        }
    }
}

/// 设置开机自启：先持久化意图到 config.json，再同步系统状态，返回系统实测状态
pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<AutostartState, String> {
    if !managed_build() {
        return Err("开发构建不管理开机自启".into());
    }
    {
        let state = app.state::<SyncState>();
        let mut cfg = state.config.lock().unwrap();
        if cfg.auto_start != enabled {
            cfg.auto_start = enabled;
            cfg.save(&config::config_path(app))
                .map_err(|e| format!("保存配置失败：{e}"))?;
        }
    }
    apply(app, enabled);
    // 以系统实测为准：应用失败时如实返回，由前端展示并回读
    let actual = app
        .autolaunch()
        .is_enabled()
        .map_err(|e| format!("读取开机自启状态失败：{e}"))?;
    if actual != enabled {
        return Err(format!(
            "开机自启{}未生效（系统当前状态：{}）",
            if enabled { "开启" } else { "关闭" },
            if actual { "开启" } else { "关闭" }
        ));
    }
    Ok(AutostartState {
        enabled: actual,
        managed: true,
    })
}

/// 应用系统状态并记录结果（不吞错误）
fn apply(app: &AppHandle, enabled: bool) {
    let result = if enabled {
        app.autolaunch().enable()
    } else {
        app.autolaunch().disable()
    };
    match result {
        Ok(()) => logger::log(
            app,
            INFO,
            format!(
                "开机自启：已{}（登录时以 --hidden 启动，仅驻留托盘）",
                if enabled { "开启" } else { "关闭" }
            ),
        ),
        Err(err) => logger::log(
            app,
            ERROR,
            format!(
                "开机自启：{}失败：{err}",
                if enabled { "开启" } else { "关闭" }
            ),
        ),
    }
}
