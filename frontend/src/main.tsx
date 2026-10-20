import React from 'react'
import ReactDOM from 'react-dom/client'
import { BrowserRouter } from 'react-router-dom'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import App from './App'
import { ApiError, api } from './api'
import { hasTimeChoice, setTimeDisplay } from './format'
import { applyTheme, hasThemeChoice, initTheme } from './theme'
import './index.css'

// A browser with no choice of its own follows the person's saved settings; the first
// paint still uses the default, so an unreachable state store costs nothing.
const followTheme = !hasThemeChoice()
const followTime = !hasTimeChoice()
initTheme()
if (followTheme || followTime) {
  api
    .settings()
    .then((saved) => {
      if (followTheme && (saved.theme === 'dark' || saved.theme === 'light')) applyTheme(saved.theme)
      // `time.display`: the state store's keys are lowercase only (#124), so the
      // ticket's camelCase name cannot be the stored key.
      const time = saved['time.display']
      if (followTime && (time === 'local' || time === 'utc')) setTimeDisplay(time)
    })
    .catch(() => {
      // No state store, or not signed in yet: this browser's default stands.
    })
}

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      refetchOnWindowFocus: false,
      // A refused request (4xx) will not succeed on a second try; a server failure gets one
      // more shot, one second later. Either way the page answers in about two seconds, not seven.
      retry: (count, error) => !(error instanceof ApiError && error.status < 500) && count < 1,
      retryDelay: 1000,
    },
  },
})

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <BrowserRouter>
        <App />
      </BrowserRouter>
    </QueryClientProvider>
  </React.StrictMode>,
)
