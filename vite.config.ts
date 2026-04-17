import { defineConfig } from 'vite'
import RubyPlugin from 'vite-plugin-ruby'
import react from '@vitejs/plugin-react'
import path from 'path'

// When HOST_URL is set (typically in mise.toml as the tunnel hostname,
// e.g. dev.trybotster.com), Vite accepts that host in CORS / Host checks.
// HMR WebSocket is routed through a dedicated hostname (vite-dev.*)
// because the vite_ruby Rack proxy can't hijack WS upgrades through
// cloudflared's HTTP/2 origin connection.
const devHost = process.env.HOST_URL
const viteHost = devHost && devHost.replace(/^/, 'vite-')

export default defineConfig({
  plugins: [
    RubyPlugin(),
    react(),
  ],
  server: {
    headers: {
      'Cache-Control': 'no-store',
    },
    cors: {
      origin: [
        /^https?:\/\/localhost(:\d+)?$/,
        ...(devHost ? [new RegExp(`^https://${devHost.replace(/\./g, '\\.')}$`)] : []),
        ...(viteHost ? [new RegExp(`^https://${viteHost.replace(/\./g, '\\.')}$`)] : []),
      ],
    },
    ...(devHost
      ? {
          allowedHosts: ['localhost', devHost, viteHost],
          hmr: {
            // HMR WS goes to vite-dev.* hostname (direct tunnel → Vite :3036).
            // Base path (/vite-dev/) is applied by Vite; don't set `path` here.
            clientPort: 443,
            protocol: 'wss',
            host: viteHost,
          },
        }
      : {}),
  },
  optimizeDeps: {
    // Group all React packages into one optimization pass so they share
    // a single CJS interop wrapper (otherwise separate wrappers can return
    // different module instances). The `include:` list alone does this;
    // `force: true` would additionally bust the cache on every restart,
    // which invalidates open browser tabs with 504s — intentionally left off.
    include: [
      'react',
      'react-dom',
      'react-dom/client',
      'react/jsx-runtime',
      'react/jsx-dev-runtime',
      'react-router-dom',
      'motion/react',
    ],
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
    },
  },
})
