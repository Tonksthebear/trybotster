import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'
import path from 'path'

export default defineConfig({
  plugins: [react()],
  test: {
    environment: 'jsdom',
    setupFiles: ['./app/frontend/test/setup.js'],
    include: ['app/frontend/**/*.test.{js,jsx,ts,tsx}'],
  },
  resolve: {
    alias: {
      'connections': path.resolve(__dirname, 'app/frontend/lib/connections'),
      'connections/': path.resolve(__dirname, 'app/frontend/lib/connections') + '/',
      'transport': path.resolve(__dirname, 'app/frontend/lib/transport'),
      'transport/': path.resolve(__dirname, 'app/frontend/lib/transport') + '/',
      'matrix': path.resolve(__dirname, 'app/frontend/lib/matrix'),
      'matrix/': path.resolve(__dirname, 'app/frontend/lib/matrix') + '/',
      'workers': path.resolve(__dirname, 'app/frontend/lib/workers'),
      'workers/': path.resolve(__dirname, 'app/frontend/lib/workers') + '/',
      'lib': path.resolve(__dirname, 'app/frontend/lib'),
      'lib/': path.resolve(__dirname, 'app/frontend/lib') + '/',
      'restty': path.resolve(__dirname, 'app/frontend/vendor/restty.js'),
      'chunk-qj4j7h9k': path.resolve(__dirname, 'app/frontend/vendor/chunk-qj4j7h9k.js'),
    },
  },
})
