# Conformance screenshots

`preferences-*.png` — the Preferences page (stardelt-nova#215) at 1280×800 and 360×740,
dark and light, from the built UI with a stubbed `/api/settings` so it reads as an
installation that has a state store.

`profile-*.png` — the Profile page (stardelt-nova#214) at 1280 and 360 px, dark, from the
built UI with an oidc `/api/me` stub carrying the #213 fields (admin group, session end,
three groups) so every card is populated.

`schedule-options-*.png` — the Schedule dialog with the parameters, the placeholder chips,
the next runs and Advanced open (stardelt-nova#224) at 1280 and 360 px, dark, from the
built UI with stubbed principals, preview and check calls. The dialog is drawn at full
height in the image so every option is visible; the page itself still scrolls the body.

`schedule-notebook-*.png` — the Schedule dialog in Notebook mode (stardelt-nova#238) at
1280 and 360 px, dark and light, from the built UI with stubbed principals, notebooks,
versions and parameters. It shows the mode control, the pinned version with its helper,
the tagged parameters and Advanced closed; the dialog is drawn at full height so the
whole form is visible. The viewport is taller than 800/740 px for the same reason.

`catalog-switcher-*.png` — the Catalog page with four catalogs (stardelt-nova#234) at
1280×800 and 360×740, dark and light, from the built UI with stubbed `/api/catalog/catalogs`
(`finance`, `product`, `sandbox`, `warehouse`), namespaces, table and preview calls.
`finance` is the chosen catalog, `seed` is open and `nation` selected, so the switcher, the
catalog's own namespace list and the table header are visible at once. With one catalog —
demo mode — the switcher renders nothing and the page looks as it did before.

`admin-catalogs-*.png` — the administrator's Catalogs page (stardelt-nova#251) at 1280×800
and 360×740, dark and light, from the built UI with a stubbed `/api/admin/catalogs` for the
four catalogs of the rollout check: `warehouse` (owner `stardelt-admins`, `stardelt-users`
browse/select/write/create), `finance` (managed access; `stardelt-finops` owns it, `finops`
holds browse and `analytics` select), `product` and `sandbox` (no status yet, so
`Unknown`). The By catalog view is the default; the phone width shows the table scrolling
inside its card. `admin-catalogs-matrix-1280-dark.png` is the same answer as a matrix:
groups down, catalogs across, the owner prefixed in its cell and an em dash where a group
holds nothing.

`overview-metrics-*.png` — the Overview with catalog metrics (stardelt-nova#250) at
1280×800 and 360×740, dark and light, from the built demo UI: the Catalogs and Tables
tiles show the totals, the Catalogs card lists `warehouse` with its schemas, tables, size,
rows and last change, and the static Iceberg tile is gone. The phone width shows the
catalog table scrolling inside its card while the page itself does not widen.

`project-footer-*.png` — the project footer (stardelt-nova#268) at 1280×800 and 360×740,
dark and light, from the built demo UI. At the bottom of the sidebar, under the person
block, the version that is running (`stardelt v0.1.0`, the frontend package's version,
baked into the build) and the two links out of the product — Documentation and Source.
The phone width shows the drawer open, because that is where the footer lives.

`overview-*`, `catalog-*`, `query-log-*`, `preferences-*` — the four colour themes
(stardelt-nova#221) on the four pages that show a theme best, at 1280×800, from the built
UI in its dev mode (so every page also shows how it reads when its upstreams are
unreachable). Dark blue and Light keep their look; Graphite is a neutral anthracite and
Silver a neutral light gray. `overview-360-graphite.png` is the phone width.
`themes-contact-sheet.png` arranges the sixteen at 640×400 in one image for review —
rows are the pages, columns the themes, in the order dark, light, graphite, silver.

`command-palette-*.png` — the ⌘K page palette (stardelt-nova#587, slice a of #509) at
1440×900 and 390×900, dark, from the built UI with a non-admin `/api/me` stub and the
Overview's metrics, catalogs and jobs stubbed so the page behind it renders. The panel is
open with an empty query, so every group is visible and the admin group is absent: the
palette applies the same admin filter as the sidebar. At 390 px the panel is 358 px wide
and the page does not scroll sideways.

`sql-panes-*.png` — the SQL editor's two side panes (stardelt-nova#586) at 1440×900 with
both open, at 1440×900 with both collapsed, and at 390×900 where both start collapsed,
dark, from the built demo UI. The Tables pane holds the search box and the catalog tree
with `warehouse` open on `nyc_taxi`, the History pane the history and the saved queries.
Collapsed, each pane is a 40 px rail with a rotated label; at 390 px the rails are compact
bars above and below the editor and the page does not scroll sideways.

`admin-team-*.png` — the New team space dialog (stardelt-nova#597) at 1280×800, dark,
from the built UI against the dev cluster: the form with the group datalist and the live
preview of the three catalogs, the `pipeline-<group>` principal and the owner line, and
the Catalogs list after one create, with `qaenergy`, `qaenergylab` and `qaenergyref` each
`Ready` with `GrantsReady: Ready`.

`workspace-*.png` — the "My workspace" card on the Notebooks page (stardelt-nova#653) at
1280×800, dark, from the built UI with stubbed `/api/workspace` answers and the published
notebooks stubbed empty: `running` with its idle stop in 47 min and the start time, with
Open JupyterLab and Stop; `stopped` with Start; and `starting` forty-one seconds into a
spawn, with the pulsing dot and the elapsed seconds. Opening is a plain link to the hub —
Nova has no `access:servers` scope.

`deployments-compare-*.png` — the Deployments page with a peer stage (stardelt-nova#667) at
1440×900 and 390×900, dark and light, from the built UI in demo mode with
`NOVA_PEER_STAGES=prod=http://127.0.0.1:9`, so the "Compare with prod" control and the peer's
URL are real. The compare answer is stubbed (`status: "unreachable"`, `detail: "prod did not
answer within 5 seconds"`): in `NOVA_AUTH_MODE=dev` the real `/api/stages/compare` answers 401,
because it forwards the caller's own token (nova#671). The table itself (`ok`) is not shown —
that needs two installs.

`shared-account-*.png` — the public-demo banner on the Catalog page (stardelt-nova#725) at
1440×900 and 390×900, Graphite and Silver, from the built demo UI with the banner's
condition forced on for the shot (in `dev` mode `/api/me` answers `shared_account: false`,
so it does not render otherwise). At 390 px it wraps to four lines above the catalog and
covers no control.

`platform-*.png` — the Platform page (stardelt-nova#865), which replaces the old Apps and
Health pages, at 1440×1100 and 390×900, dark, from the built UI with stubbed `/api/me`
(administrator), `/api/apps`, `/api/apps/links`, `/api/workloads`, `/api/trino/cluster`
and `/api/catalog/warehouse`. It shows the grouped service cards, the Admin group with
**Platform** once, and the four tiles at the top; the extra height at 1440 px keeps the
whole Admin group in the image.
