import { createRouter, createWebHistory } from 'vue-router'
import FormView from '@/views/FormView.vue'
import RunView from '../views/RunView.vue'
import HomeView from '@/views/HomeView.vue'

const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes: [
    {
      path: '/',
      name: 'home',
      component: HomeView,
    },
    {
      path: '/:namespace/:slug/:version',
      name: 'run--details',
      component: RunView,
    },
    {
      path: '/form',
      name: 'form',
      meta: { requiresAuth: false },
      component: FormView
    }
  ],
})

export default router
