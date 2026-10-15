use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::Manager;

/// MQTT 剪贴板同步配置（持久化到应用配置目录）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MqttConfig {
    /// MQTT 服务器地址
    pub host: String,
    /// MQTT 服务器端口
    pub port: u16,
    /// 用户名（可选）
    pub username: String,
    /// 密码（可选）
    pub password: String,
    /// 是否使用 TLS 加密连接
    pub tls: bool,
    /// 分组号，同一分组内的设备互相同步
    pub group: String,
    /// 本机唯一标识，用于区分消息来源
    pub device_id: String,
}

impl Default for MqttConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 1883,
            username: String::new(),
            password: String::new(),
            tls: false,
            group: String::new(),
            device_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

impl MqttConfig {
    /// 同步所用 topic：texthub/<group号>
    pub fn topic(&self) -> String {
        format!("texthub/{}", self.group.trim())
    }

    pub fn load(path: &Path) -> Self {
        let mut config = match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str::<MqttConfig>(&text).unwrap_or_default(),
            Err(_) => MqttConfig::default(),
        };
        // 兼容旧配置 / 手工删除 deviceId 的情况
        if config.device_id.trim().is_empty() {
            config.device_id = uuid::Uuid::new_v4().to_string();
        }
        config
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("无法创建配置目录：{e}"))?;
        }
        let text =
            serde_json::to_string_pretty(self).map_err(|e| format!("配置序列化失败：{e}"))?;
        fs::write(path, text).map_err(|e| format!("无法写入配置文件：{e}"))?;
        Ok(())
    }

    /// 校验用户输入，返回错误信息
    pub fn validate(&self) -> Result<(), String> {
        if self.host.trim().is_empty() {
            return Err("请填写 MQTT 服务器地址".into());
        }
        if self.port == 0 {
            return Err("端口必须在 1 ~ 65535 之间".into());
        }
        let group = self.group.trim();
        if group.is_empty() {
            return Err("请填写 Group 号".into());
        }
        if !group
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err("Group 号只能包含字母、数字、中划线和下划线".into());
        }
        Ok(())
    }
}

/// 配置文件路径：<app_config_dir>/config.json
pub fn config_path(app: &tauri::AppHandle) -> PathBuf {
    app.path()
        .app_config_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("config.json")
}

/// 剪贴板历史文件路径：<app_config_dir>/history.json
pub fn history_path(app: &tauri::AppHandle) -> PathBuf {
    app.path()
        .app_config_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("history.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_contains_group() {
        let mut cfg = MqttConfig::default();
        cfg.group = " my-group ".into();
        assert_eq!(cfg.topic(), "texthub/my-group");
    }

    #[test]
    fn validate_rejects_empty_host_or_group() {
        let cfg = MqttConfig::default();
        assert!(cfg.validate().is_err());

        let mut cfg = MqttConfig::default();
        cfg.host = "broker.emqx.io".into();
        assert!(cfg.validate().is_err());

        cfg.group = "bad group!".into();
        assert!(cfg.validate().is_err());

        cfg.group = "ok_group-1".into();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn roundtrip_save_and_load() {
        let dir = std::env::temp_dir().join(format!("desktools-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.json");
        let mut cfg = MqttConfig::default();
        cfg.host = "broker.emqx.io".into();
        cfg.group = "g1".into();
        cfg.save(&path).unwrap();
        let loaded = MqttConfig::load(&path);
        assert_eq!(loaded.host, "broker.emqx.io");
        assert_eq!(loaded.group, "g1");
        assert_eq!(loaded.device_id, cfg.device_id);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
