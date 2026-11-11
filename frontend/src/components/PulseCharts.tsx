import type { RunBucket } from '../charts'

/**
 * The Overview's two small charts (stardelt-nova#767, slice 2 of #510): inline SVG and the
 * theme's own variables, so there is no chart library and the light theme follows for free.
 * Both take the day midnights from the caller and never call `Date.now()` themselves.
 */

const WIDTH = 140
const HEIGHT = 36
/** Room above and below the line, so a stroke is not cut at the viewBox edge. */
const PAD = 2

const CSS = {
  accent: 'rgb(var(--c-accent))',
  success: 'rgb(var(--c-success))',
  bad: 'rgb(var(--c-bad))',
  info: 'rgb(var(--c-info))',
  border: 'rgb(var(--c-border))',
}

/** One point per day over the counts; an all-zero window is a flat line at the bottom. */
export function QuerySparkline({ counts, days }: { counts: number[]; days: number[] }) {
  const n = counts.length
  const max = counts.reduce((m, c) => (c > m ? c : m), 0)
  const total = counts.reduce((sum, c) => sum + c, 0)
  const inner = HEIGHT - PAD * 2
  const x = (i: number) => (n <= 1 ? WIDTH / 2 : (i / (n - 1)) * WIDTH)
  const y = (c: number) => (max <= 0 ? HEIGHT - PAD : HEIGHT - PAD - (c / max) * inner)
  const points = counts.map((c, i) => `${x(i).toFixed(2)},${y(c).toFixed(2)}`).join(' ')
  const label = `${days.length || n} days · ${total} ${total === 1 ? 'query' : 'queries'}`
  return (
    <svg
      data-chart="queries"
      role="img"
      viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
      preserveAspectRatio="none"
      className="block w-full h-12"
    >
      <title>{label}</title>
      <polyline
        points={points}
        stroke={CSS.accent}
        strokeWidth="1.5"
        fill="none"
        vectorEffect="non-scaling-stroke"
      />
      {counts.map((c, i) => (
        <circle key={i} cx={x(i)} cy={y(c)} r="1.5" fill={CSS.accent} data-day={i} data-count={c} />
      ))}
    </svg>
  )
}

/** One bar per day, stacked from the bottom: ok, then failed, then running, then other. */
export function RunBars({ buckets, days }: { buckets: RunBucket[]; days: number[] }) {
  const n = buckets.length
  const total = buckets.reduce((sum, b) => sum + b.ok + b.failed + b.running + b.other, 0)
  const failed = buckets.reduce((sum, b) => sum + b.failed, 0)
  const max = buckets.reduce((m, b) => {
    const all = b.ok + b.failed + b.running + b.other
    return all > m ? all : m
  }, 0)
  // A gap between bars, but never wider than the bar itself.
  const slot = n > 0 ? WIDTH / n : WIDTH
  const gap = Math.min(2, slot / 3)
  const barWidth = Math.max(slot - gap, 0.5)
  const scale = (count: number) => (max <= 0 ? 0 : (count / max) * HEIGHT)
  const label = `${days.length || n} days · ${total} ${total === 1 ? 'run' : 'runs'} · ${failed} failed`
  return (
    <svg
      data-chart="runs"
      role="img"
      viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
      preserveAspectRatio="none"
      className="block w-full h-12"
    >
      <title>{label}</title>
      {buckets.map((b, i) => {
        const left = i * slot + gap / 2
        const stack: Array<{ count: number; fill: string }> = [
          { count: b.ok, fill: CSS.success },
          { count: b.failed, fill: CSS.bad },
          { count: b.running, fill: CSS.info },
          { count: b.other, fill: CSS.border },
        ]
        let bottom = HEIGHT
        return (
          <g key={i} data-day={i} data-ok={b.ok} data-failed={b.failed} data-running={b.running} data-other={b.other}>
            {stack.map((step, k) => {
              const h = scale(step.count)
              bottom -= h
              if (h <= 0) return null
              return <rect key={k} x={left} y={bottom} width={barWidth} height={h} fill={step.fill} />
            })}
          </g>
        )
      })}
    </svg>
  )
}
