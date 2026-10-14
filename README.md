# desktools

基于 Tauri 2 + Vue 3 + TypeScript 的桌面工具箱（Windows / macOS）。

## 功能

- **自动更新**：内置 minisign 公钥验签，启动时自动检查 GitHub Releases 更新（也支持手动检测），下载安装后自动重启。版本号由 git 提交自动生成（`1.0.<提交数>`）。
- **开机自动启动**：默认随系统登录自启动（带 `--hidden` 参数，仅驻留托盘不弹窗），可在应用内关闭/开启。
- **最小化到托盘**：关闭窗口不退出程序，最小化到系统托盘；托盘菜单可「显示主窗口」或「退出」。

## 开发

```bash
pnpm install
pnpm tauri dev
```

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
