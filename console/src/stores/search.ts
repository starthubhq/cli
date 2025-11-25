// stores/useSearchStore.ts
import { defineStore } from 'pinia'
import { supabase } from '@/lib/supabase'
import Fuse from 'fuse.js'

interface Action {
  id: string
  slug: string
  description: string | null
  namespace: string | null
  version: string | null
}

export const useSearchStore = defineStore('search', {
  state: () => ({
    actions: [] as Action[],
    fuse: null as Fuse<Action> | null,
    isLoaded: false
  }),

  actions: {
    async fetchActions() {
      if (this.isLoaded) return

      const { data, error } = await supabase
        .from('actions')
        .select('id, slug, description, namespace, action_versions!actions_latest_action_version_id_fkey(version_number)')

      if (error) {
        console.error('Error fetching actions:', error)
        return
      }

      this.actions = (data || []).map((action: any) => {
        const version = Array.isArray(action.action_versions) 
          ? action.action_versions[0]?.version_number 
          : action.action_versions?.version_number;
        return {
          id: action.id,
          slug: action.slug,
          description: action.description,
          namespace: action.namespace || null,
          version: version || null
        }
      }) as Action[]
      this.fuse = new Fuse(this.actions, {
        keys: ['slug', 'description'],
        threshold: 0.3,
        ignoreLocation: true
      })

      this.isLoaded = true
    },

    search(query: string): Action[] {
      if (!this.fuse || !query) return this.actions
      const results = this.fuse.search(query)
      return results.map(r => r.item)
    }
  }
})
