// The diagram paints with the theme tokens (index.css), so it follows the four themes.
const ACCENT = 'rgb(var(--c-accent))'
const MUTED = 'rgb(var(--c-text-mute))'

export default function Lineage() {
  return (
    <div className="max-w-6xl space-y-6">
      <header>
        <h1 className="text-2xl font-bold tracking-tight">Lineage</h1>
        <p className="text-text-dim text-sm mt-1">
          Table and column lineage across the lakehouse. The graph below reflects the demo schema;
          automatic capture lands with OpenMetadata and OpenLineage.
        </p>
      </header>

      <div className="card p-6 overflow-auto">
        <svg viewBox="0 0 1000 380" className="w-full min-w-[720px]" role="img" aria-label="Lineage graph">
          <defs>
            <marker id="arr" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
              <path d="M0,0 L8,4 L0,8 Z" style={{ fill: ACCENT }} />
            </marker>
            <linearGradient id="node" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" style={{ stopColor: 'rgb(var(--c-bg-soft))' }} />
              <stop offset="100%" style={{ stopColor: 'rgb(var(--c-bg-elev))' }} />
            </linearGradient>
          </defs>

          <Edge x1={210} y1={130} x2={390} y2={90} />
          <Edge x1={210} y1={130} x2={390} y2={285} />
          <Edge x1={570} y1={90} x2={750} y2={190} />
          <Edge x1={570} y1={285} x2={750} y2={190} />

          <Node x={40} y={90} title="nyc_taxi.yellow_trips" sub="source · 12 rows" accent />
          <Node x={390} y={40} title="analytics.trips_by_borough" sub="aggregate · 5 rows" />
          <Node x={390} y={235} title="analytics.daily_revenue" sub="aggregate · 3 rows" />
          <Node x={750} y={150} title="Superset dashboard" sub="consumer" />

          <text x={268} y={78} style={{ fill: MUTED }} fontSize="11">GROUP BY borough</text>
          <text x={228} y={258} style={{ fill: MUTED }} fontSize="11">GROUP BY day</text>
          <text x={655} y={120} style={{ fill: MUTED }} fontSize="11">read</text>
        </svg>
      </div>

      <div className="grid md:grid-cols-3 gap-4">
        <Fact label="Capture" value="OpenLineage" hint="emitted by engines and pipelines" />
        <Fact label="Store" value="OpenMetadata" hint="table + column lineage" />
        <Fact label="Scope" value="Catalog-wide" hint="one identity, one graph" />
      </div>
    </div>
  )
}

function Edge({ x1, y1, x2, y2 }: { x1: number; y1: number; x2: number; y2: number }) {
  const mx = (x1 + x2) / 2
  return (
    <path
      d={`M${x1},${y1} C${mx},${y1} ${mx},${y2} ${x2},${y2}`}
      fill="none"
      style={{ stroke: ACCENT }}
      strokeOpacity="0.55"
      strokeWidth="1.6"
      markerEnd="url(#arr)"
    />
  )
}

function Node({ x, y, title, sub, accent }: { x: number; y: number; title: string; sub: string; accent?: boolean }) {
  return (
    <g>
      <rect
        x={x}
        y={y}
        width={220}
        height={64}
        rx={12}
        fill="url(#node)"
        style={{ stroke: accent ? ACCENT : 'rgb(var(--c-border))' }}
        strokeWidth={accent ? 1.6 : 1}
      />
      <text x={x + 14} y={y + 26} style={{ fill: 'rgb(var(--c-text))' }} fontSize="13" fontWeight="600">{title}</text>
      <text x={x + 14} y={y + 46} style={{ fill: MUTED }} fontSize="11">{sub}</text>
    </g>
  )
}

function Fact({ label, value, hint }: { label: string; value: string; hint: string }) {
  return (
    <div className="card p-4">
      <div className="label">{label}</div>
      <div className="text-lg font-semibold mt-1">{value}</div>
      <div className="text-xs text-text-mute">{hint}</div>
    </div>
  )
}
