import { ref } from "vue";
import type { KeyCombo, KeySyncRule } from "./keysync";

/* 侧边栏与各页面共享的状态与类型 */

/** 按键同步的组合 / 规则类型（语义见 src/keysync.ts） */
export type { KeyCombo, KeySyncRule };

/** 文件中转站（MinIO）配置；Access Key / Secret Key 本地加密存储 */
export interface MinioConfig {
  /** 服务器地址（主机名 / IP，也兼容粘贴 http://ip:9000 形式） */
  host: string;
  port: number;
  /** 存储桶名称（默认 hub，不存在时自动创建） */
  bucket: string;
  tls: boolean;
  accessKey: string;
  secretKey: string;
}

export interface MqttConfig {
  host: string;
  port: number;
  username: string;
  password: string;
  tls: boolean;
  group: string;
  deviceId: string;
  /** 离人自动锁屏：收到 csi/notify 的 leave 事件时自动锁屏 */
  autoLock: boolean;
  /** 收到 leave 后延迟多少秒锁屏（1~300） */
  autoLockDelaySecs: number;
  /** 锁屏同步（收发分离）：本机锁屏 / 解锁事件通过 MQTT 通知同组其他设备 */
  lockSyncPublish: boolean;
  /** 锁屏同步（收发分离）：跟随同组其他设备的锁屏事件（未锁且活跃守卫未触发时才锁定） */
  lockSyncFollow: boolean;
  /** 活跃守卫阈值（秒）：本机近 X 秒内有键鼠输入则跳过远端锁屏；0 = 关闭守卫 */
  lockSyncIdleGuardSecs: number;
  /** 按键同步：总开关（本机按下源组合后广播，收到远端组合按本机规则注入） */
  keySyncEnabled: boolean;
  /** 按键同步规则列表（源组合 → 目标组合；规则需在收发两端各自配置） */
  keySyncRules: KeySyncRule[];
  /** 开机自动启动（持久化意图；由应用启动时与系统实际状态对齐） */
  autoStart: boolean;
  /** 文件中转站（MinIO）配置 */
  minio: MinioConfig;
}

export type MqttState = "idle" | "connecting" | "connected" | "reconnecting" | "error";

export interface MqttStatus {
  state: MqttState;
  message: string;
  topic: string;
}

export interface HistoryItem {
  id: string;
  text: string;
  source: "local" | "remote";
  ts: number;
}

export type LogLevel = "INFO" | "WARN" | "ERROR";

export interface LogEntry {
  ts: number;
  level: LogLevel;
  message: string;
}

export const STATUS_TEXT: Record<MqttState, string> = {
  idle: "未启用",
  connecting: "连接中",
  connected: "已连接",
  reconnecting: "重连中",
  error: "连接失败",
};

/** 当前 MQTT 连接状态（侧边栏徽章与同步页共用） */
export const mqttStatus = ref<MqttStatus>({ state: "idle", message: "", topic: "" });

/** 应用版本号 */
export const version = ref("...");
