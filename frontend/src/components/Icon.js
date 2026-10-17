import { jsx as _jsx, Fragment as _Fragment, jsxs as _jsxs } from "react/jsx-runtime";
const paths = {
    grid: (_jsxs(_Fragment, { children: [_jsx("rect", { x: "3", y: "3", width: "7", height: "7", rx: "1.5" }), _jsx("rect", { x: "14", y: "3", width: "7", height: "7", rx: "1.5" }), _jsx("rect", { x: "3", y: "14", width: "7", height: "7", rx: "1.5" }), _jsx("rect", { x: "14", y: "14", width: "7", height: "7", rx: "1.5" })] })),
    database: (_jsxs(_Fragment, { children: [_jsx("ellipse", { cx: "12", cy: "5", rx: "8", ry: "3" }), _jsx("path", { d: "M4 5v6c0 1.7 3.6 3 8 3s8-1.3 8-3V5" }), _jsx("path", { d: "M4 11v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6" })] })),
    terminal: (_jsxs(_Fragment, { children: [_jsx("rect", { x: "3", y: "4", width: "18", height: "16", rx: "2" }), _jsx("path", { d: "m7 9 3 3-3 3" }), _jsx("path", { d: "M13 15h4" })] })),
    upload: (_jsxs(_Fragment, { children: [_jsx("path", { d: "M12 16V4" }), _jsx("path", { d: "m7 9 5-5 5 5" }), _jsx("path", { d: "M4 16v3a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-3" })] })),
    clock: (_jsxs(_Fragment, { children: [_jsx("circle", { cx: "12", cy: "12", r: "8.5" }), _jsx("path", { d: "M12 7.5V12l3 2" })] })),
    lineage: (_jsxs(_Fragment, { children: [_jsx("circle", { cx: "5", cy: "6", r: "2.2" }), _jsx("circle", { cx: "19", cy: "6", r: "2.2" }), _jsx("circle", { cx: "12", cy: "18", r: "2.2" }), _jsx("path", { d: "M7 7.2 10.4 16" }), _jsx("path", { d: "M17 7.2 13.6 16" }), _jsx("path", { d: "M7.2 6h9.6" })] })),
    chart: (_jsxs(_Fragment, { children: [_jsx("path", { d: "M4 20V4" }), _jsx("path", { d: "M4 20h16" }), _jsx("rect", { x: "7", y: "12", width: "3", height: "5", rx: "1" }), _jsx("rect", { x: "12", y: "8", width: "3", height: "9", rx: "1" }), _jsx("rect", { x: "17", y: "5", width: "3", height: "12", rx: "1" })] })),
    pulse: _jsx("path", { d: "M3 12h4l2.2-6 3.6 12 2.2-6H21" }),
    search: (_jsxs(_Fragment, { children: [_jsx("circle", { cx: "11", cy: "11", r: "7" }), _jsx("path", { d: "m20 20-3.6-3.6" })] })),
    plus: _jsx("path", { d: "M12 5v14M5 12h14" }),
    arrow: _jsx("path", { d: "M5 12h14m-6-6 6 6-6 6" }),
    download: (_jsxs(_Fragment, { children: [_jsx("path", { d: "M12 4v12" }), _jsx("path", { d: "m7 11 5 5 5-5" }), _jsx("path", { d: "M4 20h16" })] })),
    table: (_jsxs(_Fragment, { children: [_jsx("rect", { x: "3", y: "4", width: "18", height: "16", rx: "2" }), _jsx("path", { d: "M3 9h18" }), _jsx("path", { d: "M9 9v11" })] })),
    spark: (_jsxs(_Fragment, { children: [_jsx("path", { d: "M12 3v4M12 17v4M3 12h4M17 12h4" }), _jsx("path", { d: "m6 6 2.5 2.5M15.5 15.5 18 18M18 6l-2.5 2.5M8.5 15.5 6 18" }), _jsx("circle", { cx: "12", cy: "12", r: "3" })] })),
};
export function Icon({ name, className }) {
    return (_jsx("svg", { viewBox: "0 0 24 24", fill: "none", stroke: "currentColor", strokeWidth: "1.7", strokeLinecap: "round", strokeLinejoin: "round", className: className ?? 'w-4 h-4', "aria-hidden": "true", children: paths[name] }));
}
