import type { CSSProperties } from 'react'

/** A loading block that reserves the height of what replaces it (nova#512). */
export function Skeleton({ className = '', height }: { className?: string; height?: number }) {
  const style: CSSProperties | undefined = height === undefined ? undefined : { height }
  return <div aria-hidden="true" className={`skeleton ${className}`} style={style} />
}

/** Eight table rows, the shape of the Catalogs card while it loads. */
export function SkeletonTable({
  rows = 8,
  label = 'Loading',
  rowHeight,
}: {
  rows?: number
  label?: string
  /** One row's height in px, the table's pitch minus the 8 px `space-y-2` gap. */
  rowHeight?: number
}) {
  return (
    <div role="status" aria-label={label} className="space-y-2">
      {Array.from({ length: rows }, (_, i) => (
        <Skeleton key={i} className="h-8" height={rowHeight} />
      ))}
    </div>
  )
}
