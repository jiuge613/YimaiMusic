# Yimai v0.1.5

精美的高性能桌面音乐播放器（Rust + Tauri 2 + React）。首个以「Yimai」品牌发布的版本。

## 本次更新

- **品牌重命名**：应用名称、窗口标题、包名、安装程序统一为 **Yimai**（原 RustMusic）。
- **全新图标**：替换全套应用图标（ICO / PNG 多尺寸栅格 / Windows Store 徽标 / macOS icns）。
- **自动更新源切换**：更新检查指向本仓库 `jiuge613/YimaiMusic`，不再依赖上游仓库。

## 功能概览

- 本地曲库管理与扫描
- 网易云 / QQ 音乐在线音源
- QRC 逐字歌词与桌面歌词窗口
- 10 段均衡器（biquad DSP）
- 主题与皮肤切换、系统托盘与 SMTC 媒体控制

## 安装说明

1. 下载下方 `Yimai_*_x64-setup.exe`。
2. 双击运行，按向导完成安装（默认安装到当前用户目录，可切换为所有用户）。
3. 启动后可在「设置 → 检查更新」中验证自动更新链路。

## 说明

- 仅提供 Windows x64 安装包。
- 本项目基于 LingyunStudio/RustMusic（Apache-2.0）二次开发，版权声明保留在原 LICENSE 中。
