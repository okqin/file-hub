---
status: accepted
---

# 上传原子性靠存储根内保留目录 staging,并在枚举层硬过滤

## 决策

上传(单文件和目录)先写入**存储根内的一个保留目录**(如 `.fh-staging/`)staging,全部校验通过后用 `rename` 原子落到目标。该保留目录在 directory listing 和 server search 中被**硬过滤**,不作为资源出现。

## 背景

PRD 要求目录上传原子:"要么完整创建整个结构,要么零创建"。标准实现是 staging + `rename`,而 `rename` 只在**同一挂载点**才保证原子。同时 PRD 要求"leading-dot 资源可见、无隐藏规则"(`.gitignore` 这类要正常显示)。两者产生张力:staging 目录若放系统 `/tmp`,可能跨挂载点导致 `rename` 退化为非原子的复制;若放存储根内,又会被"leading-dot 可见"规则暴露给浏览和搜索。

## Considered Options

- **系统临时目录(如 `/tmp`)staging**:不污染存储根。但极可能跨挂载点,`rename` 退化为 copy+delete,丧失原子性,违背 PRD 核心要求。
- **存储根内保留目录 + 枚举层过滤(选定)**:与目标同挂载点,`rename` 真原子;代价是引入一个"保留名"概念,并在 listing/search 显式排除它。

## Consequences

- **有意偏离** PRD"leading-dot 资源全部可见"——保留目录是系统内部目录,不是用户资源,故不展示。这是唯一的例外。
- 保留名需在资源名校验中被拒绝,防止用户创建同名资源造成冲突。
- 启动时应清理保留目录内的残留 staging(上次崩溃遗留),避免磁盘泄漏。
- 单文件上传同走 staging("临时文件 + rename"),保证半截上传永不可见,路径统一。
