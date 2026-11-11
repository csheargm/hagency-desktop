# Hagency Desktop 品牌资源

更新：2026-11-09。

- 新 logo 来自用户提供的 Cloud `assets/hagency-mark.svg`，原样复制为 `resources/hagency-mark.svg`。
- `python tools/generate-brand-icons.py` 从 SVG 生成桌面 PNG、Windows ICO 和 macOS ICNS；依赖 CairoSVG、Pillow。不要单独修改派生位图。
- macOS ICNS 单独使用 1024px 透明画布，品牌图居中缩至 824px（四边各留 100px），避免 Dock 中比其他应用偏大。PNG、Windows ICO 和源 SVG 仍使用原始占比。
- 登录页通过现有 Makepad `Image` 加载 `resources/icon_256.png`；旧 `resources/robrix_logo_alpha.png` 已删除。
- cargo-packager 使用 32–1024px PNG 和 ICO；Windows build.rs 跟踪 ICO 变化。macOS 开发 runner 将 ICNS 复制进应用包，并更新 CFBundleIconFile，缓存指纹包含图标。
- 本轮只更新桌面资源，独立的 iOS AppIcon 和 Google Play 商店图片没有修改。
- 已验证源 SVG 与 Cloud 完全一致、PNG 尺寸及 ICO/ICNS 全部表示可解码、浅/深背景多尺寸预览、Python/Bash 语法和 git diff --check。未编译或运行完整客户端，也未构建安装包。
