import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import { api } from '../api'
import { ClusterNotice } from '../components/ClusterNotice'
import { isAdmin } from '../roles'

/**
 * Platform settings, for administrators.
 *
 * Deliberately assembled from what Nova already knows about itself — the session's
 * identity, the workloads it can read, the apps it probes — instead of a page of
 * editable fields that would imply Nova holds state it does not: the platform lives in
 * CRDs and values, and Nova is a viewport onto it (master spec §4).
 */
export default function Settings() {
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  const workloads = useQuery({ queryKey: ['workloads'], queryFn: api.workloads, refetchInterval: 30_000 })
  const apps = useQuery({ queryKey: ['apps'], queryFn: api.apps, refetchInterval: 30_000 })

  const snapshot = workloads.data
  const rows = snapshot?.workloads ?? []
  const images = [...new Set(rows.map(w => w.image).filter(Boolean))]
  const running = rows.filter(w => w.desired > 0 && w.ready === w.desired).length
  const up = (apps.data ?? []).filter(a => a.status === 'up').length

  // A count is a fact only while the cluster answers. `stale` is still worth showing —
  // it is the last known state, and the notice above says so — but an unanswered read is
  // not a zero, so it shows as `—` instead of "0 workloads".
  const workloadCount = workloads.isLoading
    ? '…'
    : snapshot && snapshot.state !== 'unreachable'
      ? `${running}/${rows.length}`
      : '—'

  return (
    <div className="max-w-5xl space-y-6">
      <header>
        <h1 className="text-2xl font-bold tracking-tight">Settings</h1>
        <p className="text-text-dim text-sm mt-1">
          What this Nova knows about the platform it runs on.
        </p>
      </header>

      <ClusterNotice snapshot={snapshot} />

      <section className="grid lg:grid-cols-2 gap-4">
        <div className="card p-5">
          <div className="label mb-3">Identity</div>
          <dl className="text-sm space-y-2">
            <Row label="Signed in as" value={me.data?.user ?? '—'} />
            <Row label="Email" value={me.data?.email ?? '—'} />
            <Row label="Mode" value={me.data?.auth_mode ?? '—'} />
            <Row
              label="Role"
              value={isAdmin(me.data) ? 'administrator' : 'user'}
              hint={me.data?.auth_mode === 'oidc' ? 'from the IdP group claim' : 'no IdP configured'}
            />
            <Row label="Groups" value={(me.data?.groups ?? []).join(', ') || '—'} />
          </dl>
          <p className="text-xs text-text-mute mt-4">
            Tokens never reach the browser: the session is an <span className="font-mono">HttpOnly</span>{' '}
            cookie and Nova reports claims, not credentials.
          </p>
        </div>

        <div className="card p-5">
          <div className="label mb-3">Platform</div>
          <dl className="text-sm space-y-2">
            <Row label="Warehouse" value={me.data?.warehouse ?? '—'} />
            <Row
              label="Workloads running"
              value={workloadCount}
              hint={snapshot?.state === 'stale' ? 'last known state' : undefined}
            />
            <Row label="Apps reachable" value={apps.isLoading ? '…' : `${up}/${apps.data?.length ?? 0}`} />
            <Row label="Distinct images" value={String(images.length)} />
          </dl>
          <div className="flex gap-2 mt-4">
            <Link to="/health" className="btn-ghost py-1.5 px-3 text-xs">Health →</Link>
            <Link to="/apps" className="btn-ghost py-1.5 px-3 text-xs">Apps →</Link>
          </div>
        </div>
      </section>

      <section className="card p-5">
        <div className="label mb-3">Identity forwarding</div>
        <p className="text-sm text-text-dim">
          {me.data?.identity_forwarding
            ? 'Nova forwards your token to Trino and Lakekeeper, so the catalog sees you — not Nova.'
            : 'Nova talks to Trino and Lakekeeper with its own service connection, so the catalog currently sees Nova rather than you. Query and catalog requests carry no user identity until that lands (identity contract, phase 2).'}
        </p>
      </section>

      <section className="card p-5">
        <div className="label mb-3">Running images</div>
        {images.length === 0 ? (
          <p className="text-sm text-text-mute">
            {snapshot?.state === 'ok'
              ? "The cluster answered — no workloads in Nova's namespace to read images from."
              : 'No cluster access — Nova lists images only when it can read its own namespace.'}
          </p>
        ) : (
          <ul className="space-y-1 text-[11px] font-mono text-text-dim">
            {images.map(image => (
              <li key={image} className="truncate" title={image}>{image}</li>
            ))}
          </ul>
        )}
      </section>

      <section className="card p-5">
        <div className="label mb-3">What Nova can and cannot do</div>
        <ul className="text-sm text-text-dim space-y-2 list-disc pl-5">
          <li>
            It reads <span className="font-mono">deployments</span> and{' '}
            <span className="font-mono">statefulsets</span> in its own namespace, and nothing else —
            that is its entire cluster permission.
          </li>
          <li>It writes no cluster state: platform changes belong in CRDs and values, applied by the operator.</li>
          <li>It holds no user database and no passwords; the identity provider does.</li>
        </ul>
      </section>
    </div>
  )
}

function Row({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <div className="flex items-baseline gap-3">
      <dt className="text-text-mute text-xs w-36 shrink-0">{label}</dt>
      <dd className="min-w-0">
        <span className="truncate">{value}</span>
        {hint && <span className="text-text-mute text-xs ml-2">({hint})</span>}
      </dd>
    </div>
  )
}
