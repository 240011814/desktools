import { ref } from "vue";

/* 侧边栏与各页面共享的状态与类型 */

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
