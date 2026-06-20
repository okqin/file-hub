# Resource Address Design

本文深化 [File Service Design](file-service-design.md) 中的 Resource Path / Resource Name 设计。领域语言沿用根目录 [CONTEXT.md](../CONTEXT.md)，staging 保留目录策略沿用 [ADR 0003](../docs/adr/0003-staging-reserved-directory.md)。

## 1. Seam

Resource Address module 是无 I/O 的 crate-private deep module。HTTP DTO 与 multipart adapter 继续接收传输层 `String`；resource/upload action 在入口立即把原始字符串转换为已验证的 owned domain value。filesystem implementation 只接收领域值或其借用视图，不再解析原始路径。

配置加载直接使用 Resource Name 的词法校验来验证 staging directory name。配置值合法并不代表用户可以访问或创建同名 Resource。

## 2. Interface 与不变量

- `ResourceName`：owned、单段、已验证。名称非空、不超过 255 bytes、不是 `.`/`..`，且不包含 `/`、`\\`、NUL 或控制字符。
- `ResourcePath`：owned、相对 storage root、最多 4096 bytes 和 256 段。空路径合法且唯一表示 Root Directory；非空路径的每段均为 Resource Name。
- Resource Path 可追加一个 Resource Name、读取各段、取得末段名称，并判断是否表示 Root Directory。Breadcrumb 是 resource 展示模型，不属于地址 module。
- Resource Address policy 持有已验证的 reserved staging name。它在 action seam 解析用户路径与写入名称，并拒绝任一段或目标名称等于 reserved name。
- 词法错误由 Resource Address module 定义；resource、upload、config 分别在自身 seam 映射为既有错误，HTTP 错误契约不变。

Root Directory 是合法的浏览和写入目标目录，但不是 Resource。rename、delete、archive 等 action 在取得非根 Resource Path 时返回各自既有的 Root Directory 错误。

## 3. 调用关系

```text
HTTP DTO / multipart field
          │ raw String
          ▼
resource or upload action seam ── Resource Address policy
          │ validated owned ResourcePath / ResourceName
          ▼
filesystem implementation
```

`AppConfig` 保存已验证的 reserved Resource Name。Resource Address policy 是纯值对象，不是可替换依赖，因此不引入 port 或 adapter。

## 4. 测试表面

- Resource Address module interface 测试全部词法限制、路径限制、Root Directory、join 和 reserved policy；边界组合使用参数化测试，结构不变量使用 property tests。
- resource/upload/config 测试只验证各 action 的错误映射和业务上下文。
- HTTP 集成测试保留外部错误码、权限、symlink/path traversal 防护和完整文件操作流程；不在每个 action 重复穷举同一非法名称集合。
- 测试不读取私有字段，也不固定 segments 的内部存储形式。
