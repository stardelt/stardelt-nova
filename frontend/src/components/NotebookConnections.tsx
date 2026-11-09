import { useQuery } from '@tanstack/react-query'
import { api } from '../api'
import { safeHttpUrl } from '../format'

// JupyterHub runs as its own app. Prefer the deployed app URL (from /api/apps,
// which derives it from NOVA_BASE_DOMAIN); fall back to a port-forward
// (`kubectl -n stardelt port-forward svc/proxy-public 8000:80`).
const FALLBACK = (import.meta.env.VITE_JUPYTERHUB_URL as string | undefined) ?? 'http://localhost:8000'

/** The hub's address and the person, or nothing while it is not usable. */
function useHub() {
  const apps = useQuery({ queryKey: ['embeds'], queryFn: api.embeds, refetchInterval: 15_000 })
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  const jupyter = apps.data?.find(a => a.id === 'jupyterhub')
  const up = jupyter?.status === 'up'
  // Only http(s) URLs may reach `href`; anything else falls back, then to nothing.
  const base = (safeHttpUrl(jupyter?.external_url) ?? safeHttpUrl(FALLBACK))?.replace(/\/+$/, '')
  const at = (path: string) => (up && base ? `${base}${path}` : undefined)
  return { up, base, at, user: me.data?.user }
}

/** Whether [`NotebookConnections`] has anything to show (the hub is up and has an address). */
export function useNotebookConnectionsAvailable(): boolean {
  const { up, base } = useHub()
  return up && !!base
}

/**
 * Your JupyterHub server, API tokens and the VS Code connection (stardelt-nova#516). They are
 * personal settings, not notebooks, so they live on Profile; the Notebooks page links here.
 */
export default function NotebookConnections() {
  const { up, base, at, user } = useHub()
  if (!up || !base) return null

  return (
    <div className="grid gap-6 md:grid-cols-2 text-sm">
      <section>
        <h3 className="font-semibold">VS Code</h3>
        <ol className="list-decimal ml-5 mt-2 space-y-1 text-text-dim">
          <li>
            Create a token under{' '}
            <a className="text-accent" href={at('/hub/token')} target="_blank" rel="noreferrer">API tokens</a>.
          </li>
          <li>
            In VS Code run <em>Jupyter: Specify Jupyter Server for Connections</em>, then choose <em>Existing</em>.
          </li>
          <li>
            Server URL <code>{base}/user/{user ?? '<you>'}/</code>, and paste the token.
          </li>
        </ol>
        <p className="text-text-mute mt-2">
          The hub sends you to your own server first; the URL is the one in the address bar
          after <em>My server</em>. A token is a password for that server — revoke it on the
          same page.
        </p>
      </section>
      <section>
        <h3 className="font-semibold">Your server and tokens</h3>
        <p className="text-text-dim mt-2">
          <a className="text-accent" href={at('/hub/spawn')} target="_blank" rel="noreferrer">Start server</a>{' '}
          creates or restarts your JupyterLab.{' '}
          <a className="text-accent" href={at('/hub/home')} target="_blank" rel="noreferrer">Hub home</a>{' '}
          shows its status, stops it and holds the server list.
        </p>
        <p className="text-text-dim mt-2">
          <a className="text-accent" href={at('/hub/token')} target="_blank" rel="noreferrer">API tokens</a>{' '}
          are what VS Code, the <code>jupyter</code> CLI and REST calls use; they are per person,
          not per notebook.
        </p>
      </section>
    </div>
  )
}
