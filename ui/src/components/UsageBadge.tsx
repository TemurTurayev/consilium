interface Props {
  usage: { inputTokens: number; outputTokens: number }
}

export function UsageBadge({ usage }: Props) {
  if (usage.inputTokens + usage.outputTokens === 0) return null
  return (
    <span className="usage" title="tokens this run">
      <span className="usage__seg">↑ {usage.inputTokens.toLocaleString()}</span>
      <span className="usage__seg">↓ {usage.outputTokens.toLocaleString()}</span>
    </span>
  )
}
