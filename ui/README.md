# consilium UI

A live web view of a `conduct` run — the council deliberating in real time over
the M3b WebSocket (`/ws/session`).

## Run it

In one terminal, start the backend:

```bash
cargo run -- serve            # listens on 127.0.0.1:7878 by default
```

In another, start the UI dev server:

```bash
cd ui
npm install
npm run dev                   # opens http://localhost:5173
```

Type a task, hit **Conduct**, and watch the events stream in. To point at a
different backend, copy `.env.example` to `.env` and edit `VITE_WS_URL`.

## How it's wired

- **Types are generated, not hand-written.** `src/protocol/*.ts` is produced by
  `ts-rs` from the Rust types in `core/src/event.rs` + `core/src/protocol.rs`.
  Run `cargo test` to regenerate them; they are committed so the UI builds without
  cargo. Never hand-edit those files.
- **The brain is a pure reducer.** `src/session/reducer.ts` folds every inbound
  frame into view state; `src/session/parseFrame.ts` parses the wire. Both are
  unit-tested (`npm run test`) with no browser or socket.
- **`useSession` is the only impure part** — it owns the `WebSocket` and
  dispatches parsed frames into the reducer.

## Scripts

| Command | What |
|---|---|
| `npm run dev` | dev server with HMR |
| `npm run build` | typecheck (`tsc -b`) + production build |
| `npm run test` | run the Vitest unit suite once |
| `npm run test:coverage` | unit suite + coverage report |

## Not yet (later slices)

Quota dashboard, the multi-agent Council view, past-runs history, WS reconnect,
and a Playwright E2E (needs a stub WS harness). Slice A is the one live Session
view, one happy path plus the error frames.
