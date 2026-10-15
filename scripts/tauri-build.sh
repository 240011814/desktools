#!/usr/bin/env bash
# 构建包装脚本：tauri-action 的 tauriScript 指向本脚本，本地/CI 也可手动调用。
#
# 用法：scripts/tauri-build.sh build [--target universal-apple-darwin] [...]
#
# 职责：调用 `pnpm tauri build`，并在 macOS 上对产物做「签名 + 架构」自检。
# 历史教训：签名无效的 .app 在 macOS 上会直接提示「文件已损坏，无法打开」
# （与芯片无关，arm64 / x86_64 都会中招），因此坏包一律不允许流出。
set -euo pipefail

pnpm tauri "$@"

# Windows / Linux 无需 macOS 自检
if [ "$(uname)" != "Darwin" ]; then
  exit 0
fi

APP=$(find src-tauri/target -path "*bundle/macos/*.app" -print -quit)
if [ -z "$APP" ]; then
  echo "==> 未找到 .app 产物（可能使用了 --no-bundle），跳过自检"
  exit 0
fi

echo "==> 自检产物: $APP"

# ---------- 1. 签名自检：签名无效是「文件已损坏」的直接原因 ----------
if ! codesign -vv --deep "$APP"; then
  echo "==> 签名无效，执行 ad-hoc 重签并重建更新包 / DMG …"
  codesign --force --deep --sign - "$APP"
  codesign -vv --deep "$APP"

  BUNDLE_MACOS_DIR=$(dirname "$APP")
  APP_NAME=$(basename "$APP" .app)

  # 更新包必须与重签后的 .app 一致，否则应用内更新会装回坏签名的版本
  ARCHIVE="$BUNDLE_MACOS_DIR/$APP_NAME.app.tar.gz"
  if [ -f "$ARCHIVE" ]; then
    if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
      echo "错误：需要 TAURI_SIGNING_PRIVATE_KEY 才能重建更新包签名" >&2
      exit 1
    fi
    rm -f "$ARCHIVE" "$ARCHIVE.sig"
    tar -czf "$ARCHIVE" -C "$BUNDLE_MACOS_DIR" "$APP_NAME.app"
    pnpm tauri signer sign "$ARCHIVE"
  fi

  # DMG 同样要包含重签后的 .app
  DMG=$(find src-tauri/target -path "*bundle/dmg/*.dmg" -print -quit)
  if [ -n "$DMG" ]; then
    STAGE=$(mktemp -d)
    cp -R "$APP" "$STAGE/"
    ln -s /Applications "$STAGE/Applications"
    rm -f "$DMG"
    hdiutil create -volname "$APP_NAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
    rm -rf "$STAGE"
    echo "==> 已重建 DMG: $DMG"
  fi
fi

# ---------- 2. 架构自检 ----------
ARCHS=$(lipo -archs "$APP/Contents/MacOS/"*)
echo "==> 架构: $ARCHS"
if ! echo "$ARCHS" | grep -q "arm64"; then
  echo "错误：产物缺少 arm64（Apple Silicon）架构" >&2
  exit 1
fi
if printf '%s\n' "$@" | grep -q "universal-apple-darwin"; then
  if ! echo "$ARCHS" | grep -q "x86_64"; then
    echo "错误：universal 构建缺少 x86_64（Intel）架构" >&2
    exit 1
  fi
fi

# ---------- 3. 确认产物不带 quarantine（避免把开发机属性带进流水线产物） ----------
xattr -cr "$APP" 2>/dev/null || true

echo "==> 自检通过：签名有效，架构 [$ARCHS]"
