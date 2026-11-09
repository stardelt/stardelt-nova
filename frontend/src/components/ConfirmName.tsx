import { useQuery } from '@tanstack/react-query'
import { useId, useState } from 'react'
import { api } from '../api'

/** On a prod stage, a destructive dialog asks for the object's name (internal#106 decision 9). */
export function useConfirmName(name: string) {
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  const needed = me.data?.stage_kind === 'prod'
  const [typed, setTyped] = useState('')
  return { needed, ok: !needed || typed.trim() === name, typed, setTyped }
}

export function ConfirmNameField({ name, typed, setTyped }: { name: string; typed: string; setTyped: (v: string) => void }) {
  const id = useId()
  return (
    <div className="mt-3">
      <label htmlFor={id} className="block text-sm text-text-dim">
        This is <strong>production</strong>. Type <span className="font-mono text-text">{name}</span> to confirm.
      </label>
      <input id={id} className="input mt-1 w-full font-mono" autoComplete="off" spellCheck={false}
        value={typed} onChange={(e) => setTyped(e.target.value)} />
    </div>
  )
}
