<script setup>
import { onMounted, reactive, ref } from 'vue'

const users = ref([])
const anonymousPermissions = reactive({ upload: false, rename: false, delete: false })
const newUser = reactive({ username: '', password: '', permissions: { upload: false, rename: false, delete: false } })
const status = ref('Loading users...')

async function request(path, options) {
  const response = options ? await fetch(path, options) : await fetch(path)
  if (!response.ok) {
    const body = await response.json().catch(() => null)
    throw new Error(body?.error?.reason || `Request failed (${response.status})`)
  }
  return response.status === 204 ? null : response.json()
}

async function load() {
  const body = await request('/api/console/users')
  users.value = body.users.map(user => ({ ...user, renameTo: user.username, password: '' }))
  Object.assign(anonymousPermissions, body.anonymousPermissions)
  status.value = `${users.value.length} users`
}

async function createUser() {
  try {
    const created = await request('/api/console/users', jsonOptions('POST', newUser))
    users.value.push({ ...created, renameTo: created.username, password: '' })
    newUser.username = ''
    newUser.password = ''
    Object.assign(newUser.permissions, { upload: false, rename: false, delete: false })
    status.value = 'User created'
  } catch (error) { status.value = error.message }
}

async function updatePermissions(user) {
  try {
    const updated = await request(`/api/console/users/${encodeURIComponent(user.username)}/permissions`, jsonOptions('PATCH', user.permissions))
    Object.assign(user.permissions, updated.permissions)
    status.value = 'Permissions updated'
  } catch (error) { status.value = error.message }
}

async function updateAnonymousPermissions() {
  try {
    const updated = await request('/api/console/anonymous-permissions', jsonOptions('PATCH', anonymousPermissions))
    Object.assign(anonymousPermissions, updated)
    status.value = 'Anonymous permissions updated'
  } catch (error) { status.value = error.message }
}

async function renameUser(user) {
  try {
    const updated = await request(`/api/console/users/${encodeURIComponent(user.username)}`, jsonOptions('PATCH', { username: user.renameTo }))
    user.username = updated.username
    user.renameTo = updated.username
    status.value = 'User renamed'
  } catch (error) { status.value = error.message }
}

async function resetPassword(user) {
  try {
    await request(`/api/console/users/${encodeURIComponent(user.username)}/password`, jsonOptions('POST', { password: user.password }))
    user.password = ''
    status.value = 'Password reset'
  } catch (error) { status.value = error.message }
}

async function deleteUser(user) {
  if (!window.confirm(`Delete user "${user.username}"?`)) return
  try {
    await request(`/api/console/users/${encodeURIComponent(user.username)}`, { method: 'DELETE' })
    users.value = users.value.filter(candidate => candidate !== user)
    status.value = 'User deleted'
  } catch (error) { status.value = error.message }
}

function jsonOptions(method, body) {
  return { method, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) }
}

onMounted(async () => {
  try { await load() } catch (error) { status.value = error.message }
})
</script>

<template>
  <div class="console-shell">
    <header><h1>File Hub Console</h1><a href="/">Files</a></header>
    <main>
      <p role="status">{{ status }}</p>
      <section>
        <h2>Anonymous permissions</h2>
        <label><input id="anonymous-upload" v-model="anonymousPermissions.upload" type="checkbox" @change="updateAnonymousPermissions"> Upload</label>
        <label><input v-model="anonymousPermissions.rename" type="checkbox" @change="updateAnonymousPermissions"> Rename</label>
        <label><input v-model="anonymousPermissions.delete" type="checkbox" @change="updateAnonymousPermissions"> Delete</label>
      </section>
      <section>
        <h2>Users</h2>
        <form id="create-user-form" class="console-form" @submit.prevent="createUser">
          <label>Username <input id="new-username" v-model="newUser.username" required maxlength="64"></label>
          <label>Password <input id="new-password" v-model="newUser.password" type="password" required minlength="8" maxlength="256"></label>
          <label class="checkbox-label"><input id="new-upload" v-model="newUser.permissions.upload" type="checkbox"> Upload</label>
          <label class="checkbox-label"><input v-model="newUser.permissions.rename" type="checkbox"> Rename</label>
          <label class="checkbox-label"><input v-model="newUser.permissions.delete" type="checkbox"> Delete</label>
          <button class="primary" type="submit">Create user</button>
        </form>
        <article v-for="user in users" :key="user.username" class="user-row">
          <strong>{{ user.username }}</strong>
          <label><input v-model="user.permissions.upload" type="checkbox" @change="updatePermissions(user)"> Upload</label>
          <label><input v-model="user.permissions.rename" type="checkbox" @change="updatePermissions(user)"> Rename</label>
          <label><input v-model="user.permissions.delete" type="checkbox" @change="updatePermissions(user)"> Delete</label>
          <form @submit.prevent="renameUser(user)"><input v-model="user.renameTo" aria-label="New username" required maxlength="64"><button type="submit">Rename</button></form>
          <form @submit.prevent="resetPassword(user)"><input v-model="user.password" aria-label="New password" type="password" required minlength="8" maxlength="256"><button type="submit">Reset password</button></form>
          <button type="button" @click="deleteUser(user)">Delete</button>
        </article>
      </section>
    </main>
  </div>
</template>

<style scoped>
.console-shell { min-height: 100vh; background: #f4f5f7; color: #202124; }
header { min-height: 58px; display: flex; align-items: center; justify-content: space-between; padding: 0 24px; background: white; border-bottom: 1px solid #d9dde3; }
h1 { font-size: 19px; }
main { width: 100%; max-width: 1200px; margin: auto; padding: 24px; }
section { padding: 18px 0; border-top: 1px solid #d9dde3; }
h2 { font-size: 16px; }
.console-form, .user-row { display: flex; flex-wrap: wrap; align-items: center; padding: 12px 0; }
.console-form > *, .user-row > * { margin-right: 10px; margin-bottom: 10px; }
.console-form label { display: grid; grid-row-gap: 4px; }
.console-form .checkbox-label { display: flex; }
input { min-height: 34px; padding: 6px 8px; border: 1px solid #aeb4bd; }
button { border: 1px solid #aeb4bd; background: white; padding: 7px 10px; cursor: pointer; }
.primary { color: white; background: #1769aa; }
</style>
