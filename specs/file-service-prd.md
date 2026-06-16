# File Service PRD

## Problem Statement

用户需要一个轻量的 File Hub，通过浏览器访问服务端配置的存储根目录，完成资源浏览、下载、上传、重命名、删除和搜索。读取类动作应对匿名用户开放，写入类动作则需要由管理员通过控制台按用户或匿名身份配置权限。

当前仓库仍是 Rust 脚手架，尚未提供资源浏览 UI、HTTP API、权限系统、用户管理、目录归档下载或搜索能力。该 PRD 定义 v1 的产品边界、交互语义、权限模型和测试接缝，使后续实现可以直接拆分给 agent 执行。

## Solution

构建一个 Web 文件服务：页面展示当前目录的资源列表，包含资源名、文件大小、修改时间和可用操作；顶部提供上传文件、上传目录、新建目录、搜索、登录和身份区域。用户可以通过面包屑跳转资源路径，点击目录名进入目录，点击文件名下载文件，点击目录下载按钮获取目录归档。

系统提供一个管理员专用控制台。管理员是唯一的管理身份，用户名固定为 `admin`，不能在控制台中创建、删除或替换。其初始密码在首次启动时由服务端随机生成、以哈希形式落库，并在启动日志中明文显示一次；管理员登录后可自助修改密码。控制台只管理普通用户和权限集，并可配置匿名用户权限。读取类动作，包括浏览、当前列表过滤、服务器搜索、文件下载和目录归档下载，不需要权限；写入类动作，包括创建资源、重命名和删除资源，按当前身份的权限集决定可用操作。

## User Stories

1. As an anonymous user, I want to browse the root directory, so that I can see top-level resources without logging in.
2. As an anonymous user, I want to browse into directories, so that I can navigate the resource tree.
3. As an anonymous user, I want to use a breadcrumb, so that I can jump back to any parent directory.
4. As an anonymous user, I want the root directory breadcrumb to navigate to the root directory, so that I can quickly return to the browsing origin.
5. As an anonymous user, I want the return-to-parent action hidden at the root directory, so that I am not shown an impossible navigation action.
6. As an anonymous user, I want to click a file name to download the file, so that downloading is direct.
7. As an anonymous user, I want to click a directory name to open the directory, so that browsing behaves like a file manager.
8. As an anonymous user, I want to click a directory download action to download a directory archive, so that I can retrieve a full directory tree.
9. As an anonymous user, I want directory archives to preserve the downloaded directory as the top-level archive entry, so that extracted files do not scatter into the destination folder.
10. As an anonymous user, I want file downloads to keep the resource name, so that downloaded files are easy to identify.
11. As an anonymous user, I want directory archive downloads to use the directory name with `.zip` appended, so that archive names are predictable.
12. As an anonymous user, I want download actions to work without a download permission, so that reading resources stays frictionless.
13. As an anonymous user, I want symbolic links to be excluded from resources, so that browsing cannot escape the storage root.
14. As an anonymous user, I want leading-dot resources to be visible, so that ordinary resources such as `.gitignore` are not hidden by implicit rules.
15. As an anonymous user, I want file size shown only for files, so that directory rows do not imply a calculated recursive size.
16. As an anonymous user, I want modified time shown for each resource itself, so that list metadata reflects the resource entry rather than descendants.
17. As an anonymous user, I want modified time displayed in the server-configured time zone, so that all users see consistent timestamps.
18. As an anonymous user, I want a fixed timestamp format, so that resource rows are easy to scan and compare.
19. As a visitor, I want directories to appear before files, so that navigation targets stay easy to find.
20. As a visitor, I want the default listing order to be name ascending with directories first, so that initial browsing is predictable.
21. As a visitor, I want to sort by resource name, so that I can find resources alphabetically.
22. As a visitor, I want to sort by file size, so that I can compare files by byte size.
23. As a visitor, I want size sorting to keep directories first and sort files by size, so that directory rows are not assigned fake sizes.
24. As a visitor, I want to sort by modified time, so that I can find recently changed resources.
25. As a visitor, I want a visible ascending or descending arrow beside the active sort field, so that I understand the current listing order.
26. As a visitor, I want current list filtering to be the default search mode, so that typing in the search box immediately narrows the current directory listing.
27. As a visitor, I want current list filtering to search only the displayed directory's direct resources, so that it does not unexpectedly search nested directories.
28. As a visitor, I want current list filtering to preserve the current sort, so that filtering does not reorder the list unexpectedly.
29. As a visitor, I want to switch the search mode to server search, so that I can find matching resources across the resource tree.
30. As a visitor, I want the server search icon to appear only in server search mode, so that I know when a search requires manual submission.
31. As a visitor, I want server search to require at least two non-whitespace characters, so that accidental broad scans are prevented.
32. As a visitor, I want search to use case-insensitive plain substring name matching, so that common searches are simple and predictable.
33. As a visitor, I want wildcard, regex, fuzzy, and content search excluded from v1, so that search behavior stays easy to understand.
34. As a visitor, I want server search results as a flat list, so that cross-directory matches are easy to scan.
35. As a visitor, I want each search result to show its containing resource path, so that same-named resources can be distinguished.
36. As a visitor, I want to click a search result file name to download it, so that search results behave like listing rows.
37. As a visitor, I want to click a search result directory name to open it, so that search results behave like listing rows.
38. As a visitor, I want to click a search result's containing path, so that I can jump to the parent directory.
39. As a visitor, I want server search results to report truncation, so that I know when results are incomplete.
40. As a visitor, I want an unpaginated directory listing, so that the current list filter can operate over the complete displayed directory.
41. As a visitor, I want oversized directory listings to fail clearly when they exceed the configured limit, so that the UI does not silently show a partial list.
42. As an anonymous user with upload permission, I want to upload a file into the current resource path, so that I can add resources without logging in.
43. As an anonymous user with upload permission, I want to upload a directory into the current resource path, so that I can preserve a local directory structure.
44. As an anonymous user with upload permission, I want to create a directory in the current resource path, so that I can organize resources.
45. As a logged-in user with upload permission, I want upload file, upload directory, and create directory actions available, so that I can create resources.
46. As a user without upload permission, I want upload and create-directory actions hidden, so that I do not see actions I cannot perform.
47. As a user uploading a file, I want visible upload progress, so that I can tell the upload is still active.
48. As a user uploading a directory, I want visible upload progress, so that large directory uploads do not appear stuck.
49. As a user uploading resources, I want upload limits enforced, so that uploads fail predictably when too large.
50. As a user uploading a directory, I want the upload to be atomic, so that partial directory structures are not created after a failure.
51. As a user uploading resources, I want name conflicts rejected, so that existing resources are not overwritten or auto-renamed.
52. As a user uploading a directory, I want the first failed relative path and reason shown, so that I know what to fix.
53. As a user with rename permission, I want to rename a file, so that I can correct resource names.
54. As a user with rename permission, I want to rename a directory, so that I can correct directory names.
55. As a user renaming a resource, I want the new resource name to reject empty names, `.` and `..`, path separators, NUL bytes, and control characters, so that unsafe paths cannot be expressed.
56. As a user renaming a resource, I want the new name to be only a resource name and not a path, so that rename cannot be used as move.
57. As a user renaming a resource, I want same-directory name conflicts rejected, so that existing resources are protected.
58. As a user without rename permission, I want rename actions hidden, so that I do not see actions I cannot perform.
59. As a user with delete permission, I want to delete a file, so that obsolete resources can be removed.
60. As a user with delete permission, I want to recursively delete a directory, so that a directory and all contained resources can be removed.
61. As a user deleting a directory, I want a confirmation that clearly states all contained resources will be removed, so that destructive intent is explicit.
62. As a user deleting a directory, I do not need a precomputed resource count, so that confirmation stays fast even for large directories.
63. As a user deleting resources, I want partial recursive delete failures reported as failures, so that the system does not claim success after incomplete deletion.
64. As a user without delete permission, I want delete actions hidden, so that I do not see actions I cannot perform.
65. As any visitor, I want the root directory to have no rename, delete, or archive download operation, so that the browsing origin cannot be operated on as a resource.
66. As a user with upload permission, I want to create top-level resources in the root directory, so that the root remains a valid current resource path.
67. As a user performing a write action from a directory listing, I want the listing refreshed afterward, so that I see the current state.
68. As a user performing a write action from a directory listing, I want current sort preserved and filter/search state cleared, so that the updated list is not misleading.
69. As a user performing a write action from server search results, I want the same server search refreshed, so that the result set reflects the change.
70. As an anonymous user, I want a login action in the identity area, so that I can authenticate when I need write permissions assigned to a user.
71. As an authenticated user, I want the identity area to show my username, so that I know which identity is active.
72. As an authenticated user, I want to log out, so that I can return to anonymous access.
73. As an authenticated user, I want to change my own password, so that I can maintain my credential without administrator help.
74. As an administrator, I want the identity area to show a console entry, so that I can manage users and permission sets.
75. As an administrator, I want to create ordinary users with username, initial password, and permission set, so that users can log in immediately.
76. As an administrator, I want usernames to be ASCII letters, digits, underscores, or hyphens, so that login identifiers are simple and safe.
77. As an administrator, I want usernames to be unique case-insensitively while preserving display casing, so that `Alice` and `alice` cannot collide.
78. As an administrator, I want passwords to require at least eight characters with no composition requirement, so that credential rules are simple.
79. As an administrator, I want newly created users to default to no write permissions unless I enable them, so that least privilege is the default.
80. As an administrator, I want to edit a user's upload permission, so that I can allow or revoke resource creation.
81. As an administrator, I want to edit a user's rename permission, so that I can allow or revoke resource renaming.
82. As an administrator, I want to edit a user's delete permission, so that I can allow or revoke file and directory deletion.
83. As an administrator, I want to reset a user's password, so that I can help a user regain access.
84. As an administrator, I want password reset to revoke the user's existing sessions, so that old sessions cannot continue after credential change.
85. As an administrator, I want to delete ordinary users, so that obsolete identities can be removed.
86. As an administrator, I want deleting a user to revoke existing sessions, so that removed users cannot continue using authenticated access.
87. As an administrator, I want to configure the anonymous permission set, so that anonymous write access can be explicitly enabled or disabled.
88. As an administrator, I want anonymous write permissions to default to off, so that deployment starts from read-only anonymous access.
89. As an administrator, I want logged-in users to use their own permission set rather than inheriting anonymous permissions, so that user-specific revocation is understandable.
90. As an administrator, I do not want to create additional administrators in the console, so that the administrative identity remains singular.
91. As an administrator, I do not want ordinary users to access the console, so that user and permission management remains restricted.
92. As an implementer, I want resource paths to be relative to the storage root, so that server absolute paths are not exposed.
93. As an implementer, I want all write actions enforced server-side, so that hidden UI buttons are not the security boundary.
94. As an implementer, I want read actions bounded by archive, search, upload, and listing limits where applicable, so that anonymous access cannot create unbounded work.
95. As an operator, I want key limits and the storage root configured server-side, so that deployments can tune resource usage.
96. As an operator, I want the administrator username fixed as `admin`, so that the single administrative identity is unambiguous and not part of configuration.
97. As an operator, I want the administrator password generated on first startup and stored only as a hash, so that no administrator secret is hard-coded or committed.
98. As an operator, I want the generated administrator password printed once to the logs, so that I can capture it for the first login.
99. As an operator, I want the administrator password reprinted on every startup until it is changed, so that a missed startup log does not lock me out before first login.
100. As an administrator, I want password reprinting to stop once I change the password, so that my chosen credential is never written to logs.

## Implementation Decisions

- The product is a single File Hub context exposing resources under a server-managed storage root.
- A resource is either a regular file or a directory. Symbolic links are excluded and must not be followed or shown.
- Resources do not belong to users. There is no owner, creator, or per-resource ownership model.
- Resource paths are relative to the storage root. Server absolute paths are not part of the product surface.
- Root directory is the browsing origin. It may receive new top-level resources but is not itself an operable resource.
- Read actions are browsing, current list filtering, server search, file download, and directory archive download. Read actions do not require permission and are available to anonymous users.
- Write actions are resource creation, rename, and delete. Write actions are controlled by permission sets.
- Upload permission covers file upload, directory upload, and create directory.
- Rename permission covers only rename. Rename changes only a resource name inside its containing directory and cannot express move.
- Delete permission covers file delete and recursive directory delete.
- Logged-in users use their own permission set and do not inherit anonymous permissions.
- Default user permission set has all write permissions disabled unless the administrator enables them.
- Default anonymous permission set has all write permissions disabled unless the administrator enables them.
- The UI hides unavailable write actions. Server-side authorization remains mandatory for all write endpoints.
- There is a single administrator identity with the fixed username `admin`. It cannot be created, deleted, or replaced in the console.
- The administrator password is not supplied by deployment configuration. On first startup, when no administrator row exists, the server generates a password with a CSPRNG, stores only its argon2id hash, and prints the plaintext to the log exactly once.
- Until the administrator changes the bootstrap password, every startup regenerates the password and reprints it at WARN level. Once the administrator changes it, regeneration and reprinting stop permanently. There is no CLI reset path; losing the changed password is unrecoverable, which is an accepted trade-off for a single-administrator deployment.
- The administrator changes its own password through the same self-service password-change flow as ordinary users, and changing it revokes the administrator's existing sessions.
- The console manages ordinary users and permission sets only.
- User creation requires username, initial password, and permission set.
- Ordinary users may change their own password.
- The administrator may reset ordinary user passwords.
- Deleting a user revokes existing sessions.
- Resetting a user password revokes existing sessions.
- Username syntax is ASCII letters, digits, underscores, or hyphens, length 1-64, unique case-insensitively, preserving original display casing.
- Passwords require at least eight characters and have no composition requirement.
- The identity area shows login for anonymous users; for authenticated users it shows username, password change, and logout; for the administrator it also shows the console entry.
- Directory listings are unpaginated lists of direct child resources.
- Directory listing implementations must still enforce a configured maximum direct-child count and fail clearly rather than silently returning a partial listing.
- Default listing order is directory-first by resource name ascending.
- Directory-first sorting applies for every sort field. Directories are grouped before files, and the selected field sorts within each resource type.
- File size is shown only for files. Directories have no displayed or calculated size.
- Modified time is the resource's own last modified time, not a recursive descendant time, and is displayed in the server-configured time zone as `YYYY-MM-DD HH:mm:ss`.
- Breadcrumbs expose clickable directory segments for the current resource path.
- Resource open semantics are: directory name navigates into the directory; file name downloads the file.
- Directory archive download is triggered by the row download action, not by opening a directory name.
- Directory archives are generated synchronously and streamed to the client. There is no archive task center, delayed download, or archive history.
- A directory archive contains the downloaded directory as the top-level archive entry and preserves nested resources.
- Download names use the file resource name for files and `{directoryName}.zip` for directory archives.
- Directory archive creation is bounded by configured archive size and resource-count limits.
- Uploads are bounded by configured single-file size, total upload size, and directory upload resource-count limits.
- Directory upload preserves the selected local directory and nested resources under the current resource path.
- Directory upload is atomic: either the complete selected directory structure is created, or no resources are created.
- Upload progress is shown for uploads. Directory upload needs visible overall progress; per-file detail is not required.
- Resource names may include ordinary human-readable characters and leading dots, but must not be empty, be `.` or `..`, contain path separators, NUL bytes, or control characters.
- Name conflicts are rejected for rename, file upload, directory upload, and create directory. The system must not overwrite, skip, or auto-rename on conflict.
- Write failures should report the affected resource path and user-actionable reason when known. Directory upload failures must at least report the first failed relative path and reason.
- Recursive delete removes a directory and all contained resources.
- Directory delete requires confirmation stating that the directory and all contained resources will be removed. It does not need to precompute resource counts.
- Delete success means the target resource no longer exists. Partial recursive delete failure must be reported as failure and followed by a refreshed view.
- After a write action from a directory listing, the UI stays in the current directory, refreshes the listing, preserves current sort, and clears active filter or search state.
- After a write action from server search results, the same server search is re-executed.
- Search mode defaults to current list filtering. Server search is selected explicitly and triggered manually by a search icon.
- Current list filtering narrows the current directory listing by resource name without searching nested directories.
- Server search searches resource names across the resource tree, not file contents.
- Search matching is case-insensitive plain substring matching. Regex, wildcard, fuzzy, pinyin, and content search are excluded.
- Server search requires at least two non-whitespace characters.
- Server search results are flat, show the containing resource path, and reuse directory listing open/action semantics.
- Server search results are bounded by a configured result limit and must indicate truncation when the result set is incomplete.
- Hidden files and directories whose names start with a dot are visible as ordinary resources.
- Backend configuration should include at least storage root, staging directory location, server time zone, upload limits, archive limits, search result limit, and directory listing limit. The administrator password is not configured: it is generated on first startup, not read from configuration.
- Application runtime logging is allowed for diagnosis, but product audit logging is out of scope.

## Testing Decisions

- Tests should verify externally observable behavior: HTTP status, response bodies, file effects under an isolated storage root, browser-visible actions, session behavior, and downloaded archive contents. Tests should not assert private helper structure.
- The highest-value backend seam is HTTP/API integration tests against a temporary storage root, using authenticated, anonymous, and administrator sessions.
- Resource path validation should be tested at the API boundary with path traversal, absolute paths, symbolic links, leading-dot names, `.`/`..`, control characters, and path separators in resource names.
- Directory listing tests should cover directory-first sorting, default listing order, size sorting with blank directory sizes, modified-time sorting, unpaginated full listing behavior, and configured listing limit failure.
- Current list filtering should be tested through the UI or API response that powers the UI, ensuring it filters only direct resources and preserves current sort.
- Server search should be tested through API integration tests over nested resources, same-named resources, result truncation, minimum query length, and case-insensitive substring matching.
- File download tests should assert suggested download names and content.
- Directory archive tests should assert suggested download names, top-level directory inclusion, nested path preservation, archive limits, and root directory rejection.
- Upload tests should cover file upload, directory upload, create directory, upload progress at the UI seam, limits, name conflict rejection, invalid resource names, and atomic directory upload failure.
- Rename tests should cover files, directories, name conflict rejection, invalid names, and the absence of move semantics.
- Delete tests should cover file delete, recursive directory delete, directory delete confirmation at the UI seam, root directory rejection, and partial failure reporting where it can be simulated.
- Permission tests should cover anonymous read access, default anonymous write denial, default user write denial, each write permission independently, logged-in users not inheriting anonymous permission, hidden unavailable actions, and server-side denial even when endpoints are called directly.
- Console tests should cover administrator-only access, user creation, permission editing, anonymous permission editing, password reset, user deletion, and session revocation.
- Identity area tests should cover anonymous login display, authenticated username/password-change/logout display, and administrator console entry.
- Frontend E2E tests should cover the prototype-level workflows: browse via breadcrumb, sort columns and arrows, switch search modes, perform current list filtering, execute server search, upload with progress, rename, delete with confirmation, and download resources.
- Since the current codebase has only a Rust binary scaffold, new seams should be introduced at the highest practical boundary: application HTTP router, storage-root-backed resource service, authentication/session service, and browser E2E against a running local server.
- Unit tests are appropriate for pure rules: resource name validation, username validation, password validation, name matching, sort ordering, and permission set evaluation.
- Rust gate expectations for implementation work are the full project gates for source changes: build, tests, formatting, and clippy with warnings denied.

## Out of Scope

- Resource ownership, creator tracking, or per-user resource attribution.
- Path-level permissions, directory ACLs, or per-resource ACLs.
- Multiple administrators, role hierarchy, or administrator creation through the console.
- Move resource functionality.
- File content search, document parsing, indexing, pinyin search, fuzzy search, wildcard search, or regex search.
- Pagination for directory listings.
- Archive background tasks, archive history, delayed downloads, or task center UI.
- Directory size calculation.
- Recursive modified time calculation.
- Symbolic link browsing, following, or downloading.
- Root directory rename, delete, or archive download.
- Download permission as a configurable permission.
- Separate create-directory permission.
- Separate file-delete and directory-delete permissions.
- Upload overwrite, rename overwrite, automatic conflict renaming, or best-effort partial directory upload.
- Precomputed resource count in delete confirmation.
- Product audit log UI or audit log retention requirements.
- Email invitation, activation flow, password reset email, or username-as-email behavior.
- Resource preview.
- Pause/resume upload, resumable upload, or per-file directory upload progress details.
- Administrator password recovery flow, CLI reset, or console reset entry for the administrator. If the administrator forgets the password after changing it, recovery is out of scope.
- Configurable or multiple administrator usernames. The administrator username is fixed as `admin`.

## Further Notes

- The source conversation used `prototype.png` as the UI reference. The PRD treats the drawing as a functional prototype rather than a visual design system.
- One prior grilling branch stopped immediately after the user rejected pagination. This PRD keeps the confirmed no-pagination behavior and adds a conservative configured directory-listing limit so implementation remains bounded and testable.
- The confirmed domain language is captured in the root glossary and should be used in issue titles, API names, docs, and tests where applicable.
- A later grilling-with-docs session revised the administrator credential source. The original PRD said the administrator credential was "provided by deployment configuration." It is now a first-start bootstrap: the username is fixed `admin`, an initial password is CSPRNG-generated, argon2id-hashed into the database, and surfaced once in the logs. The plaintext is never persisted. Until the administrator changes the password, every startup re-generates and re-logs it; after the first self-service change, regeneration stops and there is no recovery path. This revision is recorded in `docs/adr/0004-admin-bootstrap-password.md`. The accompanying technical design lives in `specs/file-service-design.md`, with `docs/adr/0001`–`0004` covering the deliberate or counter-intuitive architecture decisions.
