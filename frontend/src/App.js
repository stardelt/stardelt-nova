import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { Route, Routes } from 'react-router-dom';
import Shell from './components/Shell';
import Overview from './routes/Overview';
import Catalog from './routes/Catalog';
import Query from './routes/Query';
import Data from './routes/Data';
import Jobs from './routes/Jobs';
import Lineage from './routes/Lineage';
import Health from './routes/Health';
import Dashboards from './routes/Dashboards';
export default function App() {
    return (_jsx(Routes, { children: _jsxs(Route, { element: _jsx(Shell, {}), children: [_jsx(Route, { path: "/", element: _jsx(Overview, {}) }), _jsx(Route, { path: "/catalog", element: _jsx(Catalog, {}) }), _jsx(Route, { path: "/query", element: _jsx(Query, {}) }), _jsx(Route, { path: "/data", element: _jsx(Data, {}) }), _jsx(Route, { path: "/jobs", element: _jsx(Jobs, {}) }), _jsx(Route, { path: "/lineage", element: _jsx(Lineage, {}) }), _jsx(Route, { path: "/dashboards", element: _jsx(Dashboards, {}) }), _jsx(Route, { path: "/health", element: _jsx(Health, {}) })] }) }));
}
