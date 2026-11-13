import { Navigate, Route, Routes } from 'react-router-dom'
import RequireAdmin from './components/RequireAdmin'
import Shell from './components/Shell'
import Overview from './routes/Overview'
import Catalog from './routes/Catalog'
import Query from './routes/Query'
import Data from './routes/Data'
import Connections from './routes/Connections'
import Destinations from './routes/Destinations'
import Jobs from './routes/Jobs'
import Pipelines from './routes/Pipelines'
import Runs from './routes/Runs'
import PipelineDetail from './routes/PipelineDetail'
import RunNotebook from './routes/RunNotebook'
import Streams from './routes/Streams'
import Compute from './routes/Compute'
import Deployments from './routes/Deployments'
import QueryLog from './routes/QueryLog'
import AppDetail from './routes/AppDetail'
import AdminCatalogs from './routes/AdminCatalogs'
import Lineage from './routes/Lineage'
import Platform from './routes/Platform'
import Dashboards from './routes/Dashboards'
import Notebooks from './routes/Notebooks'
import NotebookDetail from './routes/NotebookDetail'
import Settings from './routes/Settings'
import Preferences from './routes/Preferences'
import Profile from './routes/Profile'
import NotFound from './routes/NotFound'

export default function App() {
  return (
    <Routes>
      <Route element={<Shell />}>
        <Route path="/"           element={<Overview   />} />
        <Route path="/catalog"    element={<Catalog    />} />
        <Route path="/query"      element={<Query      />} />
        <Route path="/data"       element={<Data       />} />
        <Route path="/connections" element={<Connections />} />
        <Route path="/destinations" element={<Destinations />} />
        <Route path="/jobs"       element={<Jobs       />} />
        <Route path="/pipelines"  element={<Pipelines  />} />
        <Route path="/pipelines/:namespace/:name" element={<PipelineDetail />} />
        <Route path="/pipelines/:namespace/:name/runs/:runId/notebook/:taskId" element={<RunNotebook />} />
        <Route path="/runs" element={<Runs />} />
        <Route path="/streams" element={<Streams />} />
        <Route path="/compute" element={<Compute />} />
        <Route path="/deployments" element={<Deployments />} />
        <Route path="/queries"    element={<QueryLog   />} />
        <Route path="/apps"       element={<Navigate to="/platform" replace />} />
        <Route path="/apps/:id"   element={<RequireAdmin><AppDetail /></RequireAdmin>} />
        <Route path="/platform"   element={<RequireAdmin><Platform  /></RequireAdmin>} />
        <Route path="/admin/catalogs" element={<RequireAdmin><AdminCatalogs /></RequireAdmin>} />
        <Route path="/lineage"    element={<Lineage    />} />
        <Route path="/dashboards" element={<Dashboards />} />
        <Route path="/notebooks"  element={<Notebooks  />} />
        <Route path="/notebooks/:group/:name" element={<NotebookDetail />} />
        <Route path="/health"     element={<Navigate to="/platform" replace />} />
        <Route path="/settings"   element={<RequireAdmin><Settings  /></RequireAdmin>} />
        <Route path="/preferences" element={<Preferences />} />
        <Route path="/profile"   element={<Profile />} />
        {/* Last, so an unknown path renders the shell with a message instead of nothing. */}
        <Route path="*"           element={<NotFound   />} />
      </Route>
    </Routes>
  )
}
