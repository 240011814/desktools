# desktools

基于 Tauri 2 + Vue 3 + TypeScript 的桌面工具箱（Windows / macOS）。

## 功能

- **侧边栏菜单**：「剪贴板历史」「文件中转站」「日志」「软件更新」「系统设置」五个页面单页切换，侧边栏常显 MQTT 连接状态。
- **剪贴板跨设备同步（MQTT）**：在「系统设置」中配置 MQTT 连接（服务器 / 端口 / 用户名 / 密码 / TLS）与 Group 号，程序启动即自动连接并订阅 `texthub/<group号>`；本机每次复制文本自动发布到该 topic，收到其他设备的内容自动写入剪贴板（消息携带本机设备 ID，自动忽略自身消息避免回环）。配置持久化保存，连接状态实时显示。
- **剪贴板历史**：自动保存最近 200 条复制记录（含远端同步内容，标记来源），支持点击条目重新复制、单条删除、一键清空；相同内容去重并置顶，历史持久化保存。
- **文件中转站（MinIO）**：在「系统设置」中配置 MinIO（S3 兼容）服务器地址、存储桶（默认 `hub`，不存在时自动创建）与 Access Key / Secret Key 后，可跨设备上传、下载、删除文件（同名文件覆盖）。大文件自动分片、流式传输并实时显示进度；下载中断自动清理半成品。凭据以机器绑定加密存储在本机（配置文件里是 `enc:v1:` 密文，更换电脑或重装系统后需重新填写）。
- **运行日志**：记录 MQTT 连接/订阅/断线重连、**收发消息内容**、历史写入、剪贴板读写等关键事件（INFO / WARN / ERROR），日志页支持级别筛选、刷新、清空；日志持久化到文件（内存保留 1000 条，文件超 1MB 自动轮转）。
- **桌面右键菜单**：桌面空白处右键 →「剪贴板历史」/「文件中转站」，直接弹出对应的独立小窗口。Windows 为注册表静态菜单项（Windows 11 需点「显示更多选项」），macOS 为 Finder「快速操作 / 服务」；均可在「系统设置 → 系统集成」中开关。托盘右键菜单同样可弹出这两个窗口。
- **自动更新**：内置 minisign 公钥验签，启动时自动检查 GitHub Releases 更新（也支持手动检测），下载安装后自动重启。版本号由 git 提交自动生成（`1.0.<提交数>`）。
- **开机自动启动**：默认随系统登录自启动（带 `--hidden` 参数，仅驻留托盘不弹窗），可在应用内关闭/开启；开关状态持久化在 `config.json`（`autoStart`），关闭后不会被下次启动重新打开。开发构建（`pnpm tauri dev`）不管理系统自启，避免自启条目指向 `target/debug` 二进制。
- **最小化到托盘**：关闭窗口不退出程序，最小化到系统托盘；托盘菜单可「剪贴板历史」「显示主窗口」或「退出」。

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

> **macOS 安装提示**：产物为 universal 包（Apple Silicon / Intel 通用）与 Windows x64 安装包。
> macOS 产物已做 ad-hoc 签名但未经过 Apple 公证（无开发者账号），首次打开若提示"无法验证开发者 / 未公证"：
>
> ```bash
> xattr -cr /Applications/desktools.app   # 清除隔离属性后正常打开
> ```
>
> 或在 Finder 中右键 → 打开（macOS 15+ 需在「系统设置 → 隐私与安全性」中点"仍要打开"）。
> 若提示"文件已损坏"，说明签名无效，请在仓库提 Issue 反馈（CI 已内置签名校验，正常不会出现）。

## Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Vue - Official](https://marketplace.visualstudio.com/items?itemName=Vue.volar) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
