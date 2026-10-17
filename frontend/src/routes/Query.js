import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useEffect, useRef, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { api, downloadCsv, formatCell } from '../api';
import { Icon } from '../components/Icon';
const HISTORY_KEY = 'stardelt-nova:history';
const HISTORY_LIMIT = 50;
const SAMPLE = 'SELECT * FROM warehouse.nyc_taxi.yellow_trips LIMIT 100';
function loadHistory() {
    try {
        return JSON.parse(localStorage.getItem(HISTORY_KEY) ?? '[]');
    }
    catch {
        return [];
    }
}
function saveHistory(h) {
    localStorage.setItem(HISTORY_KEY, JSON.stringify(h.slice(0, HISTORY_LIMIT)));
}
export default function Query() {
    const [params] = useSearchParams();
    const [sql, setSql] = useState(params.get('sql') ?? SAMPLE);
    const [running, setRunning] = useState(false);
    const [error, setError] = useState(null);
    const [result, setResult] = useState(null);
    const [elapsed, setElapsed] = useState(null);
    const [history, setHistory] = useState(loadHistory);
    const taRef = useRef(null);
    useEffect(() => {
        const fromUrl = params.get('sql');
        if (fromUrl)
            setSql(fromUrl);
    }, [params]);
    async function run() {
        setRunning(true);
        setError(null);
        setResult(null);
        setElapsed(null);
        const t0 = performance.now();
        try {
            const r = await api.query(sql);
            setResult(r);
            setElapsed(performance.now() - t0);
            const next = [sql, ...history.filter(h => h !== sql)].slice(0, HISTORY_LIMIT);
            setHistory(next);
            saveHistory(next);
        }
        catch (e) {
            setError(e instanceof Error ? e.message : String(e));
            setElapsed(performance.now() - t0);
        }
        finally {
            setRunning(false);
        }
    }
    function onKey(e) {
        if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
            e.preventDefault();
            run();
        }
    }
    return (_jsxs("div", { className: "grid grid-cols-[1fr_280px] gap-5 h-full min-h-0", children: [_jsxs("div", { className: "flex flex-col gap-4 min-h-0", children: [_jsxs("div", { className: "card p-3", children: [_jsx("textarea", { ref: taRef, value: sql, onChange: e => setSql(e.target.value), onKeyDown: onKey, className: "w-full h-36 bg-transparent font-mono text-sm outline-none resize-y placeholder:text-text-mute", spellCheck: false, placeholder: "SELECT ..." }), _jsxs("div", { className: "flex items-center gap-2 mt-2", children: [_jsx("div", { className: "text-xs text-text-mute", children: "\u2318/Ctrl+Enter to run" }), _jsx("button", { onClick: () => setSql(SAMPLE), className: "btn-ghost ml-auto py-1.5", children: "Sample" }), _jsxs("button", { onClick: run, disabled: running, className: "btn-primary py-1.5", children: [_jsx(Icon, { name: "terminal", className: "w-4 h-4" }), running ? 'Running…' : 'Run'] })] })] }), error && (_jsx("div", { className: "rounded-[14px] border border-red-500/40 bg-red-500/10 p-4 text-sm text-red-300 font-mono whitespace-pre-wrap", children: error })), result && (_jsxs("div", { className: "card flex-1 min-h-0 overflow-hidden flex flex-col", children: [_jsxs("div", { className: "px-4 py-2 text-xs text-text-mute border-b border-border flex items-center gap-3", children: [_jsxs("span", { children: [result.row_count, " row", result.row_count === 1 ? '' : 's'] }), elapsed != null && _jsxs("span", { children: ["\u00B7 ", (elapsed / 1000).toFixed(2), "s"] }), _jsx("span", { className: "font-mono truncate", children: result.query_id }), _jsxs("button", { onClick: () => downloadCsv(`nova-query-${result.query_id ?? 'result'}.csv`, result.columns.map(c => c.name), result.rows), className: "btn-ghost ml-auto py-1 px-2 text-xs", children: [_jsx(Icon, { name: "download", className: "w-3.5 h-3.5" }), "CSV"] })] }), _jsx("div", { className: "overflow-auto flex-1", children: _jsxs("table", { className: "w-full text-sm font-mono", children: [_jsx("thead", { className: "text-text-mute sticky top-0 bg-bg-elev", children: _jsx("tr", { children: result.columns.map(c => _jsx("th", { className: "th px-4 py-2", children: c.name }, c.name)) }) }), _jsx("tbody", { children: result.rows.map((row, i) => (_jsx("tr", { className: "border-t border-border/60", children: row.map((cell, j) => _jsx("td", { className: "td px-4 text-text-dim", children: formatCell(cell) }, j)) }, i))) })] }) })] }))] }), _jsxs("aside", { className: "card p-4 overflow-auto", children: [_jsxs("div", { className: "flex items-center justify-between mb-3", children: [_jsx("h3", { className: "label", children: "History" }), _jsx("button", { onClick: () => { setHistory([]); saveHistory([]); }, className: "text-xs text-text-mute hover:text-text", children: "clear" })] }), history.length === 0 && _jsx("div", { className: "text-text-mute text-sm", children: "no queries yet" }), _jsx("ul", { className: "space-y-2", children: history.map((h, i) => (_jsx("li", { children: _jsx("button", { onClick: () => { setSql(h); taRef.current?.focus(); }, className: "w-full text-left text-xs font-mono text-text-dim hover:text-text break-all", title: h, children: h.length > 60 ? h.slice(0, 60) + '…' : h }) }, i))) })] })] }));
}
