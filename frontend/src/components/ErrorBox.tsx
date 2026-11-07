import { errorMessage } from '../api'
import { Icon } from './Icon'

/**
 * A failed load: a calm notice with the message in words and a way to try again.
 * The first letter is raised by CSS only, so the text the tests read stays the server's.
 */
export default function ErrorBox({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  return (
    <div
      role="alert"
      className="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-[10px] border border-bad/30 bg-bad/5 px-3 py-2 text-sm text-text"
    >
      <Icon name="alert" className="w-4 h-4 shrink-0 text-bad" />
      <span className="min-w-0 flex-1 break-words first-letter:uppercase">{errorMessage(error)}</span>
      {onRetry && (
        <button type="button" className="btn-ghost px-2.5 py-1 text-xs" onClick={onRetry}>
          Retry
        </button>
      )}
    </div>
  )
}
