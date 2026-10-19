use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::Manager;

use crate::fs_atomic;
use crate::secret;

/// MinIO 默认存储桶名（bucket 可配置，留空 / 旧配置缺字段时回落该值）
pub const DEFAULT_BUCKET: &str = "hub";

/// MinIO（文件中转站）配置
///
/// accessKey / secretKey 只在写盘时加密（见 `MqttConfig::save` / `load`，`enc:v1:` 前缀），
/// 内存中与前端交互的始终是明文。host 留空表示未配置 / 已停用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MinioConfig {
    /// 服务器地址（主机名 / IP；也兼容粘贴 `http://ip:9000` 形式，保存时规范化）
    pub host: String,
    /// 端口
    pub port: u16,
    /// 存储桶名称（默认 `hub`，不存在时自动创建）
    pub bucket: String,
    /// 是否使用 TLS（https）
    pub tls: bool,
    /// Access Key
    pub access_key: String,
    /// Secret Key
    pub secret_key: String,
}

impl Default for MinioConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 9000,
            bucket: DEFAULT_BUCKET.to_string(),
            tls: false,
            access_key: String::new(),
            secret_key: String::new(),
        }
    }
}

impl MinioConfig {
    /// 是否已配置（host 为空视为未配置）
    pub fn configured(&self) -> bool {
        !self.host.trim().is_empty()
    }

    /// 规范化后的副本：地址容错解析、去除凭据首尾空白、bucket 留空时回落默认值
    pub fn normalized(&self) -> Self {
        let (host, port, tls) = normalize_endpoint(&self.host, self.port, self.tls);
        let bucket = self.bucket.trim();
        Self {
            host,
            port,
            bucket: if bucket.is_empty() {
                DEFAULT_BUCKET.to_string()
            } else {
                bucket.to_string()
            },
            tls,
            access_key: self.access_key.trim().to_string(),
            secret_key: self.secret_key.trim().to_string(),
        }
    }

    /// 校验（调用前应先 `normalized()`；host 为空代表清除配置，不在此校验）
    pub fn validate(&self) -> Result<(), String> {
        if self.port == 0 {
            return Err("端口必须在 1 ~ 65535 之间".into());
        }
        validate_bucket(&self.bucket)?;
        if self.access_key.is_empty() || self.secret_key.is_empty() {
            return Err("请填写 Access Key 和 Secret Key".into());
        }
        Ok(())
    }
}

/// 校验 bucket 名（S3 / MinIO 规范）：3 ~ 63 位小写字母、数字、中划线或点，
/// 首尾必须是字母或数字，且不能出现相邻的 `.` / `.-` / `-.` 组合
pub fn validate_bucket(bucket: &str) -> Result<(), String> {
    if !(3..=63).contains(&bucket.len()) {
        return Err("存储桶名需为 3 ~ 63 个字符".into());
    }
    if !bucket
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
    {
        return Err("存储桶名只能包含小写字母、数字、中划线和点".into());
    }
    let alnum = |c: char| c.is_ascii_alphanumeric();
    if !bucket.starts_with(alnum) || !bucket.ends_with(alnum) {
        return Err("存储桶名必须以字母或数字开头和结尾".into());
    }
    if bucket.contains("..") || bucket.contains(".-") || bucket.contains("-.") {
        return Err("存储桶名不能包含相邻的点和中划线组合".into());
    }
    Ok(())
}

/// 容错解析用户填写的 MinIO 服务器地址：
/// 接受 `http://1.2.3.4:9000`、`https://host`、`host:9000`、`host`；
/// scheme 存在时优先决定 tls；host 内显式端口优先于表单端口。
pub fn normalize_endpoint(host: &str, port: u16, tls: bool) -> (String, u16, bool) {
    let mut rest = host.trim();
    let mut tls = tls;
    if let Some(stripped) = rest.strip_prefix("https://") {
        tls = true;
        rest = stripped;
    } else if let Some(stripped) = rest.strip_prefix("http://") {
        tls = false;
        rest = stripped;
    }
    let rest = rest.trim_end_matches('/');
    let (host, parsed_port) = split_host_port(rest);
    (host, parsed_port.unwrap_or(port), tls)
}

/// 拆分 `host:port` / `[IPv6]:port`；无端口或非数字端口时原样保留 host
fn split_host_port(input: &str) -> (String, Option<u16>) {
    let input = input.trim();
    if let Some(rest) = input.strip_prefix('[') {
        // IPv6 字面量：[::1]:9000 / [::1]（保留方括号，URL 里需要）
        if let Some((addr, tail)) = rest.split_once(']') {
            let port = tail.strip_prefix(':').and_then(|p| p.parse::<u16>().ok());
            return (format!("[{addr}]"), port);
        }
        return (input.to_string(), None);
    }
    // 只有一个冒号才视为 host:port（无括号的 IPv6 地址原样保留）
    if input.matches(':').count() == 1
        && let Some((host, port)) = input.split_once(':')
        && let Ok(port) = port.parse::<u16>()
    {
        return (host.to_string(), Some(port));
    }
    (input.to_string(), None)
}

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
    /// 离人自动锁屏：收到 csi/notify 的 leave 事件时自动锁屏
    #[serde(default)]
    pub auto_lock: bool,
    /// 收到 leave 后延迟多少秒锁屏（期间检测到有人回来则取消）
    #[serde(default = "default_auto_lock_delay_secs")]
    pub auto_lock_delay_secs: u64,
    /// 锁屏同步（收发分离）：把本机锁屏 / 解锁事件通过 MQTT（locksync/<group>）通知同组设备。
    /// 与「跟随」拆成两个独立开关——工作机等「正在被使用的设备」可以只广播不跟随。
    #[serde(default)]
    pub lock_sync_publish: bool,
    /// 锁屏同步（收发分离）：跟随同组其他设备的锁屏事件（本机未锁且活跃守卫未触发时才锁定）
    #[serde(default)]
    pub lock_sync_follow: bool,
    /// 活跃守卫阈值（秒）：收到远端锁屏事件时，本机最近这么长时间内有键鼠输入则跳过
    /// （人正在用的设备不被联动锁屏）。0 = 关闭守卫，总是跟随。
    #[serde(default = "default_lock_sync_idle_guard_secs")]
    pub lock_sync_idle_guard_secs: u64,
    /// 开机自动启动（持久化「意图」；应用启动时与系统实际状态对齐，见 autostart 模块）
    #[serde(default = "default_auto_start")]
    pub auto_start: bool,
    /// 文件中转站（MinIO）配置
    #[serde(default)]
    pub minio: MinioConfig,
}

fn default_auto_lock_delay_secs() -> u64 {
    15
}

/// 活跃守卫阈值默认 300 秒：5 分钟内用过这台机器的人，不该被远端锁屏事件锁掉
fn default_lock_sync_idle_guard_secs() -> u64 {
    300
}

fn default_auto_start() -> bool {
    true
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
            auto_lock: false,
            auto_lock_delay_secs: default_auto_lock_delay_secs(),
            lock_sync_publish: false,
            lock_sync_follow: false,
            lock_sync_idle_guard_secs: default_lock_sync_idle_guard_secs(),
            auto_start: default_auto_start(),
            minio: MinioConfig::default(),
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
            Ok(text) => {
                // 版本迁移：旧配置只有单一 lockSync 开关，按值映射到收 / 发两个开关
                // （活跃守卫阈值取默认 300）。新格式保存后不再写 lockSync，映射不再触发。
                let legacy_lock_sync = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v.get("lockSync").and_then(|b| b.as_bool()));
                let mut cfg = serde_json::from_str::<MqttConfig>(&text).unwrap_or_default();
                if let Some(enabled) = legacy_lock_sync {
                    cfg.lock_sync_publish = enabled;
                    cfg.lock_sync_follow = enabled;
                }
                cfg
            }
            Err(_) => MqttConfig::default(),
        };
        // 兼容旧配置 / 手工删除 deviceId 的情况
        if config.device_id.trim().is_empty() {
            config.device_id = uuid::Uuid::new_v4().to_string();
        }
        // 凭据落盘时加密：解密失败（换机器 / 文件被改）按空串处理，表现为需重新填写。
        // MinIO Access / Secret Key：
        config.minio.access_key = secret::decrypt(&config.minio.access_key).unwrap_or_default();
        config.minio.secret_key = secret::decrypt(&config.minio.secret_key).unwrap_or_default();
        // MQTT 密码（无前缀的历史明文原样透传，兼容旧配置）：
        config.password = secret::decrypt(&config.password).unwrap_or_default();
        config
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("无法创建配置目录：{e}"))?;
        }
        // 凭据在写盘前加密（内存与前端接口保持明文）：MinIO Access / Secret Key、MQTT 密码
        let mut persisted = self.clone();
        persisted.minio.access_key = secret::encrypt(&self.minio.access_key)?;
        persisted.minio.secret_key = secret::encrypt(&self.minio.secret_key)?;
        persisted.password = secret::encrypt(&self.password)?;
        let text =
            serde_json::to_string_pretty(&persisted).map_err(|e| format!("配置序列化失败：{e}"))?;
        // 原子写盘：进程被杀时不能留下写了一半的 config.json（读回按损坏处理会重置全部配置）
        fs_atomic::write_text(path, &text).map_err(|e| format!("无法写入配置文件：{e}"))?;
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
        if self.auto_lock && (self.auto_lock_delay_secs == 0 || self.auto_lock_delay_secs > 300) {
            return Err("锁屏延迟需在 1 ~ 300 秒之间".into());
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
    // 测试里按 Default 逐字段赋值是为了直白表达「改这一项」,clippy 建议的
    // 结构体初始化写法在这里反而降低可读性
    #![allow(clippy::field_reassign_with_default)]

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

    #[test]
    fn auto_start_defaults_to_true_for_legacy_config() {
        // 旧版 config.json 没有 autoStart 字段：按默认开启处理，保持升级前后行为一致
        let legacy = r#"{
            "host": "broker.emqx.io",
            "port": 1883,
            "username": "",
            "password": "",
            "tls": false,
            "group": "g1",
            "deviceId": "dev-1"
        }"#;
        let cfg: MqttConfig = serde_json::from_str(legacy).unwrap();
        assert!(cfg.auto_start);
    }

    #[test]
    fn auto_start_roundtrips() {
        let dir = std::env::temp_dir().join(format!("desktools-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.json");
        let mut cfg = MqttConfig::default();
        cfg.auto_start = false;
        cfg.save(&path).unwrap();
        assert!(!MqttConfig::load(&path).auto_start);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_sync_defaults_off_for_legacy_config() {
        // 没有 lockSync / 新字段的历史配置（更早版本）：默认全部关闭，升级后不改变行为
        let legacy = r#"{
            "host": "broker.emqx.io",
            "port": 1883,
            "username": "",
            "password": "",
            "tls": false,
            "group": "g1",
            "deviceId": "dev-1"
        }"#;
        let cfg: MqttConfig = serde_json::from_str(legacy).unwrap();
        assert!(!cfg.lock_sync_publish);
        assert!(!cfg.lock_sync_follow);
        assert_eq!(cfg.lock_sync_idle_guard_secs, 300);
    }

    #[test]
    fn legacy_lock_sync_flag_migrates_to_two_switches() {
        // 旧版单开关 lockSync=true：迁移为广播 + 跟随都开（守卫阈值取默认，防误伤直接生效）
        // 迁移发生在 MqttConfig::load 里，所以这里走文件读写而非直接反序列化
        let dir = std::env::temp_dir().join(format!("desktools-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.json");
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            &path,
            r#"{"host":"a","port":1883,"username":"","password":"","tls":false,"group":"g","deviceId":"d","lockSync":true}"#,
        )
        .unwrap();
        let cfg_on = MqttConfig::load(&path);
        assert!(cfg_on.lock_sync_publish);
        assert!(cfg_on.lock_sync_follow);
        assert_eq!(cfg_on.lock_sync_idle_guard_secs, 300);

        std::fs::write(
            &path,
            r#"{"host":"a","port":1883,"username":"","password":"","tls":false,"group":"g","deviceId":"d","lockSync":false}"#,
        )
        .unwrap();
        let cfg_off = MqttConfig::load(&path);
        assert!(!cfg_off.lock_sync_publish);
        assert!(!cfg_off.lock_sync_follow);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_sync_roundtrips() {
        let dir = std::env::temp_dir().join(format!("desktools-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.json");
        let mut cfg = MqttConfig::default();
        cfg.host = "broker.emqx.io".into();
        cfg.group = "g1".into();
        cfg.lock_sync_publish = true;
        cfg.lock_sync_follow = false;
        cfg.lock_sync_idle_guard_secs = 120;
        cfg.save(&path).unwrap();
        let loaded = MqttConfig::load(&path);
        assert!(loaded.lock_sync_publish);
        assert!(!loaded.lock_sync_follow);
        assert_eq!(loaded.lock_sync_idle_guard_secs, 120);

        // 新格式文件不再写旧版 lockSync 字段（避免再次触发迁移覆盖新开关联动）
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("\"lockSync\":"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn normalize_endpoint_accepts_common_formats() {
        // 粘贴完整 URL：scheme 决定 tls，URL 内端口覆盖表单端口
        assert_eq!(
            normalize_endpoint("http://192.168.1.5:9000", 9000, true),
            ("192.168.1.5".into(), 9000, false)
        );
        assert_eq!(
            normalize_endpoint("https://minio.example.com", 9000, false),
            ("minio.example.com".into(), 9000, true)
        );
        // 纯 host 原样保留；host:port 覆盖端口
        assert_eq!(
            normalize_endpoint("play.min.io", 9000, false),
            ("play.min.io".into(), 9000, false)
        );
        assert_eq!(
            normalize_endpoint("192.168.1.5:9010", 9000, false),
            ("192.168.1.5".into(), 9010, false)
        );
        // 尾部斜杠、空格容错；IPv6 带方括号形式
        assert_eq!(
            normalize_endpoint(" minio.example.com/ ", 9000, false),
            ("minio.example.com".into(), 9000, false)
        );
        assert_eq!(
            normalize_endpoint("[::1]:9000", 9000, false),
            ("[::1]".into(), 9000, false)
        );
    }

    #[test]
    fn minio_defaults_and_legacy_compat() {
        // 旧版 config.json 没有 minio 字段：默认未配置，端口 9000
        let legacy = r#"{
            "host": "broker.emqx.io",
            "port": 1883,
            "username": "",
            "password": "",
            "tls": false,
            "group": "g1",
            "deviceId": "dev-1"
        }"#;
        let cfg: MqttConfig = serde_json::from_str(legacy).unwrap();
        assert!(!cfg.minio.configured());
        assert_eq!(cfg.minio.port, 9000);
        // 旧配置缺 bucket 字段 → 默认 hub
        assert_eq!(cfg.minio.bucket, "hub");

        // minio 内部字段缺失时应拿到默认端口（MinioConfig 容器级 default）
        let partial = r#"{
            "host": "broker.emqx.io",
            "port": 1883,
            "username": "",
            "password": "",
            "tls": false,
            "group": "g1",
            "deviceId": "dev-1",
            "minio": { "host": "minio.local" }
        }"#;
        let cfg: MqttConfig = serde_json::from_str(partial).unwrap();
        assert!(cfg.minio.configured());
        assert_eq!(cfg.minio.port, 9000);
        assert!(!cfg.minio.tls);
    }

    #[test]
    fn minio_credentials_are_encrypted_in_file() {
        let dir = std::env::temp_dir().join(format!("desktools-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.json");
        let mut cfg = MqttConfig::default();
        cfg.minio = MinioConfig {
            host: "minio.local".into(),
            port: 9000,
            tls: false,
            access_key: "AK-test-123".into(),
            secret_key: "SK-test-456".into(),
            ..MinioConfig::default()
        };
        cfg.save(&path).unwrap();

        // 文件内容：明文凭据不能出现，密文带 enc:v1: 前缀
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("AK-test-123"));
        assert!(!text.contains("SK-test-456"));
        assert!(text.contains("enc:v1:"));

        // 读回后恢复明文
        let loaded = MqttConfig::load(&path);
        assert_eq!(loaded.minio.access_key, "AK-test-123");
        assert_eq!(loaded.minio.secret_key, "SK-test-456");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn minio_credentials_stay_plaintext_in_memory_serialization() {
        // 前端接口（get_mqtt_config / save_mqtt_config 的返回值）走 serde 序列化：
        // 必须拿到明文，加密只发生在写盘时（save）
        let mut cfg = MqttConfig::default();
        cfg.minio = MinioConfig {
            host: "minio.local".into(),
            port: 9000,
            tls: false,
            access_key: "AK-plain".into(),
            secret_key: "SK-plain".into(),
            ..MinioConfig::default()
        };
        cfg.password = "MQTT-plain".into();
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("AK-plain"));
        assert!(json.contains("SK-plain"));
        assert!(json.contains("MQTT-plain"));
        assert!(!json.contains("enc:v1:"));
    }

    #[test]
    fn mqtt_password_is_encrypted_in_file() {
        let dir = std::env::temp_dir().join(format!("desktools-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.json");
        let mut cfg = MqttConfig::default();
        cfg.host = "broker.emqx.io".into();
        cfg.group = "g1".into();
        cfg.password = "p@ss w0rd".into();
        cfg.save(&path).unwrap();

        // 文件内容：明文密码不能出现
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("p@ss w0rd"));
        assert!(text.contains("enc:v1:"));

        // 读回后恢复明文
        let loaded = MqttConfig::load(&path);
        assert_eq!(loaded.password, "p@ss w0rd");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_plaintext_password_still_loads() {
        // 升级前的 config.json 里 password 是明文（无 enc:v1: 前缀）：
        // MqttConfig::load 走 secret::decrypt 读取，无前缀应按明文透传
        let legacy = r#"{
            "host": "broker.emqx.io",
            "port": 1883,
            "username": "u",
            "password": "old-plain-pass",
            "tls": false,
            "group": "g1",
            "deviceId": "dev-1"
        }"#;
        let cfg: MqttConfig = serde_json::from_str(legacy).unwrap();
        assert_eq!(secret::decrypt(&cfg.password).unwrap(), "old-plain-pass");
    }

    #[test]
    fn minio_validate_requires_credentials_and_port() {
        let mut minio = MinioConfig {
            host: "minio.local".into(),
            ..MinioConfig::default()
        };
        assert!(minio.validate().is_err());
        minio.access_key = " ak ".into();
        minio.secret_key = " sk ".into();
        assert!(minio.normalized().validate().is_ok());
        minio.port = 0;
        assert!(minio.validate().is_err());
        minio.port = 9000;
        minio.bucket = "Bad_Bucket".into();
        assert!(minio.validate().is_err());
    }

    #[test]
    fn minio_bucket_defaults_and_validation() {
        // 默认 / 留空 → 回落 hub；首尾空白在规范化时去掉
        assert_eq!(MinioConfig::default().bucket, DEFAULT_BUCKET);
        let mut minio = MinioConfig::default();
        minio.bucket = "   ".into();
        assert_eq!(minio.normalized().bucket, "hub");
        minio.bucket = " team-files.v2 ".into();
        assert_eq!(minio.normalized().bucket, "team-files.v2");

        for ok in ["hub", "abc", "team-files", "a1.b2"] {
            assert!(validate_bucket(ok).is_ok(), "{ok} 应为合法 bucket 名");
        }
        for bad in [
            "",
            "ab",
            "Hub",
            "bucket name",
            "bucket_1",
            "-abc",
            "abc-",
            "a..b",
            "a.-b",
            "a-.b",
        ] {
            assert!(validate_bucket(bad).is_err(), "{bad} 应为非法 bucket 名");
        }
    }

    #[test]
    fn minio_bucket_roundtrips() {
        let dir = std::env::temp_dir().join(format!("desktools-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.json");
        let mut cfg = MqttConfig::default();
        cfg.minio = MinioConfig {
            host: "minio.local".into(),
            bucket: "team-files".into(),
            access_key: "ak".into(),
            secret_key: "sk".into(),
            ..MinioConfig::default()
        };
        cfg.save(&path).unwrap();
        assert_eq!(MqttConfig::load(&path).minio.bucket, "team-files");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
