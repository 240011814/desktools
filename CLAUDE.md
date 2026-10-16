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
- **文件中转站**：通过 MinIO（S3 兼容）在多台设备间上传 / 下载 / 删除文件，bucket 可配置（默认 `hub`，不存在时自动创建，名称为 3~63 位小写字母、数字、中划线或点）；服务器地址、端口、存储桶、Access Key / Secret Key 在「系统设置 → 文件中转站」配置，凭据机器绑定加密后存本机（`enc:v1:` 密文，换机器需重填）。大文件自动分片、流式传输并显示进度，下载失败自动清理半成品文件。支持拖拽文件到页面直接上传与「粘贴图片」（截图 / 复制图片后一键转 PNG 上传；也可 Ctrl+V）。桌面右键 / 托盘菜单可弹出独立的文件中转站小窗口。
- **运行日志**：MQTT 连接与收发消息内容、历史读写、剪贴板操作等关键事件（INFO/WARN/ERROR），持久化 JSONL 文件（内存 1000 条、文件 1MB 轮转），日志页支持级别筛选。
- **离人自动锁屏**（可选开关，`config.json` 的 `autoLock`）：开启后在现有 MQTT 连接上额外订阅 `csi/notify`（QoS 1，wol_rs/ESP32 的人体感知通知），收到 `event=leave` 开始倒计时（`autoLockDelaySecs`，默认 15s），**期间收到 `approach` 自动取消，倒计时结束才真正锁屏**；两次锁屏至少间隔 60s。锁屏链路逐级降级并记录原因：macOS `SACLockScreenImmediate`（LoginUIKit 私有接口，真锁屏无需权限）→ Ctrl+Cmd+Q（需辅助功能）→ `pmset displaysleepnow`（等效锁屏需「唤醒时需要密码」）；Windows `LockWorkStation`。**启动时做锁屏能力自检**（SAC 可用性 / 辅助功能 / 自动化 / 唤醒密码），结果记日志并在设置页展示。
- **系统集成**：桌面右键菜单弹出历史 / 文件中转站小窗口（Windows 注册表静态菜单项；macOS Finder「快速操作」workflow，两条目统一开关管理）；托盘右键菜单含「剪贴板历史 / 文件中转站 / 显示主窗口 / 退出」。
- **其他**：开机自启（意图持久化到 `config.json` 的 `autoStart`，启动时与系统状态对齐；`--hidden` 仅托盘、debug 构建不管理）、关闭最小化到托盘、minisign 验签自动更新（版本号 `1.0.<git 提交数>`）。

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
  App.vue                     # 外壳：侧边栏菜单 + 页面切换（?popup=history / ?popup=files 时渲染对应独立小窗口）
  store.ts                    # 共享状态与类型（mqttStatus、version、MqttConfig、HistoryItem、LogEntry）
  styles.css                  # 全局设计变量（深浅色自适应）与通用组件样式（卡片/按钮/表单/开关/徽章/空状态）
  components/
    HistoryPage.vue           # 剪贴板历史（compact 属性 = 小窗口紧凑布局）
    FileHubPage.vue           # 文件中转站（MinIO 上传 / 下载 / 删除 + 拖拽 / 粘贴图片上传 + 传输进度，compact 属性 = 小窗口紧凑布局）
    LogPage.vue               # 运行日志（级别筛选 / 刷新 / 清空）
    UpdatePage.vue            # 软件更新检测与安装
    SettingsPage.vue          # 系统设置：MQTT 同步配置 + 文件中转站（MinIO）+ 桌面右键集成 + 开机自启
src-tauri/
  src/
    lib.rs                    # 入口：托盘菜单、窗口管理、命令注册、单实例、启动参数（--history / --files / --hidden）
    auto_lock.rs              # 离人自动锁屏：csi/notify 的 leave/approach 事件 → 延迟锁屏/取消（冷却 60s）+ 锁屏能力自检
    autostart.rs              # 开机自启：config.json 的 autoStart 意图持久化 + 启动时与系统状态对齐（dev 构建跳过）
    clipboard.rs              # 剪贴板统一入口：自持 arboard 单例（读文本/图片，写后读回校验 + 竞争重试）
    config.rs                 # MqttConfig / MinioConfig 持久化（config.json）与校验、MinIO 地址容错解析、bucket 名校验
    file_hub.rs               # 文件中转站：MinIO 上传 / 下载 / 删除 / 剪贴板图片转 PNG 上传（rust-s3，bucket 可配置，进度事件 filehub://progress）
    secret.rs                 # 本地凭据加密：机器绑定 HKDF-SHA256 + AES-256-GCM（enc:v1: 格式）
    mqtt.rs                   # MQTT 连接/订阅/收发、剪贴板监听发布（rumqttc + tokio）；收包只入队，后台单线程消费
    history.rs                # 剪贴板历史（200 条上限、去重置顶、history.json）
    logger.rs                 # 运行日志（内存 + JSONL 轮转，preview() 截断长文本）
    shell_integration.rs      # 桌面右键集成（Windows 注册表 / macOS 快速操作 workflow，条目表 ENTRIES）
  capabilities/               # 窗口权限：default.json（core/opener，覆盖 main + history + files）、desktop.json（桌面端插件权限）
  tauri.conf.json             # 窗口（main / history / files）与打包配置
```

## 代码规范

- **前端**：Vue 3 `<script setup lang="ts">`；共享状态放 `src/store.ts`（ref + 接口），页面组件放 `src/components/`；通用样式写 `styles.css` 全局类（`.card` `.btn` `.switch` `.hint` `.empty` 等），组件只写 `scoped` 差异样式；与 Rust 通信用 `invoke`（命令）与 `listen`（事件），全部包 try/catch 保证浏览器预览不报错。
- **后端**：每个领域一个模块（config / mqtt / history / logger / shell_integration / file_hub / secret）；命令集中在 `lib.rs` 注册；事件名形如 `mqtt://status`、`history://changed`、`history://refresh`、`log://changed`、`filehub://progress`、`filehub://refresh`；serde 统一 `#[serde(rename_all = "camelCase")]`，前端接口与之对应。
- **测试**：纯逻辑（校验、去重、轮转、消息解析）写单元测试，触碰系统（AppHandle / 文件 / 注册表）的逻辑抽成可注入路径的纯函数再测。

## 开发经验 / 注意事项

- **剪贴板不走 `tauri-plugin-clipboard-manager`**：该插件在 `RunEvent::Exit` 时会 `take()` 掉内部 arboard 实例（arboard 建议退出前 drop），其后到进程退出之间是窗口期（`cleanup_before_exit` 卸托盘等仍有几十~几百 ms），700ms 轮询的监听线程一旦此时调用 `read_text()`，就会命中插件内部 `.as_mut().unwrap()` panic；release 构建 `panic = "abort"`，表现为**退出瞬间 SIGABRT**（1.0.14 崩溃报告实证：崩溃线程 clipboard-watcher，主线程正卡在 `NSStatusBar _removeStatusItem`）。现在 `clipboard.rs` 自持 arboard 单例（进程内所有线程共用、永不 take，退出时由进程统一回收），从结构上消除竞态；连带移除了插件依赖 / capability `clipboard-manager:*` 权限 / npm 包。**教训：第三方封装凡是「退出时置空共享状态、又可能被后台线程并发使用」的，都要提前隔离掉。**
- **MQTT client id = `desktools-<deviceId>`**：同一 deviceId 起两个实例会互踢连接（broker 端 client id 冲突）。已加 `tauri-plugin-single-instance`，桌面右键/快速操作二次启动会转发 `--history` 给已运行实例。排查时注意别同时跑 `pnpm tauri dev` 和手动启动的二进制。
- **防回环三道闸**：消息带 `deviceId`（自己的忽略）→ 收到远端先更新 `last_clip` 再写剪贴板 → 监听线程发现内容等于 `last_clip` 不再发布/记录。前端点击复制走 `clipboard::copy` 但**不**更新 `last_clip`（由监听线程当新复制发布，保持点击复制也能跨端同步）。
- **监听线程的 TOCTOU 竞态会把过期内容发出去（回流）**：「读剪贴板快照 → 查 `last_clip` → 发布」不是原子的，期间收到新消息改掉 `last_clip` 后，过期快照会被当新复制发布回对端，~700ms 后回流覆盖对端刚同步到的新内容（表象就是"收到 mq 但剪贴板没变"）。已用 `SyncState::with_clip_gate` 把监听线程决策与消息处理线程「更新基准→写剪贴板」互斥，并用 `recent_sent`（5 秒窗口）忽略回流的本机内容。改这段逻辑时必须保持「先拿闸门、再拿内部锁」的加锁顺序。
- **macOS `NSPasteboard` 写入会静默失败**：与其它进程（用户复制、pbcopy、其它 app）并发时 `writeObjects` 间歇性返回 false（alacritty 有大量同类 issue），且 arboard 是 `clearContents` + `writeContents` 两步，失败会丢内容。所以写剪贴板统一走 `clipboard::write_text_verified`（写后读回校验 + 退避重试 4 次），成功/竞争/失败都如实记日志；禁止 `let _ = write_text` 吞错误。
- **MQTT 收包与处理解耦**：收到的消息只入内存队列（`SyncState::enqueue_incoming`，容量 512 满则丢弃记 WARN），由 `mqtt-message-worker` 单线程顺序消费（解析 → 记历史 → 写剪贴板），不在 MQTT 事件循环里做剪贴板/历史 IO，同时保证处理顺序。
- **`arboard` 读剪贴板返回 Err 是常态**（剪贴板里是图片/文件时），直接 `continue` 忽略；监听线程 700ms 轮询（读入口 `clipboard::read_text()`，与写入共用自持单例，访问全部串行化）。
- **桌面右键集成条目表**（`shell_integration.rs` 的 `ENTRIES`）：当前两条——剪贴板历史 `--history`、文件中转站 `--files`，设置页开关统一安装 / 卸载。macOS workflow 装在 `~/Library/Services/DeskTools <条目名>.workflow`（bundle id 按条目区分），首次可能需在「系统设置 → 键盘 → 快捷键 → 服务」启用；`pbs -update` 刷新缓存。Windows 注册表项在 `HKCU\Software\Classes\Directory\Background\shell\DeskTools<History|FileHub>`。旧版本只装过主条目，release 启动时 `sync_on_startup()` 自动补装新条目（debug 构建跳过，避免指向 target/debug 二进制）。
- **配置/历史/日志位置**：`~/Library/Application Support/com.hyy.desktools/`（config.json、history.json），日志在 `~/Library/Logs/com.hyy.desktools/desktools.jsonl`；Windows 在 `%APPDATA%` / `%LOCALAPPDATA%` 对应目录。排查问题可直接删 config.json 重置。
- **开机自启由 `autostart` 模块统一管理**：`config.json` 的 `autoStart` 是唯一「意图」来源，release 构建启动时只在与系统状态不一致时才写系统（一致时不重写 `~/Library/LaunchAgents/desktools.plist`，否则每次启动都会触发 macOS BTM 重新注册后台项、通知配额被耗尽）；**debug 构建跳过**，防止自启条目被指向 `target/debug` 二进制。设置页开关经 `get_autostart_state` / `set_autostart` 读写（不再直接调 plugin JS API）；`save_mqtt_config` 保存整份表单时会保留已持久化的 `autoStart`，不会被前端旧值覆盖。
- **锁屏能力自检**：启动时 `auto_lock::self_check()` 非破坏性探测（dlopen/dlsym `SACLockScreenImmediate`、`AXIsProcessTrusted`、`AEDeterminePermissionToAutomateTarget` 以 `askUserIfNeeded=false` 静默查询、`sysadminctl -screenLock status` 读「进入睡眠后需要密码」），结果记日志并经 `get_lock_capability` 在设置页「离人自动锁屏」卡片展示；运行中锁屏降级时逐级记 WARN（能看到 Ctrl+Cmd+Q 失败的具体原因）。
- **Tauri 类实例不能进深度响应式**：`Update`（`@tauri-apps/plugin-updater`）等继承 `Resource` 的类带 `#` 私有字段，放进 Vue `ref()` 被 Proxy 代理后调用方法会报 `TypeError: Cannot read private member from an object whose class did not declare it`（Windows/macOS 同病）。必须用 `shallowRef` 或 `markRaw` 保存类实例；纯数据对象则无此问题。
- **MinIO 凭据加密只发生在写盘/读盘**：`secret.rs` 用机器绑定密钥（HKDF-SHA256：ikm=机器码，machine-uid 读不到时退回 hostname；salt=`com.hyy.desktools`，info=`minio-credentials`）+ AES-256-GCM，密文格式 `enc:v1:<base64(nonce‖ciphertext‖tag)>`；`MqttConfig::save()` 加密、`load()` 解密（无前缀按历史明文透传，解密失败按空串 = 换机器后重填）。**不要**给字段加 serde `serialize_with/deserialize_with` 加密属性——那样 `get_mqtt_config` 返回给前端的也会是密文，必须在 save/load 这一层做。tests 用 `minio_credentials_are_encrypted_in_file` / `minio_credentials_stay_plaintext_in_memory_serialization` 双向守住这个边界。
- **rust-s3 0.38 的几个坑**（file_hub.rs 已处理）：① `list_page` / `delete_object` / `head_object` 都用 `response_data(false)`，HTTP 4xx 不会返回 Err（`fail-on-err` 对它们无效）——列举失败靠 XML 解析错误体现，删除后要自己检查 `status_code()`；② 建桶用 `Bucket::create_with_path_style`，成功与否看返回的 `response_code`（200 成功 / 409 已存在），非 200 不会 Err；③ MinIO 必须 path-style（`with_path_style()`）；region 用 `Region::Custom{region:"us-east-1"}`，MinIO 未配置 region 时 `parseLocationConstraint` 放行任意 LocationConstraint；④ `put_object_stream` 按 8MiB 阈值自动在单次 PUT / 分片上传间切换，读写全程流式不进内存；⑤ multipart 时本地读取进度可能先到 100%，前端进度条封顶 99% 直到 `done` 事件。
- **文件中转站真实联调**：`src-tauri` 下 `DESKTOOLS_MINIO_HOST=127.0.0.1:9000 DESKTOOLS_MINIO_KEY=xx DESKTOOLS_MINIO_SECRET=yy cargo test file_hub::tests::e2e_minio_roundtrip -- --ignored --nocapture`（可加 `DESKTOOLS_MINIO_BUCKET=xxx` 换桶；会建桶、上传、GET 校验、删除，用小文件）。文件对话框需 `tauri-plugin-dialog`（Rust 插件 + npm 包 + capability `dialog:default`，且窗口 label 必须在该 capability 的 `windows` 列表里）。
- **Tauri v2 的 ACL 只拦 `plugin:` 命令**：`tauri` 源码（`src/webview/mod.rs`）里只有当请求是插件命令（含 `core:*`，如 `core:event|listen`）或应用自带 ACL manifest 或来自远程 origin 时才校验 capability；自定义命令（`generate_handler!` 注册的）默认不受 ACL 限制。窗口 label 不在任何 capability 的 `windows` 列表时，它的 `listen` / dialog / opener 等插件命令全部被拒（release 报 "Command plugin:xxx not allowed by ACL"，debug 有更详细的 `resolve_access_message`）。历史小窗口曾因为没进 `windows` 列表静默收不到 `history://refresh`（listen 失败被 try/catch 吞了，表象是「右键打开不是最新数据」）；修复后仍保留前端 `window` focus 兜底刷新（HistoryPage / FileHubPage compact）双保险。新增窗口（如 files）后必须把 label 加进 `capabilities/default.json`（core/opener）和所需插件权限文件（dialog 等）。
- **拖拽上传与剪贴板图片上传**（FileHubPage / file_hub.rs）：窗口 `dragDropEnabled` 默认 `true`（原生接管文件拖放，会禁用 webview 的 HTML5 dnd），前端用 `getCurrentWindow().onDragDropEvent`（底层是 `core:event|listen`，窗口需在 capability `windows` 列表里）拿 `enter/over/drop/leave` + 文件路径。剪贴板图片在 Rust 侧用 `clipboard::read_image()` 读取（自持 arboard 实例，arboard 需启用 `image-data` feature），RGBA 数据在 `spawn_blocking` 里用 `png` crate 编码为 PNG 再走 `put_object_stream_with_content_type` 上传（图片小、不发进度事件）；前端 Ctrl+V 只在剪贴板没有文本时接管，避免拦截正常粘贴。
- **开发时 `tauri dev` 会自动重编译重启**：改 Rust 后用户桌面上的 dev 实例会闪断重连，日志里会出现重连记录，属正常现象。

