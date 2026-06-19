import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'

import ConsoleView from './ConsoleView.vue'

describe('Administrator console', () => {
  afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks() })

  it('lists users, creates a user, and updates anonymous permissions', async () => {
    const fetch = vi.fn()
      .mockResolvedValueOnce(response({ users: [{ username: 'reader', permissions: { upload: false, rename: false, delete: false } }], anonymousPermissions: { upload: false, rename: false, delete: false } }))
      .mockResolvedValueOnce(response({ username: 'writer', permissions: { upload: true, rename: false, delete: false } }, 201))
      .mockResolvedValueOnce(response({ upload: true, rename: false, delete: false }))
    vi.stubGlobal('fetch', fetch)
    const wrapper = mount(ConsoleView)
    await flushPromises()
    expect(wrapper.text()).toContain('reader')
    await wrapper.get('#new-username').setValue('writer')
    await wrapper.get('#new-password').setValue('writer-password')
    await wrapper.get('#new-upload').setValue(true)
    await wrapper.get('#create-user-form').trigger('submit')
    await flushPromises()
    expect(wrapper.text()).toContain('writer')
    await wrapper.get('#anonymous-upload').setValue(true)
    await flushPromises()
    expect(fetch).toHaveBeenLastCalledWith('/api/console/anonymous-permissions', expect.objectContaining({ method: 'PATCH' }))
  })
})

function response(body, status = 200) {
  return { ok: status >= 200 && status < 300, status, json: async () => body }
}
