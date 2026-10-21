/**
 * The medallion layer a catalog belongs to (stardelt-nova#109): `bronze`, `silver` or
 * `gold`, named as a person reads it. The API validates the value and omits it for
 * every other catalog, so this renders nothing there. An unexpected value is still
 * rendered as text — React escapes it — never interpreted.
 */

/** The layers, in the order the platform reads them. */
export const LAYERS = ['bronze', 'silver', 'gold'] as const
export type Layer = (typeof LAYERS)[number]

/** The layer a value names, or `null` when it is not one of the three. */
export function knownLayer(value: string | null | undefined): Layer | null {
  const name = value?.trim().toLowerCase()
  return LAYERS.find((layer) => layer === name) ?? null
}

/** `bronze` → `Bronze`; used for the badge and the switcher's group labels. */
export function layerLabel(layer: Layer): string {
  return layer.charAt(0).toUpperCase() + layer.slice(1)
}

export default function LayerBadge({ layer, className }: { layer?: string | null; className?: string }) {
  const value = layer?.trim()
  if (!value) return null
  const known = knownLayer(value)
  return (
    <span
      className={[
        'badge border-accent/40 bg-accent/10 text-accent max-w-full break-words',
        className,
      ].filter(Boolean).join(' ')}
    >
      {known ? layerLabel(known) : value}
    </span>
  )
}
