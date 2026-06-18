import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'

// https://vitest.dev/config/
export default defineConfig({
  plugins: [react()],
  server: { port: 5173 },
  test: {
    // The priority units (reducer + parser) are pure — no DOM needed.
    environment: 'node',
    include: ['src/**/*.test.ts'],
  },
})
