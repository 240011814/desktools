# desktools

基于 Tauri 2 + Vue 3 + TypeScript 的桌面工具箱（Windows / macOS）。

## 功能

- **侧边栏菜单**：「剪贴板同步」「剪贴板历史」「软件更新」「系统设置」四个页面单页切换，侧边栏常显 MQTT 连接状态。
- **剪贴板跨设备同步（MQTT）**：配置 MQTT 连接（服务器 / 端口 / 用户名 / 密码 / TLS）与 Group 号后，程序启动即自动连接并订阅 `texthub/<group号>`；本机每次复制文本自动发布到该 topic，收到其他设备的内容自动写入剪贴板（消息携带本机设备 ID，自动忽略自身消息避免回环）。配置持久化保存，连接状态实时显示。
- **剪贴板历史**：自动保存最近 200 条复制记录（含远端同步内容，标记来源），支持点击条目重新复制、单条删除、一键清空；相同内容去重并置顶，历史持久化保存。
- **运行日志**：记录 MQTT 连接/订阅/断线重连、同步收发、历史写入、剪贴板读写等关键事件（INFO / WARN / ERROR），日志页支持级别筛选、刷新、清空；日志持久化到文件（内存保留 1000 条，文件超 1MB 自动轮转）。
- **自动更新**：内置 minisign 公钥验签，启动时自动检查 GitHub Releases 更新（也支持手动检测），下载安装后自动重启。版本号由 git 提交自动生成（`1.0.<提交数>`）。
- **开机自动启动**：默认随系统登录自启动（带 `--hidden` 参数，仅驻留托盘不弹窗），可在应用内关闭/开启。
- **最小化到托盘**：关闭窗口不退出程序，最小化到系统托盘；托盘菜单可「显示主窗口」或「退出」。

## 开发

```bash
pnpm install
pnpm tauri dev
```

> 同步配置保存在 `~/Library/Application Support/com.hyy.desktools/config.json`，剪贴板历史保存在同目录的 `history.json`（Windows 为 `%APPDATA%\com.hyy.desktools\`）。

## 构建

```bash
pnpm tauri build
```

产物位于 `src-tauri/target/release/bundle/`（macOS：`.dmg`；Windows：`.exe` NSIS / `.msi`）。

## CI / 发布

- `.github/workflows/ci.yml`：`develop` / `release/**` / `hotfix/**` / `feature/**` 分支及 PR 推送时构建验证（macOS + Windows）。
- `.github/workflows/release.yml`：**每次推送到 `main`** 自动构建 macOS + Windows 并发布到 GitHub Releases，无需手动打 tag：
  - 版本号：`1.0.<git 提交数>+<短SHA>`（写入应用配置）
  - Release tag：`v1.0.<提交数>-<短SHA>`

> macOS 产物未签名/公证，用户首次打开需在「系统设置 → 隐私与安全性」中允许。

## Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Vue - Official](https://marketplace.visualstudio.com/items?itemName=Vue.volar) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
