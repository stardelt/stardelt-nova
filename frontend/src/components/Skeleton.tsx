/** A loading block that reserves the height of what replaces it (nova#512). */
export function Skeleton({ className = '' }: { className?: string }) {
  return <div aria-hidden="true" className={`skeleton ${className}`} />
}

/** Eight table rows, the shape of the Catalogs card while it loads. */
export function SkeletonTable({ rows = 8 }: { rows?: number }) {
  return (
    <div role="status" aria-label="Loading catalogs" className="space-y-2">
      {Array.from({ length: rows }, (_, i) => (
        <Skeleton key={i} className="h-8" />
      ))}
    </div>
  )
}
