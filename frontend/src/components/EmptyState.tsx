import { Icon, type IconName } from './Icon'
import type { ReactNode } from 'react'

export function EmptyState({
  icon,
  title,
  children,
  action,
}: {
  icon: IconName
  title: string
  children: ReactNode
  action?: ReactNode
}) {
  return (
    <div className="flex flex-col items-center text-center gap-3 py-10 px-6">
      <div className="relative motion-safe:animate-[emptyIn_400ms_ease-out]">
        <svg aria-hidden="true" viewBox="0 0 96 96" className="text-accent w-24 h-24">
          <circle cx="48" cy="48" r="44" fill="rgb(var(--c-bg-soft))" />
          <polygon points="48,14 82,72 14,72" fill="none" stroke="currentColor" strokeOpacity="0.18" strokeWidth="1.5" />
          <circle cx="20" cy="24" r="1.6" fill="currentColor" opacity="0.5" />
          <circle cx="78" cy="20" r="1.6" fill="currentColor" opacity="0.5" />
          <circle cx="74" cy="82" r="1.6" fill="currentColor" opacity="0.5" />
        </svg>
        <div className="absolute inset-0 flex items-center justify-center">
          <Icon name={icon} className="w-8 h-8 text-accent" />
        </div>
      </div>
      <p className="text-text font-semibold">{title}</p>
      <p className="text-text-mute text-sm max-w-sm">{children}</p>
      {action}
    </div>
  )
}
