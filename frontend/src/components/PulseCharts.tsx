import { useLayoutEffect, useRef, useState } from 'react'
import type { RunBucket } from '../charts'

/**
 * The Overview's two small charts (stardelt-nova#767, slice 2 of #510): inline SVG and the
 * theme's own variables, so there is no chart library and the light theme follows for free.
 * Both take the day midnights from the caller and never call `Date.now()` themselves.
 *
 * Every day is a focusable value (stardelt-nova#881): hovering or tabbing to a point or a bar
 * shows its local date and its numbers, the same sentence is the element's accessible name, and
 * an `sr-only` table repeats the whole window for a screen reader. One date per label and a
 * visible `max` say what the shape means; the drawing is measured, so its circles stay round.
 */

/** The plot's height in CSS pixels; the width is measured, so the viewBox is 1:1. */
const HEIGHT = 56
/** Room above and below the line, so a stroke is not cut at the viewBox edge. */
const PAD = 4
/** Before the first measurement, and on a browser without a ResizeObserver. */
const FALLBACK_WIDTH = 240
/** A point sits this far from either edge, so its dot is never clipped. */
const INSET = 4
/** An axis label (`12 Nov`) needs about this much room, else every other/middle one is skipped. */
const LABEL_ROOM = 46
/** The tooltip's height, to keep it inside the plot's box. */
const TOOLTIP_HEIGHT = 26

const CSS = {
  accent: 'rgb(var(--c-accent))',
  success: 'rgb(var(--c-success))',
  bad: 'rgb(var(--c-bad))',
  info: 'rgb(var(--c-info))',
  border: 'rgb(var(--c-border))',
}

/** `Wed 12 Nov` in the reader's own zone and locale — the page formats times the same way. */
const dayFormat = new Intl.DateTimeFormat(undefined, { weekday: 'short', day: 'numeric', month: 'short' })
/** The axis has no room for the weekday: `12 Nov`. */
const tickFormat = new Intl.DateTimeFormat(undefined, { day: 'numeric', month: 'short' })

/** `Wed 12 Nov`, or a plain day number when the caller has no date for that index. */
function dayLabel(day: number | undefined, index: number): string {
  if (typeof day !== 'number' || !Number.isFinite(day)) return `day ${index + 1}`
  return dayFormat.format(new Date(day))
}

/** `12 Nov`: the same day as `dayLabel` without the weekday, for the axis under the plot. */
function tickLabel(day: number | undefined, index: number): string {
  if (typeof day !== 'number' || !Number.isFinite(day)) return `${index + 1}`
  return tickFormat.format(new Date(day))
}

function plural(count: number, one: string, many: string): string {
  return `${count} ${count === 1 ? one : many}`
}

/** The measured width of the chart's column, so the drawing's units are its pixels. */
function useChartWidth() {
  const ref = useRef<HTMLDivElement>(null)
  const [width, setWidth] = useState(0)
  useLayoutEffect(() => {
    const node = ref.current
    if (!node) return
    const read = () => {
      const next = node.getBoundingClientRect().width
      if (next > 0) setWidth(next)
    }
    read()
    if (typeof ResizeObserver === 'undefined') return
    const observer = new ResizeObserver(read)
    observer.observe(node)
    return () => observer.disconnect()
  }, [])
  return [ref, width > 0 ? width : FALLBACK_WIDTH] as const
}

/** Which days get a date under the chart: the newest and the oldest always, the rest while they fit. */
function labelIndices(n: number, width: number): number[] {
  if (n <= 0) return []
  const cell = width / n
  const stride = Math.max(1, Math.ceil(LABEL_ROOM / Math.max(cell, 1)))
  const out: number[] = []
  for (let i = n - 1; i >= 0; i -= stride) out.push(i)
  if (!out.includes(0)) out.push(0)
  // The first label is left-aligned and the last one right-aligned, so a middle label closer
  // than its own width would overlap them.
  return out
    .filter((i) => i === 0 || i === n - 1 || (i * cell >= LABEL_ROOM && (n - 1 - i) * cell >= LABEL_ROOM))
    .sort((a, b) => a - b)
}

/** Left, centre or right, so a tooltip on an edge point stays inside the column. */
function alignFor(x: number, width: number): 'flex-start' | 'center' | 'flex-end' {
  if (width <= 0) return 'center'
  if (x < width * 0.2) return 'flex-start'
  if (x > width * 0.8) return 'flex-end'
  return 'center'
}

function Tooltip({ kind, text, x, top, width }: {
  kind: 'queries' | 'runs'
  text: string
  x: number
  top: number
  width: number
}) {
  return (
    <div
      className="pointer-events-none absolute inset-x-0 flex"
      style={{ top: Math.max(0, Math.min(HEIGHT - TOOLTIP_HEIGHT, top)), justifyContent: alignFor(x, width) }}
    >
      <span
        role="tooltip"
        data-tooltip={kind}
        className="max-w-full rounded-[6px] border border-border bg-bg-elev px-1.5 py-0.5 text-[10px] leading-snug text-text shadow-sm"
      >
        {text}
      </span>
    </div>
  )
}

/** One date per label row cell, aligned under its point and never in the column's padding. */
function DateAxis({ ticks, chart, width }: { ticks: string[]; chart: 'queries' | 'runs'; width: number }) {
  const n = ticks.length
  return (
    <div className="relative h-3.5 mt-0.5" aria-hidden="true" data-axis={chart}>
      {labelIndices(n, width).map((i) => {
        const left = n <= 1 ? 50 : (INSET + (i / (n - 1)) * (width - INSET * 2)) / width * 100
        return (
          <span
            key={i}
            data-day={i}
            className="absolute top-0 whitespace-nowrap text-[10px] leading-none text-text-mute"
            style={{ left: `${left}%`, transform: i === 0 ? undefined : i === n - 1 ? 'translateX(-100%)' : 'translateX(-50%)' }}
          >
            {ticks[i]}
          </span>
        )
      })}
    </div>
  )
}

/** The plot's own caption: what the window is and its largest value, so the shape has a scale. */
function Caption({ scope, max, chart }: { scope?: string; max: number; chart: 'queries' | 'runs' }) {
  return (
    <div className="flex items-baseline justify-between gap-2 text-[10px] text-text-mute">
      <span>{scope}</span>
      <span data-chart-max={chart}>max {max}</span>
    </div>
  )
}

/** One point per day over the counts; an all-zero window is a flat line at the bottom. */
export function QuerySparkline({ counts, days, scope }: { counts: number[]; days: number[]; scope?: string }) {
  const [ref, width] = useChartWidth()
  const [active, setActive] = useState<number | null>(null)
  const n = counts.length
  const max = counts.reduce((m, c) => (c > m ? c : m), 0)
  const total = counts.reduce((sum, c) => sum + c, 0)
  const inner = HEIGHT - PAD * 2
  const x = (i: number) => (n <= 1 ? width / 2 : INSET + (i / (n - 1)) * (width - INSET * 2))
  const y = (c: number) => (max <= 0 ? HEIGHT - PAD : HEIGHT - PAD - (c / max) * inner)
  const points = counts.map((c, i) => `${x(i).toFixed(2)},${y(c).toFixed(2)}`).join(' ')
  const texts = counts.map((c, i) => `${dayLabel(days[i], i)} · ${plural(c, 'query', 'queries')}`)
  const summary = `${plural(days.length || n, 'day', 'days')} · ${plural(total, 'query', 'queries')}`
  const dayTexts = counts.map((_, i) => dayLabel(days[i], i))
  const tickTexts = counts.map((_, i) => tickLabel(days[i], i))
  return (
    <div>
      <Caption scope={scope} max={max} chart="queries" />
      <div ref={ref} className="relative">
        <svg
          data-chart="queries"
          role="group"
          aria-label={`Queries per day: ${summary}.`}
          viewBox={`0 0 ${width} ${HEIGHT}`}
          height={HEIGHT}
          preserveAspectRatio="xMidYMid meet"
          className="block w-full"
          onMouseLeave={() => setActive(null)}
        >
          <title>{summary}</title>
          <polyline
            points={points}
            stroke={CSS.accent}
            strokeWidth="1.5"
            fill="none"
            vectorEffect="non-scaling-stroke"
          />
          {counts.map((c, i) => (
            <g
              key={i}
              data-day={i}
              data-count={c}
              role="img"
              tabIndex={0}
              aria-label={texts[i]}
              className="cursor-pointer outline-none"
              onMouseEnter={() => setActive(i)}
              onFocus={() => setActive(i)}
              onBlur={() => setActive(null)}
            >
              <title>{texts[i]}</title>
              {active === i && (
                <circle cx={x(i)} cy={y(c)} r={6} fill="none" stroke={CSS.accent} strokeWidth="1" />
              )}
              <circle cx={x(i)} cy={y(c)} r={2.5} fill={CSS.accent} />
              {/* A 9 px target: a 2.5 px dot is impossible to hit with a pointer or a thumb. */}
              <circle cx={x(i)} cy={y(c)} r={9} fill="transparent" pointerEvents="all" />
            </g>
          ))}
        </svg>
        {active !== null && (
          <Tooltip
            kind="queries"
            text={texts[active]}
            x={x(active)}
            top={y(counts[active]) - TOOLTIP_HEIGHT - 2}
            width={width}
          />
        )}
      </div>
      <DateAxis ticks={tickTexts} chart="queries" width={width} />
      <table className="sr-only" data-chart-table="queries">
        <caption>Queries per day, last {n} days</caption>
        <thead>
          <tr><th scope="col">Day</th><th scope="col">Queries</th></tr>
        </thead>
        <tbody>
          {counts.map((c, i) => (
            <tr key={i}><th scope="row">{dayTexts[i]}</th><td>{c}</td></tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

/** The middle of a bar's slot, which is where its tooltip points. */
function slotCenter(index: number, slot: number): number {
  return index * slot + slot / 2
}

const RUN_LEGEND = [
  { label: 'ok', fill: CSS.success },
  { label: 'failed', fill: CSS.bad },
  { label: 'running', fill: CSS.info },
  { label: 'other', fill: CSS.border },
]

/** One bar per day, stacked from the bottom: ok, then failed, then running, then other. */
export function RunBars({ buckets, days, scope }: { buckets: RunBucket[]; days: number[]; scope?: string }) {
  const [ref, width] = useChartWidth()
  const [active, setActive] = useState<number | null>(null)
  const n = buckets.length
  const totals = buckets.map((b) => b.ok + b.failed + b.running + b.other)
  const total = totals.reduce((sum, c) => sum + c, 0)
  const failed = buckets.reduce((sum, b) => sum + b.failed, 0)
  const max = totals.reduce((m, c) => (c > m ? c : m), 0)
  // A gap between bars, but never wider than the bar itself.
  const slot = n > 0 ? width / n : width
  const gap = Math.min(4, slot / 4)
  const barWidth = Math.max(slot - gap, 1)
  const scale = (count: number) => (max <= 0 ? 0 : (count / max) * (HEIGHT - PAD))
  const texts = buckets.map((b, i) =>
    `${dayLabel(days[i], i)} · ${b.ok} ok · ${b.failed} failed · ${b.running} running · ${b.other} other`)
  const summary = `${plural(days.length || n, 'day', 'days')} · ${plural(total, 'run', 'runs')} · ${plural(failed, 'failure', 'failures')}`
  const dayTexts = buckets.map((_, i) => dayLabel(days[i], i))
  const tickTexts = buckets.map((_, i) => tickLabel(days[i], i))
  return (
    <div>
      <Caption scope={scope} max={max} chart="runs" />
      <div ref={ref} className="relative">
        <svg
          data-chart="runs"
          role="group"
          aria-label={`Runs per day: ${summary}.`}
          viewBox={`0 0 ${width} ${HEIGHT}`}
          height={HEIGHT}
          preserveAspectRatio="xMidYMid meet"
          className="block w-full"
          onMouseLeave={() => setActive(null)}
        >
          <title>{summary}</title>
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
              <g
                key={i}
                data-day={i}
                data-ok={b.ok}
                data-failed={b.failed}
                data-running={b.running}
                data-other={b.other}
                role="img"
                tabIndex={0}
                aria-label={texts[i]}
                className="cursor-pointer outline-none"
                onMouseEnter={() => setActive(i)}
                onFocus={() => setActive(i)}
                onBlur={() => setActive(null)}
              >
                <title>{texts[i]}</title>
                {stack.map((step, k) => {
                  const h = scale(step.count)
                  bottom -= h
                  if (h <= 0) return null
                  return <rect key={k} x={left} y={bottom} width={barWidth} height={h} fill={step.fill} />
                })}
                {/* The whole day's slot is the target: a one-run bar is a 1 px line. */}
                <rect x={i * slot} y={0} width={slot} height={HEIGHT} fill="transparent" pointerEvents="all" />
                {active === i && (
                  <rect x={left} y={0} width={barWidth} height={HEIGHT} fill="none" stroke={CSS.accent} strokeWidth="1" />
                )}
              </g>
            )
          })}
        </svg>
        {active !== null && (
          <Tooltip
            kind="runs"
            text={texts[active]}
            x={slotCenter(active, slot)}
            top={HEIGHT - PAD - scale(totals[active]) - TOOLTIP_HEIGHT - 2}
            width={width}
          />
        )}
      </div>
      <DateAxis ticks={tickTexts} chart="runs" width={width} />
      {/* Not a list: the freshness card's own rows are the page's only list items. */}
      <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-[10px] text-text-mute" data-chart-legend="runs">
        {RUN_LEGEND.map((step) => (
          <span key={step.label} className="flex items-center gap-1" data-legend={step.label}>
            <span aria-hidden="true" className="inline-block h-2 w-2 rounded-[2px]" style={{ background: step.fill }} />
            {step.label}
          </span>
        ))}
      </div>
      <table className="sr-only" data-chart-table="runs">
        <caption>Runs per day, last {n} days</caption>
        <thead>
          <tr>
            <th scope="col">Day</th><th scope="col">Ok</th><th scope="col">Failed</th>
            <th scope="col">Running</th><th scope="col">Other</th>
          </tr>
        </thead>
        <tbody>
          {buckets.map((b, i) => (
            <tr key={i}>
              <th scope="row">{dayTexts[i]}</th>
              <td>{b.ok}</td><td>{b.failed}</td><td>{b.running}</td><td>{b.other}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

