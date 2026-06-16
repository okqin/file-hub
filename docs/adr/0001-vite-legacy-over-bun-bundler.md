---
status: accepted
---

# 前端用 Vite + plugin-legacy 打包,而非 bun 自带 bundler

## 决策

File Hub 前端为 Vue 3 SPA,用 **Vite + `@vitejs/plugin-legacy`** 打包,产出 modern/legacy 双包(legacy 包走 SystemJS + core-js polyfill)。bun 仅作为包管理器和脚本运行时,不用其内置 bundler。最终构建产物由 `rust-embed` 内嵌进 Rust 二进制。

## 背景

运行环境可能存在 Chromium 69 / Firefox 59 这类 2018 年的老浏览器,大致对应 ES2017(FF59)~ES2018(C69)。可选链 `?.`、空值合并 `??`、`Object.fromEntries`、`Array.flat` 等都不能裸用,必须转译到 ES2017 目标并注入 core-js polyfill。开发环境已有 bun,自然会考虑用其内置 bundler 作为统一工具链。

## Considered Options

- **bun 内置 bundler**:速度最快、与既有 bun 环境零摩擦。但它不面向"老浏览器自动降级"场景——不会把 `??` 降级,也不会自动注入 core-js,需要手工维护 polyfill 列表,容易在某个 API 上漏网导致老浏览器白屏。
- **Vite + plugin-legacy(选定)**:`@vitejs/plugin-legacy` 专为 C69/FF59 这类场景设计,自动产出 modern/legacy 双包、自动按 browserslist 注入 polyfill。bun 仍可作为运行时跑 Vite,不与"环境里有 bun"的前提冲突。

## Consequences

- 工具链多一层 Vite,构建比纯 bun bundler 慢,但换来对老浏览器的可靠兼容。
- 需维护 `browserslist` 明确声明 Chromium 69 / Firefox 59 下限,作为 polyfill 注入的事实来源。
- 未来若放弃老浏览器支持,可移除 plugin-legacy 回到单 modern 包,迁移成本低。
