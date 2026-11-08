import { useEffect, useId, useRef, useState } from 'react'
import { api, errorMessage, type PipelineView, type RunView } from '../api'

/**
 * The one destructive confirmation of the Pipelines page: stopping an active run.
 *
 * A native `<dialog>` opened with `showModal()`: the browser traps focus, makes the page
 * behind it inert and closes it on Escape. Focus starts on Cancel, returns to the button
 * that opened it when it closes, and the confirm button sends exactly one request.
 *
 * The wording is the promise the backend keeps: the run is marked failed, the loader
 * cancels the running statement, and the remaining tasks are skipped — but nothing that
 * already committed is undone.
 */
export default function StopRunDialog({
  pipeline,
  run,
  onClose,
  onStopped,
}: {
  pipeline: PipelineView
  run: RunView
  onClose: () => void
  onStopped: (runId: string) => void
}) {
  const ref = useRef<HTMLDialogElement>(null)
  const opener = useRef<HTMLElement | null>(null)
  const inFlight = useRef(false)
  const id = useId()
  const titleId = `${id}-title`
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const dialog = ref.current
    if (!dialog) return
    opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null
    if (!dialog.open) dialog.showModal()
    // Cancel is focused first: Return does not stop a run by accident.
    dialog.querySelector<HTMLElement>('[data-autofocus]')?.focus()
    return () => {
      if (dialog.open) dialog.close()
      opener.current?.focus()
    }
  }, [])

  async function confirm() {
    if (inFlight.current) return
    inFlight.current = true
    setSubmitting(true)
    setError(null)
    try {
      await api.stopPipelineRun(pipeline.namespace, pipeline.name, run.runId)
      onStopped(run.runId)
    } catch (e: unknown) {
      setError(errorMessage(e))
      setSubmitting(false)
      inFlight.current = false
    }
  }

  return (
    <dialog
      ref={ref}
      aria-labelledby={titleId}
      onCancel={(event) => {
        event.preventDefault()
        onClose()
      }}
      className="m-auto w-[min(32rem,calc(100vw_-_2rem))] max-w-none rounded-[14px] border border-border bg-bg-elev p-0 text-text shadow-2xl backdrop:bg-black/60"
    >
      <div className="flex max-h-[85vh] flex-col">
        <div className="px-5 pt-5">
          <h2 id={titleId} className="text-lg font-semibold">Stop this run?</h2>
        </div>
        <div className="min-h-0 overflow-y-auto px-5 py-4 space-y-3 text-sm">
          <p className="text-text-dim">
            The running statement is cancelled and the remaining tasks are skipped. A change that was
            already committed is not undone.
          </p>
          <p className="break-words">
            Run <span className="font-mono">{run.runId}</span> of{' '}
            <span className="font-mono">{pipeline.name}</span>.
          </p>
          {error && (
            <div
              role="alert"
              className="rounded-[10px] border border-red-500/40 bg-red-500/10 p-3 text-sm text-bad break-words"
            >
              {error}
            </div>
          )}
        </div>
        <div className="flex flex-wrap items-center justify-end gap-2 border-t border-border px-5 py-3">
          <button type="button" className="btn-danger" disabled={submitting} onClick={() => void confirm()}>
            {submitting ? 'Stopping…' : 'Stop run'}
          </button>
          <button type="button" className="btn-ghost" onClick={onClose} data-autofocus>
            Cancel
          </button>
        </div>
      </div>
    </dialog>
  )
}
