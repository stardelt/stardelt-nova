import { useQuery } from '@tanstack/react-query'
import { useSearchParams } from 'react-router-dom'
import { api, type Catalog } from '../api'
import { LAYERS, knownLayer, layerLabel } from './LayerBadge'

/**
 * The catalogs the person may browse (stardelt-nova#234). One query key, so a page that
 * also reads the list — the Catalog page has to tell "denied" from "one catalog" — shares
 * the answer instead of asking twice.
 */
export function useCatalogs() {
  return useQuery({ queryKey: ['catalog', 'catalogs'], queryFn: api.catalogs })
}

/** One `<optgroup>` of the switcher; `key` is stable and `label` is what a person reads. */
type CatalogGroup = { key: string; label: string; catalogs: Catalog[] }

/**
 * The medallion layers as groups (stardelt-nova#109), or `null` when no catalog has one:
 * a flat list is what the switcher showed before layers existed, and a group header with
 * nothing to group would only add noise. The API sends the catalogs already ordered
 * bronze, silver, gold, then alphabetically; this keeps that order inside each group and
 * in `Other`, which holds every catalog without a layer.
 *
 * A layer value the API never sends (it validates) is not invented as a group label: the
 * catalog stays in the API's order under `Other`.
 */
function catalogGroups(catalogs: Catalog[]): CatalogGroup[] | null {
  if (!catalogs.some((catalog) => knownLayer(catalog.layer))) return null
  const groups: CatalogGroup[] = LAYERS
    .map((layer) => ({
      key: layer,
      label: layerLabel(layer),
      catalogs: catalogs.filter((catalog) => knownLayer(catalog.layer) === layer),
    }))
    .filter((group) => group.catalogs.length > 0)
  const other = catalogs.filter((catalog) => !knownLayer(catalog.layer))
  if (other.length > 0) groups.push({ key: 'other', label: 'Other', catalogs: other })
  return groups
}

/**
 * The catalog switcher: a native `<select>` naming the catalogs the API says the person
 * may browse. A catalog they may not browse never reaches the list, and Nova does not
 * re-implement that rule (design/multitenant-catalogs.md §5.4).
 *
 * With medallion layers (stardelt-nova#109) the options are grouped `Bronze`, `Silver`,
 * `Gold`, `Other`; without a layer anywhere the select stays one flat list.
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
  const list = catalogs.data?.catalogs ?? []
  const names = list.map((catalog) => catalog.name)
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

  const groups = catalogGroups(list)
  const options = (catalogs: Catalog[]) =>
    catalogs.map((catalog) => <option key={catalog.name} value={catalog.name}>{catalog.name}</option>)

  return (
    <select
      aria-label="Catalog"
      value={value}
      onChange={(event) => choose(event.target.value)}
      className={['input py-1.5', className].filter(Boolean).join(' ')}
    >
      {groups
        ? groups.map((group) => (
          <optgroup key={group.key} label={group.label}>
            {options(group.catalogs)}
          </optgroup>
        ))
        : options(list)}
    </select>
  )
}
