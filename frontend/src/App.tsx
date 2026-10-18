import { Route, Routes } from 'react-router-dom'
import RequireAdmin from './components/RequireAdmin'
import Shell from './components/Shell'
import Overview from './routes/Overview'
import Catalog from './routes/Catalog'
import Query from './routes/Query'
import Data from './routes/Data'
import Jobs from './routes/Jobs'
import Apps from './routes/Apps'
import AppDetail from './routes/AppDetail'
import Lineage from './routes/Lineage'
import Health from './routes/Health'
import Dashboards from './routes/Dashboards'
import Notebooks from './routes/Notebooks'
import Settings from './routes/Settings'

export default function App() {
  return (
    <Routes>
      <Route element={<Shell />}>
        <Route path="/"           element={<Overview   />} />
        <Route path="/catalog"    element={<Catalog    />} />
        <Route path="/query"      element={<Query      />} />
        <Route path="/data"       element={<Data       />} />
        <Route path="/jobs"       element={<Jobs       />} />
        <Route path="/apps"       element={<RequireAdmin><Apps      /></RequireAdmin>} />
        <Route path="/apps/:id"   element={<RequireAdmin><AppDetail /></RequireAdmin>} />
        <Route path="/lineage"    element={<Lineage    />} />
        <Route path="/dashboards" element={<Dashboards />} />
        <Route path="/notebooks"  element={<Notebooks  />} />
        <Route path="/health"     element={<RequireAdmin><Health    /></RequireAdmin>} />
        <Route path="/settings"   element={<RequireAdmin><Settings  /></RequireAdmin>} />
      </Route>
    </Routes>
  )
}
