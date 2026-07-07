import type { InboundFrame } from '../protocol'

/**
 * A canned conduct run for the zero-backend demo. It is replayed through the
 * exact same reducer the live socket feeds, so it exercises the real rendering
 * path — it just needs no backend, no CLIs, and no quota. It also showcases the
 * multi-provider council: a Claude conductor, a Codex worker, a Gemini reviewer.
 */
export const demoSession: InboundFrame[] = [
  { type: 'session_started', session_id: 'demo-conductor', provider: 'claude', model: 'claude-opus-4-8' },
  { type: 'thinking', text: 'Decomposing the task into subtasks and assigning workers…' },
  { type: 'message', text: 'Plan: (1) add the parser, (2) wire it into the CLI, (3) cover it with tests.' },
  // A short operator beat: the chief physician pauses the council, leaves a
  // note for the next decision point, then resumes — showcasing the pause /
  // interject / resume controls with zero backend.
  { type: 'paused' },
  { type: 'operator_note', text: 'Prefer the simpler fix, please' },
  { type: 'resumed' },
  { type: 'session_started', session_id: 'demo-worker', provider: 'codex', model: 'gpt-5.4' },
  { type: 'tool_call', name: 'edit', detail: 'src/parser.rs (+48 lines)' },
  { type: 'file_changed', path: 'src/parser.rs' },
  { type: 'tool_call', name: 'edit', detail: 'src/cli.rs (+12 lines)' },
  { type: 'file_changed', path: 'src/cli.rs' },
  { type: 'usage', input_tokens: 18432, output_tokens: 2105 },
  { type: 'message', text: 'Implemented the parser and wired it into the CLI entrypoint.' },
  { type: 'session_started', session_id: 'demo-reviewer', provider: 'gemini', model: 'gemini-3-pro-preview' },
  { type: 'thinking', text: 'Cross-family review: checking the diff for edge cases…' },
  { type: 'message', text: 'Review passed — error paths are handled; suggested one extra test.' },
  { type: 'tool_call', name: 'shell', detail: 'cargo test' },
  { type: 'usage', input_tokens: 9210, output_tokens: 845 },
  { type: 'completed', result: 'All subtasks accepted; tests green.' },
  { type: 'run_complete', completed: [1, 2, 3], halted: null, failed: null },
]
