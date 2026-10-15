use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, Transport};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::config::MqttConfig;
use crate::history::{now_ms, HistoryState};
use crate::logger::{self, INFO, WARN};

/// 前端监听的连接状态事件
pub const STATUS_EVENT: &str = "mqtt://status";

/// 连接状态（同步给前端展示）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MqttStatus {
    /// idle / connecting / connected / reconnecting / error
    pub state: String,
    pub message: String,
    pub topic: String,
}

/// 消息体：JSON { deviceId, text, ts }
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncMessage {
    pub device_id: String,
    pub text: String,
    #[serde(default)]
    pub ts: i64,
}

/// 全局同步状态
pub struct SyncState {
    pub config: Mutex<MqttConfig>,
    pub status: Mutex<MqttStatus>,
    client: Mutex<Option<AsyncClient>>,
    /// 每次（重）连接自增，用于让旧的事件循环退出
    run_id: AtomicU64,
    /// 最近一次已收/已发的剪贴板文本，用于去重、防回环
    last_clip: Arc<Mutex<String>>,
}

impl SyncState {
    pub fn new(config: MqttConfig) -> Self {
        Self {
            config: Mutex::new(config),
            status: Mutex::new(MqttStatus {
                state: "idle".into(),
                message: String::new(),
                topic: String::new(),
            }),
            client: Mutex::new(None),
            run_id: AtomicU64::new(0),
            last_clip: Arc::new(Mutex::new(String::new())),
        }
    }

    pub fn client(&self) -> Option<AsyncClient> {
        self.client.lock().unwrap().clone()
    }
}

pub fn set_status(app: &AppHandle, state: &str, message: impl Into<String>, topic: String) {
    let status = MqttStatus {
        state: state.into(),
        message: message.into(),
        topic,
    };
    if let Some(sync) = app.try_state::<SyncState>() {
        *sync.status.lock().unwrap() = status.clone();
    }
    let _ = app.emit(STATUS_EVENT, status);
}

/// 断开旧连接并按新配置启动 MQTT 连接（自动订阅 texthub/<group>）
pub fn start(app: AppHandle, cfg: MqttConfig) {
    let sync = app.state::<SyncState>();
    // 让旧事件循环退出：清掉旧 client 后其 poll() 会结束
    let run_id = sync.run_id.fetch_add(1, Ordering::SeqCst) + 1;
    *sync.client.lock().unwrap() = None;

    let topic = cfg.topic();
    if cfg.host.trim().is_empty() || cfg.group.trim().is_empty() {
        set_status(&app, "idle", "请先填写服务器地址与 Group 号", topic);
        logger::log(&app, WARN, "同步未启用：未填写服务器地址或 Group 号");
        return;
    }
    set_status(&app, "connecting", "正在连接…", topic.clone());
    logger::log(
        &app,
        INFO,
        format!("开始连接 MQTT {}:{}（订阅 {}）", cfg.host.trim(), cfg.port, topic),
    );

    tauri::async_runtime::spawn(async move {
        let mut opts = MqttOptions::new(
            format!("desktools-{}", cfg.device_id),
            cfg.host.trim().to_string(),
            cfg.port,
        );
        opts.set_keep_alive(Duration::from_secs(30));
        opts.set_clean_session(true);
        if cfg.tls {
            opts.set_transport(Transport::tls_with_default_config());
        }
        if !cfg.username.trim().is_empty() {
            opts.set_credentials(cfg.username.trim().to_string(), cfg.password.clone());
        }

        let (client, mut eventloop) = AsyncClient::new(opts, 64);
        {
            let sync = app.state::<SyncState>();
            *sync.client.lock().unwrap() = Some(client);
        }

        loop {
            let poll_result = eventloop.poll().await;
            // 期间若已用新配置重启，则退出旧的事件循环
            if app.state::<SyncState>().run_id.load(Ordering::SeqCst) != run_id {
                break;
            }
            match poll_result {
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    // 连接成功后自动订阅
                    if let Some(client) = app.state::<SyncState>().client() {
                        let _ = client.subscribe(topic.clone(), QoS::AtLeastOnce).await;
                        logger::log(&app, INFO, format!("MQTT 已连接，已订阅 {topic}"));
                    }
                    set_status(&app, "connected", "已连接", topic.clone());
                }
                Ok(Event::Incoming(Packet::Publish(publish))) => {
                    handle_incoming(&app, &publish.payload);
                }
                Ok(_) => {}
                Err(err) => {
                    set_status(
                        &app,
                        "reconnecting",
                        format!("连接中断，正在重连：{err}"),
                        topic.clone(),
                    );
                    logger::log(&app, WARN, format!("MQTT 连接中断，正在重连：{err}"));
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }

        // 若仍是当前这轮连接，清理 client
        let sync = app.state::<SyncState>();
        if sync.run_id.load(Ordering::SeqCst) == run_id {
            *sync.client.lock().unwrap() = None;
        }
    });
}

/// 处理收到的消息：非本机发出的内容写入剪贴板
fn handle_incoming(app: &AppHandle, payload: &[u8]) {
    let raw = String::from_utf8_lossy(payload).to_string();
    let Some(text) = parse_payload(&raw, &current_device_id(app)) else {
        return;
    };

    // 先记录再写剪贴板，避免监听线程把它当成新的复制内容又发出去
    *app.state::<SyncState>().last_clip.lock().unwrap() = text.clone();
    // 远端内容同样进入历史记录
    app.state::<HistoryState>().push(app, &text, "remote");
    logger::log(
        app,
        INFO,
        format!(
            "收到远端内容，已写入剪贴板（{} 字符）：{}",
            text.chars().count(),
            logger::preview(&text)
        ),
    );
    #[cfg(desktop)]
    {
        use tauri_plugin_clipboard_manager::ClipboardExt;
        let _ = app.clipboard().write_text(text);
    }
    #[cfg(not(desktop))]
    let _ = text;
}

/// 解析消息内容，返回需要写入剪贴板的文本。
/// - JSON 消息（{ deviceId, text }）：本机发出的返回 None，避免回环
/// - 纯文本消息：视为远端内容，直接返回
fn parse_payload(raw: &str, device_id: &str) -> Option<String> {
    let (text, from_self) = match serde_json::from_str::<SyncMessage>(raw) {
        Ok(msg) => (msg.text, msg.device_id == device_id),
        Err(_) => (raw.to_string(), false),
    };
    if from_self || text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn current_device_id(app: &AppHandle) -> String {
    app.state::<SyncState>()
        .config
        .lock()
        .unwrap()
        .device_id
        .clone()
}

/// 监听本机剪贴板变化，每次复制文本后发布到 texthub/<group>
#[cfg(desktop)]
pub fn spawn_clipboard_watcher(app: AppHandle) {
    use tauri_plugin_clipboard_manager::ClipboardExt;

    let _ = std::thread::Builder::new()
        .name("clipboard-watcher".into())
        .spawn(move || {
            let last_clip = app.state::<SyncState>().last_clip.clone();
            // 启动时的剪贴板内容作为基准：不记录、不对外发送
            let mut last_seen = app.clipboard().read_text().unwrap_or_default();
            *last_clip.lock().unwrap() = last_seen.clone();
            loop {
                std::thread::sleep(Duration::from_millis(700));
                let text = match app.clipboard().read_text() {
                    Ok(text) => text,
                    Err(_) => continue, // 剪贴板里不是文本，忽略
                };
                if text.trim().is_empty() {
                    continue;
                }
                if text != last_seen {
                    last_seen = text.clone();
                    // 与最近收/发内容一致说明是远端写入的，收到时已记录过
                    if *last_clip.lock().unwrap() != text {
                        app.state::<HistoryState>().push(&app, &text, "local");
                    }
                }
                // 已发过/已收过的内容不重复发送；未连接时不更新基准，重连后自动补发
                if *last_clip.lock().unwrap() == text {
                    continue;
                }
                let Some(client) = app.state::<SyncState>().client() else {
                    continue;
                };
                let cfg = app.state::<SyncState>().config.lock().unwrap().clone();
                let preview = logger::preview(&text);
                let payload = serde_json::json!({
                    "deviceId": cfg.device_id,
                    "text": text,
                    "ts": now_ms(),
                })
                .to_string();
                let topic = cfg.topic();
                tauri::async_runtime::spawn(async move {
                    let _ = client.publish(topic, QoS::AtLeastOnce, false, payload).await;
                });
                logger::log(
                    &app,
                    INFO,
                    format!(
                        "已发送同步内容（{} 字符）：{preview}",
                        text.chars().count()
                    ),
                );
                *last_clip.lock().unwrap() = text;
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_message_from_self() {
        let payload = r#"{"deviceId":"me","text":"hello","ts":1}"#;
        assert_eq!(parse_payload(payload, "me"), None);
    }

    #[test]
    fn accepts_message_from_other_device() {
        let payload = r#"{"deviceId":"other","text":"hello","ts":1}"#;
        assert_eq!(parse_payload(payload, "me"), Some("hello".into()));
    }

    #[test]
    fn accepts_plain_text_message() {
        assert_eq!(parse_payload("plain text", "me"), Some("plain text".into()));
    }

    #[test]
    fn ignores_empty_text() {
        let payload = r#"{"deviceId":"other","text":"","ts":1}"#;
        assert_eq!(parse_payload(payload, "me"), None);
    }
}
