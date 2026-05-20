import { Route, Routes } from 'react-router-dom'
import Shell from './components/Shell'
import Overview from './routes/Overview'
import Catalog from './routes/Catalog'
import Query from './routes/Query'
import Health from './routes/Health'
import Dashboards from './routes/Dashboards'

export default function App() {
  return (
    <Routes>
      <Route element={<Shell />}>
        <Route path="/"           element={<Overview   />} />
        <Route path="/catalog"    element={<Catalog    />} />
        <Route path="/query"      element={<Query      />} />
        <Route path="/dashboards" element={<Dashboards />} />
        <Route path="/health"     element={<Health     />} />
      </Route>
    </Routes>
  )
}
