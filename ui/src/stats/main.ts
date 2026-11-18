import { createVaporApp } from 'vue'
import '#design/index.css'
import StatsShell from './StatsShell.vue'
import './stats.css'

createVaporApp(StatsShell).mount('#app')
