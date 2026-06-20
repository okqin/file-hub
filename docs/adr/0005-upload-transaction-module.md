---
status: accepted
---

# 上传事务由传输无关的 deep module 持有

File Upload 与 Atomic Directory Upload 通过同一个 crate-private upload module 执行。HTTP multipart adapter 按 File 将名称或相对路径及其异步内容流交给该 module；module 独占 staging、Upload Limit、rollback、commit 与取消清理 lifecycle，并且每个事务只发布一个顶层 Resource。

## Considered Options

- **HTTP 直接操纵 staging lifecycle**：实现直接，但将调用顺序、失败清理和提交规则泄漏到 adapter，形成 shallow module。
- **逐 chunk 语义事件源**：interface 看似统一，但 Axum multipart Field 借用 Multipart，难以安全地跨拉取调用持有；同时会把更多事件顺序暴露在 seam 上。
- **adapter 按 File 驱动输入，module 消费内容流（选定）**：保留流式处理，adapter 不理解 staging，module 可通过同一 interface 使用生产 multipart adapter 与内存 adapter 测试全部事务行为。

## Consequences

- multipart 字段顺序和传输错误属于 HTTP adapter；Resource 校验、限制、写入、publish 和 rollback 属于 upload module。
- adapter 不获得 start、write、finish、commit 或 abort 操作；输入正常结束才由 module 发布，任何输入错误或取消都由 module 清理。
- 多个顶层 Resource 是多个独立事务；前端将多选拆成多个请求，Atomic Directory Upload 不跨顶层 Directory。
