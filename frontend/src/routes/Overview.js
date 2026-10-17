import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useQuery } from '@tanstack/react-query';
import { Link } from 'react-router-dom';
import { api, allTables, formatTime } from '../api';
import { Icon } from '../components/Icon';
export default function Overview() {
    const me = useQuery({ queryKey: ['me'], queryFn: api.me });
    const tables = useQuery({ queryKey: ['catalog', 'all'], queryFn: allTables });
    const jobs = useQuery({ queryKey: ['jobs'], queryFn: api.jobs, refetchInterval: 10_000 });
    const cluster = useQuery({ queryKey: ['trino', 'cluster'], queryFn: api.trinoCluster, retry: false });
    const refs = tables.data ?? [];
    const schemas = new Set(refs.map(r => r.ns)).size;
    const info = cluster.data;
    const engineState = typeof info?.state === 'string' ? info.state : cluster.isError ? 'unreachable' : '…';
    const recent = (jobs.data ?? []).slice(0, 6);
    return (_jsxs("div", { className: "space-y-6 max-w-6xl", children: [_jsxs("header", { className: "flex items-end justify-between gap-4 flex-wrap", children: [_jsxs("div", { children: [_jsx("h1", { className: "text-2xl font-bold tracking-tight", children: "Lakehouse overview" }), _jsxs("p", { className: "text-text-dim text-sm mt-1", children: ["One catalog, one engine, one place to load and move data.", me.data?.demo && _jsx("span", { className: "text-amber-300", children: " Demo data is in memory." })] })] }), _jsxs("div", { className: "flex gap-2", children: [_jsxs(Link, { to: "/data", className: "btn-primary", children: [_jsx(Icon, { name: "upload" }), "Load data"] }), _jsxs(Link, { to: "/query", className: "btn-ghost", children: [_jsx(Icon, { name: "terminal" }), "New query"] })] })] }), _jsxs("section", { className: "grid grid-cols-2 lg:grid-cols-4 gap-4", children: [_jsx(Stat, { label: "Tables", value: tables.isLoading ? '…' : String(refs.length), hint: `${schemas} schemas`, icon: "table" }), _jsx(Stat, { label: "Engine", value: String(engineState), hint: "Trino", icon: "spark", tone: engineState === 'ACTIVE' ? 'ok' : undefined }), _jsx(Stat, { label: "Jobs", value: jobs.isLoading ? '…' : String(jobs.data?.length ?? 0), hint: "recent activity", icon: "clock" }), _jsx(Stat, { label: "Format", value: "Iceberg", hint: "open tables on S3", icon: "database" })] }), _jsxs("section", { className: "grid lg:grid-cols-3 gap-4", children: [_jsxs("div", { className: "card p-5 lg:col-span-2", children: [_jsxs("div", { className: "flex items-center justify-between mb-3", children: [_jsx("h2", { className: "text-sm font-semibold", children: "Recent activity" }), _jsx(Link, { to: "/jobs", className: "text-xs text-text-dim hover:text-text", children: "View all \u2192" })] }), jobs.isLoading && _jsx("div", { className: "text-text-mute text-sm", children: "loading\u2026" }), !jobs.isLoading && recent.length === 0 && (_jsx("div", { className: "text-text-mute text-sm", children: "No activity yet. Run a query or load a file." })), _jsx("div", { className: "space-y-1", children: recent.map(j => (_jsxs("div", { className: "flex items-center gap-3 py-2 border-b border-border/60 last:border-0", children: [_jsx("span", { className: kindBadge(j.kind), children: j.kind }), _jsx("span", { className: "text-sm truncate flex-1", children: j.title }), _jsx("span", { className: "text-xs text-text-mute", children: j.rows != null ? `${j.rows} rows` : '' }), _jsx("span", { className: "text-xs text-text-mute w-36 text-right", children: formatTime(j.started_at) })] }, j.id))) })] }), _jsxs("div", { className: "card p-5", children: [_jsx("h2", { className: "text-sm font-semibold mb-3", children: "Stack" }), _jsx("div", { className: "space-y-2 text-sm", children: [
                                    ['Trino', 'query engine'],
                                    ['Lakekeeper', 'Iceberg REST catalog'],
                                    ['SeaweedFS', 'S3-compatible storage'],
                                    ['Airflow', 'orchestration'],
                                    ['Superset', 'BI & dashboards'],
                                    ['Nova', 'control plane'],
                                ].map(([name, role]) => (_jsxs("div", { className: "flex items-center justify-between gap-3", children: [_jsxs("span", { className: "flex items-center gap-2", children: [_jsx("span", { className: "w-1.5 h-1.5 rounded-full bg-success" }), name] }), _jsx("span", { className: "text-xs text-text-mute", children: role })] }, name))) })] })] }), _jsxs("section", { className: "grid md:grid-cols-3 gap-4", children: [_jsx(Action, { to: "/catalog", icon: "database", title: "Browse the catalog", body: "Explore schemas, tables and column types." }), _jsx(Action, { to: "/data", icon: "upload", title: "Load & transfer", body: "Ingest CSV or JSON, or copy a table." }), _jsx(Action, { to: "/query", icon: "terminal", title: "Run SQL", body: "Query Iceberg tables through Trino." })] })] }));
}
function Stat({ label, value, hint, icon, tone }) {
    return (_jsxs("div", { className: "card p-4", children: [_jsxs("div", { className: "flex items-center justify-between", children: [_jsx("span", { className: "label", children: label }), _jsx(Icon, { name: icon, className: "w-4 h-4 text-text-mute" })] }), _jsx("div", { className: `text-2xl font-semibold mt-2 ${tone === 'ok' ? 'text-success' : ''}`, children: value }), _jsx("div", { className: "text-xs text-text-mute mt-0.5", children: hint })] }));
}
function Action({ to, icon, title, body }) {
    return (_jsxs(Link, { to: to, className: "card p-5 hover:border-accent/40 transition group", children: [_jsxs("div", { className: "flex items-center gap-2 text-sm font-semibold", children: [_jsx(Icon, { name: icon, className: "w-4 h-4 text-accent" }), title, _jsx(Icon, { name: "arrow", className: "w-4 h-4 ml-auto opacity-0 group-hover:opacity-100 transition" })] }), _jsx("p", { className: "text-sm text-text-dim mt-2", children: body })] }));
}
function kindBadge(kind) {
    const map = {
        query: 'badge border-sky-400/40 bg-sky-400/10 text-sky-300',
        load: 'badge border-emerald-400/40 bg-emerald-400/10 text-emerald-300',
        transfer: 'badge border-violet-400/40 bg-violet-400/10 text-violet-300',
    };
    return map[kind] ?? 'badge border-border bg-bg-soft text-text-dim';
}
