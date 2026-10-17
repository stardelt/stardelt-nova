import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useQuery } from '@tanstack/react-query';
import { api, formatTime } from '../api';
import { Icon } from '../components/Icon';
export default function Jobs() {
    const jobs = useQuery({ queryKey: ['jobs'], queryFn: api.jobs, refetchInterval: 5_000 });
    return (_jsxs("div", { className: "max-w-6xl space-y-6", children: [_jsxs("header", { className: "flex items-end justify-between gap-4", children: [_jsxs("div", { children: [_jsx("h1", { className: "text-2xl font-bold tracking-tight", children: "Jobs" }), _jsx("p", { className: "text-text-dim text-sm mt-1", children: "Every query, load and transfer, newest first." })] }), _jsxs("div", { className: "text-xs text-text-mute flex items-center gap-2", children: [_jsx("span", { className: "w-1.5 h-1.5 rounded-full bg-success animate-pulse" }), "live"] })] }), _jsx("div", { className: "card overflow-hidden", children: _jsxs("table", { className: "w-full text-sm", children: [_jsx("thead", { className: "bg-bg-soft/50", children: _jsxs("tr", { children: [_jsx("th", { className: "th px-4 py-2.5", children: "Kind" }), _jsx("th", { className: "th px-4 py-2.5", children: "Title" }), _jsx("th", { className: "th px-4 py-2.5", children: "Status" }), _jsx("th", { className: "th px-4 py-2.5 text-right", children: "Rows" }), _jsx("th", { className: "th px-4 py-2.5 text-right", children: "Started" })] }) }), _jsxs("tbody", { children: [jobs.isLoading && (_jsx("tr", { children: _jsx("td", { className: "px-4 py-5 text-text-mute", colSpan: 5, children: "loading\u2026" }) })), !jobs.isLoading && (jobs.data ?? []).length === 0 && (_jsx("tr", { children: _jsx("td", { className: "px-4 py-5 text-text-mute", colSpan: 5, children: "No jobs yet." }) })), (jobs.data ?? []).map(j => (_jsxs("tr", { className: "border-t border-border/70 hover:bg-bg-soft/30", children: [_jsx("td", { className: "td px-4", children: _jsx(Kind, { kind: j.kind }) }), _jsxs("td", { className: "td px-4", children: [_jsx("div", { className: "font-mono text-xs truncate max-w-[520px]", title: j.title, children: j.title }), j.detail && _jsx("div", { className: "text-[11px] text-text-mute mt-0.5 truncate max-w-[520px]", children: j.detail })] }), _jsx("td", { className: "td px-4", children: _jsx(Status, { status: j.status }) }), _jsx("td", { className: "td px-4 text-right text-text-dim", children: j.rows != null ? j.rows : '—' }), _jsx("td", { className: "td px-4 text-right text-text-mute text-xs whitespace-nowrap", children: formatTime(j.started_at) })] }, j.id)))] })] }) }), _jsxs("div", { className: "flex items-center gap-2 text-xs text-text-mute", children: [_jsx(Icon, { name: "clock", className: "w-3.5 h-3.5" }), "Refreshes every 5s. History is kept in memory for the MVP."] })] }));
}
function Kind({ kind }) {
    const map = {
        query: 'badge border-sky-400/40 bg-sky-400/10 text-sky-300',
        load: 'badge border-emerald-400/40 bg-emerald-400/10 text-emerald-300',
        transfer: 'badge border-violet-400/40 bg-violet-400/10 text-violet-300',
    };
    return _jsx("span", { className: map[kind] ?? 'badge border-border bg-bg-soft text-text-dim', children: kind });
}
function Status({ status }) {
    const ok = status === 'succeeded';
    return (_jsx("span", { className: ok
            ? 'badge border-emerald-400/40 bg-emerald-400/10 text-emerald-300'
            : 'badge border-red-400/40 bg-red-400/10 text-red-300', children: status }));
}
