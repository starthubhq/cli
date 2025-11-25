import { describe, it, expect, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import InputsComponent from '../InputsComponent.vue'

// Mock the router
const mockRoute = {
  params: {
    namespace: 'tgirotto',
    slug: 'parse-wasm',
    version: '0.0.5'
  },
  query: {}
}

vi.mock('vue-router', () => ({
  useRoute: () => mockRoute
}))

// Mock fetch
global.fetch = vi.fn()

describe('InputsComponent', () => {
  it('renders properly with lock file data', async () => {
    // Mock the lock file response
    const mockLockFileData = {
      name: 'parse-wasm',
      description: 'Generated manifest',
      version: '0.0.5',
      kind: 'wasm',
      manifest_version: 1,
      repository: 'https://github.com/starthubhq/parse-wasm',
      license: 'MIT',
      inputs: [
        {
          name: 'string',
          description: 'The string to parse',
          type: 'string',
          required: true,
          default: null
        },
        {
          name: 'type',
          description: 'The type of the object we want to parse the string into',
          type: 'type',
          required: true,
          default: null
        }
      ],
      outputs: [
        {
          name: 'response',
          description: 'The HTTP response data parsed into the',
          type: 'T',
          required: true,
          default: null
        }
      ],
      distribution: {
        primary: 'https://smltnjrrzkmazvbrqbkq.storage.supabase.co/storage/v1/s3/@sha256:parse-wasm'
      },
      digest: 'sha256:parse-wasm'
    }

    // Mock fetch to return the lock file data
    ;(global.fetch as any).mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve(mockLockFileData)
    })

    const wrapper = mount(InputsComponent)
    
    // Wait for the component to load and fetch data
    await wrapper.vm.$nextTick()
    await new Promise(resolve => setTimeout(resolve, 100)) // Wait for async fetch
    await wrapper.vm.$nextTick()
    
    // Check that the component renders the action name and version
    expect(wrapper.text()).toContain('parse-wasm@0.0.5')
    expect(wrapper.text()).toContain('Generated manifest')
    
    // Check that inputs are rendered
    expect(wrapper.text()).toContain('string')
    expect(wrapper.text()).toContain('type')
    
    // Check that outputs are rendered
    expect(wrapper.text()).toContain('response')
  })

  it('handles fetch errors gracefully', async () => {
    // Mock fetch to return an error
    ;(global.fetch as any).mockRejectedValueOnce(new Error('Network error'))

    const wrapper = mount(InputsComponent)
    
    // Wait for the component to load and handle error
    await wrapper.vm.$nextTick()
    await new Promise(resolve => setTimeout(resolve, 100)) // Wait for async fetch
    await wrapper.vm.$nextTick()
    
    // Check that error is displayed
    expect(wrapper.text()).toContain('Network error')
  })

  it('constructs correct lock file URL', async () => {
    const mockLockFileData = {
      name: 'test-action',
      description: 'Test action',
      version: '1.0.0',
      kind: 'wasm',
      manifest_version: 1,
      repository: 'https://github.com/test/test-action',
      license: 'MIT',
      inputs: [],
      outputs: [],
      distribution: { primary: 'test-url' },
      digest: 'sha256:test'
    }

    ;(global.fetch as any).mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve(mockLockFileData)
    })

    mount(InputsComponent)
    
    // Check that fetch was called with the correct URL
    expect(global.fetch).toHaveBeenCalledWith(
      'https://api.starthub.so/storage/v1/object/public/artifacts/tgirotto/parse-wasm/0.0.5/lock.json'
    )
  })
})
