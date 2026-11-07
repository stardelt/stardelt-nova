import { Icon, type IconName } from './Icon'

export type ObjectKind = 'catalog' | 'schema' | 'table' | 'view' | 'volume'

const GLYPH: Record<ObjectKind, IconName> = {
  catalog: 'database',
  schema: 'folder',
  table: 'table',
  view: 'eye',
  volume: 'volume',
}

/** A small, type-only mark in front of an object's name. Decorative: the row's text is the name. */
export default function ObjectIcon({ kind, className }: { kind: ObjectKind; className?: string }) {
  return (
    <span data-kind={kind} aria-hidden="true" className="text-text-mute shrink-0 inline-flex">
      <Icon name={GLYPH[kind]} className={className ?? 'w-3.5 h-3.5'} />
    </span>
  )
}
