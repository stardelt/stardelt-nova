import { useQuery } from '@tanstack/react-query'
import { api } from '../api'
import { formatBytes, formatCount, sqlTableRef } from '../format'
import { useCatalogs } from './CatalogSelect'

/** 128 MiB: the size below which a file is a small file (stardelt-nova#598). */
const SMALL_FILE_BYTES = 134217728

/** A number from the engine's answer; a missing or non-numeric value is absent, not zero. */
function num(value: unknown): number | null {
  if (value == null) return null
  const n = Number(value)
  return Number.isFinite(n) ? n : null
}

/**
 * "Maintenance": how many files the table has, how many of them are smaller than 128 MB,
 * how much they weigh, and how many snapshots exist. Iceberg's `$files` and `$snapshots`
 * metadata tables answer it (stardelt-nova#598).
 *
 * The card never fails the page: a view, a non-Iceberg table or an engine that does not
 * answer hides it entirely, so a person sees no red error next to a table that works.
 */
export default function TableHealth({
  ns,
  tbl,
  catalog,
}: {
  ns: string
  tbl: string
  /** The catalog the table was browsed in; absent means the API's default. */
  catalog?: string
}) {
  const catalogs = useCatalogs()
  const catalogName = catalog ?? catalogs.data?.default
  // No catalog is known yet: the queries cannot be built, and the card stays out of the way.
  // `warehouse` is the historical default `sqlTableRef` uses; it is never sent unless the
  // API named a catalog, because both queries are disabled until then.
  const refCatalog = catalogName ?? 'warehouse'
  const enabled = Boolean(catalogName)

  const filesRef = sqlTableRef(ns, `${tbl}$files`, refCatalog)
  const snapsRef = sqlTableRef(ns, `${tbl}$snapshots`, refCatalog)

  const files = useQuery({
    queryKey: ['table-health', 'files', catalogName, ns, tbl],
    queryFn: () =>
      api.query(
        `SELECT count(*) AS files,
       sum(CASE WHEN file_size_in_bytes < ${SMALL_FILE_BYTES} THEN 1 ELSE 0 END) AS small_files,
       sum(file_size_in_bytes) AS bytes
FROM ${filesRef}`,
      ),
    enabled,
    retry: false,
    staleTime: 60_000,
  })
  const snaps = useQuery({
    queryKey: ['table-health', 'snapshots', catalogName, ns, tbl],
    queryFn: () => api.query(`SELECT count(*) AS snapshots, max(committed_at) AS last_commit\nFROM ${snapsRef}`),
    enabled,
    retry: false,
    staleTime: 60_000,
  })

  if (!enabled) return null
  // A failure here is not the page's failure: the metadata queries may simply not apply.
  if (files.isError || snaps.isError) return null

  const filesRow = files.data?.rows?.[0]
  const snapsRow = snaps.data?.rows?.[0]
  const fileCount = num(filesRow?.[0])
  const smallFiles = num(filesRow?.[1])
  const bytes = num(filesRow?.[2])
  const snapshots = num(snapsRow?.[0])
  const lastCommit = snapsRow?.[1] == null ? null : String(snapsRow[1])

  return (
    <section className="card p-3" aria-labelledby="table-maintenance-title">
      <h2 id="table-maintenance-title" className="label">Maintenance</h2>
      <p className="mt-2 text-sm text-text-dim break-words">
        {fileCount == null ? '—' : formatCount(fileCount)} files ·{' '}
        {smallFiles == null ? '—' : formatCount(smallFiles)} smaller than 128 MB ·{' '}
        {bytes == null ? '—' : formatBytes(bytes)}
      </p>
      <p className="mt-1 text-sm text-text-dim break-words">
        {snapshots == null ? '—' : formatCount(snapshots)} snapshots · last committed{' '}
        {lastCommit ?? '—'}
      </p>
      {smallFiles != null && smallFiles > 0 && (
        <p className="mt-2 text-xs text-text-dim break-words">
          {formatCount(smallFiles)} {smallFiles === 1 ? 'file is' : 'files are'} smaller than 128 MB.
          {' '}Compaction merges them; the engine's <code>optimize</code> procedure does it.
        </p>
      )}
      {smallFiles === 0 && <p className="mt-2 text-xs text-text-mute">No file is smaller than 128 MB.</p>}
    </section>
  )
}
