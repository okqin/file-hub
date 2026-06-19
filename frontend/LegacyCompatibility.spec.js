import { readFile } from 'node:fs/promises'

import { describe, expect, it } from 'vitest'

const COMPONENTS = ['frontend/App.vue', 'frontend/ConsoleView.vue']
const UNSUPPORTED_CSS = [':has(', 'width: min(', 'align-items: end']

describe('legacy browser styles', () => {
  it('avoids CSS features unavailable in Chromium 69 and Firefox 59', async () => {
    const source = (await Promise.all(COMPONENTS.map(path => readFile(path, 'utf8')))).join('\n')

    for (const syntax of UNSUPPORTED_CSS) expect(source).not.toContain(syntax)
    expect(source).not.toMatch(/(?:^|[;{])\s*gap\s*:/m)
  })
})
