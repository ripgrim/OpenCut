import { defineConfig } from 'vitest/config'
import viteReact from '@vitejs/plugin-react'

// Kept separate from vite.config.ts on purpose: the Cloudflare Vite plugin
// configures a Worker SSR environment that rejects Vitest's own resolve.external
// settings and crashes the runner at startup.
export default defineConfig({
  resolve: { tsconfigPaths: true },
  plugins: [viteReact()],
  test: {
    environment: 'jsdom',
    passWithNoTests: true,
    include: ['src/**/*.{test,spec}.{ts,tsx}'],
  },
})
