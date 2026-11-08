import type { Me } from '../api'

export default function StageBadge({ me }: { me?: Me | null }) {
  if (!me?.stage_name) return null

  const kind = me.stage_kind ?? 'dev'
  const colour =
    kind === 'prod'
      ? 'border-amber-500/60 bg-amber-500/15 text-amber-300 [[data-theme=light]_&]:text-amber-700 [[data-theme=silver]_&]:text-amber-700'
      : kind === 'test'
        ? 'border-accent/40 bg-accent/10 text-accent'
        : 'border-border text-text-dim'

  return (
    <span
      title="Platform stage"
      aria-label={`Platform stage: ${me.stage_name}`}
      className={`rounded-full px-2.5 py-0.5 text-xs font-semibold uppercase tracking-wide border ${colour}`}
    >
      {me.stage_name}
    </span>
  )
}
