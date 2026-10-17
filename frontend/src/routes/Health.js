import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useQuery } from '@tanstack/react-query';
export default function Health() {
    const q = useQuery({
        queryKey: ['trino', 'cluster'],
        queryFn: () => fetch('/api/trino/cluster').then(r => r.json()),
        refetchInterval: 5_000,
    });
    return (_jsxs("div", { children: [_jsx("h1", { className: "text-3xl font-bold tracking-tight", children: "Cluster Health" }), _jsx("p", { className: "mt-2 text-text-dim", children: "Auto-refreshing Trino cluster summary." }), _jsx("pre", { className: "mt-6 rounded-[14px] border border-border bg-bg-elev/60 p-4 text-sm font-mono overflow-auto", children: q.isLoading ? 'loading…' : JSON.stringify(q.data, null, 2) })] }));
}
