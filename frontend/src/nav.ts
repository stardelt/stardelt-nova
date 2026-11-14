import type { IconName } from './components/Icon'

export type NavItem = { to: string; label: string; icon: IconName; end?: boolean }
/** `admin: true` hides the whole group from non-admins (see `roles.ts`). */
export type NavGroup = { label: string; items: NavItem[]; admin?: boolean }

/**
 * The one list of pages: the sidebar renders it, and the command palette searches it,
 * so a page that is reachable in navigation is reachable from ⌘K and the two cannot drift.
 */
export const NAV_GROUPS: NavGroup[] = [
  {
    label: 'Explore',
    items: [
      { to: '/', label: 'Overview', icon: 'grid', end: true },
      { to: '/catalog', label: 'Catalog', icon: 'database' },
      { to: '/query', label: 'SQL editor', icon: 'terminal' },
      { to: '/queries', label: 'Query log', icon: 'table' },
      { to: '/lineage', label: 'Lineage', icon: 'lineage' },
    ],
  },
  {
    label: 'Build',
    items: [
      { to: '/notebooks', label: 'Notebooks', icon: 'notebook' },
      { to: '/pipelines', label: 'Pipelines', icon: 'play' },
      { to: '/runs', label: 'Runs', icon: 'list' },
      { to: '/streams', label: 'Streams', icon: 'pulse' },
      { to: '/compute', label: 'Compute', icon: 'spark' },
      { to: '/connections', label: 'Connections', icon: 'key' },
      { to: '/destinations', label: 'Destinations', icon: 'mail' },
      { to: '/data', label: 'Load & transfer', icon: 'upload' },
    ],
  },
  {
    label: 'Share',
    items: [{ to: '/dashboards', label: 'Dashboards', icon: 'chart' }],
  },
  {
    // Operating the platform is not a data-engineering task: an analyst has no use for
    // the service inventory, the workload list or the settings page, and the noise
    // makes the parts they do use harder to find.
    label: 'Admin',
    admin: true,
    items: [
      { to: '/admin/catalogs', label: 'Catalogs', icon: 'database' },
      { to: '/platform', label: 'Platform', icon: 'grid' },
      { to: '/settings', label: 'Platform settings', icon: 'sliders' },
      // What `stardelt bundle deploy` published, by bundle and stage: an administrator's
      // overview, not the list of a data engineer (stardelt-nova#516, nova#618).
      { to: '/deployments', label: 'Deployments', icon: 'arrow' },
    ],
  },
]
