import { useState, type ReactNode } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import { api } from '../api'
import ErrorBox from '../components/ErrorBox'
import { useYourWarehouse, warehouseHint, warehouseText } from '../components/WarehouseNote'
import { formatTime } from '../format'
import { isAdmin } from '../roles'

/**
 * Who Nova thinks the person is, and what that gives them (stardelt-nova#214,
 * U0/D32). Every statement on this page comes from `GET /api/me`; Nova holds no
 * identity of its own, so there is nothing to edit here.
 *
 * The page is reached from the person menu (#216) and by URL. It deliberately has
 * no navigation item: it is a reference, not a place people work.
 */

const MEMBER_SENTENCE =
  'Administrators run the platform: they see all connections and pipelines of all groups and manage the platform apps. Ask one to change your role.'

const NO_GROUPS_SENTENCE =
  'You are not in any group. Personal connections and the SQL editor work; features that belong to a group need one.'

const GROUPS_OVERAGE =
  'Your identity provider sent more groups than fit in your sign-in token, so Nova sees only some of them. Ask an administrator.'

const DEV_SENTENCE =
  'Development mode: nobody signs in; you are the operator of this installation.'

/** One label/value pair. Long values wrap; nothing is truncated, because it is the answer. */
function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-0.5 sm:flex-row sm:items-baseline sm:gap-3">
      <dt className="text-text-mute text-xs sm:w-32 sm:shrink-0">{label}</dt>
      <dd className="min-w-0 text-sm break-words">{children}</dd>
    </div>
  )
}

/** The user id with Copy; the text stays selectable when the clipboard refuses. */
function UserId({ sub }: { sub: string }) {
  const [copied, setCopied] = useState(false)
  const [failed, setFailed] = useState(false)

  async function copy() {
    setCopied(false)
    setFailed(false)
    try {
      if (!navigator.clipboard) throw new Error('no clipboard')
      await navigator.clipboard.writeText(sub)
      setCopied(true)
    } catch {
      setFailed(true)
    }
  }

  return (
    <span className="flex flex-wrap items-center gap-2">
      <span className="font-mono text-xs break-all select-all">{sub}</span>
      <button
        type="button"
        className="btn-ghost px-2 py-1 text-xs"
        aria-label="Copy your user id"
        onClick={() => void copy()}
      >
        Copy
      </button>
      {copied && (
        <span role="status" className="text-xs text-ok">
          Copied
        </span>
      )}
      {failed && (
        <span role="alert" className="text-xs text-bad break-words">
          Copy failed: select the user id and copy it
        </span>
      )}
    </span>
  )
}

function Card({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="card p-5">
      <h2 className="text-base font-semibold">{title}</h2>
      {children}
    </section>
  )
}

export default function Profile() {
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  const data = me.data
  const admin = isAdmin(data)
  const dev = data?.auth_mode === 'static-dev'
  const groups = data?.groups ?? []
  // The SQL compute class this person lands in (#236); `undefined` while it is unknown.
  const warehouse = useYourWarehouse()

  return (
    <div className="max-w-3xl space-y-6">
      <header>
        <h1 className="text-2xl font-bold tracking-tight">Profile</h1>
        <p className="text-text-dim text-sm mt-1">Who Nova thinks you are, and what that gives you.</p>
      </header>

      {me.isLoading && (
        <Card title="Account">
          <p className="mt-3 text-sm text-text-mute">Loading…</p>
        </Card>
      )}

      {me.isError && (
        <Card title="Account">
          <div className="mt-3">
            <ErrorBox error={me.error} onRetry={() => void me.refetch()} />
          </div>
        </Card>
      )}

      {data && (
        <>
          <Card title="Account">
            <dl className="mt-3 space-y-2">
              {dev ? (
                <Row label="Username">{data.user}</Row>
              ) : (
                <>
                  <Row label="Name">{data.name || '—'}</Row>
                  <Row label="Username">{data.user}</Row>
                  <Row label="Email">{data.email || '—'}</Row>
                  <Row label="User id">
                    {data.sub ? <UserId sub={data.sub} /> : '—'}
                  </Row>
                </>
              )}
            </dl>
            {dev && <p className="mt-3 text-sm text-text-dim">{DEV_SENTENCE}</p>}
          </Card>

          <Card title="Sign-in">
            <dl className="mt-3 space-y-2">
              <Row label="Method">{dev ? 'None (development mode)' : 'Single sign-on'}</Row>
              {data.via && (
                <Row label="Session">{data.via === 'bearer' ? 'API token' : 'Browser session'}</Row>
              )}
            </dl>
            {data.session_expires_at != null && data.session_expires_at > 0 && (
              <p className="mt-3 text-sm text-text-dim">
                <span title={new Date(data.session_expires_at * 1000).toISOString()}>
                  Your session ends {formatTime(data.session_expires_at * 1000)}. You sign in again
                  then.
                </span>
              </p>
            )}
          </Card>

          <Card title={`Groups (${groups.length})`}>
            {groups.length > 0 ? (
              <ul className="mt-3 flex flex-wrap gap-2" aria-label="Your groups">
                {groups.map((group, index) => (
                  <li
                    key={`${index}-${group}`}
                    className="badge max-w-full min-w-0 whitespace-normal break-words border-accent/30 bg-accent/10 px-2.5 py-1 text-text"
                  >
                    {group}
                  </li>
                ))}
              </ul>
            ) : (
              <p className="mt-3 text-sm text-text-dim">{NO_GROUPS_SENTENCE}</p>
            )}
            {data.groups_overage && (
              <div
                role="status"
                className="mt-3 rounded-[10px] border border-amber-400/40 bg-amber-400/10 p-3 text-sm text-text-dim break-words"
              >
                {GROUPS_OVERAGE}
              </div>
            )}
          </Card>

          <Card title="Role">
            <p className="mt-3 text-sm font-medium">{admin ? 'Administrator' : 'Member'}</p>
            {admin && data.admin_group && (
              <p className="mt-2 text-sm text-text-dim break-words">
                You are an administrator because you{' '}
                {data.admin_by === 'role' ? 'hold the role' : 'belong to the group'}{' '}
                <span className="font-mono">{data.admin_group}</span>.
              </p>
            )}
            {!admin && <p className="mt-2 text-sm text-text-dim">{MEMBER_SENTENCE}</p>}
          </Card>

          <Card title="What your groups give you today">
            <p className="mt-1 text-xs text-text-mute">As more features arrive this list grows.</p>
            <ul className="mt-3 space-y-2 text-sm">
              <li>
                <Link to="/connections" className="underline decoration-border underline-offset-2 hover:text-accent">
                  Connections: members of a group use and rotate its connections (Connections)
                </Link>
              </li>
              <li>
                <Link to="/pipelines" className="underline decoration-border underline-offset-2 hover:text-accent">
                  Pipelines: members see, run and pause the pipelines their group owns (Pipelines)
                </Link>
              </li>
              <li>
                <Link to="/catalog" className="underline decoration-border underline-offset-2 hover:text-accent">
                  Table descriptions: members of a table&apos;s owner group edit its description and
                  tags (Catalog)
                </Link>
              </li>
            </ul>
            {warehouse && (
              <dl className="mt-3 space-y-2">
                <Row label="SQL warehouse">
                  {/* The class the engine schedules this person's queries into (#236). Not
                      `me.warehouse` above: that one is the default catalog. */}
                  <span title={warehouseHint(warehouse)}>Warehouse: {warehouseText(warehouse)}</span>
                </Row>
              </dl>
            )}
          </Card>

          <Card title="Where this comes from">
            <p className="mt-3 text-sm text-text-dim">
              Your groups and roles come from the identity provider, not from Nova. After an
              administrator changes them, sign out and in again.
            </p>
            <p className="mt-2 text-sm">
              <Link to="/preferences" className="text-accent underline decoration-border underline-offset-2">
                Preferences
              </Link>{' '}
              — choose how Nova looks and how times are shown.
            </p>
          </Card>
        </>
      )}
    </div>
  )
}
