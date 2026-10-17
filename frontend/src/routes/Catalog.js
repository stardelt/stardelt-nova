import { jsx as _jsx, jsxs as _jsxs, Fragment as _Fragment } from "react/jsx-runtime";
import { useEffect, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { Link, useSearchParams } from 'react-router-dom';
import { api, formatCell, formatTime } from '../api';
export default function Catalog() {
    const [params] = useSearchParams();
    const [selected, setSelected] = useState(null);
    const [filter, setFilter] = useState(params.get('q') ?? '');
    useEffect(() => {
        const ns = params.get('ns');
        const tbl = params.get('tbl');
        if (ns && tbl)
            setSelected({ ns, tbl });
    }, [params]);
    useEffect(() => {
        const q = params.get('q');
        if (q)
            setFilter(q);
    }, [params]);
    const namespaces = useQuery({ queryKey: ['catalog', 'namespaces'], queryFn: api.namespaces });
    return (_jsxs("div", { className: "grid grid-cols-[300px_1fr] gap-5 h-full min-h-0", children: [_jsxs("aside", { className: "card p-3 flex flex-col min-h-0", children: [_jsxs("div", { className: "px-1 pb-2", children: [_jsx("div", { className: "label mb-1.5", children: "Catalog" }), _jsx("input", { value: filter, onChange: e => setFilter(e.target.value), placeholder: "Filter tables\u2026", className: "input py-1.5" })] }), _jsxs("div", { className: "overflow-auto flex-1", children: [namespaces.isLoading && _jsx("div", { className: "text-text-mute text-sm p-2", children: "loading\u2026" }), namespaces.isError && _jsx("div", { className: "text-red-400 text-sm p-2", children: String(namespaces.error) }), _jsx("ul", { className: "space-y-1", children: namespaces.data?.namespaces.map(parts => {
                                    const ns = parts.join('.');
                                    return _jsx(NamespaceItem, { ns: ns, filter: filter, onSelect: setSelected, selected: selected }, ns);
                                }) })] })] }), _jsx("section", { className: "min-w-0 min-h-0 overflow-auto", children: selected ? (_jsx(TableDetail, { ns: selected.ns, tbl: selected.tbl })) : (_jsx("div", { className: "card p-8 h-full grid place-items-center text-center", children: _jsxs("div", { children: [_jsx("div", { className: "text-text-dim", children: "Select a table to inspect it." }), _jsx("div", { className: "text-text-mute text-sm mt-1", children: "Or load a file to create one." }), _jsx(Link, { to: "/data", className: "btn-ghost mt-4", children: "Load data" })] }) })) })] }));
}
function NamespaceItem({ ns, filter, onSelect, selected }) {
    const [open, setOpen] = useState(true);
    const tables = useQuery({ queryKey: ['catalog', 'tables', ns], queryFn: () => api.tables(ns), enabled: open });
    const matches = (name) => {
        if (!filter.trim())
            return true;
        return `${ns}.${name}`.toLowerCase().includes(filter.toLowerCase());
    };
    const visible = (tables.data?.identifiers ?? []).filter(id => matches(id.name));
    if (filter.trim() && visible.length === 0)
        return null;
    return (_jsxs("li", { children: [_jsxs("button", { onClick: () => setOpen(o => !o), className: "w-full flex items-center gap-2 text-left px-2 py-1.5 rounded-[8px] hover:bg-bg-soft/40", children: [_jsx("span", { className: "text-text-mute text-xs", children: open ? '▾' : '▸' }), _jsx("span", { className: "text-sm font-medium", children: ns }), _jsx("span", { className: "ml-auto text-[11px] text-text-mute", children: visible.length })] }), open && (_jsxs("ul", { className: "ml-3 mt-0.5 space-y-0.5", children: [tables.isLoading && _jsx("li", { className: "text-text-mute text-xs px-2 py-1", children: "loading\u2026" }), visible.map(id => {
                        const sel = selected?.ns === ns && selected?.tbl === id.name;
                        return (_jsx("li", { children: _jsxs("button", { onClick: () => onSelect({ ns, tbl: id.name }), className: [
                                    'w-full text-left px-2 py-1.5 rounded-[8px] text-sm flex items-center gap-2',
                                    sel ? 'bg-accent/15 text-text border border-accent/30' : 'text-text-dim hover:text-text hover:bg-bg-soft/40 border border-transparent',
                                ].join(' '), children: [_jsx("span", { className: "text-text-mute", children: "\u25C7" }), _jsx("span", { className: "truncate", children: id.name })] }) }, id.name));
                    })] }))] }));
}
function TableDetail({ ns, tbl }) {
    const [tab, setTab] = useState('preview');
    const meta = useQuery({ queryKey: ['catalog', 'table', ns, tbl], queryFn: () => api.tableMeta(ns, tbl) });
    const preview = useQuery({
        queryKey: ['preview', ns, tbl],
        queryFn: () => api.query(`SELECT * FROM warehouse.${ns}.${tbl} LIMIT 50`),
        enabled: tab === 'preview',
    });
    const m = meta.data?.metadata;
    const schemaId = m?.['current-schema-id'] ?? m?.schemas[0]?.['schema-id'] ?? 0;
    const schema = m?.schemas.find(s => s['schema-id'] === schemaId) ?? m?.schemas[0];
    const partSpec = m?.['partition-specs']?.[0];
    return (_jsxs("div", { className: "space-y-4", children: [_jsxs("header", { className: "flex items-start justify-between gap-4 flex-wrap", children: [_jsxs("div", { children: [_jsx("div", { className: "text-[11px] uppercase tracking-widest text-text-mute", children: ns }), _jsx("h1", { className: "text-2xl font-bold tracking-tight font-mono", children: tbl }), m && (_jsx("div", { className: "text-xs text-text-mute mt-1 font-mono truncate max-w-[620px]", children: m.location }))] }), _jsxs("div", { className: "flex gap-2", children: [_jsx(Link, { to: `/query?sql=${encodeURIComponent(`SELECT * FROM warehouse.${ns}.${tbl} LIMIT 100`)}`, className: "btn-primary", children: "Query table" }), _jsx(Link, { to: "/data", className: "btn-ghost", children: "Load data" })] })] }), _jsx("div", { className: "flex gap-2", children: ['preview', 'columns', 'details'].map(t => (_jsx("button", { onClick: () => setTab(t), className: [
                        'px-3 py-1.5 rounded-[8px] text-sm border capitalize',
                        tab === t
                            ? 'bg-accent/15 border-accent/30 text-text'
                            : 'border-border text-text-dim hover:text-text',
                    ].join(' '), children: t }, t))) }), meta.isError && _jsx("div", { className: "text-red-400 text-sm", children: String(meta.error) }), tab === 'preview' && _jsx(Preview, { q: preview }), tab === 'columns' && (_jsx("div", { className: "card overflow-hidden", children: _jsxs("table", { className: "w-full text-sm", children: [_jsx("thead", { className: "bg-bg-soft/50", children: _jsxs("tr", { children: [_jsx("th", { className: "th px-4 py-2.5", children: "#" }), _jsx("th", { className: "th px-4 py-2.5", children: "Column" }), _jsx("th", { className: "th px-4 py-2.5", children: "Type" }), _jsx("th", { className: "th px-4 py-2.5", children: "Required" })] }) }), _jsxs("tbody", { children: [schema?.fields.map(f => (_jsxs("tr", { className: "border-t border-border/70", children: [_jsx("td", { className: "td px-4 text-text-mute font-mono", children: f.id }), _jsx("td", { className: "td px-4 font-mono", children: f.name }), _jsx("td", { className: "td px-4 text-text-dim font-mono", children: typeof f.type === 'string' ? f.type : JSON.stringify(f.type) }), _jsx("td", { className: "td px-4 text-text-mute", children: f.required ? 'yes' : '—' })] }, f.id))), !schema && _jsx("tr", { children: _jsx("td", { className: "px-4 py-5 text-text-mute", colSpan: 4, children: "loading\u2026" }) })] })] }) })), tab === 'details' && meta.data && _jsx(Details, { meta: meta.data, partition: partSpec })] }));
}
function Preview({ q }) {
    if (q.isLoading)
        return _jsx("div", { className: "card p-6 text-text-mute text-sm", children: "loading preview\u2026" });
    if (q.error)
        return _jsx("div", { className: "card p-6 text-red-400 text-sm", children: String(q.error) });
    if (!q.data)
        return null;
    return (_jsxs("div", { className: "card overflow-hidden", children: [_jsxs("div", { className: "px-4 py-2 text-xs text-text-mute border-b border-border flex justify-between", children: [_jsxs("span", { children: [q.data.row_count, " rows \u00B7 preview"] }), _jsx("span", { className: "font-mono", children: q.data.query_id })] }), _jsx("div", { className: "overflow-auto max-h-[60vh]", children: _jsxs("table", { className: "w-full text-sm font-mono", children: [_jsx("thead", { className: "bg-bg-soft/60 sticky top-0", children: _jsx("tr", { children: q.data.columns.map(c => _jsx("th", { className: "th px-4 py-2", children: c.name }, c.name)) }) }), _jsx("tbody", { children: q.data.rows.map((row, i) => (_jsx("tr", { className: "border-t border-border/60", children: row.map((cell, j) => _jsx("td", { className: "td px-4 text-text-dim", children: formatCell(cell) }, j)) }, i))) })] }) })] }));
}
function Details({ meta, partition }) {
    const m = meta.metadata;
    return (_jsxs("div", { className: "grid md:grid-cols-2 gap-4", children: [_jsxs("div", { className: "card p-5", children: [_jsx("div", { className: "label mb-3", children: "Table" }), _jsx(Row, { k: "UUID", v: m['table-uuid'], mono: true }), _jsx(Row, { k: "Location", v: m.location, mono: true }), _jsx(Row, { k: "Updated", v: formatTime(m['last-updated-ms']) }), _jsx(Row, { k: "Metadata", v: meta['metadata-location'], mono: true })] }), _jsxs("div", { className: "card p-5", children: [_jsx("div", { className: "label mb-3", children: "Partitioning" }), partition && partition.fields.length > 0 ? (_jsx("ul", { className: "text-sm font-mono space-y-1", children: partition.fields.map(f => (_jsxs("li", { children: [_jsx("span", { className: "text-accent", children: f.transform }), _jsx("span", { className: "text-text-mute", children: " \u2192 " }), f.name] }, f.name))) })) : (_jsx("div", { className: "text-sm text-text-mute", children: "Unpartitioned" })), m.properties && (_jsxs(_Fragment, { children: [_jsx("div", { className: "label mt-5 mb-2", children: "Properties" }), _jsx("ul", { className: "text-sm font-mono space-y-1", children: Object.entries(m.properties).map(([k, v]) => (_jsxs("li", { children: [_jsx("span", { className: "text-text-dim", children: k }), " = ", v] }, k))) })] }))] })] }));
}
function Row({ k, v, mono }) {
    return (_jsxs("div", { className: "py-1.5 border-b border-border/60 last:border-0", children: [_jsx("div", { className: "text-[11px] uppercase tracking-widest text-text-mute", children: k }), _jsx("div", { className: `text-sm break-all ${mono ? 'font-mono' : ''}`, children: v })] }));
}
