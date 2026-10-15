# CLAUDE.md

# 提示词优化规则

**规则说明：**  
在执行任何操作之前，第一步始终是对用户的提示词进行优化，使其更加清晰和有效。

**优化原则：**

- 保持用户原始意图不变
- 仅优化表达清晰度、结构性和可执行性
- 不改变业务逻辑、技术选型或具体需求
- 优化程度：微调为主，避免大幅重写

**执行流程：**

1. **第一步：判断是否需要优化**

   - 如果用户明确要求"不优化"、"直接执行"等，跳过优化步骤
   - 如果提示词已经非常清晰具体，可以跳过优化
   - 其他情况进入优化流程

2. **第二步：优化提示词**

   - 分析用户原始提示词的核心意图
   - 识别模糊、不清晰或可能产生歧义的部分
   - 生成优化后的提示词（更清晰、更具体、更易执行）
   - 记录优化理由和主要改动点
   - 如果是需求开发类的功能，看看有没有比用户所想要的更好的解决方案，如果有列出来，并说明

3. **第三步：展示 A/B/C 选项（仅展示，不执行）**

   - **A：原始提示词** - 使用用户原始提示词执行
   - **B：优化后的提示词** - 使用优化后的提示词执行
   - **C：重新优化** - 根据反馈重新优化提示词，然后再次展示选项
   - **必须完整展示提示词内容**，不要只显示一行摘要或简化版本
   - 使用代码块或引用格式完整展示，确保用户能看到完整的提示词内容
   - **必须说明优化理由**：简要说明为什么这样优化，主要改进了哪些方面

4. **第四步：等待用户选择**

   - **必须等待用户明确选择 A、B 或 C**
   - 如果用户选择 C：
     - 询问用户希望如何调整（更具体、更简洁、关注不同方面等），或根据用户的反馈意见
     - 根据用户反馈重新优化提示词
     - 返回第三步，再次展示 A/B/C 选项
     - 可以多次迭代，直到用户选择 A 或 B
   - 在用户选择之前，**不得执行任何后续操作**
   - 不得进行代码分析、文件读取、工具调用等操作
   - 只能展示选项，等待用户交互

5. **第五步：执行操作**
   - 用户选择 A 或 B 后，使用选定的提示词执行后续操作

**重要约束：**

- ❌ 禁止在展示选项后立即继续执行
- ❌ 禁止在用户未选择时进行任何分析或工具调用
- ❌ 禁止只展示一行摘要或简化版本的提示词
- ❌ 禁止改变用户原始意图和核心需求
- ❌ 禁止过度优化导致提示词面目全非
- ✅ 必须完整展示提示词内容
- ✅ 必须说明优化理由
- ✅ 只能展示选项并明确说明"请选择 A、B 或 C"
- ✅ 用户选择后再继续执行
- ✅ 如果用户选择 C，根据反馈重新优化并再次展示选项

**用户反馈处理：**

- 如果用户选择 C（重新优化），AI 应该：

  1. 询问用户希望如何调整（更具体、更简洁、关注不同方面等），或根据用户提供的具体反馈
  2. 根据用户反馈重新优化提示词
  3. 返回第三步，再次展示 A/B/C 选项供用户选择
  4. 可以多次迭代，直到用户选择 A 或 B 为止

- ✅ 只能展示选项并明确说明"请选择 A 或 B"
- ✅ 用户选择后再继续执行

---

## 项目概述

desktools 是基于 **Tauri 2 + Vue 3 + TypeScript** 的桌面工具箱（macOS / Windows），核心是**跨设备剪贴板同步**：

- **MQTT 剪贴板同步**：配置 MQTT 连接（host / port / 用户名 / 密码 / TLS）与 Group 号后，启动即自动连接并订阅 `texthub/<group号>`；本机复制的文本连同本机 `deviceId` 发布到该 topic，收到其他设备的内容自动写入剪贴板（忽略自身消息防回环）。
- **剪贴板历史**：最近 200 条复制记录（含远端同步、标记来源），支持点击复制 / 单条删除 / 一键清空 / 去重置顶，持久化到 `history.json`。
- **运行日志**：MQTT 连接与收发消息内容、历史读写、剪贴板操作等关键事件（INFO/WARN/ERROR），持久化 JSONL 文件（内存 1000 条、文件 1MB 轮转），日志页支持级别筛选。
- **离人自动锁屏**（可选开关，`config.json` 的 `autoLock`）：开启后在现有 MQTT 连接上额外订阅 `csi/notify`（QoS 1，wol_rs/ESP32 的人体感知通知），收到 `event=leave` 开始倒计时（`autoLockDelaySecs`，默认 15s），**期间收到 `approach` 自动取消，倒计时结束才真正锁屏**；两次锁屏至少间隔 60s。锁屏链路逐级降级并记录原因：macOS `SACLockScreenImmediate`（LoginUIKit 私有接口，真锁屏无需权限）→ Ctrl+Cmd+Q（需辅助功能）→ `pmset displaysleepnow`（等效锁屏需「唤醒时需要密码」）；Windows `LockWorkStation`。
- **系统集成**：桌面右键菜单弹出历史小窗口（Windows 注册表静态菜单项；macOS Finder「快速操作」workflow）；托盘右键菜单含「剪贴板历史 / 显示主窗口 / 退出」。
- **其他**：开机自启（`--hidden` 仅托盘）、关闭最小化到托盘、minisign 验签自动更新（版本号 `1.0.<git 提交数>`）。

## 常用命令

```bash
# 开发（前端 vite + Tauri 窗口）
pnpm install
pnpm tauri dev

# 前端类型检查 + 构建
pnpm build

# 打包（产物在 src-tauri/target/release/bundle/）
pnpm tauri build

# Rust 单元测试（在 src-tauri/ 下执行）
cd src-tauri && cargo test

# 仅编译检查 Rust
cd src-tauri && cargo check
```

## 项目结构

```
src/                          # Vue 3 前端
  App.vue                     # 外壳：侧边栏菜单 + 页面切换（?popup=history 时渲染独立历史小窗口）
  store.ts                    # 共享状态与类型（mqttStatus、version、MqttConfig、HistoryItem、LogEntry）
  styles.css                  # 全局设计变量（深浅色自适应）与通用组件样式（卡片/按钮/表单/开关/徽章/空状态）
  components/
    HistoryPage.vue           # 剪贴板历史（compact 属性 = 小窗口紧凑布局）
    LogPage.vue               # 运行日志（级别筛选 / 刷新 / 清空）
    UpdatePage.vue            # 软件更新检测与安装
    SettingsPage.vue          # 系统设置：MQTT 同步配置 + 桌面右键集成 + 开机自启
src-tauri/
  src/
    lib.rs                    # 入口：托盘菜单、窗口管理、命令注册、单实例、启动参数（--history / --hidden）
    auto_lock.rs              # 离人自动锁屏：csi/notify 的 leave/approach 事件 → 延迟锁屏/取消（冷却 60s）
    clipboard.rs              # 写剪贴板统一入口（写后读回校验 + 竞争重试），copy_to_clipboard 共用
    config.rs                 # MqttConfig 持久化（config.json）与校验
    mqtt.rs                   # MQTT 连接/订阅/收发、剪贴板监听发布（rumqttc + tokio）；收包只入队，后台单线程消费
    history.rs                # 剪贴板历史（200 条上限、去重置顶、history.json）
    logger.rs                 # 运行日志（内存 + JSONL 轮转，preview() 截断长文本）
    shell_integration.rs      # 桌面右键集成（Windows 注册表 / macOS 快速操作 workflow）
  capabilities/desktop.json   # 桌面端插件权限（剪贴板必须显式声明！）
  tauri.conf.json             # 窗口（main / history）与打包配置
```

## 代码规范

- **前端**：Vue 3 `<script setup lang="ts">`；共享状态放 `src/store.ts`（ref + 接口），页面组件放 `src/components/`；通用样式写 `styles.css` 全局类（`.card` `.btn` `.switch` `.hint` `.empty` 等），组件只写 `scoped` 差异样式；与 Rust 通信用 `invoke`（命令）与 `listen`（事件），全部包 try/catch 保证浏览器预览不报错。
- **后端**：每个领域一个模块（config / mqtt / history / logger / shell_integration）；命令集中在 `lib.rs` 注册；事件名形如 `mqtt://status`、`history://changed`、`log://changed`；serde 统一 `#[serde(rename_all = "camelCase")]`，前端接口与之对应。
- **测试**：纯逻辑（校验、去重、轮转、消息解析）写单元测试，触碰系统（AppHandle / 文件 / 注册表）的逻辑抽成可注入路径的纯函数再测。

## 开发经验 / 注意事项

- **剪贴板插件权限是空集**：`clipboard-manager:default` 不含任何权限（插件故意如此），必须在 `capabilities/desktop.json` 显式加 `clipboard-manager:allow-read-text / allow-write-text / allow-clear`，否则前端 `writeText` 报"权限"错误（macOS/Windows 都一样）。前端复制统一走后端命令 `copy_to_clipboard` 更稳。
- **MQTT client id = `desktools-<deviceId>`**：同一 deviceId 起两个实例会互踢连接（broker 端 client id 冲突）。已加 `tauri-plugin-single-instance`，桌面右键/快速操作二次启动会转发 `--history` 给已运行实例。排查时注意别同时跑 `pnpm tauri dev` 和手动启动的二进制。
- **防回环三道闸**：消息带 `deviceId`（自己的忽略）→ 收到远端先更新 `last_clip` 再写剪贴板 → 监听线程发现内容等于 `last_clip` 不再发布/记录。前端点击复制走 `clipboard::copy` 但**不**更新 `last_clip`（由监听线程当新复制发布，保持点击复制也能跨端同步）。
- **监听线程的 TOCTOU 竞态会把过期内容发出去（回流）**：「读剪贴板快照 → 查 `last_clip` → 发布」不是原子的，期间收到新消息改掉 `last_clip` 后，过期快照会被当新复制发布回对端，~700ms 后回流覆盖对端刚同步到的新内容（表象就是"收到 mq 但剪贴板没变"）。已用 `SyncState::with_clip_gate` 把监听线程决策与消息处理线程「更新基准→写剪贴板」互斥，并用 `recent_sent`（5 秒窗口）忽略回流的本机内容。改这段逻辑时必须保持「先拿闸门、再拿内部锁」的加锁顺序。
- **macOS `NSPasteboard` 写入会静默失败**：与其它进程（用户复制、pbcopy、其它 app）并发时 `writeObjects` 间歇性返回 false（alacritty 有大量同类 issue），且 arboard 是 `clearContents` + `writeContents` 两步，失败会丢内容。所以写剪贴板统一走 `clipboard::write_text_verified`（写后读回校验 + 退避重试 4 次），成功/竞争/失败都如实记日志；禁止 `let _ = write_text` 吞错误。
- **MQTT 收包与处理解耦**：收到的消息只入内存队列（`SyncState::enqueue_incoming`，容量 512 满则丢弃记 WARN），由 `mqtt-message-worker` 单线程顺序消费（解析 → 记历史 → 写剪贴板），不在 MQTT 事件循环里做剪贴板/历史 IO，同时保证处理顺序。
- **`arboard` 读剪贴板返回 Err 是常态**（剪贴板里是图片/文件时），直接 `continue` 忽略；监听线程 700ms 轮询。
- **macOS 快速操作**：workflow 装在 `~/Library/Services/DeskTools 剪贴板历史.workflow`，首次可能需在「系统设置 → 键盘 → 快捷键 → 服务」启用；`pbs -update` 刷新缓存。Windows 注册表项在 `HKCU\Software\Classes\Directory\Background\shell\DeskToolsHistory`。
- **配置/历史/日志位置**：`~/Library/Application Support/com.hyy.desktools/`（config.json、history.json），日志在 `~/Library/Logs/com.hyy.desktools/desktools.jsonl`；Windows 在 `%APPDATA%` / `%LOCALAPPDATA%` 对应目录。排查问题可直接删 config.json 重置。
- **Tauri 类实例不能进深度响应式**：`Update`（`@tauri-apps/plugin-updater`）等继承 `Resource` 的类带 `#` 私有字段，放进 Vue `ref()` 被 Proxy 代理后调用方法会报 `TypeError: Cannot read private member from an object whose class did not declare it`（Windows/macOS 同病）。必须用 `shallowRef` 或 `markRaw` 保存类实例；纯数据对象则无此问题。
- **开发时 `tauri dev` 会自动重编译重启**：改 Rust 后用户桌面上的 dev 实例会闪断重连，日志里会出现重连记录，属正常现象。

