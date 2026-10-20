import { errorMessage } from '../api'

/** A failed load: the message in words and a way to try again. */
export default function ErrorBox({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  return (
    <div role="alert" className="text-bad text-sm flex flex-wrap items-center gap-x-3 gap-y-1">
      <span className="break-words">{errorMessage(error)}</span>
      {onRetry && (
        <button type="button" className="underline" onClick={onRetry}>
          Retry
        </button>
      )}
    </div>
  )
}
