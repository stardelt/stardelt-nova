import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useState } from 'react';
// Superset embed URL. The browser reaches Superset via its own port-forward
// (default localhost:8089 per `make pf`), not via Nova's backend. This is
// configurable so a future production setup can swap in a same-origin
// reverse-proxy path.
const DEFAULT_SUPERSET_URL = import.meta.env.VITE_SUPERSET_URL ?? 'http://localhost:8089';
const ENTRY_PATHS = [
    { label: 'Welcome', path: '/superset/welcome/' },
    { label: 'SQL Lab', path: '/sqllab/' },
    { label: 'Dashboards', path: '/dashboard/list/' },
    { label: 'Charts', path: '/chart/list/' },
    { label: 'Datasets', path: '/tablemodelview/list/' },
    { label: 'Databases', path: '/databaseview/list/' },
];
export default function Dashboards() {
    const [base, setBase] = useState(DEFAULT_SUPERSET_URL);
    const [path, setPath] = useState('/superset/welcome/');
    const src = `${base}${path}`;
    return (_jsxs("div", { className: "h-full flex flex-col gap-4", children: [_jsxs("header", { className: "flex items-center justify-between gap-4 flex-wrap", children: [_jsxs("div", { children: [_jsx("h1", { className: "text-3xl font-bold tracking-tight", children: "Dashboards" }), _jsxs("p", { className: "text-text-dim text-sm mt-1", children: ["Apache Superset embedded. Login: ", _jsx("span", { className: "font-mono", children: "admin / admin" }), ". Requires ", _jsx("span", { className: "font-mono", children: "make pf" }), " running."] })] }), _jsx("a", { href: src, target: "_blank", rel: "noreferrer", className: "px-4 py-2 rounded-[10px] border border-border bg-bg-elev/60 hover:border-accent/50 text-sm", children: "Open in new tab \u2197" })] }), _jsxs("div", { className: "flex items-center gap-2 flex-wrap", children: [ENTRY_PATHS.map(e => (_jsx("button", { onClick: () => setPath(e.path), className: [
                            'px-3 py-1 rounded-[10px] text-sm border',
                            path === e.path
                                ? 'bg-accent/15 border-accent/30 text-text'
                                : 'bg-bg-elev/40 border-border text-text-dim hover:text-text',
                        ].join(' '), children: e.label }, e.path))), _jsx("input", { type: "text", value: base, onChange: e => setBase(e.target.value), className: "ml-auto bg-bg-elev/60 border border-border rounded-[10px] px-3 py-1 text-xs font-mono w-72", placeholder: "Superset base URL" })] }), _jsx("iframe", { title: "Superset", src: src, className: "flex-1 w-full rounded-[14px] border border-border bg-white" })] }));
}
