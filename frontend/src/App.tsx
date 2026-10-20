import { Route, Routes } from 'react-router-dom'
import RequireAdmin from './components/RequireAdmin'
import Shell from './components/Shell'
import Overview from './routes/Overview'
import Catalog from './routes/Catalog'
import Query from './routes/Query'
import Data from './routes/Data'
import Connections from './routes/Connections'
import Jobs from './routes/Jobs'
import Pipelines from './routes/Pipelines'
import PipelineDetail from './routes/PipelineDetail'
import QueryLog from './routes/QueryLog'
import Apps from './routes/Apps'
import AppDetail from './routes/AppDetail'
import Lineage from './routes/Lineage'
import Health from './routes/Health'
import Dashboards from './routes/Dashboards'
import Notebooks from './routes/Notebooks'
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
        <Route path="/jobs"       element={<Jobs       />} />
        <Route path="/pipelines"  element={<Pipelines  />} />
        <Route path="/pipelines/:namespace/:name" element={<PipelineDetail />} />
        <Route path="/queries"    element={<QueryLog   />} />
        <Route path="/apps"       element={<RequireAdmin><Apps      /></RequireAdmin>} />
        <Route path="/apps/:id"   element={<RequireAdmin><AppDetail /></RequireAdmin>} />
        <Route path="/lineage"    element={<Lineage    />} />
        <Route path="/dashboards" element={<Dashboards />} />
        <Route path="/notebooks"  element={<Notebooks  />} />
        <Route path="/health"     element={<RequireAdmin><Health    /></RequireAdmin>} />
        <Route path="/settings"   element={<RequireAdmin><Settings  /></RequireAdmin>} />
        <Route path="/preferences" element={<Preferences />} />
        <Route path="/profile"   element={<Profile />} />
        {/* Last, so an unknown path renders the shell with a message instead of nothing. */}
        <Route path="*"           element={<NotFound   />} />
      </Route>
    </Routes>
  )
}
