# File Hub

File Hub is a browsable file-management context for resources exposed through a server-managed storage root.

## Language

**Resource**:
A browsable entry in the hub. A resource is either a regular file or a directory; symbolic links are not resources, and resources do not belong to users.
_Avoid_: Item, object, asset, owned resource

**Resource Open**:
The primary click action on a resource name: opening a directory navigates into it, while opening a file downloads it.
_Avoid_: Preview, select

**File**:
A resource with downloadable content and a size.
_Avoid_: Document, blob

**Directory**:
A resource that contains other resources and can be browsed as a path segment.
_Avoid_: Folder, collection

**Root Directory**:
The browsing origin of the hub. It contains top-level resources and may receive new resources, but is not itself an operable resource.
_Avoid_: Root resource, deletable root

**Resource Path**:
A path that identifies a resource inside the hub, relative to the hub's storage root.
_Avoid_: Server path, absolute path, filesystem path

**Breadcrumb**:
A clickable representation of the current resource path, with each directory segment navigating to that directory.
_Avoid_: Raw path, location text

**Anonymous User**:
A visitor using the hub without an authenticated user identity. Anonymous users may still have configured permissions.
_Avoid_: Guest, public user

**Default Anonymous Permission Set**:
The anonymous user's permission set before the administrator explicitly enables write permissions.
_Avoid_: Public write access, implicit anonymous write

**Administrator**:
The single administrative identity for the hub. The administrator can manage users and permission sets through the console, but cannot be created, deleted, or replaced through the console.
_Avoid_: Superuser, owner

**User**:
An authenticated non-administrator identity that may be assigned a permission set for resource write actions and may change its own password.
_Avoid_: Account, member

**User Creation**:
The console action that creates a user with a username, initial password, and permission set.
_Avoid_: User invitation, inactive user

**Username**:
A user's login identifier. Usernames use ASCII letters, digits, underscores, or hyphens, are unique case-insensitively, and retain their original display casing.
_Avoid_: Email address, display name, account name

**Password**:
A secret used to authenticate a user. Passwords must be at least eight characters and have no composition requirement.
_Avoid_: PIN, passcode

**Session Revocation**:
The invalidation of a user's existing authenticated sessions after that user is deleted or their password is reset.
_Avoid_: Session persistence, delayed logout

**Console**:
The administrator-only area for managing ordinary users and permission sets.
_Avoid_: Dashboard, admin panel

**Identity Area**:
The navigation area that shows login for anonymous users, and shows username, password change, and logout for authenticated users.
_Avoid_: Login button, account menu

**Read Action**:
A non-mutating action that views, searches, or downloads resources.
_Avoid_: Download permission, view permission

**Write Action**:
An action that changes resources, resource names, or the resource hierarchy.
_Avoid_: Edit action, management action

**Rename**:
A write action that changes only a resource's name within its containing directory.
_Avoid_: Move, path rename

**Available Action**:
An action shown to the current visitor because their permission set allows it.
_Avoid_: Disabled action, hidden permission

**Permission Set**:
The hub-wide write actions that a user or anonymous user is allowed to perform. A logged-in user uses that user's permission set rather than inheriting anonymous permissions.
_Avoid_: Path permission, directory permission, ACL

**Default User Permission Set**:
The permission set assigned when a user is created unless the administrator explicitly enables write permissions.
_Avoid_: Inherited permission, implicit write permission

**Upload Permission**:
The permission to create resources by uploading files, uploading directories, or creating directories.
_Avoid_: Create-directory permission, separate folder permission

**Rename Permission**:
The permission to rename resources.
_Avoid_: Modify permission, edit permission

**Delete Permission**:
The permission to delete files and recursively delete directories.
_Avoid_: File-only delete permission, directory-only delete permission

**Directory Upload**:
An upload that preserves a selected local directory and its nested resources under the current resource path.
_Avoid_: Bulk upload, flattened upload

**Upload Limit**:
A configured boundary on uploaded file size, total upload size, or directory upload resource count.
_Avoid_: Unlimited upload, best-effort limit

**Upload Progress**:
The visible progress of an in-flight upload.
_Avoid_: Background upload, silent upload

**Recursive Delete**:
A delete action that removes a directory and all resources contained inside it.
_Avoid_: Empty-directory delete, shallow delete

**Delete Result**:
The outcome of a delete action. Success means the target resource no longer exists; a partial recursive delete failure is reported as a failure.
_Avoid_: Best-effort success, silent partial delete

**Delete Confirmation**:
A required confirmation for deleting a directory that states the directory and all contained resources will be removed.
_Avoid_: Resource count confirmation, silent directory delete

**Directory Archive**:
A synchronous downloadable archive that contains a directory as the archive's top-level entry and preserves all nested resources below it.
_Avoid_: Flattened archive, contents-only archive

**Download Name**:
The filename suggested for a download: files use their resource name, and directory archives use the directory name with `.zip` appended.
_Avoid_: Generated random name, root archive name

**Archive Limit**:
A configured boundary on the size or resource count of a directory archive.
_Avoid_: Unlimited archive, best-effort archive

**Server Search**:
A read action that finds resources by name across the resource tree.
_Avoid_: Full-text search, content search

**Search Mode**:
The selected behavior of the search box. The default mode is current list filtering; server search is chosen explicitly and triggered manually.
_Avoid_: Global search default, automatic server search

**Search Result**:
A resource matched by server search, shown with its containing resource path to disambiguate same-named resources. Search results use the same open and available-action semantics as directory listings, and write actions from search results refresh the same server search.
_Avoid_: Tree result, grouped result

**Search Result Limit**:
The maximum number of server search results returned before the result set is reported as truncated.
_Avoid_: Unbounded search, silent truncation

**Server Search Query**:
A server search input with at least two non-whitespace characters.
_Avoid_: Empty server search, one-character server search

**Name Match**:
A case-insensitive plain substring match against a resource name.
_Avoid_: Regex search, wildcard search, fuzzy search

**Current List Filter**:
A read action that narrows the currently displayed directory listing by resource name without searching nested directories.
_Avoid_: Current page search, recursive filter

**Directory-First Sort**:
A listing order where directories appear before files, with the selected sort field applied within each resource type.
_Avoid_: Fully mixed sort, file-first sort

**Default Listing Order**:
The initial directory-first sort by resource name in ascending order.
_Avoid_: Recent-first listing, size-first listing

**Listing Refresh**:
The post-write state where the current directory listing is reloaded, current sort is preserved, and active filtering or search state is cleared.
_Avoid_: Stale listing, preserved filter after write

**File Size**:
The byte size shown for file resources only. Directories do not have a displayed or calculated size.
_Avoid_: Directory size, recursive size

**Modified Time**:
The last modified time of the resource itself, not a recursive time derived from nested resources, shown in the server-configured time zone.
_Avoid_: Recursive modified time, business updated time

**Name Conflict**:
A write action failure caused by another resource with the same name already existing in the target directory.
_Avoid_: Auto-overwrite, auto-rename

**Write Failure**:
A failed write action reported with the affected resource path and a user-actionable reason when that path is known.
_Avoid_: Generic failure, silent failure

**Atomic Directory Upload**:
A directory upload that either creates the complete selected directory structure or creates no resources when any part fails.
_Avoid_: Partial upload, best-effort upload

**Resource Name**:
The visible name of a resource within its containing directory. It may use ordinary human-readable characters, including leading dots, but it must not be empty, be `.` or `..`, contain path separators, NUL bytes, or control characters.
_Avoid_: Raw path segment, sanitized name
