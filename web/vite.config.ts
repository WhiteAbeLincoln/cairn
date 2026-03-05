import { defineConfig } from 'vite'
import solid from 'vite-plugin-solid'

export default defineConfig({
  plugins: [solid()],
  server: {
    proxy: {
      '/graphql/ws': {
        target: 'http://127.0.0.1:3001',
        ws: true,
        changeOrigin: true,
      },
      '/graphql': 'http://127.0.0.1:3001',
    },
  },
})
