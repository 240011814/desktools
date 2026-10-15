//! 桌面右键菜单系统集成
//!
//! - Windows：注册表静态菜单项（桌面空白处右键 → 「剪贴板历史」）
//! - macOS：Finder「快速操作 / 服务」（桌面空白处右键 → 快速操作 → 「剪贴板历史」）
//!
//! 两个平台均通过标准系统机制注册，点击后以 `--history` 启动本程序弹出历史窗口。

use serde::Serialize;

#[cfg(target_os = "macos")]
use std::path::PathBuf;

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

fn current_exe() -> Result<String, String> {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| format!("无法获取程序路径：{e}"))
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
const REG_KEY: &str = r"HKCU\Software\Classes\Directory\Background\shell\DeskToolsHistory";

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
    let output = std::process::Command::new("reg")
        .args(["query", REG_KEY, "/ve"])
        .output();
    let enabled = output
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("DeskTools"))
        .unwrap_or(false);
    IntegrationStatus {
        supported: true,
        enabled,
        detail: if enabled {
            format!("已添加到桌面右键菜单（程序路径：{exe}）")
        } else {
            "未启用".into()
        },
    }
}

#[cfg(target_os = "windows")]
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if enabled {
        let exe = current_exe()?;
        run_reg(&["add", REG_KEY, "/ve", "/d", "剪贴板历史", "/f"])?;
        run_reg(&["add", REG_KEY, "/v", "Icon", "/d", &exe, "/f"])?;
        run_reg(&[
            "add",
            &format!(r"{REG_KEY}\command"),
            "/ve",
            "/d",
            &format!("\"{exe}\" --history"),
            "/f",
        ])?;
    } else {
        run_reg(&["delete", REG_KEY, "/f"]).ok();
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

/// 快速操作（服务）名称，右键菜单中显示
#[cfg(target_os = "macos")]
const WORKFLOW_NAME: &str = "DeskTools 剪贴板历史.workflow";

#[cfg(target_os = "macos")]
fn services_dir() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|e| format!("无法获取用户目录：{e}"))?;
    Ok(PathBuf::from(home).join("Library/Services"))
}

/// 生成 workflow 的 document.wflow 内容
#[cfg(target_os = "macos")]
fn workflow_document(exe: &str) -> String {
    let script = format!("nohup \"{exe}\" --history >/dev/null 2>&1 &");
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

/// 生成 workflow 的 Info.plist 内容
#[cfg(target_os = "macos")]
fn workflow_info() -> String {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>com.hyy.desktools.quickaction</string>
	<key>CFBundleName</key>
	<string>DeskTools 剪贴板历史</string>
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
"#
    .to_string()
}

/// 把快速操作写入指定目录（供测试与真实安装共用）
#[cfg(target_os = "macos")]
pub fn install_workflow_to(dir: &std::path::Path, exe: &str) -> Result<PathBuf, String> {
    let bundle = dir.join(WORKFLOW_NAME);
    let contents = bundle.join("Contents");
    std::fs::create_dir_all(&contents).map_err(|e| format!("无法创建快速操作目录：{e}"))?;
    std::fs::write(contents.join("Info.plist"), workflow_info())
        .map_err(|e| format!("无法写入 Info.plist：{e}"))?;
    std::fs::write(contents.join("document.wflow"), workflow_document(exe))
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
    let enabled = services_dir()
        .map(|dir| dir.join(WORKFLOW_NAME).exists())
        .unwrap_or(false);
    IntegrationStatus {
        supported: true,
        enabled,
        detail: if enabled {
            "已添加到 Finder 右键 → 快速操作（若未显示，请在「系统设置 → 键盘 → 快捷键 → 服务」中启用）".into()
        } else {
            format!("未启用（启用后安装到 ~/Library/Services，程序路径：{exe}）")
        },
    }
}

#[cfg(target_os = "macos")]
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let dir = services_dir()?;
    if enabled {
        let exe = current_exe()?;
        install_workflow_to(&dir, &exe)?;
    } else {
        let bundle = dir.join(WORKFLOW_NAME);
        if bundle.exists() {
            std::fs::remove_dir_all(&bundle).map_err(|e| format!("无法移除快速操作：{e}"))?;
        }
        let _ = std::process::Command::new("pbs").arg("-update").output();
    }
    Ok(())
}

// ============================ 其他平台 ============================

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

    /// macOS：验证快速操作 workflow 文件结构与内容
    #[cfg(target_os = "macos")]
    #[test]
    fn installs_workflow_into_temp_dir() {
        let dir = std::env::temp_dir().join(format!("desktools-svc-{}", uuid::Uuid::new_v4()));
        let bundle = install_workflow_to(&dir, "/Applications/DeskTools.app/Contents/MacOS/desktools")
            .unwrap();
        assert!(bundle.join("Contents/Info.plist").exists());
        let wflow = std::fs::read_to_string(bundle.join("Contents/document.wflow")).unwrap();
        assert!(wflow.contains("--history"));
        assert!(wflow.contains("com.apple.Automator.servicesMenu"));
        // plist 格式合法
        let check = std::process::Command::new("plutil")
            .arg("-lint")
            .arg(bundle.join("Contents/document.wflow"))
            .output()
            .unwrap();
        assert!(check.status.success(), "{:?}", check);
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
        let bundle = install_workflow_to(&services_dir().unwrap(), &exe).unwrap();
        println!("installed: {}", bundle.display());
    }

    /// 清理真实安装的快速操作
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn e2e_uninstall_real_workflow() {
        let bundle = services_dir().unwrap().join(WORKFLOW_NAME);
        let _ = std::fs::remove_dir_all(&bundle);
        let _ = std::process::Command::new("pbs").arg("-update").output();
        println!("removed: {}", bundle.display());
    }
}
