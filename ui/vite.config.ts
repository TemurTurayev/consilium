import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'

// https://vitest.dev/config/
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    // Proxy REST calls to the consilium backend (matches `consilium serve`'s
    // default --addr) so the UI fetches `/api/quota` same-origin in dev.
    proxy: { '/api': 'http://localhost:7878' },
  },
  test: {
    // The priority units (reducer + parser) are pure — no DOM needed.
    environment: 'node',
    include: ['src/**/*.test.ts'],
  },
})
