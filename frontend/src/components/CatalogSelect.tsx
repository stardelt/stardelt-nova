import { useQuery } from '@tanstack/react-query'
import { useSearchParams } from 'react-router-dom'
import { api } from '../api'

/**
 * The catalogs the person may browse (stardelt-nova#234). One query key, so a page that
 * also reads the list — the Catalog page has to tell "denied" from "one catalog" — shares
 * the answer instead of asking twice.
 */
export function useCatalogs() {
  return useQuery({ queryKey: ['catalog', 'catalogs'], queryFn: api.catalogs })
}

/**
 * The catalog switcher: a native `<select>` naming the catalogs the API says the person
 * may browse. A catalog they may not browse never reaches the list, and Nova does not
 * re-implement that rule (design/multitenant-catalogs.md §5.4).
 *
 * The choice lives in the URL (`?catalog=`), so the table list, the table detail, a
 * reload and a pasted link all agree on it, and Back returns to the previous catalog.
 * With one catalog — demo mode, a single-warehouse install — there is nothing to choose
 * and the component renders nothing at all, exactly as the page looked before.
 *
 * Changing the catalog drops `ns`/`tbl`: a table name belongs to the catalog it was
 * chosen in, so the new catalog starts on its own list instead of inheriting a stale one.
 */
export default function CatalogSelect({ className }: { className?: string }) {
  const [params, setParams] = useSearchParams()
  const catalogs = useCatalogs()
  const names = (catalogs.data?.catalogs ?? []).map((catalog) => catalog.name)
  if (names.length < 2) return null

  const wanted = params.get('catalog')
  const fallback = catalogs.data?.default
  const value = wanted && names.includes(wanted)
    ? wanted
    : fallback && names.includes(fallback)
      ? fallback
      : names[0]

  function choose(name: string) {
    const next = new URLSearchParams(params)
    next.set('catalog', name)
    next.delete('ns')
    next.delete('tbl')
    setParams(next)
  }

  return (
    <select
      aria-label="Catalog"
      value={value}
      onChange={(event) => choose(event.target.value)}
      className={['input py-1.5', className].filter(Boolean).join(' ')}
    >
      {names.map((name) => <option key={name} value={name}>{name}</option>)}
    </select>
  )
}
