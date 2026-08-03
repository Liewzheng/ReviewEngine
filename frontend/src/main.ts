import { createApp } from 'vue'
import { createPinia } from 'pinia'
import router from './router'
import App from './App.vue'

// Element Plus on-demand: unplugin-vue-components (ElementPlusResolver)
// injects per-component JS + CSS for template usage. API-style components
// invoked from script (ElNotification / ElMessageBox) are not visible to the
// resolver, so their styles are imported explicitly below.
import 'element-plus/es/components/notification/style/css'
import 'element-plus/es/components/message-box/style/css'
import './style.css'

const app = createApp(App)

app.use(createPinia())
app.use(router)

app.mount('#app')
