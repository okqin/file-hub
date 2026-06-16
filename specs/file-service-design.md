# File Service Design

本文将 [File Service PRD](file-service-prd.md) 的产品语义落成可拆给 agent 执行的技术设计。术语沿用根 [CONTEXT.md](../CONTEXT.md);有意偏离/反直觉的决策见 `docs/adr/`。

## 1. 架构总览

```
浏览器 (Vue 3 SPA, ES2017/legacy 双包)
   │  JSON over HTTP / multipart 上传 / 流式下载
   ▼
axum + tower (router, body limit, timeout, rate limit, session)
   │
   ├── auth      axum-login + tower-sessions(SQLite store)
   ├── console   admin-only 用户/权限/匿名权限 CRUD
   ├── resource  无状态 async FS 层(tokio::fs + spawn_blocking + Semaphore)
   └── storage   SQLite / sqlx(users, permissions, anon_permission, sessions)
        │
        ▼
   存储根 (storage_root) + 保留 staging 目录 (.fh-staging/)
```

- 前端构建产物经 `rust-embed` 内嵌进二进制,单文件部署。
- 后端单进程;FS 操作并发由 `Semaphore` 兜底,不串行化(ADR 0002)。

## 2. 模块划分(crate 内 mod)

| 模块 | 职责 | 关键类型 |
|------|------|----------|
| `config` | 启动加载 YAML + `validator` 校验 | `AppConfig` |
| `domain` | 边界 newtype 与纯规则 | `ResourcePath`, `ResourceName`, `Username`, `Password`, `PermissionSet` |
| `storage` | SQLite 访问,迁移 | `Db`, 各 repo |
| `auth` | 登录/会话/撤销、admin 引导 | `AuthUser`, `Backend` |
| `resource` | FS 浏览/下载/归档/搜索/写 | 无状态 async fn |
| `console` | admin-only 管理端点 | handler 组 |
| `http` | router、错误映射、提取器 | `ApiError`, `AppState` |

域规则(名称校验、排序、name match、权限求值)放 `domain`,纯函数,单元测试覆盖。

## 3. 域 newtype(边界一次性校验)

- **`ResourcePath`**:`TryFrom<&str>`,拒绝空、`.`/`..`、路径分隔符、NUL、控制字符、穿越;拒绝保留 staging 名。GET 类从 `?path=` 解析,写类从 JSON body 解析。解析后 `canonicalize` 并校验 `starts_with(storage_root)`;open 后对 symlink 再校验一次,symlink 在 `symlink_metadata` 层直接排除,不作为资源。
- **`ResourceName`**:单段名,同上字符规则,用于 rename / create-directory / 上传 part。
- **`Username`**:ASCII 字母数字下划线连字符,`^[A-Za-z0-9_-]{1,64}$`,大小写不敏感唯一,保留原显示大小写。
- **`Password`**:≥8 字符,无组成要求。
- **`PermissionSet`**:`bitflags`,三独立位 `UPLOAD / RENAME / DELETE`,DB 存整数列。

## 4. HTTP API

路径载荷约定:**GET 走 `?path=`,写操作走 JSON body**。统一错误体:

```json
{ "error": { "code": "name_conflict", "path": "a/b", "reason": "..." } }
```

`thiserror` 定义 domain 错误枚举,`http` 层映射到状态码 + 上述结构。

| 方法 | 路径 | 权限 | 说明 |
|------|------|------|------|
| GET | `/api/list?path=&sort=&order=` | 读,公开 | 目录列表;directory-first;超 listing limit → 413 |
| GET | `/api/search?q=&` | 读,公开 | 实时遍历 + 早停,truncated 标志 |
| GET | `/api/download?path=` | 读,公开 | 文件下载,Content-Disposition 用资源名 |
| GET | `/api/archive?path=` | 读,公开 | 目录归档,预检后流式 zip,名用 `<dir>.zip` |
| POST | `/api/upload` | upload | multipart,单文件或目录(part 带相对路径) |
| POST | `/api/mkdir` | upload | body `{ path, name }` |
| POST | `/api/rename` | rename | body `{ path, newName }` |
| POST | `/api/delete` | delete | body `{ path }`,目录递归 fail-fast |
| POST | `/api/login` `/api/logout` | — | 会话 |
| POST | `/api/password` | 登录态 | 自助改密,撤销旧会话 |
| GET/POST/... | `/api/console/*` | admin | 用户/权限/匿名权限 CRUD、重置普通用户密码 |

读类端点不校验权限;写类按当前身份的 `PermissionSet` 服务端强制(即使直接调端点)。前端额外按权限隐藏不可用操作(纵深防御,非唯一防线)。

## 5. 资源服务机制

### 5.1 列表与搜索
- **列表**:`tokio::fs::read_dir` 读直接子项,`symlink_metadata` 排除 symlink,内存内排序(directory-first + 选定字段)。超 listing limit → `413`,绝不返回部分列表。文件才显示 size,目录无 size。modified time 取条目自身 mtime,按配置时区格式化 `YYYY-MM-DD HH:mm:ss`。
- **服务器搜索**:实时遍历资源树,case-insensitive 子串匹配资源名。两道闸:凑够 result limit 立刻早停并标记 `truncated=true`;另设遍历预算上限防止无界扫描。结果扁平,每条带 containing path。保留 staging 名在遍历层硬过滤(ADR 0003)。零一致性窗口(ADR 未单列,见 PRD 决策)。

### 5.2 目录归档(ADR 未单列,预检 + 流式)
1. **预检**:walk 一遍,累加资源 count 与**未压缩**总字节。任一超 archive count / size limit → 在发首字节前返回 `413` + 可读原因。
2. **流式**:过闸后用 `async_zip` 边 walk 边写响应,目录自身作为 zip 顶层条目,保留嵌套路径。`Content-Disposition` 用 `<dirName>.zip`。
- 根目录无归档操作(根不是可操作资源)。

### 5.3 上传与原子性(ADR 0003)
- **协议**:`multipart/form-data`。目录上传用 `<input webkitdirectory>`,每个 part 携带 `webkitRelativePath`。后端流式解析 multipart。
- **staging**:写入存储根内保留目录 `.fh-staging/`(与目标同挂载点,保证 `rename` 原子)。全部校验(conflict / limit / `ResourceName`)在落目标前完成。
- **目录上传原子性**:完整结构 stage 通过后,一次 `rename` 落目标;任一步失败则清理 staging,零资源落地。失败至少报告**首个**失败相对路径 + 原因。
- **单文件上传**:同走 staging + rename,半截上传永不可见。
- **进度**:前端用 `XMLHttpRequest` + `upload.onprogress`(C69/FF59 不支持 fetch 上传进度)。目录上传显示整体进度。
- **limit**:单文件 size、总上传 size、目录上传资源 count 三项,服务端强制。

### 5.4 删除
- 文件删除:直接 `remove_file`。
- 目录删除:**递归 fail-fast**——遇首个删不动的资源即停,报告该 path + 原因,不继续删其余。前端删目录前需确认,文案声明"目录及其全部内容将被移除",不预算资源数。
- 删除成功 = 目标资源不再存在;部分失败如实报为失败,随后刷新视图。根目录无删除操作。

## 6. 认证、会话与权限求值

- **库**:`axum-login` + `tower-sessions`,session 存 SQLite(`tower-sessions-sqlx-store`)。
- **会话撤销**(不建额外表,顺 axum-login 设计):
  - 改密/重置密码:`AuthUser::session_auth_hash` 从密码哈希派生,密码一变旧会话校验失败 → 自动失效。
  - 删用户:`Backend::get_user` 查不到 → 会话视为登出;残留 session 行随过期清理。
- **权限求值**:`AuthSession` 给 `Option<User>`。登录用户用其自身 `PermissionSet`,**不继承匿名**;匿名(`None`)落到 DB 单行 `anon_permission`。默认全部写权限关闭。
- **密码哈希**:argon2id,参数调到目标硬件 ≥250ms。

## 7. 管理员引导(ADR 0004)

- 用户名固定 `admin`,不可创建/删除/替换/改名。
- 首次启动(库内无 admin 行)用 CSPRNG 生成密码 → argon2id 落库,明文仅在日志现一次。
- **未改密前每次启动 WARN 重打**该密码;admin 自助改密后停打。无 CLI 重置入口,锁死风险已接受。
- admin 改密走与普通用户一致的 auth_hash 路径,改完旧会话失效。

## 8. 配置(`config` + YAML + `validator`)

启动加载并校验。装:storage root、staging 目录、upload limit(单文件/总量/目录数)、archive limit(size/count)、search result limit、遍历预算、listing limit、server 时区。**匿名权限不在配置**——它运行时由控制台改,存 DB。secrets 不入配置明文(admin 密码引导生成,不读配置)。

## 9. 测试接缝(沿用 PRD Testing Decisions)

- **最高价值后端缝**:HTTP/API 集成测试,临时 storage root,匿名/登录/admin 三种会话。
- **单元测试**(纯规则):`ResourcePath`/`ResourceName`/`Username`/`Password` 校验、name match、排序、`PermissionSet` 求值。
- **下载/归档**:断言建议下载名、归档顶层目录、嵌套保留、limit、根目录拒绝。
- **上传**:文件/目录/mkdir、limit、conflict 拒绝、非法名、目录上传原子失败。
- **E2E(Playwright)**:面包屑浏览、排序箭头、搜索模式切换、当前列表过滤、服务器搜索、带进度上传、rename、带确认删除、下载。
- Rust 源改动跑全 gate:build、test、fmt、clippy `-D warnings`。

## 10. 关键依赖(待实现时 web 核对最新版)

`axum` / `tower-http` / `tower_governor`、`axum-login` / `tower-sessions` / `tower-sessions-sqlx-store`、`sqlx`(sqlite)、`argon2`、`secrecy`、`subtle`、`async_zip`、`bitflags`、`validator`、`config`、`rust-embed`、`time`/`chrono-tz`(时区)、`tracing`。前端:`vue`、`vite`、`@vitejs/plugin-legacy`。
