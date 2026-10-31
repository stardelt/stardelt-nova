import { useQuery } from '@tanstack/react-query'
import { api, type SqlWarehouse } from '../api'

/**
 * The SQL warehouse — the Trino compute class — a person uses, shown on the SQL editor and
 * on the Profile page (stardelt-nova#236, warehouses 3/3). Nova is not the authority: the
 * engine picks the class from the identity (the first selector the person's groups match,
 * largest class first), and `GET /api/warehouses` repeats that pick as `yours`.
 *
 * This is **not** `me.warehouse`: that is the default *catalog* (the Iceberg REST warehouse)
 * a query addresses, a different thing with the same word in it.
 */

/** `Warehouse: <displayName or name> (<size>)` — the one line both pages show. */
export function warehouseText(warehouse: SqlWarehouse): string {
  const label = warehouse.displayName || warehouse.name
  // The administrator row has no size (it is the `admin` root group, not a class).
  return warehouse.size ? `${label} (${warehouse.size})` : label
}

/** What the class allows, for the `title` tooltip: the numbers the resource group renders with. */
export function warehouseHint(warehouse: SqlWarehouse): string {
  return `${warehouse.running} running, ${warehouse.queued} queued, ${warehouse.memoryShare} memory`
}

/**
 * The class this person lands in: the `yours` row of `GET /api/warehouses`. `undefined`
 * while the answer is on its way, when the list is empty, and when no row is `yours` —
 * callers render nothing then.
 */
export function useYourWarehouse(): SqlWarehouse | undefined {
  const { data } = useQuery({ queryKey: ['warehouses'], queryFn: api.warehouses })
  return data?.find(warehouse => warehouse.yours)
}

/** `Warehouse: Small (small)`, next to the Run button. Nothing while the list is empty. */
export default function WarehouseNote({ className }: { className?: string }) {
  const warehouse = useYourWarehouse()
  if (!warehouse) return null
  return (
    <span className={className} title={warehouseHint(warehouse)}>
      Warehouse: {warehouseText(warehouse)}
    </span>
  )
}
