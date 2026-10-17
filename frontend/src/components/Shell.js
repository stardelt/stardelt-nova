import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useState } from 'react';
import { NavLink, Outlet, useLocation, useNavigate } from 'react-router-dom';
import { useQuery } from '@tanstack/react-query';
import { api } from '../api';
import { Icon } from './Icon';
const groups = [
    {
        label: 'Workspace',
        items: [
            { to: '/', label: 'Overview', icon: 'grid', end: true },
            { to: '/catalog', label: 'Catalog', icon: 'database' },
        ],
    },
    {
        label: 'Data',
        items: [
            { to: '/query', label: 'SQL editor', icon: 'terminal' },
            { to: '/data', label: 'Load & transfer', icon: 'upload' },
            { to: '/jobs', label: 'Jobs', icon: 'clock' },
        ],
    },
    {
        label: 'Governance',
        items: [{ to: '/lineage', label: 'Lineage', icon: 'lineage' }],
    },
    {
        label: 'Analytics',
        items: [{ to: '/dashboards', label: 'Dashboards', icon: 'chart' }],
    },
    {
        label: 'Platform',
        items: [{ to: '/health', label: 'Health', icon: 'pulse' }],
    },
];
const titles = {
    '/': 'Overview',
    '/catalog': 'Catalog',
    '/query': 'SQL editor',
    '/data': 'Load & transfer',
    '/jobs': 'Jobs',
    '/lineage': 'Lineage',
    '/dashboards': 'Dashboards',
    '/health': 'Health',
};
function navClass({ isActive }) {
    return [
        'flex items-center gap-2.5 px-3 py-2 rounded-[10px] text-sm transition-colors',
        isActive
            ? 'bg-accent/15 text-text border border-accent/30'
            : 'text-text-dim hover:text-text hover:bg-bg-soft border border-transparent',
    ].join(' ');
}
export default function Shell() {
    const me = useQuery({ queryKey: ['me'], queryFn: api.me });
    const location = useLocation();
    const navigate = useNavigate();
    const [q, setQ] = useState('');
    const title = titles[location.pathname] ?? 'Nova';
    return (_jsxs("div", { className: "h-full grid grid-cols-[250px_1fr]", children: [_jsxs("aside", { className: "border-r border-border bg-bg-elev/50 backdrop-blur flex flex-col min-h-0", children: [_jsxs("div", { className: "p-5 flex items-center gap-3", children: [_jsx("div", { className: "w-8 h-8 rounded-[11px] bg-gradient-to-b from-accent to-accent-deep shadow-lg shadow-accent/20" }), _jsxs("div", { className: "leading-tight", children: [_jsx("div", { className: "font-bold tracking-tight", children: "stardelt" }), _jsx("div", { className: "text-[11px] text-text-mute -mt-0.5", children: "Nova" })] })] }), _jsx("div", { className: "px-3 pb-3", children: _jsxs("div", { className: "card px-3 py-2 flex items-center gap-2", children: [_jsx(Icon, { name: "database", className: "w-4 h-4 text-accent" }), _jsxs("div", { className: "min-w-0", children: [_jsx("div", { className: "text-[10px] uppercase tracking-widest text-text-mute", children: "Warehouse" }), _jsx("div", { className: "text-sm truncate", children: me.data?.warehouse ?? 'warehouse' })] }), me.data?.demo && (_jsx("span", { className: "badge ml-auto border-amber-400/40 bg-amber-400/10 text-amber-300", children: "demo" }))] }) }), _jsx("nav", { className: "px-3 pb-4 overflow-auto flex-1", children: groups.map(group => (_jsxs("div", { className: "mb-4", children: [_jsx("div", { className: "label px-3 mb-1.5", children: group.label }), _jsx("div", { className: "space-y-1", children: group.items.map(item => (_jsxs(NavLink, { to: item.to, end: item.end, className: navClass, children: [_jsx(Icon, { name: item.icon, className: "w-4 h-4" }), item.label] }, item.to))) })] }, group.label))) }), _jsx("div", { className: "p-3 border-t border-border", children: _jsxs("div", { className: "flex items-center gap-2.5 px-2 py-1.5", children: [_jsx("div", { className: "w-7 h-7 rounded-full bg-accent/20 border border-accent/30 grid place-items-center text-xs font-semibold text-accent", children: (me.data?.user ?? '?').slice(0, 1).toUpperCase() }), _jsxs("div", { className: "min-w-0", children: [_jsx("div", { className: "text-sm truncate", children: me.data?.user ?? 'loading…' }), _jsx("div", { className: "text-[11px] text-text-mute", children: me.data?.auth_mode ?? '' })] })] }) })] }), _jsxs("div", { className: "flex flex-col min-w-0 min-h-0", children: [_jsxs("header", { className: "h-14 shrink-0 border-b border-border bg-bg/30 backdrop-blur flex items-center gap-4 px-6", children: [_jsx("div", { className: "text-sm font-medium", children: title }), _jsxs("form", { className: "ml-auto relative", onSubmit: e => {
                                    e.preventDefault();
                                    if (q.trim())
                                        navigate(`/catalog?q=${encodeURIComponent(q.trim())}`);
                                }, children: [_jsx(Icon, { name: "search", className: "w-4 h-4 absolute left-3 top-1/2 -translate-y-1/2 text-text-mute" }), _jsx("input", { value: q, onChange: e => setQ(e.target.value), placeholder: "Search catalog\u2026", className: "input pl-9 w-64 py-1.5" })] }), _jsxs(NavLink, { to: "/data", className: "btn-primary", children: [_jsx(Icon, { name: "plus", className: "w-4 h-4" }), "Load data"] })] }), _jsx("main", { className: "flex-1 min-h-0 overflow-auto p-6", children: _jsx(Outlet, {}) })] })] }));
}
