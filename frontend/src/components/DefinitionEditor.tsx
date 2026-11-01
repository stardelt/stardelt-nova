import { useEffect, useState } from 'react'
import { ApiError, api, type PipelineSpec, type PipelineView, type ValidatePipelineResponse } from '../api'
import { CHANGE_WORDS, editorRule, parseDefinition, toUpdateRequest, toValidateRequest } from '../definition'
import { formatWhen } from '../routes/Pipelines'
import { toYaml } from '../yaml'
import ErrorBox from './ErrorBox'

/** How long the text rests before Nova is asked to check it. */
const CHECK_DELAY_MS = 500

type Check =
  | { kind: 'checking' }
  | { kind: 'broken'; message: string; line: number | null }
  | { kind: 'answer'; answer: ValidatePipelineResponse }
  | { kind: 'failed'; error: unknown }

/**
 * The definition of one pipeline as text the person can change (stardelt-nova#351). While they
 * type, the text is parsed here and, once it rests, Nova checks it (`pipelines_validate`, which
 * writes nothing); the answer says what the change would do. **Apply changes** is possible only
 * when the check passed and something would change, and it sends the version that was read, so a
 * change made by somebody else in between is refused (409) instead of overwritten.
 */
export default function DefinitionEditor({
  namespace,
  name,
  spec,
  onSaved,
  onReload,
  onClose,
}: {
  namespace: string
  name: string
  spec: PipelineSpec
  onSaved: (view: PipelineView) => void
  onReload: () => void
  onClose: () => void
}) {
  const [text, setText] = useState(() => toYaml(spec.document))
  const [check, setCheck] = useState<Check>({ kind: 'checking' })
  const [saving, setSaving] = useState(false)
  const [conflict, setConflict] = useState<string | null>(null)
  const [saveError, setSaveError] = useState<unknown>(null)

  useEffect(() => {
    const parsed = parseDefinition(text)
    if (!parsed.ok) {
      setCheck({ kind: 'broken', message: parsed.message, line: parsed.line })
      return
    }
    const rule = editorRule(parsed.document, spec.document)
    if (rule) {
      setCheck({ kind: 'broken', message: rule, line: null })
      return
    }
    setCheck({ kind: 'checking' })
    let current = true
    const timer = setTimeout(() => {
      api.validatePipeline(toValidateRequest(parsed.document)).then(
        (answer) => {
          if (current) setCheck({ kind: 'answer', answer })
        },
        (error) => {
          if (current) setCheck({ kind: 'failed', error })
        },
      )
    }, CHECK_DELAY_MS)
    return () => {
      current = false
      clearTimeout(timer)
    }
  }, [text, spec.document])

  const answer = check.kind === 'answer' ? check.answer : null
  const changes = answer?.ok && answer.action === 'update' ? answer.changes : []
  const canApply = changes.length > 0 && !saving && conflict === null

  async function apply() {
    const parsed = parseDefinition(text)
    if (!parsed.ok) return
    setSaving(true)
    setSaveError(null)
    try {
      onSaved(await api.updatePipeline(namespace, name, toUpdateRequest(parsed.document, spec.resourceVersion)))
    } catch (error) {
      if (error instanceof ApiError && error.status === 409) setConflict(error.message)
      else setSaveError(error)
    } finally {
      setSaving(false)
    }
  }

  return (
    <div className="space-y-3">
      <label className="block">
        <span className="sr-only">Definition as YAML (editable)</span>
        <textarea
          aria-label="Definition as YAML (editable)"
          className="input min-h-72 w-full font-mono text-xs"
          rows={18}
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          value={text}
          onChange={(event) => {
            setText(event.target.value)
            setConflict(null)
            setSaveError(null)
          }}
        />
      </label>

      <div aria-live="polite" className="text-sm break-words">
        {check.kind === 'checking' && <p className="text-text-mute">Checking…</p>}
        {check.kind === 'broken' && (
          <p role="alert" className="text-bad">
            {check.line !== null ? `Line ${check.line}: ` : ''}
            {check.message}
          </p>
        )}
        {check.kind === 'failed' && <ErrorBox error={check.error} />}
        {answer && !answer.ok && (
          <ul role="alert" className="list-disc pl-5 text-bad">
            {answer.errors.map((issue) => (
              <li key={issue.message}>{issue.message}</li>
            ))}
          </ul>
        )}
        {answer?.ok && answer.action !== 'update' && (
          <p role="alert" className="text-bad">This pipeline no longer exists. Reload the definition.</p>
        )}
        {answer?.ok && answer.action === 'update' && changes.length === 0 && (
          <p className="text-text-dim">Nothing changed.</p>
        )}
        {changes.length > 0 && (
          <div className="space-y-1">
            <p className="text-text-dim">This changes: {changes.map((change) => CHANGE_WORDS[change] ?? change).join(', ')}.</p>
            {answer && answer.nextRuns.length > 0 && (
              <p className="text-text-mute text-xs">
                Next runs: {answer.nextRuns.map((run) => formatWhen(run).text).join(' · ')}
              </p>
            )}
          </div>
        )}
        {conflict !== null && (
          <p role="alert" className="text-bad">
            {conflict}{' '}
            <button type="button" className="underline" onClick={onReload}>
              Reload the definition
            </button>
          </p>
        )}
        {saveError !== null && <ErrorBox error={saveError} />}
      </div>

      <div className="flex flex-wrap gap-2">
        <button type="button" className="btn-primary py-1.5 text-xs" disabled={!canApply} onClick={() => void apply()}>
          {saving ? 'Applying…' : 'Apply changes'}
        </button>
        <button type="button" className="btn-ghost py-1.5 text-xs" onClick={onClose}>
          Cancel
        </button>
      </div>
    </div>
  )
}
