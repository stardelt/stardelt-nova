import { useQuery } from '@tanstack/react-query'

export default function Health() {
  const q = useQuery({
    queryKey: ['trino', 'cluster'],
    queryFn: () => fetch('/api/trino/cluster').then(r => r.json()),
    refetchInterval: 5_000,
  })

  return (
    <div>
      <h1 className="text-3xl font-bold tracking-tight">Cluster Health</h1>
      <p className="mt-2 text-text-dim">Auto-refreshing Trino cluster summary.</p>
      <pre className="mt-6 rounded-[14px] border border-border bg-bg-elev/60 p-4 text-sm font-mono overflow-auto">
{q.isLoading ? 'loading…' : JSON.stringify(q.data, null, 2)}
      </pre>
    </div>
  )
}
