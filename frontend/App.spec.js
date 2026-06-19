import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'

import App from './App.vue'

describe('File Hub browser', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  it('shows anonymous login and root resources', async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse({
        authenticated: false,
        actions: { login: true, upload: false, rename: false, delete: false },
      }))
      .mockResolvedValueOnce(jsonResponse({
        path: '',
        sort: { field: 'name', order: 'asc' },
        filter: { query: '' },
        breadcrumbs: [{ label: 'Root Directory', path: '' }],
        resources: [
          { name: 'docs', resourcePath: 'docs', kind: 'directory', modifiedTime: '2026-06-19 09:00:00' },
          { name: 'readme.txt', resourcePath: 'readme.txt', kind: 'file', size: 5, modifiedTime: '2026-06-19 09:01:00' },
        ],
      }))
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await flushPromises()

    expect(wrapper.get('#login-action').text()).toBe('Login')
    expect(wrapper.text()).toContain('docs')
    expect(wrapper.text()).toContain('readme.txt')
    expect(wrapper.get('nav[aria-label="Breadcrumb"]').text()).toBe('Root Directory')
    expect(fetch).toHaveBeenCalledWith('/api/identity')
    expect(fetch).toHaveBeenCalledWith('/api/list?path=&sort=name&order=asc&filter=')
  })

  it('logs in and exposes a file read action', async () => {
    const anonymous = {
      authenticated: false,
      actions: { login: true, upload: false, rename: false, delete: false },
    }
    const administrator = {
      authenticated: true,
      username: 'admin',
      actions: { login: false, logout: true, console: true, upload: true, rename: true, delete: true },
    }
    const listing = {
      path: '',
      sort: { field: 'name', order: 'asc' },
      filter: { query: '' },
      breadcrumbs: [],
      resources: [
        { name: 'readme.txt', resourcePath: 'readme.txt', kind: 'file', size: 5, modifiedTime: '2026-06-19 09:01:00' },
      ],
    }
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(anonymous))
      .mockResolvedValueOnce(jsonResponse(listing))
      .mockResolvedValueOnce(jsonResponse(null, 204))
      .mockResolvedValueOnce(jsonResponse(administrator))
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await flushPromises()
    await wrapper.get('#login-action').trigger('click')
    await wrapper.get('#login-username').setValue('admin')
    await wrapper.get('#login-password').setValue('bootstrap-password')
    await wrapper.get('#login-form').trigger('submit')
    await flushPromises()

    expect(wrapper.get('[aria-label="Identity Area"]').text()).toContain('admin')
    expect(wrapper.get('a[aria-label="Download readme.txt"]').attributes('href')).toBe(
      '/api/download?path=readme.txt',
    )
    expect(fetch).toHaveBeenCalledWith('/api/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username: 'admin', password: 'bootstrap-password' }),
    })
  })

  it('filters the current directory without changing its sort', async () => {
    const listing = {
      path: '', sort: { field: 'name', order: 'asc' }, filter: { query: '' }, breadcrumbs: [], resources: [],
    }
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse({ authenticated: false, actions: { login: true } }))
      .mockResolvedValueOnce(jsonResponse(listing))
      .mockResolvedValueOnce(jsonResponse({ ...listing, filter: { query: 'read' } }))
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await flushPromises()
    await wrapper.get('#current-list-filter').setValue('read')
    await wrapper.get('#current-list-filter-form').trigger('submit')
    await flushPromises()

    expect(fetch).toHaveBeenLastCalledWith('/api/list?path=&sort=name&order=asc&filter=read')
  })

  it('runs server search and exposes result read actions', async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse({ authenticated: false, actions: { login: true } }))
      .mockResolvedValueOnce(jsonResponse(emptyListing()))
      .mockResolvedValueOnce(jsonResponse({
        resources: [
          {
            containingPath: 'docs',
            resource: {
              name: 'guide.txt', resourcePath: 'docs/guide.txt', kind: 'file', size: 12,
              modifiedTime: '2026-06-19 09:01:00',
            },
          },
          {
            containingPath: '',
            resource: {
              name: 'guides', resourcePath: 'guides', kind: 'directory',
              modifiedTime: '2026-06-19 09:02:00',
            },
          },
        ],
        truncated: true,
      }))
      .mockResolvedValueOnce(jsonResponse({ ...emptyListing(), path: 'docs' }))
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await flushPromises()
    await wrapper.get('#search-mode').setValue('serverSearch')
    await wrapper.get('#search-query').setValue('guide')
    await wrapper.get('#search-form').trigger('submit')
    await flushPromises()

    expect(fetch).toHaveBeenLastCalledWith('/api/search?q=guide')
    expect(wrapper.text()).toContain('Search results truncated')
    expect(wrapper.get('a[aria-label="Download guide.txt"]').attributes('href')).toBe(
      '/api/download?path=docs%2Fguide.txt',
    )
    expect(wrapper.get('a[aria-label="Download archive for guides"]').attributes('href')).toBe(
      '/api/archive?path=guides',
    )
    await wrapper.get('button[aria-label="Open containing directory docs"]').trigger('click')
    expect(fetch).toHaveBeenLastCalledWith('/api/list?path=docs&sort=name&order=asc&filter=')
  })

  it('renders authenticated identity controls and logs out', async () => {
    const authenticated = {
      authenticated: true,
      username: 'admin',
      actions: { login: false, passwordChange: true, logout: true, console: true },
    }
    const anonymous = { authenticated: false, actions: { login: true } }
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(authenticated))
      .mockResolvedValueOnce(jsonResponse(emptyListing()))
      .mockResolvedValueOnce(jsonResponse(null, 204))
      .mockResolvedValueOnce(jsonResponse(anonymous))
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await flushPromises()

    expect(wrapper.get('#console-entry').attributes('href')).toBe('/console')
    expect(wrapper.get('#password-change-action').text()).toBe('Change password')
    await wrapper.get('#logout-action').trigger('click')
    await flushPromises()

    expect(fetch).toHaveBeenCalledWith('/api/logout', { method: 'POST' })
    expect(wrapper.get('#login-action').text()).toBe('Login')
  })

  it('changes password and returns to anonymous identity', async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse({
        authenticated: true, username: 'admin',
        actions: { login: false, passwordChange: true, logout: true, console: true },
      }))
      .mockResolvedValueOnce(jsonResponse(emptyListing()))
      .mockResolvedValueOnce(jsonResponse(null, 204))
      .mockResolvedValueOnce(jsonResponse({ authenticated: false, actions: { login: true } }))
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await flushPromises()
    await wrapper.get('#password-change-action').trigger('click')
    await wrapper.get('#old-password').setValue('bootstrap-password')
    await wrapper.get('#new-password').setValue('replacement-password')
    await wrapper.get('#password-change-form').trigger('submit')
    await flushPromises()

    expect(fetch).toHaveBeenCalledWith('/api/password', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ oldPassword: 'bootstrap-password', newPassword: 'replacement-password' }),
    })
    expect(wrapper.get('#login-action').text()).toBe('Login')
  })

  it('gates write controls by permissions and creates a directory', async () => {
    const listing = {
      ...emptyListing(),
      resources: [
        { name: 'old.txt', resourcePath: 'old.txt', kind: 'file', size: 3, modifiedTime: '2026-06-19' },
      ],
    }
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse({
        authenticated: true, username: 'writer',
        actions: { upload: true, rename: true, delete: true },
      }))
      .mockResolvedValueOnce(jsonResponse(listing))
      .mockResolvedValueOnce(jsonResponse(null, 201))
      .mockResolvedValueOnce(jsonResponse({ ...listing, resources: [] }))
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await flushPromises()
    expect(wrapper.get('#upload-file-action').exists()).toBe(true)
    expect(wrapper.get('#upload-directory-action').exists()).toBe(true)
    expect(wrapper.get('button[aria-label="Rename old.txt"]').exists()).toBe(true)
    expect(wrapper.get('button[aria-label="Delete old.txt"]').exists()).toBe(true)

    await wrapper.get('#create-directory-action').trigger('click')
    await wrapper.get('#create-directory-name').setValue('new-directory')
    await wrapper.get('#create-directory-form').trigger('submit')
    await flushPromises()

    expect(fetch).toHaveBeenCalledWith('/api/mkdir', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ path: '', name: 'new-directory' }),
    })
    expect(fetch).toHaveBeenLastCalledWith('/api/list?path=&sort=name&order=asc&filter=')
  })

  it('renames a resource and refreshes the current listing', async () => {
    const listing = {
      ...emptyListing(),
      resources: [
        { name: 'old.txt', resourcePath: 'old.txt', kind: 'file', size: 3, modifiedTime: '2026-06-19' },
      ],
    }
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse({
        authenticated: true, username: 'writer', actions: { rename: true },
      }))
      .mockResolvedValueOnce(jsonResponse(listing))
      .mockResolvedValueOnce(jsonResponse(null))
      .mockResolvedValueOnce(jsonResponse({ ...listing, resources: [] }))
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await flushPromises()
    await wrapper.get('button[aria-label="Rename old.txt"]').trigger('click')
    await wrapper.get('#rename-resource-name').setValue('new.txt')
    await wrapper.get('#rename-resource-form').trigger('submit')
    await flushPromises()

    expect(fetch).toHaveBeenCalledWith('/api/rename', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ path: 'old.txt', newName: 'new.txt' }),
    })
    expect(fetch).toHaveBeenLastCalledWith('/api/list?path=&sort=name&order=asc&filter=')
  })

  it('confirms directory deletion and reruns the active server search', async () => {
    const search = {
      resources: [{
        containingPath: '',
        resource: { name: 'guides', resourcePath: 'guides', kind: 'directory', modifiedTime: '2026-06-19' },
      }],
      truncated: false,
    }
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse({
        authenticated: true, username: 'writer', actions: { delete: true },
      }))
      .mockResolvedValueOnce(jsonResponse(emptyListing()))
      .mockResolvedValueOnce(jsonResponse(search))
      .mockResolvedValueOnce(jsonResponse(null, 204))
      .mockResolvedValueOnce(jsonResponse({ resources: [], truncated: false }))
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true)
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await flushPromises()
    await wrapper.get('#search-mode').setValue('serverSearch')
    await wrapper.get('#search-query').setValue('guide')
    await wrapper.get('#search-form').trigger('submit')
    await flushPromises()
    await wrapper.get('button[aria-label="Delete guides"]').trigger('click')
    await flushPromises()

    expect(confirm).toHaveBeenCalledWith('Delete directory "guides" and all of its contents?')
    expect(fetch).toHaveBeenCalledWith('/api/delete', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ path: 'guides' }),
    })
    expect(fetch).toHaveBeenLastCalledWith('/api/search?q=guide')
  })

  it('reruns the active server search after renaming a result', async () => {
    const result = { resources: [{ containingPath: '', resource: { name: 'old.txt', resourcePath: 'old.txt', kind: 'file', modifiedTime: 'now' } }], truncated: false }
    const fetch = vi.fn()
      .mockResolvedValueOnce(jsonResponse({ authenticated: true, username: 'writer', actions: { rename: true } }))
      .mockResolvedValueOnce(jsonResponse(emptyListing()))
      .mockResolvedValueOnce(jsonResponse(result))
      .mockResolvedValueOnce(jsonResponse(null, 204))
      .mockResolvedValueOnce(jsonResponse({ resources: [], truncated: false }))
    vi.stubGlobal('fetch', fetch)
    const wrapper = mount(App)
    await flushPromises()
    await wrapper.get('#search-mode').setValue('serverSearch')
    await wrapper.get('#search-query').setValue('old')
    await wrapper.get('#search-form').trigger('submit')
    await flushPromises()
    await wrapper.get('button[aria-label="Rename old.txt"]').trigger('click')
    await wrapper.get('#rename-resource-name').setValue('new.txt')
    await wrapper.get('#rename-resource-form').trigger('submit')
    await flushPromises()
    expect(fetch).toHaveBeenLastCalledWith('/api/search?q=old')
  })

  it('uploads selected files with progress and refreshes the listing', async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse({ authenticated: true, username: 'writer', actions: { upload: true } }))
      .mockResolvedValueOnce(jsonResponse(emptyListing()))
      .mockResolvedValueOnce(jsonResponse(emptyListing()))
      .mockResolvedValueOnce(jsonResponse(emptyListing()))
    const requests = []
    class FakeRequest {
      constructor() {
        this.upload = {}
        requests.push(this)
      }
      open(method, url) { this.method = method; this.url = url }
      send(body) {
        this.body = body
        this.upload.onprogress({ lengthComputable: true, loaded: 3, total: 4 })
        this.status = 201
        this.onload()
      }
    }
    vi.stubGlobal('fetch', fetch)
    vi.stubGlobal('XMLHttpRequest', FakeRequest)

    const wrapper = mount(App)
    await flushPromises()
    const input = wrapper.get('#upload-file-input')
    Object.defineProperty(input.element, 'files', { value: [new File(['data'], 'report.txt')] })
    await input.trigger('change')
    await flushPromises()

    expect(requests).toHaveLength(1)
    expect([requests[0].method, requests[0].url]).toEqual(['POST', '/api/upload'])
    expect(requests[0].body.get('path')).toBe('')
    expect(requests[0].body.get('file').name).toBe('report.txt')
    expect(wrapper.get('#upload-progress').attributes('value')).toBe('75')
    expect(fetch).toHaveBeenLastCalledWith('/api/list?path=&sort=name&order=asc&filter=')

    const directoryFile = new File(['nested'], 'guide.txt')
    Object.defineProperty(directoryFile, 'webkitRelativePath', { value: 'docs/guide.txt' })
    const directoryInput = wrapper.get('#upload-directory-input')
    Object.defineProperty(directoryInput.element, 'files', { value: [directoryFile] })
    await directoryInput.trigger('change')
    await flushPromises()
    expect(requests).toHaveLength(2)
    expect(requests[1].body.get('relativePath')).toBe('docs/guide.txt')
    expect(requests[1].body.get('file').name).toBe('guide.txt')
  })
})

function emptyListing() {
  return {
    path: '', sort: { field: 'name', order: 'asc' }, filter: { query: '' }, breadcrumbs: [], resources: [],
  }
}

function jsonResponse(body, status = 200) {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
  }
}
