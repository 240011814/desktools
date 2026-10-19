use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, Transport};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::auto_lock::{self, AutoLockState};
use crate::clipboard;
use crate::config::MqttConfig;
use crate::history::{HistoryState, now_ms};
use crate::lock_sync::{self, LockSyncState};
use crate::logger::{self, ERROR, INFO, WARN};

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

/// 收到消息的内存队列容量（超出后丢弃并记 WARN，避免慢消费拖垮 MQTT 事件循环）
const INCOMING_QUEUE_CAPACITY: usize = 512;

/// 单条同步文本的字节上限：超限则不同步、不入史。
///
/// - 过大 payload 会被 broker 拒收甚至断开连接（EMQX 默认 1MB），表象是复制大段
///   内容后本机进入无限重连；
/// - history.json 会随单条记录一起膨胀，历史里也没有任何场景需要放 0.5MB 文本。
pub const MAX_TEXT_BYTES: usize = 512 * 1024;

/// 入队门槛（对 raw JSON 信封）：超出直接丢弃，不做无谓的解析与内存拷贝。
/// 文本上限的 4 倍，覆盖 JSON 转义（引号 / 换行 / 控制字符转义与信封字段）的膨胀。
pub const MAX_PAYLOAD_BYTES: usize = MAX_TEXT_BYTES * 4;

/// 消息队列中的待处理消息（按订阅 topic 分型）
#[derive(Debug, PartialEq)]
pub enum QueuedMsg {
    /// 剪贴板同步消息（texthub/<group>）
    Clip(String),
    /// CSI 人体感知消息（csi/notify，自动锁屏用）
    Csi(String),
    /// 锁屏同步消息（locksync/<group>）
    LockSync(String),
}
/// 本机发布内容的回流识别：只保留最近若干条、数十秒内有效。
/// 回流链路 = 本机发布 → 对端收到（网络延迟）→ 对端监听线程周期（≤700ms）→ 回发 → 本机收到，
/// 带宽紧张时可到数秒，故窗口取 10s。副作用：10s 内跨设备重复复制同一文本会被当作回流忽略，
/// 但内容相同，只是这一次同步不落剪贴板，可接受。
const RECENT_SENT_CAP: usize = 16;
const RECENT_ECHO_WINDOW_MS: i64 = 10_000;

/// 全局同步状态
pub struct SyncState {
    pub config: Mutex<MqttConfig>,
    pub status: Mutex<MqttStatus>,
    client: Mutex<Option<AsyncClient>>,
    /// 每次（重）连接自增，用于让旧的事件循环退出
    run_id: AtomicU64,
    /// 最近一次已收/已发的剪贴板文本，用于去重、防回环
    last_clip: Arc<Mutex<String>>,
    /// 剪贴板决策闸门：监听线程「读剪贴板 → 判定 → 发布 → 更新基准」与
    /// 消息处理线程「更新基准 → 写剪贴板」互斥，杜绝过期快照被当新复制发布出去（回流）
    clip_gate: Mutex<()>,
    /// 最近发布出去的（文本, 时间），用于识别对端回流回来的本机内容
    recent_sent: Mutex<VecDeque<(String, i64)>>,
    /// 收到的 MQTT 消息入队，由后台单线程顺序处理
    incoming_tx: SyncSender<QueuedMsg>,
    /// 仅允许取一次：消费线程启动时取走 Receiver
    incoming_rx: Mutex<Option<Receiver<QueuedMsg>>>,
}

impl SyncState {
    pub fn new(config: MqttConfig) -> Self {
        let (incoming_tx, incoming_rx) = sync_channel(INCOMING_QUEUE_CAPACITY);
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
            clip_gate: Mutex::new(()),
            recent_sent: Mutex::new(VecDeque::new()),
            incoming_tx,
            incoming_rx: Mutex::new(Some(incoming_rx)),
        }
    }

    pub fn client(&self) -> Option<AsyncClient> {
        self.client.lock().unwrap().clone()
    }

    /// 更新防回环基准：最近一次已收/已发/手动复制的剪贴板文本
    pub fn set_last_clip(&self, text: &str) {
        *self.last_clip.lock().unwrap() = text.to_string();
    }

    /// 串行化剪贴板相关决策（见 `clip_gate` 字段说明）。
    /// 约定：所有调用方都是先拿闸门、再拿内部锁，闭包内取其它锁不会死锁。
    pub fn with_clip_gate<R>(&self, f: impl FnOnce() -> R) -> R {
        let _guard = self.clip_gate.lock().unwrap();
        f()
    }

    /// 记录一条已发布内容（识别回流用）
    pub fn mark_sent(&self, text: &str) {
        self.mark_sent_at(text, now_ms());
    }

    fn mark_sent_at(&self, text: &str, ts: i64) {
        let mut recent = self.recent_sent.lock().unwrap();
        recent.push_back((text.to_string(), ts));
        while recent.len() > RECENT_SENT_CAP {
            recent.pop_front();
        }
    }

    /// 是否是本机最近发布内容的回流（对端防回环失效时会把自己的内容原样发回来）
    pub fn is_recent_echo(&self, text: &str) -> bool {
        self.is_recent_echo_at(text, now_ms())
    }

    fn is_recent_echo_at(&self, text: &str, now: i64) -> bool {
        self.recent_sent
            .lock()
            .unwrap()
            .iter()
            .any(|(sent, ts)| sent == text && now - *ts <= RECENT_ECHO_WINDOW_MS)
    }

    /// 收到的 MQTT 消息入队（队列满时丢弃，由调用方记日志）
    pub fn enqueue_incoming(&self, msg: QueuedMsg) -> Result<(), String> {
        self.incoming_tx
            .try_send(msg)
            .map_err(|e| format!("消息入队失败：{e}"))
    }

    /// 取走队列接收端（幂等，仅首次成功）
    pub fn take_incoming_receiver(&self) -> Option<Receiver<QueuedMsg>> {
        self.incoming_rx.lock().unwrap().take()
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
        format!(
            "开始连接 MQTT {}:{}（订阅 {}）",
            cfg.host.trim(),
            cfg.port,
            topic
        ),
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
                        // 离人自动锁屏开启时，额外订阅 CSI 人体感知主题
                        if cfg.auto_lock {
                            let _ = client
                                .subscribe(auto_lock::CSI_NOTIFY_TOPIC, QoS::AtLeastOnce)
                                .await;
                            logger::log(
                                &app,
                                INFO,
                                format!("自动锁屏已开启，已订阅 {}", auto_lock::CSI_NOTIFY_TOPIC),
                            );
                        }
                        // 锁屏同步开启时，额外订阅 locksync/<group>
                        if cfg.lock_sync {
                            let lock_topic = lock_sync::topic(&cfg.group);
                            let _ = client.subscribe(lock_topic.clone(), QoS::AtLeastOnce).await;
                            logger::log(&app, INFO, format!("锁屏同步已开启，已订阅 {lock_topic}"));
                        }
                    }
                    set_status(&app, "connected", "已连接", topic.clone());
                }
                Ok(Event::Incoming(Packet::Publish(publish))) => {
                    handle_incoming(&app, &publish.topic, &publish.payload);
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

/// 收到消息：按 topic 分型入队，处理交给后台单线程（不在 MQTT 事件循环里做剪贴板/历史/锁屏 IO）
fn handle_incoming(app: &AppHandle, topic: &str, payload: &[u8]) {
    // 超大消息在队列门外直接丢弃：解析（from_utf8_lossy 会整份拷贝）和入队都省掉
    if payload.len() > MAX_PAYLOAD_BYTES {
        logger::log(
            app,
            WARN,
            format!(
                "MQTT 消息过大（{} 字节 > {} KB），已丢弃：{topic}",
                payload.len(),
                MAX_PAYLOAD_BYTES / 1024
            ),
        );
        return;
    }
    let raw = String::from_utf8_lossy(payload).to_string();
    let msg = if topic == auto_lock::CSI_NOTIFY_TOPIC {
        QueuedMsg::Csi(raw)
    } else if topic == lock_sync::topic(&current_group(app)) {
        QueuedMsg::LockSync(raw)
    } else {
        QueuedMsg::Clip(raw)
    };
    if let Err(err) = app.state::<SyncState>().enqueue_incoming(msg) {
        logger::log(app, WARN, format!("{err}，消息已丢弃"));
    }
}

/// 启动后台消息处理线程：单线程顺序消费队列
/// （剪贴板消息：解析 → 记历史 → 写剪贴板；CSI 消息：自动锁屏判断）
pub fn spawn_message_worker(app: AppHandle) {
    let Some(rx) = app.state::<SyncState>().take_incoming_receiver() else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("mqtt-message-worker".into())
        .spawn(move || {
            while let Ok(msg) = rx.recv() {
                match msg {
                    QueuedMsg::Clip(raw) => process_incoming(&app, &raw),
                    QueuedMsg::Csi(raw) => app.state::<AutoLockState>().handle_event(&app, &raw),
                    QueuedMsg::LockSync(raw) => {
                        app.state::<LockSyncState>().handle_event(&app, &raw)
                    }
                }
            }
        });
}

/// 处理一条消息：非本机发出的内容写入剪贴板
fn process_incoming(app: &AppHandle, raw: &str) {
    let Some(text) = parse_payload(raw, &current_device_id(app)) else {
        return;
    };
    // 超限的远端内容不写剪贴板也不入史（正常情况下对端的同步上限会先拦住，这里兜底）
    if is_oversized_text(&text) {
        logger::log(
            app,
            WARN,
            format!(
                "忽略超大远端内容（{} 字符 > {} KB）",
                text.chars().count(),
                MAX_TEXT_BYTES / 1024
            ),
        );
        return;
    }

    let sync = app.state::<SyncState>();
    // 对端防回环失效时，本机发布的内容会被原样回流回来；
    // 若不拦截，回流的旧内容会在几百毫秒后覆盖本机刚同步到的新内容
    if sync.is_recent_echo(&text) {
        logger::log(
            app,
            INFO,
            format!(
                "忽略回流的本机内容（{} 字符）：{}",
                text.chars().count(),
                logger::preview(&text)
            ),
        );
        return;
    }

    sync.with_clip_gate(|| {
        // 先记录再写剪贴板，避免监听线程把它当成新的复制内容又发出去
        sync.set_last_clip(&text);
        // 远端内容同样进入历史记录
        app.state::<HistoryState>().push(app, &text, "remote");
        // 写剪贴板（读回校验 + 竞争重试），结果如实记日志
        let chars = text.chars().count();
        let preview = logger::preview(&text);
        match clipboard::write_text_verified(&text) {
            Ok(1) => logger::log(app, INFO, format!("收到远端内容，已写入剪贴板（{chars} 字符）：{preview}")),
            Ok(attempts) => logger::log(
                app,
                WARN,
                format!("收到远端内容，写入剪贴板时发生竞争，第 {attempts} 次尝试成功（{chars} 字符）：{preview}"),
            ),
            Err(err) => logger::log(
                app,
                ERROR,
                format!("收到远端内容，但写入剪贴板失败（{chars} 字符）：{preview}，原因：{err}"),
            ),
        }
    });
}

/// 超过同步上限的内容（字节数）：超限不同步、不入史，与 history::push 的兜底护栏保持一致
fn is_oversized_text(text: &str) -> bool {
    text.len() > MAX_TEXT_BYTES
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

fn current_group(app: &AppHandle) -> String {
    app.state::<SyncState>()
        .config
        .lock()
        .unwrap()
        .group
        .clone()
}

/// 监听本机剪贴板变化，每次复制文本后发布到 texthub/<group>
#[cfg(desktop)]
pub fn spawn_clipboard_watcher(app: AppHandle) {
    let _ = std::thread::Builder::new()
        .name("clipboard-watcher".into())
        .spawn(move || {
            let last_clip = app.state::<SyncState>().last_clip.clone();
            // 启动时的剪贴板内容作为基准：不记录、不对外发送
            let mut last_seen = clipboard::read_text().unwrap_or_default();
            *last_clip.lock().unwrap() = last_seen.clone();
            loop {
                std::thread::sleep(Duration::from_millis(700));
                // 整段决策放在剪贴板闸门内：与消息处理线程的「更新基准 → 写剪贴板」互斥，
                // 防止读到的过期快照被当成新复制发布出去（回流的旧内容会覆盖对端刚同步的新内容）
                app.state::<SyncState>().with_clip_gate(|| {
                    let text = match clipboard::read_text() {
                        Ok(text) => text,
                        Err(_) => return, // 剪贴板里不是文本，忽略
                    };
                    if text.trim().is_empty() {
                        return;
                    }
                    // 超限内容不同步也不入史：会撞 broker 报文上限导致断线重连，
                    // history.json 也会被单条撑大。更新 last_seen 避免每个轮询周期重复告警。
                    if text.len() > MAX_TEXT_BYTES {
                        if text != last_seen {
                            last_seen = text.clone();
                            logger::log(
                                &app,
                                WARN,
                                format!(
                                    "复制的内容超过同步上限（{} 字符 > {} KB），不同步也不同入历史",
                                    text.chars().count(),
                                    MAX_TEXT_BYTES / 1024
                                ),
                            );
                        }
                        return;
                    }
                    let sync = app.state::<SyncState>();
                    if text != last_seen {
                        last_seen = text.clone();
                        // 与最近收/发内容一致说明是远端写入的，收到时已记录过
                        if *last_clip.lock().unwrap() != text {
                            app.state::<HistoryState>().push(&app, &text, "local");
                        }
                    }
                    // 已发过/已收过的内容不重复发送；未连接时不更新基准，重连后自动补发
                    if *last_clip.lock().unwrap() == text {
                        return;
                    }
                    let Some(client) = sync.client() else {
                        return;
                    };
                    let cfg = sync.config.lock().unwrap().clone();
                    let preview = logger::preview(&text);
                    let payload = serde_json::json!({
                        "deviceId": cfg.device_id,
                        "text": text,
                        "ts": now_ms(),
                    })
                    .to_string();
                    let topic = cfg.topic();
                    tauri::async_runtime::spawn(async move {
                        let _ = client
                            .publish(topic, QoS::AtLeastOnce, false, payload)
                            .await;
                    });
                    logger::log(
                        &app,
                        INFO,
                        format!("已发送同步内容（{} 字符）：{preview}", text.chars().count()),
                    );
                    *last_clip.lock().unwrap() = text.clone();
                    // 记录已发布内容：对端把它原样回流回来时直接忽略
                    sync.mark_sent(&text);
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queues_incoming_messages_in_order() {
        let sync = SyncState::new(MqttConfig::default());
        sync.enqueue_incoming(QueuedMsg::Clip("first".into()))
            .unwrap();
        sync.enqueue_incoming(QueuedMsg::Csi("second".into()))
            .unwrap();
        sync.enqueue_incoming(QueuedMsg::LockSync("third".into()))
            .unwrap();
        let rx = sync.take_incoming_receiver().unwrap();
        assert_eq!(rx.recv().unwrap(), QueuedMsg::Clip("first".into()));
        assert_eq!(rx.recv().unwrap(), QueuedMsg::Csi("second".into()));
        assert_eq!(rx.recv().unwrap(), QueuedMsg::LockSync("third".into()));
        // 接收端只能取一次
        assert!(sync.take_incoming_receiver().is_none());
    }

    #[test]
    fn detects_recent_echo_of_sent_text() {
        let sync = SyncState::new(MqttConfig::default());
        sync.mark_sent_at("hello", 1_000);
        assert!(sync.is_recent_echo_at("hello", 3_000));
        // 超出回流识别窗口后不再拦截
        assert!(!sync.is_recent_echo_at("hello", 1_000 + RECENT_ECHO_WINDOW_MS + 1));
        // 没发过的内容不拦
        assert!(!sync.is_recent_echo_at("other", 3_000));
    }

    #[test]
    fn keeps_only_recent_sent_items() {
        let sync = SyncState::new(MqttConfig::default());
        let last = RECENT_SENT_CAP as i64 + 4;
        for i in 0..=last {
            sync.mark_sent_at(&format!("item-{i}"), i);
        }
        // 最早的已被淘汰
        assert!(!sync.is_recent_echo_at("item-0", last));
        // 最新的仍在
        assert!(sync.is_recent_echo_at(&format!("item-{last}"), last));
    }

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

    #[test]
    fn oversized_text_is_detected() {
        let small = "a".repeat(MAX_TEXT_BYTES);
        let big = "a".repeat(MAX_TEXT_BYTES + 1);
        assert!(!is_oversized_text(&small));
        assert!(is_oversized_text(&big));
    }

    #[test]
    fn payload_cap_admits_text_up_to_sync_limit() {
        // 合法上限内的文本（含全换行这类转义膨胀最大的输入），其 JSON 信封
        // 不应超过入队门槛——否则「同步上限内」的文本会在队列门外被误丢。
        let text = "\n".repeat(MAX_TEXT_BYTES);
        let payload = serde_json::json!({ "deviceId": "d", "text": text, "ts": 0 }).to_string();
        assert!(
            payload.len() <= MAX_PAYLOAD_BYTES,
            "payload {} > {}",
            payload.len(),
            MAX_PAYLOAD_BYTES
        );
        assert!(payload.len() > MAX_PAYLOAD_BYTES / 2); // 门槛确实兜住了转义膨胀
    }
}
