import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { Link } from 'react-router-dom';
import { api, allTables } from '../api';
import { Icon } from '../components/Icon';
const SAMPLE_CSV = `vendor_id,pickup_at,dropoff_at,passenger_count,trip_distance,fare_amount,tip_amount,total_amount,payment_type,pickup_borough
1,2026-10-02 08:00,2026-10-02 08:20,2,3.1,13.0,3.0,18.2,card,Manhattan
2,2026-10-02 08:35,2026-10-02 08:52,1,1.9,8.5,1.7,12.1,card,Brooklyn
1,2026-10-02 09:10,2026-10-02 09:28,3,4.2,16.5,3.9,23.4,cash,Queens`;
const SAMPLE_JSON = JSON.stringify([
    { zone_id: 1, zone: 'Manhattan', borough: 'Manhattan' },
    { zone_id: 2, zone: 'Brooklyn', borough: 'Brooklyn' },
], null, 2);
export default function Data() {
    const [tab, setTab] = useState('load');
    const me = useQuery({ queryKey: ['me'], queryFn: api.me });
    return (_jsxs("div", { className: "max-w-5xl space-y-6", children: [_jsxs("header", { children: [_jsx("h1", { className: "text-2xl font-bold tracking-tight", children: "Load & transfer" }), _jsxs("p", { className: "text-text-dim text-sm mt-1", children: ["Bring files into Iceberg, or move data between tables. Every operation is recorded as a job.", me.data?.demo && _jsx("span", { className: "text-amber-300", children: " Demo mode writes to memory." })] })] }), _jsxs("div", { className: "flex gap-2", children: [_jsx(Tab, { active: tab === 'load', onClick: () => setTab('load'), icon: "upload", label: "Load data" }), _jsx(Tab, { active: tab === 'transfer', onClick: () => setTab('transfer'), icon: "arrow", label: "Transfer table" })] }), tab === 'load' ? _jsx(LoadForm, {}) : _jsx(TransferForm, {})] }));
}
function Tab({ active, onClick, icon, label }) {
    return (_jsxs("button", { onClick: onClick, className: [
            'btn border',
            active
                ? 'bg-accent/15 border-accent/30 text-text'
                : 'border-border bg-bg-elev/40 text-text-dim hover:text-text',
        ].join(' '), children: [_jsx(Icon, { name: icon, className: "w-4 h-4" }), label] }));
}
function LoadForm() {
    const qc = useQueryClient();
    const [namespace, setNamespace] = useState('nyc_taxi');
    const [table, setTable] = useState('yellow_trips_copy');
    const [format, setFormat] = useState('csv');
    const [content, setContent] = useState(SAMPLE_CSV);
    const [result, setResult] = useState(null);
    const [error, setError] = useState(null);
    const upload = useMutation({
        mutationFn: api.upload,
        onMutate: () => { setError(null); setResult(null); },
        onSuccess: r => {
            setResult(r);
            qc.invalidateQueries({ queryKey: ['catalog'] });
            qc.invalidateQueries({ queryKey: ['jobs'] });
        },
        onError: (e) => setError(e instanceof Error ? e.message : String(e)),
    });
    return (_jsxs("div", { className: "grid lg:grid-cols-[1fr_320px] gap-4", children: [_jsxs("div", { className: "card p-5 space-y-4", children: [_jsxs("div", { className: "grid sm:grid-cols-2 gap-4", children: [_jsx(Field, { label: "Schema", children: _jsx("input", { value: namespace, onChange: e => setNamespace(e.target.value), className: "input", placeholder: "nyc_taxi" }) }), _jsx(Field, { label: "Table", children: _jsx("input", { value: table, onChange: e => setTable(e.target.value), className: "input", placeholder: "yellow_trips" }) })] }), _jsxs("div", { className: "flex items-center gap-2", children: [_jsx("span", { className: "label mr-2", children: "Format" }), ['csv', 'json'].map(f => (_jsx("button", { onClick: () => { setFormat(f); setContent(f === 'csv' ? SAMPLE_CSV : SAMPLE_JSON); }, className: [
                                    'px-2.5 py-1 rounded-[8px] text-xs border',
                                    format === f
                                        ? 'bg-accent/15 border-accent/30 text-text'
                                        : 'border-border text-text-dim hover:text-text',
                                ].join(' '), children: f.toUpperCase() }, f))), _jsxs("label", { className: "ml-auto btn-ghost cursor-pointer", children: [_jsx(Icon, { name: "upload", className: "w-4 h-4" }), "Choose file", _jsx("input", { type: "file", accept: ".csv,.json,text/csv,application/json", className: "hidden", onChange: e => {
                                            const file = e.target.files?.[0];
                                            if (file) {
                                                file.text().then(text => {
                                                    setContent(text);
                                                    setFormat(file.name.endsWith('.json') ? 'json' : 'csv');
                                                });
                                            }
                                        } })] })] }), _jsx(Field, { label: "Content", children: _jsx("textarea", { value: content, onChange: e => setContent(e.target.value), spellCheck: false, className: "w-full h-56 bg-bg-soft/40 border border-border rounded-[10px] p-3 font-mono text-xs outline-none focus:border-accent/60 resize-y" }) }), _jsxs("div", { className: "flex items-center gap-3", children: [_jsxs("button", { className: "btn-primary", disabled: upload.isPending || !namespace.trim() || !table.trim() || !content.trim(), onClick: () => upload.mutate({ namespace, table, format, content }), children: [_jsx(Icon, { name: "upload", className: "w-4 h-4" }), upload.isPending ? 'Loading…' : 'Load into table'] }), _jsxs("span", { className: "text-xs text-text-mute", children: ["Creates or replaces ", _jsxs("span", { className: "font-mono", children: [namespace, ".", table] })] })] }), error && _jsx(ErrorBox, { text: error }), result && (_jsxs("div", { className: "rounded-[10px] border border-emerald-400/30 bg-emerald-400/10 p-4 text-sm", children: [_jsxs("div", { className: "font-medium text-emerald-300", children: ["Loaded ", result.rows_loaded, " rows into ", result.namespace, ".", result.table] }), _jsxs("div", { className: "text-text-dim mt-1 flex gap-3", children: [_jsx(Link, { to: `/catalog?ns=${result.namespace}&tbl=${result.table}`, className: "hover:text-text", children: "Open in catalog \u2192" }), _jsx(Link, { to: "/jobs", className: "hover:text-text", children: "View job \u2192" })] })] }))] }), _jsxs("aside", { className: "card p-5 h-fit", children: [_jsx("h3", { className: "text-sm font-semibold mb-2", children: "How it works" }), _jsxs("ol", { className: "text-sm text-text-dim space-y-2 list-decimal list-inside", children: [_jsx("li", { children: "The file is parsed and column types are inferred." }), _jsx("li", { children: "A table is created (or replaced) in the selected schema." }), _jsx("li", { children: "Rows are written through the engine into Iceberg." }), _jsx("li", { children: "The job appears under Jobs with a row count." })] }), _jsx("div", { className: "mt-4 pt-4 border-t border-border text-xs text-text-mute", children: "CSV and JSON arrays of objects are supported. Keep files small for the MVP; bulk ingestion lands with the pipeline pillar." })] })] }));
}
function TransferForm() {
    const qc = useQueryClient();
    const tables = useQuery({ queryKey: ['catalog', 'all'], queryFn: allTables });
    const [source, setSource] = useState('');
    const [targetNs, setTargetNs] = useState('analytics');
    const [targetTable, setTargetTable] = useState('trips_copy');
    const [mode, setMode] = useState('overwrite');
    const [result, setResult] = useState(null);
    const [error, setError] = useState(null);
    const transfer = useMutation({
        mutationFn: api.transfer,
        onMutate: () => { setError(null); setResult(null); },
        onSuccess: r => {
            setResult(r);
            qc.invalidateQueries({ queryKey: ['catalog'] });
            qc.invalidateQueries({ queryKey: ['jobs'] });
        },
        onError: (e) => setError(e instanceof Error ? e.message : String(e)),
    });
    const refs = tables.data ?? [];
    return (_jsxs("div", { className: "grid lg:grid-cols-[1fr_320px] gap-4", children: [_jsxs("div", { className: "card p-5 space-y-4", children: [_jsx(Field, { label: "Source table", children: _jsxs("select", { value: source, onChange: e => setSource(e.target.value), className: "input", children: [_jsx("option", { value: "", children: "Select a table\u2026" }), refs.map(r => (_jsxs("option", { value: `${r.ns}.${r.name}`, children: [r.ns, ".", r.name] }, `${r.ns}.${r.name}`)))] }) }), _jsxs("div", { className: "grid sm:grid-cols-2 gap-4", children: [_jsx(Field, { label: "Target schema", children: _jsx("input", { value: targetNs, onChange: e => setTargetNs(e.target.value), className: "input" }) }), _jsx(Field, { label: "Target table", children: _jsx("input", { value: targetTable, onChange: e => setTargetTable(e.target.value), className: "input" }) })] }), _jsx(Field, { label: "Mode", children: _jsx("div", { className: "flex gap-2", children: ['overwrite', 'append'].map(m => (_jsx("button", { onClick: () => setMode(m), className: [
                                    'px-3 py-1.5 rounded-[8px] text-xs border',
                                    mode === m
                                        ? 'bg-accent/15 border-accent/30 text-text'
                                        : 'border-border text-text-dim hover:text-text',
                                ].join(' '), children: m }, m))) }) }), _jsxs("button", { className: "btn-primary", disabled: transfer.isPending || !source || !targetNs.trim() || !targetTable.trim(), onClick: () => transfer.mutate({ source, target: `${targetNs}.${targetTable}`, mode }), children: [_jsx(Icon, { name: "arrow", className: "w-4 h-4" }), transfer.isPending ? 'Transferring…' : 'Transfer table'] }), error && _jsx(ErrorBox, { text: error }), result && (_jsxs("div", { className: "rounded-[10px] border border-emerald-400/30 bg-emerald-400/10 p-4 text-sm", children: [_jsxs("div", { className: "font-medium text-emerald-300", children: ["Transferred ", result.rows, " rows \u2192 ", result.target] }), _jsx(Link, { to: "/jobs", className: "text-text-dim mt-1 inline-block hover:text-text", children: "View job \u2192" })] }))] }), _jsxs("aside", { className: "card p-5 h-fit", children: [_jsx("h3", { className: "text-sm font-semibold mb-2", children: "What this does" }), _jsxs("p", { className: "text-sm text-text-dim", children: [_jsx("span", { className: "font-mono text-xs", children: "overwrite" }), " recreates the target from the source (", _jsx("span", { className: "font-mono text-xs", children: "CREATE TABLE AS SELECT" }), ").", ' ', _jsx("span", { className: "font-mono text-xs", children: "append" }), " inserts the source rows into the target."] }), _jsx("div", { className: "mt-4 pt-4 border-t border-border text-xs text-text-mute", children: "Cross-schema copies stay inside the same catalog, so lineage and policy apply to both ends." })] })] }));
}
function Field({ label, children }) {
    return (_jsxs("label", { className: "block", children: [_jsx("span", { className: "label block mb-1.5", children: label }), children] }));
}
function ErrorBox({ text }) {
    return (_jsx("div", { className: "rounded-[10px] border border-red-500/40 bg-red-500/10 p-3 text-sm text-red-300 font-mono whitespace-pre-wrap", children: text }));
}
