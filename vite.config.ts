import { defineConfig } from 'vite'
import RubyPlugin from 'vite-plugin-ruby'
import react from '@vitejs/plugin-react'
import path from 'path'

export default defineConfig({
  plugins: [
    RubyPlugin(),
    react(),
  ],
  server: {
    headers: {
      'Cache-Control': 'no-store',
    },
  },
  optimizeDeps: {
    // Force all React packages into a single optimization pass so they share
    // one CJS interop wrapper. Without this, the optimizer may create separate
    // wrappers that return different module instances.
    include: [
      'react',
      'react-dom',
      'react-dom/client',
      'react/jsx-runtime',
      'react/jsx-dev-runtime',
      'react-router-dom',
      'motion/react',
    ],
    force: true,
  },
  resolve: {
    dedupe: ['react', 'react-dom'],
    alias: {
      // Map bare-specifier imports used by the connection infrastructure
      // (previously resolved by importmap-rails, now resolved by Vite)
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
