import { Link } from 'react-router-dom'

/** The catalog page's URL: `?catalog=C` (stardelt-nova#585). */
function catalogHref(catalog: string): string {
  return `/catalog?${new URLSearchParams({ catalog }).toString()}`
}

/** The schema page's URL: `?catalog=C&ns=S`. */
function schemaHref(catalog: string, ns: string): string {
  return `/catalog?${new URLSearchParams({ catalog, ns }).toString()}`
}

/**
 * Where the person is: `catalog › schema › table`, only the parts that exist
 * (stardelt-nova#585). Every part except the last links to its level, so each level is one
 * click back; the last part is the page itself and carries `aria-current="page"`.
 */
export default function CatalogBreadcrumb({
  catalog,
  ns,
  tbl,
}: {
  catalog?: string | null
  ns?: string | null
  tbl?: string | null
}) {
  // A table only exists inside a schema, a schema inside a catalog.
  const parts = [
    ...(catalog ? [{ label: catalog, href: catalogHref(catalog) }] : []),
    ...(catalog && ns ? [{ label: ns, href: schemaHref(catalog, ns) }] : []),
    ...(catalog && ns && tbl ? [{ label: tbl, href: '' }] : []),
  ]
  if (parts.length === 0) return null

  return (
    <nav aria-label="Breadcrumb" className="min-w-0">
      <ol className="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-sm">
        {parts.map((part, i) => {
          const last = i === parts.length - 1
          return (
            <li key={`${i}-${part.label}`} className="flex min-w-0 items-center gap-x-2">
              {i > 0 && <span aria-hidden="true" className="text-text-mute">›</span>}
              {last ? (
                <span aria-current="page" className="min-w-0 break-all font-mono text-text">
                  {part.label}
                </span>
              ) : (
                <Link to={part.href} className="min-w-0 break-all font-mono text-text-dim hover:text-text">
                  {part.label}
                </Link>
              )}
            </li>
          )
        })}
      </ol>
    </nav>
  )
}
