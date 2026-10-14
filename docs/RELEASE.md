# 发布与自动更新配置

## 1. GitHub Secret（必需）

应用内自动更新使用 minisign 签名验证，构建时需要私钥签名、应用内置公钥验签。

私钥已在本机生成：`~/.tauri/desktools.key`（**绝不可提交到仓库**）。

将私钥内容添加为仓库 Secret：

```bash
# 方式一：GitHub CLI
gh secret set TAURI_SIGNING_PRIVATE_KEY --body "$(cat ~/.tauri/desktools.key)"

# 方式二：网页
# https://github.com/240011814/desktools/settings/secrets/actions
# New repository secret → Name: TAURI_SIGNING_PRIVATE_KEY → Value: 粘贴 ~/.tauri/desktools.key 全部内容
```

> 私钥若丢失，已发布版本将无法再被更新签名验证通过，务必离线备份。

## 2. 版本号规则

| 项目 | 规则 | 示例 |
| --- | --- | --- |
| 应用版本 | `1.0.<git 提交数>`（纯数字，满足 MSI/WiX 限制） | `1.0.7` |
| Release tag | `v<应用版本>-<短SHA>` | `v1.0.7-b497821` |
| Release 标题 | `desktools <应用版本> (<短SHA>)` | `desktools 1.0.7 (b497821)` |

每次推送到 `main` 自动生成 tag 并发布 Release，无需手动打 tag。

## 3. 自动更新

- 更新清单 `latest.json` 由 `tauri-action` 生成并上传到最新 Release。
- 应用启动时自动检查更新（也支持手动「检测新版本」），确认后下载、验签、安装并自动重启。
- 更新端点：`https://github.com/240011814/desktools/releases/latest/download/latest.json`
- 公钥已内置于 `src-tauri/tauri.conf.json` → `plugins.updater.pubkey`。

## 4. 注意

- macOS 产物未做 Apple 开发者签名/公证，用户首次打开需在「系统设置 → 隐私与安全性」中放行；但这不影响 updater 的 minisign 验签。
- Release 为正式发布（非 draft/prerelease），`releases/latest` 始终指向最新一次推送的产物。
