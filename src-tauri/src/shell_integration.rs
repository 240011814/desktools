//! 桌面右键菜单系统集成
//!
//! - Windows：注册表静态菜单项（桌面空白处右键 →「剪贴板历史」/「文件中转站」）
//! - macOS：Finder「快速操作 / 服务」（桌面空白处右键 → 快速操作 → 对应条目）
//!
//! 两个平台均通过标准系统机制注册，点击后以 `--history` / `--files` 启动本程序弹出对应窗口。
//! 设置页开关统一控制全部条目；旧版本只装过「剪贴板历史」，升级后由 `sync_on_startup` 补齐新条目。

use serde::Serialize;

#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};

/// 集成状态（返回给前端展示）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationStatus {
    /// 当前平台是否支持桌面右键集成
    pub supported: bool,
    /// 是否已启用
    pub enabled: bool,
    /// 启用位置说明 / 错误信息
    pub detail: String,
}

/// 一条右键菜单条目：点击后按 `arg` 启动本程序弹出对应窗口
#[derive(Debug, Clone, Copy)]
pub struct MenuEntry {
    /// Windows 注册表子键名（`...\shell\<key>`）
    key: &'static str,
    /// 菜单显示名（macOS 快速操作名称的一部分）
    label: &'static str,
    /// 启动参数
    arg: &'static str,
}

/// 全部条目；第一个是主条目，设置页开关的启用状态以它为准（历史版本只安装过它）
const ENTRIES: [MenuEntry; 2] = [
    MenuEntry {
        key: "DeskToolsHistory",
        label: "剪贴板历史",
        arg: "--history",
    },
    MenuEntry {
        key: "DeskToolsFileHub",
        label: "文件中转站",
        arg: "--files",
    },
];

fn current_exe() -> Result<String, String> {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| format!("无法获取程序路径：{e}"))
}

/// 已安装条目说明（未安装的给出补装提示）
fn entries_detail(installed: &[&'static str]) -> String {
    let mut detail = format!("已添加：{}", installed.join("、"));
    let missing: Vec<&'static str> = ENTRIES
        .iter()
        .map(|entry| entry.label)
        .filter(|label| !installed.contains(label))
        .collect();
    if !missing.is_empty() {
        detail.push_str(&format!(
            "；未安装：{}（重新开启开关可补装）",
            missing.join("、")
        ));
    }
    detail
}

/// 升级补齐：旧版本只安装了主条目（剪贴板历史），用户已启用集成时把新增条目补上。
/// 仅打包构建执行（debug 构建跳过，避免把菜单指向 target/debug 二进制）；返回本次补装的条目名。
pub fn sync_on_startup() -> Result<Vec<&'static str>, String> {
    if cfg!(debug_assertions) || !entry_installed(&ENTRIES[0]) {
        return Ok(Vec::new());
    }
    let mut added = Vec::new();
    for entry in &ENTRIES[1..] {
        if !entry_installed(entry) {
            install_entry(entry)?;
            added.push(entry.label);
        }
    }
    Ok(added)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn unsupported() -> IntegrationStatus {
    IntegrationStatus {
        supported: false,
        enabled: false,
        detail: "当前平台不支持桌面右键菜单集成".into(),
    }
}

// ============================ Windows ============================

#[cfg(target_os = "windows")]
fn reg_key(entry: &MenuEntry) -> String {
    format!(
        r"HKCU\Software\Classes\Directory\Background\shell\{}",
        entry.key
    )
}

#[cfg(target_os = "windows")]
fn entry_installed(entry: &MenuEntry) -> bool {
    std::process::Command::new("reg")
        .args(["query", &reg_key(entry), "/ve"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "windows")]
fn install_entry(entry: &MenuEntry) -> Result<(), String> {
    let exe = current_exe()?;
    let key = reg_key(entry);
    run_reg(&["add", &key, "/ve", "/d", entry.label, "/f"])?;
    run_reg(&["add", &key, "/v", "Icon", "/d", &exe, "/f"])?;
    run_reg(&[
        "add",
        &format!(r"{key}\command"),
        "/ve",
        "/d",
        &format!("\"{exe}\" {}", entry.arg),
        "/f",
    ])?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn remove_entry(entry: &MenuEntry) {
    let _ = run_reg(&["delete", &reg_key(entry), "/f"]);
}

#[cfg(target_os = "windows")]
pub fn status() -> IntegrationStatus {
    let exe = match current_exe() {
        Ok(exe) => exe,
        Err(detail) => {
            return IntegrationStatus {
                supported: true,
                enabled: false,
                detail,
            }
        }
    };
    let installed: Vec<&'static str> = ENTRIES
        .iter()
        .filter(|entry| entry_installed(entry))
        .map(|entry| entry.label)
        .collect();
    IntegrationStatus {
        supported: true,
        enabled: entry_installed(&ENTRIES[0]),
        detail: if installed.is_empty() {
            "未启用".into()
        } else {
            format!(
                "{}（桌面右键菜单，程序路径：{exe}）",
                entries_detail(&installed)
            )
        },
    }
}

#[cfg(target_os = "windows")]
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if enabled {
        for entry in &ENTRIES {
            install_entry(entry)?;
        }
    } else {
        for entry in &ENTRIES {
            remove_entry(entry);
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn run_reg(args: &[&str]) -> Result<(), String> {
    let output = std::process::Command::new("reg")
        .args(args)
        .output()
        .map_err(|e| format!("执行 reg 失败：{e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "reg {} 失败：{}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

// ============================ macOS ============================

/// 条目对应的快速操作（服务）名称，右键菜单中显示
#[cfg(target_os = "macos")]
fn workflow_name(entry: &MenuEntry) -> String {
    format!("DeskTools {}.workflow", entry.label)
}

#[cfg(target_os = "macos")]
fn services_dir() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|e| format!("无法获取用户目录：{e}"))?;
    Ok(PathBuf::from(home).join("Library/Services"))
}

#[cfg(target_os = "macos")]
fn entry_installed(entry: &MenuEntry) -> bool {
    services_dir()
        .map(|dir| dir.join(workflow_name(entry)).exists())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn install_entry(entry: &MenuEntry) -> Result<(), String> {
    install_workflow_to(&services_dir()?, &current_exe()?, entry)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn remove_entry(entry: &MenuEntry) -> Result<(), String> {
    let bundle = services_dir()?.join(workflow_name(entry));
    if bundle.exists() {
        std::fs::remove_dir_all(&bundle).map_err(|e| format!("无法移除快速操作：{e}"))?;
    }
    Ok(())
}

/// 生成 workflow 的 document.wflow 内容
#[cfg(target_os = "macos")]
fn workflow_document(exe: &str, entry: &MenuEntry) -> String {
    let script = format!("nohup \"{exe}\" {} >/dev/null 2>&1 &", entry.arg);
    // 整段脚本做 XML 转义（含 & > < "）
    let script = script
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>AMApplicationBuild</key>
	<string>523</string>
	<key>AMApplicationVersion</key>
	<string>2.10</string>
	<key>AMDocumentVersion</key>
	<string>2</string>
	<key>actions</key>
	<array>
		<dict>
			<key>action</key>
			<dict>
				<key>AMAccepts</key>
				<dict>
					<key>Container</key>
					<string>List</string>
					<key>Optional</key>
					<true/>
					<key>Types</key>
					<array>
						<string>com.apple.cocoa.string</string>
					</array>
				</dict>
				<key>AMActionVersion</key>
				<string>2.0.3</string>
				<key>AMApplication</key>
				<array>
					<string>Automator</string>
				</array>
				<key>AMParameterProperties</key>
				<dict>
					<key>COMMAND_STRING</key>
					<dict/>
					<key>CheckedForUserDefaultShell</key>
					<dict/>
					<key>inputMethod</key>
					<dict/>
					<key>shell</key>
					<dict/>
					<key>source</key>
					<dict/>
				</dict>
				<key>AMProvides</key>
				<dict>
					<key>Container</key>
					<string>List</string>
					<key>Types</key>
					<array>
						<string>com.apple.cocoa.string</string>
					</array>
				</dict>
				<key>ActionBundlePath</key>
				<string>/System/Library/Automator/Run Shell Script.action</string>
				<key>ActionName</key>
				<string>Run Shell Script</string>
				<key>ActionParameters</key>
				<dict>
					<key>COMMAND_STRING</key>
					<string>{script}</string>
					<key>CheckedForUserDefaultShell</key>
					<true/>
					<key>inputMethod</key>
					<integer>0</integer>
					<key>shell</key>
					<string>/bin/zsh</string>
					<key>source</key>
					<string></string>
				</dict>
				<key>BundleIdentifier</key>
				<string>com.apple.RunShellScript</string>
				<key>CFBundleVersion</key>
				<string>2.0.3</string>
				<key>CanShowSelectedItemsWhenRun</key>
				<false/>
				<key>CanShowWhenRun</key>
				<true/>
				<key>Category</key>
				<array>
					<string>AMCategoryUtilities</string>
				</array>
				<key>Class Name</key>
				<string>RunShellScriptAction</string>
				<key>InputUUID</key>
				<string>5A7B4C51-7A0D-4E6A-9D2B-0B4C3D2E1F00</string>
				<key>OutputUUID</key>
				<string>6B8C5D62-8B1E-4F7B-AE3C-1C5D4E3F2A11</string>
				<key>UUID</key>
				<string>7C9D6E73-9C2F-408C-BF4D-2D6E5F403B22</string>
			</dict>
			<key>isViewVisible</key>
			<integer>1</integer>
			<key>location</key>
			<string>309.000000:253.000000</string>
			<key>nibPath</key>
			<string>/System/Library/Automator/Run Shell Script.action/Contents/Resources/Base.lproj/main.nib</string>
		</dict>
	</array>
	<key>connectors</key>
	<dict/>
	<key>workflowMetaData</key>
	<dict>
		<key>applicationBundleIDsByPath</key>
		<dict/>
		<key>applicationPaths</key>
		<array/>
		<key>inputTypeIdentifier</key>
		<string>com.apple.Automator.nothing</string>
		<key>outputTypeIdentifier</key>
		<string>com.apple.Automator.nothing</string>
		<key>presentationMode</key>
		<integer>15</integer>
		<key>processesInput</key>
		<integer>0</integer>
		<key>serviceInputTypeIdentifier</key>
		<string>com.apple.Automator.nothing</string>
		<key>serviceOutputTypeIdentifier</key>
		<string>com.apple.Automator.nothing</string>
		<key>serviceProcessesInput</key>
		<integer>0</integer>
		<key>systemImageName</key>
		<string>NSActionTemplate</string>
		<key>useAutomaticInputType</key>
		<integer>0</integer>
		<key>workflowTypeIdentifier</key>
		<string>com.apple.Automator.servicesMenu</string>
	</dict>
</dict>
</plist>
"#
    )
}

/// 生成 workflow 的 Info.plist 内容（bundle id 按条目区分，避免两个服务互相覆盖注册）
#[cfg(target_os = "macos")]
fn workflow_info(entry: &MenuEntry) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>com.hyy.desktools.quickaction.{key}</string>
	<key>CFBundleName</key>
	<string>DeskTools {label}</string>
	<key>CFBundlePackageType</key>
	<string>BNDL</string>
	<key>CFBundleShortVersionString</key>
	<string>1.0</string>
	<key>CFBundleVersion</key>
	<string>1</string>
	<key>NSPrincipalClass</key>
	<string>AMWorkflow</string>
</dict>
</plist>
"#,
        key = entry.key,
        label = entry.label,
    )
}

/// 把某条目的快速操作写入指定目录（供测试与真实安装共用）
#[cfg(target_os = "macos")]
pub fn install_workflow_to(dir: &Path, exe: &str, entry: &MenuEntry) -> Result<PathBuf, String> {
    let bundle = dir.join(workflow_name(entry));
    let contents = bundle.join("Contents");
    std::fs::create_dir_all(&contents).map_err(|e| format!("无法创建快速操作目录：{e}"))?;
    std::fs::write(contents.join("Info.plist"), workflow_info(entry))
        .map_err(|e| format!("无法写入 Info.plist：{e}"))?;
    std::fs::write(contents.join("document.wflow"), workflow_document(exe, entry))
        .map_err(|e| format!("无法写入 document.wflow：{e}"))?;
    // 刷新系统服务缓存（失败不影响功能，系统会在下次登录时刷新）
    let _ = std::process::Command::new("pbs").arg("-update").output();
    Ok(bundle)
}

#[cfg(target_os = "macos")]
pub fn status() -> IntegrationStatus {
    let exe = match current_exe() {
        Ok(exe) => exe,
        Err(detail) => {
            return IntegrationStatus {
                supported: true,
                enabled: false,
                detail,
            }
        }
    };
    let installed: Vec<&'static str> = ENTRIES
        .iter()
        .filter(|entry| entry_installed(entry))
        .map(|entry| entry.label)
        .collect();
    IntegrationStatus {
        supported: true,
        enabled: entry_installed(&ENTRIES[0]),
        detail: if installed.is_empty() {
            format!("未启用（启用后安装到 ~/Library/Services，程序路径：{exe}）")
        } else {
            format!(
                "{}（Finder 右键 → 快速操作；若未显示，请在「系统设置 → 键盘 → 快捷键 → 服务」中启用）",
                entries_detail(&installed)
            )
        },
    }
}

#[cfg(target_os = "macos")]
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if enabled {
        for entry in &ENTRIES {
            install_entry(entry)?;
        }
    } else {
        for entry in &ENTRIES {
            remove_entry(entry)?;
        }
        let _ = std::process::Command::new("pbs").arg("-update").output();
    }
    Ok(())
}

// ============================ 其他平台 ============================

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn entry_installed(_entry: &MenuEntry) -> bool {
    false
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn install_entry(_entry: &MenuEntry) -> Result<(), String> {
    Err("当前平台不支持桌面右键菜单集成".into())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn status() -> IntegrationStatus {
    unsupported()
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn set_enabled(_enabled: bool) -> Result<(), String> {
    Err("当前平台不支持桌面右键菜单集成".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reports_supported_platform() {
        let s = status();
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        assert!(s.supported);
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        assert!(!s.supported);
    }

    #[test]
    fn entries_are_distinct() {
        for (index, entry) in ENTRIES.iter().enumerate() {
            assert!(entry.arg.starts_with("--"), "启动参数应以 -- 开头");
            assert!(!entry.key.is_empty() && !entry.label.is_empty());
            for other in &ENTRIES[index + 1..] {
                assert_ne!(entry.key, other.key);
                assert_ne!(entry.label, other.label);
                assert_ne!(entry.arg, other.arg);
            }
        }
    }

    /// macOS：验证快速操作 workflow 文件结构与内容（每个条目各生成一套）
    #[cfg(target_os = "macos")]
    #[test]
    fn installs_workflow_into_temp_dir() {
        let dir = std::env::temp_dir().join(format!("desktools-svc-{}", uuid::Uuid::new_v4()));
        let exe = "/Applications/DeskTools.app/Contents/MacOS/desktools";
        let mut names = Vec::new();
        for entry in &ENTRIES {
            let bundle = install_workflow_to(&dir, exe, entry).unwrap();
            names.push(workflow_name(entry));
            assert!(bundle.join("Contents/Info.plist").exists());
            let wflow = std::fs::read_to_string(bundle.join("Contents/document.wflow")).unwrap();
            assert!(
                wflow.contains(entry.arg),
                "workflow 应包含启动参数 {}",
                entry.arg
            );
            assert!(wflow.contains("com.apple.Automator.servicesMenu"));
            let info = std::fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap();
            assert!(info.contains(&format!("DeskTools {}", entry.label)));
            assert!(info.contains(entry.key), "bundle id 应按条目区分");
            // plist 格式合法
            for file in ["Contents/document.wflow", "Contents/Info.plist"] {
                let check = std::process::Command::new("plutil")
                    .arg("-lint")
                    .arg(bundle.join(file))
                    .output()
                    .unwrap();
                assert!(check.status.success(), "{:?}", check);
            }
        }
        names.sort();
        names.dedup();
        assert_eq!(names.len(), ENTRIES.len(), "每个条目的 workflow 名称应唯一");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 手动端到端验证：把快速操作安装到真实 ~/Library/Services
    /// （`cargo test e2e_install_real -- --ignored` 触发，验证完可用 uninstall_real 清理）
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn e2e_install_real_workflow() {
        let exe = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/debug/desktools")
            .to_string_lossy()
            .to_string();
        for entry in &ENTRIES {
            let bundle = install_workflow_to(&services_dir().unwrap(), &exe, entry).unwrap();
            println!("installed: {}", bundle.display());
        }
    }

    /// 清理真实安装的快速操作
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn e2e_uninstall_real_workflow() {
        let dir = services_dir().unwrap();
        for entry in &ENTRIES {
            let bundle = dir.join(workflow_name(entry));
            let _ = std::fs::remove_dir_all(&bundle);
            println!("removed: {}", bundle.display());
        }
        let _ = std::process::Command::new("pbs").arg("-update").output();
    }
}
