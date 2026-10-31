export const meta = {
  name: 'rate-limit',
  description: 'Plan the parts of a change, then build them side by side',
  phases: [{ name: 'Plan' }, { name: 'Build' }],
}

await agent(`Plan the parts of this change: ${args.request}`, {
  access: 'read',
  label: 'plan',
  phase: 'Plan',
  model: 'claude-code/claude-haiku-5-5',
})
await parallel([
  () => agent('Limit each API key with its own token bucket.', {
    label: 'build:per-api-key',
    phase: 'Build',
    model: 'codex/gpt-6-luna',
    isolation: 'worktree',
  }),
  () => agent('Cap how fast a bucket refills.', {
    label: 'build:cap-bucket-refills',
    phase: 'Build',
    model: 'claude-code/claude-haiku-5-5',
    isolation: 'worktree',
  }),
])
