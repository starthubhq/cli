import './assets/main.css'
import 'vue-json-pretty/lib/styles.css';

import { createApp } from 'vue'
import { createPinia } from 'pinia'
import VueJsonPretty from 'vue-json-pretty'

import App from './App.vue'
import router from './router'

const app = createApp(App)

app.use(createPinia())
app.use(router)

// Use VueJsonPretty
app.use(VueJsonPretty)

app.mount('#app')
