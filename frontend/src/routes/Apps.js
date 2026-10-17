import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useQuery } from '@tanstack/react-query';
import { api } from '../api';
import { Icon } from '../components/Icon';
export default function Apps() {
    const apps = useQuery({ queryKey: ['apps'], queryFn: api.apps, refetchInterval: 15_000 });
    const groups = new Map();
    for (const app of apps.data ?? []) {
        const list = groups.get(app.category) ?? [];
        list.push(app);
        groups.set(app.category, list);
    }
    const up = (apps.data ?? []).filter(a => a.status === 'up').length;
    const total = apps.data?.length ?? 0;
    return (_jsxs("div", { className: "max-w-6xl space-y-6", children: [_jsxs("header", { className: "flex items-end justify-between gap-4 flex-wrap", children: [_jsxs("div", { children: [_jsx("h1", { className: "text-2xl font-bold tracking-tight", children: "Platform apps" }), _jsx("p", { className: "text-text-dim text-sm mt-1", children: "Every service in the platform, with live status and a direct link." })] }), _jsxs("div", { className: "text-sm text-text-dim", children: [_jsx("span", { className: "text-success font-semibold", children: up }), " / ", total, " up"] })] }), apps.isLoading && _jsx("div", { className: "text-text-mute", children: "loading\u2026" }), apps.isError && _jsx("div", { className: "text-red-500 text-sm", children: String(apps.error) }), [...groups.entries()].map(([category, list]) => (_jsxs("section", { children: [_jsx("div", { className: "label mb-2", children: category }), _jsx("div", { className: "grid sm:grid-cols-2 lg:grid-cols-3 gap-4", children: list.map(app => _jsx(AppCard, { app: app }, app.id)) })] }, category))), _jsxs("p", { className: "text-xs text-text-mute", children: ["Status is probed from the backend every 15s. Apps without a link are not exposed through the ingress (set ", _jsx("span", { className: "font-mono", children: "NOVA_BASE_DOMAIN" }), " to enable links)."] })] }));
}
function AppCard({ app }) {
    const up = app.status === 'up';
    return (_jsxs("div", { className: "card p-5 flex flex-col gap-3 hover:border-accent/40 transition", children: [_jsxs("div", { className: "flex items-start gap-3", children: [_jsx("div", { className: "w-9 h-9 rounded-[10px] bg-accent/15 border border-accent/25 grid place-items-center text-accent shrink-0", children: _jsx(Icon, { name: "grid", className: "w-4 h-4" }) }), _jsxs("div", { className: "min-w-0 flex-1", children: [_jsx("div", { className: "font-semibold truncate", children: app.name }), _jsx("div", { className: "text-xs text-text-dim leading-snug", children: app.description })] })] }), _jsxs("div", { className: "flex items-center gap-2 mt-auto", children: [_jsxs("span", { className: [
                            'badge',
                            up ? 'border-success/40 bg-success/10 text-success' : 'border-border bg-bg-soft text-text-mute',
                        ].join(' '), children: [_jsx("span", { className: ['w-1.5 h-1.5 rounded-full', up ? 'bg-success' : 'bg-text-mute'].join(' ') }), up ? 'running' : app.status] }), app.external_url ? (_jsxs("a", { href: app.external_url, target: "_blank", rel: "noreferrer", className: "btn-ghost ml-auto py-1.5 px-3 text-xs", children: ["Open ", _jsx(Icon, { name: "arrow", className: "w-3.5 h-3.5" })] })) : (_jsx("span", { className: "ml-auto text-[11px] text-text-mute", children: "no link" }))] })] }));
}
