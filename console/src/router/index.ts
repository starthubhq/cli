import { createRouter, createWebHistory } from 'vue-router'
import RunView from '../views/RunView.vue'
import HomeView from '@/views/HomeView.vue'
import MyActionsView from '@/views/MyActionsView.vue'
import ActionNewView from '@/views/ActionNewView.vue'
import BuilderView from '@/views/BuilderView.vue'

const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes: [
    {
      path: '/',
      name: 'home',
      component: MyActionsView,
    },
    {
      path: '/:namespace/:slug/:version/edit',
      name: 'edit',
      meta: { requiresAuth: false },
      component: BuilderView
    },
    {
      path: '/:namespace/:slug/:version',
      name: 'run--details',
      component: RunView,
    },
    {
      path: '/my-actions',
      name: 'my-actions',
      meta: { requiresAuth: false },
      component: MyActionsView
    },
    {
      path: '/new',
      name: 'new',
      meta: { requiresAuth: false },
      component: ActionNewView
    }
  ],
})

export default router
