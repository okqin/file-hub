---
status: accepted
---

# 文件系统层用无状态 async 函数 + Semaphore,而非 Actor

## 决策

访问存储根的文件系统层是一组**无状态 async 函数**(`tokio::fs`,CPU/阻塞活儿走 `spawn_blocking`),用 `tokio::sync::Semaphore` 设并发上限。**不**把它包成 Actor、不用 channel 串行化。

## 背景

项目规范(CLAUDE.md)要求"用 Actor 模型组织子系统,actor 间用 channel 通信"。文件系统层是本项目最核心的子系统,自然会被推定为 Actor。

## Considered Options

- **单 Actor + channel(规范默认)**:符合既有规范。但文件操作天然无内部状态,浏览/下载/归档/搜索本可几十路并发;把它们塞进单 actor 等于给所有文件操作加一把全局串行锁,吞吐崩塌,且毫无收益——actor 模型的价值在于保护长生命周期的内部状态,而 FS 层没有这种状态。
- **无状态函数 + Semaphore(选定)**:保留 FS 天然的并发性,用信号量满足 PRD"匿名访问不能产生无界工作量"的约束,同时避免无意义的串行化。

## Consequences

- **有意偏离** CLAUDE.md 的 Actor 规范。Actor 模型仍保留给真正有长生命周期内部状态的子系统(如会话过期清理这类后台任务)。
- 并发上限由 Semaphore 配置项控制,是抵御无界工作量的主要闸门。
- symlink 排除、路径校验等不变量由 `ResourcePath` newtype 和 `symlink_metadata` 在调用点保证,不依赖 actor 的串行性。
