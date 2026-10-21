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
