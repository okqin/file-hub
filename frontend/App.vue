<script setup>
import { computed, onMounted, reactive, ref } from 'vue'
import ConsoleView from './ConsoleView.vue'

const isConsole = window.location.pathname === '/console'

const identity = ref(null)
const listing = ref({ breadcrumbs: [], resources: [] })
const status = ref('Loading files...')
const showLogin = ref(false)
const showPasswordChange = ref(false)
const credentials = reactive({ username: '', password: '' })
const passwordChange = reactive({ oldPassword: '', newPassword: '' })
const query = reactive({ path: '', sort: 'name', order: 'asc', filter: '' })
const searchMode = ref('currentListFilter')
const searchQuery = ref('')
const searchResults = ref([])
const searchTruncated = ref(false)
const showCreateDirectory = ref(false)
const createDirectoryName = ref('')
const renameTarget = ref(null)
const renameName = ref('')
const renameSource = ref('directoryListing')
const fileInput = ref(null)
const directoryInput = ref(null)
const uploadProgress = ref(0)
const visibleRows = computed(() => {
  if (searchMode.value === 'serverSearch') return searchResults.value
  return listing.value.resources.map(resource => ({ resource, containingPath: null }))
})

async function loadIdentity() {
  identity.value = await api('/api/identity')
}

async function loadDirectory(path = query.path) {
  query.path = path
  status.value = 'Loading files...'
  listing.value = await api(
    `/api/list?path=${encodeURIComponent(query.path)}&sort=${query.sort}&order=${query.order}&filter=${encodeURIComponent(query.filter)}`,
  )
  status.value = `${listing.value.resources.length} resources`
}

async function sortBy(field) {
  if (query.sort === field) {
    query.order = query.order === 'asc' ? 'desc' : 'asc'
  } else {
    query.sort = field
    query.order = 'asc'
  }
  await loadDirectory()
}

async function runSearch() {
  if (searchMode.value === 'currentListFilter') {
    query.filter = searchQuery.value
    await loadDirectory()
    return
  }
  status.value = 'Searching...'
  const results = await api(`/api/search?q=${encodeURIComponent(searchQuery.value)}`)
  searchResults.value = results.resources
  searchTruncated.value = results.truncated
  status.value = `${results.resources.length} search results`
}

async function openDirectory(path) {
  searchMode.value = 'currentListFilter'
  searchQuery.value = ''
  query.filter = ''
  searchResults.value = []
  searchTruncated.value = false
  await loadDirectory(path)
}

async function refreshAfterWrite() {
  searchMode.value = 'currentListFilter'
  searchQuery.value = ''
  query.filter = ''
  searchResults.value = []
  searchTruncated.value = false
  await loadDirectory(query.path)
}

async function createDirectory() {
  try {
    await api('/api/mkdir', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ path: query.path, name: createDirectoryName.value }),
    })
    createDirectoryName.value = ''
    showCreateDirectory.value = false
    await refreshAfterWrite()
    status.value = 'Directory created'
  } catch (error) {
    status.value = error.message
  }
}

function beginRename(resource) {
  renameTarget.value = resource
  renameName.value = resource.name
  renameSource.value = searchMode.value
}

async function renameResource() {
  try {
    await api('/api/rename', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ path: renameTarget.value.resourcePath, newName: renameName.value }),
    })
    renameTarget.value = null
    renameName.value = ''
    if (renameSource.value === 'serverSearch') await runSearch()
    else await refreshAfterWrite()
    status.value = 'Resource renamed'
  } catch (error) {
    status.value = error.message
  }
}

async function deleteResource(resource) {
  const description = resource.kind === 'directory'
    ? `directory "${resource.name}" and all of its contents`
    : `file "${resource.name}"`
  if (!window.confirm(`Delete ${description}?`)) return

  try {
    await api('/api/delete', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ path: resource.resourcePath }),
    })
    if (searchMode.value === 'serverSearch') await runSearch()
    else await refreshAfterWrite()
    status.value = 'Resource deleted'
  } catch (error) {
    status.value = error.message
  }
}

function uploadSelection(event, directory) {
  const files = Array.from(event.target.files || [])
  if (files.length === 0) return
  const form = new FormData()
  form.append('path', query.path)
  for (const file of files) {
    if (directory) form.append('relativePath', file.webkitRelativePath)
    form.append('file', file)
  }

  uploadProgress.value = 0
  const request = new XMLHttpRequest()
  request.open('POST', '/api/upload')
  request.upload.onprogress = progress => {
    if (progress.lengthComputable && progress.total > 0) {
      uploadProgress.value = Math.round((progress.loaded / progress.total) * 100)
    }
  }
  request.onload = async () => {
    if (request.status >= 200 && request.status < 300) {
      event.target.value = ''
      await refreshAfterWrite()
      status.value = 'Upload complete'
    } else {
      status.value = `Upload failed (${request.status})`
    }
  }
  request.onerror = () => { status.value = 'Upload failed' }
  request.send(form)
}

async function login() {
  try {
    await api('/api/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(credentials),
    })
    credentials.password = ''
    showLogin.value = false
    await loadIdentity()
    status.value = `Logged in as ${identity.value.username}`
  } catch (error) {
    status.value = error.message
  }
}

async function logout() {
  try {
    await api('/api/logout', { method: 'POST' })
    await loadIdentity()
    status.value = 'Logged out'
  } catch (error) {
    status.value = error.message
  }
}

async function changePassword() {
  try {
    await api('/api/password', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(passwordChange),
    })
    passwordChange.oldPassword = ''
    passwordChange.newPassword = ''
    showPasswordChange.value = false
    await loadIdentity()
    status.value = 'Password changed. Log in again.'
  } catch (error) {
    status.value = error.message
  }
}

async function api(path, options) {
  const response = options ? await fetch(path, options) : await fetch(path)
  if (!response.ok) {
    const body = await response.json().catch(() => null)
    throw new Error(body && body.error ? body.error.reason : `Request failed (${response.status})`)
  }
  return response.status === 204 ? null : response.json()
}

onMounted(async () => {
  if (isConsole) return
  try {
    await Promise.all([loadIdentity(), loadDirectory('')])
  } catch (error) {
    status.value = error.message
  }
})
</script>

<template>
  <ConsoleView v-if="isConsole" />
  <div v-else class="shell">
    <header>
      <h1>File Hub</h1>
      <div aria-label="Identity Area">
        <button v-if="identity?.actions.login" id="login-action" type="button" @click="showLogin = true">Login</button>
        <template v-else-if="identity?.username">
          <span>{{ identity.username }}</span>
          <a v-if="identity.actions.console" id="console-entry" href="/console">Console</a>
          <button v-if="identity.actions.passwordChange" id="password-change-action" type="button" @click="showPasswordChange = true">Change password</button>
          <button v-if="identity.actions.logout" id="logout-action" type="button" @click="logout">Logout</button>
        </template>
      </div>
    </header>
    <main>
      <form v-if="showLogin" id="login-form" class="login" @submit.prevent="login">
        <label>Username <input id="login-username" v-model="credentials.username" autocomplete="username" required maxlength="64"></label>
        <label>Password <input id="login-password" v-model="credentials.password" type="password" autocomplete="current-password" required maxlength="256"></label>
        <button class="primary" type="submit">Login</button>
        <button type="button" @click="showLogin = false">Cancel</button>
      </form>
      <form v-if="showPasswordChange" id="password-change-form" class="login" @submit.prevent="changePassword">
        <label>Current password <input id="old-password" v-model="passwordChange.oldPassword" type="password" required maxlength="256"></label>
        <label>New password <input id="new-password" v-model="passwordChange.newPassword" type="password" required minlength="8" maxlength="256"></label>
        <button class="primary" type="submit">Change password</button>
        <button type="button" @click="showPasswordChange = false">Cancel</button>
      </form>
      <div v-if="identity?.actions.upload" class="actions-bar">
        <button id="upload-file-action" type="button" @click="fileInput.click()">Upload file</button>
        <input id="upload-file-input" ref="fileInput" type="file" hidden @change="uploadSelection($event, false)">
        <button id="upload-directory-action" type="button" @click="directoryInput.click()">Upload directory</button>
        <input id="upload-directory-input" ref="directoryInput" type="file" webkitdirectory multiple hidden @change="uploadSelection($event, true)">
        <button id="create-directory-action" type="button" @click="showCreateDirectory = true">Create directory</button>
        <progress id="upload-progress" :value="uploadProgress" max="100">{{ uploadProgress }}%</progress>
      </div>
      <form v-if="showCreateDirectory" id="create-directory-form" class="login" @submit.prevent="createDirectory">
        <label>Directory name <input id="create-directory-name" v-model="createDirectoryName" required maxlength="255"></label>
        <button class="primary" type="submit">Create</button>
        <button type="button" @click="showCreateDirectory = false">Cancel</button>
      </form>
      <form v-if="renameTarget" id="rename-resource-form" class="login" @submit.prevent="renameResource">
        <label>New name <input id="rename-resource-name" v-model="renameName" required maxlength="255"></label>
        <button class="primary" type="submit">Rename</button>
        <button type="button" @click="renameTarget = null">Cancel</button>
      </form>
      <nav aria-label="Breadcrumb">
        <template v-for="breadcrumb in listing.breadcrumbs" :key="breadcrumb.path">
          <span v-if="breadcrumb.path">/</span>
          <button type="button" @click="openDirectory(breadcrumb.path)">{{ breadcrumb.label }}</button>
        </template>
      </nav>
      <form id="search-form" class="toolbar" @submit.prevent="runSearch">
        <label>
          Search mode
          <select id="search-mode" v-model="searchMode">
            <option value="currentListFilter">Current List Filter</option>
            <option value="serverSearch">Server Search</option>
          </select>
        </label>
        <label>
          Search
          <input id="search-query" v-model="searchQuery" maxlength="256">
        </label>
        <button id="server-search-submit" class="primary" type="submit">Search</button>
      </form>
      <form id="current-list-filter-form" class="toolbar" @submit.prevent="loadDirectory()">
        <label>
          Current List Filter
          <input id="current-list-filter" v-model="query.filter" maxlength="256" placeholder="Filter this directory">
        </label>
        <button class="primary" type="submit">Apply</button>
      </form>
      <p role="status">{{ status }}</p>
      <p v-if="searchMode === 'serverSearch' && searchTruncated" class="notice">Search results truncated</p>
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th><button type="button" @click="sortBy('name')">Name {{ query.sort === 'name' ? (query.order === 'asc' ? '↑' : '↓') : '' }}</button></th>
              <th>Kind</th>
              <th><button type="button" @click="sortBy('size')">Size {{ query.sort === 'size' ? (query.order === 'asc' ? '↑' : '↓') : '' }}</button></th>
              <th><button type="button" @click="sortBy('modifiedTime')">Modified {{ query.sort === 'modifiedTime' ? (query.order === 'asc' ? '↑' : '↓') : '' }}</button></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="row in visibleRows" :key="row.resource.resourcePath">
              <td>
                <button v-if="row.resource.kind === 'directory'" type="button" @click="openDirectory(row.resource.resourcePath)">
                  {{ row.resource.name }}
                </button>
                <a
                  v-else
                  :aria-label="`Download ${row.resource.name}`"
                  :href="`/api/download?path=${encodeURIComponent(row.resource.resourcePath)}`"
                >{{ row.resource.name }}</a>
              </td>
              <td>{{ row.resource.kind }}</td>
              <td>{{ row.resource.size ?? '' }}</td>
              <td>{{ row.resource.modifiedTime }}</td>
              <td>
                <button
                  v-if="row.containingPath !== null"
                  type="button"
                  :aria-label="`Open containing directory ${row.containingPath || 'Root Directory'}`"
                  @click="openDirectory(row.containingPath)"
                >{{ row.containingPath || 'Root Directory' }}</button>
                <a
                  v-if="row.resource.kind === 'directory'"
                  :aria-label="`Download archive for ${row.resource.name}`"
                  :href="`/api/archive?path=${encodeURIComponent(row.resource.resourcePath)}`"
                >Download archive</a>
                <button
                  v-if="identity?.actions.rename"
                  type="button"
                  :aria-label="`Rename ${row.resource.name}`"
                  @click="beginRename(row.resource)"
                >Rename</button>
                <button
                  v-if="identity?.actions.delete"
                  type="button"
                  :aria-label="`Delete ${row.resource.name}`"
                  @click="deleteResource(row.resource)"
                >Delete</button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </main>
  </div>
</template>

<style>
:root {
  font-family: Inter, ui-sans-serif, system-ui, sans-serif;
  color: #202124;
  background: #f4f5f7;
}
* { box-sizing: border-box; }
body { margin: 0; }
button, input { font: inherit; }
select { min-height: 36px; font: inherit; }
button, a { color: #1769aa; }
button { border: 0; background: transparent; cursor: pointer; padding: 4px; }
.primary { background: #1769aa; color: #fff; padding: 8px 14px; }
.actions-bar { display: flex; margin: 14px 0; }
.actions-bar > * { margin-right: 8px; }
.actions-bar button { border: 1px solid #aeb4bd; padding: 7px 10px; }
header {
  min-height: 58px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 24px;
  background: #fff;
  border-bottom: 1px solid #d9dde3;
}
header [aria-label="Identity Area"] { display: flex; align-items: center; }
header [aria-label="Identity Area"] > * { margin-left: 10px; }
h1 { margin: 0; font-size: 19px; letter-spacing: 0; }
main { width: 100%; max-width: 1200px; margin: 0 auto; padding: 24px; }
.login { display: flex; align-items: flex-end; padding: 14px 0 20px; }
.login > * { margin-right: 12px; }
.login label { display: grid; grid-row-gap: 5px; font-size: 13px; }
.login input { min-height: 36px; padding: 7px 9px; border: 1px solid #aeb4bd; }
.toolbar { display: flex; align-items: flex-end; margin: 14px 0; }
.toolbar > * { margin-right: 10px; }
.toolbar label { display: grid; grid-row-gap: 5px; font-size: 13px; }
.toolbar input { width: 70vw; max-width: 340px; min-height: 36px; padding: 7px 9px; border: 1px solid #aeb4bd; }
.notice { color: #7a4b00; }
nav { display: flex; align-items: center; min-height: 32px; }
nav > * { margin-right: 4px; }
.table-wrap { overflow-x: auto; border: 1px solid #d9dde3; background: #fff; }
table { width: 100%; border-collapse: collapse; }
th, td { padding: 11px 12px; border-bottom: 1px solid #e3e6ea; text-align: left; white-space: nowrap; }
th { font-size: 12px; color: #5f6368; background: #fafbfc; }
tbody tr:last-child td { border-bottom: 0; }
@media (max-width: 640px) {
  header, main { padding-left: 14px; padding-right: 14px; }
  .login { align-items: stretch; flex-direction: column; }
  .login > * { margin-right: 0; margin-bottom: 12px; }
}
</style>
