import { createRouter, createWebHashHistory } from 'vue-router'

const routes = [
  { path: '/', redirect: '/dashboard' },
  {
    path: '/dashboard',
    name: 'Dashboard',
    component: () => import('../views/Dashboard.vue'),
  },
  {
    path: '/history',
    name: 'ReviewHistory',
    component: () => import('../views/ReviewHistory.vue'),
  },
  {
    path: '/config',
    name: 'Configuration',
    component: () => import('../views/Configuration.vue'),
  },
  {
    path: '/queue',
    name: 'QueueMonitor',
    component: () => import('../views/QueueMonitor.vue'),
  },
  {
    path: '/llm',
    name: 'LlmStatus',
    component: () => import('../views/LlmStatus.vue'),
  },
  {
    path: '/logs',
    name: 'SystemLogs',
    component: () => import('../views/SystemLogs.vue'),
  },
  {
    path: '/experts',
    name: 'ExpertsManagement',
    component: () => import('../views/ExpertsManagement.vue'),
  },
]

export default createRouter({
  history: createWebHashHistory(),
  routes,
})
