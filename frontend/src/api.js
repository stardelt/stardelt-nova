// API client for the Nova backend.
// Same-origin in production (backend serves static UI), proxied via vite dev server locally.
async function jget(path) {
    const r = await fetch(path);
    if (!r.ok)
        throw new Error(`${path} → ${r.status}`);
    return r.json();
}
async function jpost(path, body) {
    const r = await fetch(path, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(body),
    });
    if (!r.ok) {
        const text = await r.text();
        throw new Error(`${path} → ${r.status}: ${text}`);
    }
    return r.json();
}
export const api = {
    me: () => jget('/api/me'),
    trinoCluster: () => jget('/api/trino/cluster'),
    warehouse: () => jget('/api/catalog/warehouse'),
    namespaces: () => jget('/api/catalog/namespaces'),
    tables: (ns) => jget(`/api/catalog/namespaces/${ns}/tables`),
    tableMeta: (ns, tbl) => jget(`/api/catalog/tables/${ns}/${tbl}`),
    query: (sql) => jpost('/api/query', { sql }),
    jobs: () => jget('/api/jobs'),
    upload: (req) => jpost('/api/ingest/upload', req),
    transfer: (req) => jpost('/api/transfer', req),
};
/** Every table across every namespace (small catalogs; fine for the MVP). */
export async function allTables() {
    const { namespaces } = await api.namespaces();
    const refs = [];
    await Promise.all(namespaces.map(async (parts) => {
        const ns = parts.join('.');
        const { identifiers } = await api.tables(ns);
        for (const id of identifiers)
            refs.push({ ns, name: id.name });
    }));
    return refs.sort((a, b) => `${a.ns}.${a.name}`.localeCompare(`${b.ns}.${b.name}`));
}
export function formatCell(c) {
    if (c === null || c === undefined)
        return '∅';
    if (typeof c === 'string')
        return c;
    return JSON.stringify(c);
}
export function formatTime(ms) {
    if (!ms)
        return '—';
    const d = new Date(ms);
    return d.toLocaleString(undefined, {
        month: 'short',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit',
        second: '2-digit',
    });
}
export function downloadCsv(filename, columns, rows) {
    const escape = (v) => {
        const s = v === null || v === undefined ? '' : String(v);
        return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
    };
    const body = [columns.join(','), ...rows.map(r => r.map(escape).join(','))].join('\n');
    const blob = new Blob([body], { type: 'text/csv;charset=utf-8' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = filename;
    a.click();
    URL.revokeObjectURL(url);
}
