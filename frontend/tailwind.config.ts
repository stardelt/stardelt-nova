import type { Config } from 'tailwindcss'

// Brand tokens are CSS variables so the whole UI can switch light/dark
// (see src/index.css). Channels are space-separated so Tailwind's
// alpha modifiers (e.g. bg-bg-elev/60) keep working.
export default {
  content: ['./index.html', './src/**/*.{ts,tsx}'],
  theme: {
    extend: {
      colors: {
        bg:           'rgb(var(--c-bg) / <alpha-value>)',
        'bg-elev':    'rgb(var(--c-bg-elev) / <alpha-value>)',
        'bg-soft':    'rgb(var(--c-bg-soft) / <alpha-value>)',
        border:       'rgb(var(--c-border) / <alpha-value>)',
        text:         'rgb(var(--c-text) / <alpha-value>)',
        'text-dim':   'rgb(var(--c-text-dim) / <alpha-value>)',
        'text-mute':  'rgb(var(--c-text-mute) / <alpha-value>)',
        accent:       'rgb(var(--c-accent) / <alpha-value>)',
        'accent-deep':'rgb(var(--c-accent-deep) / <alpha-value>)',
        success:      'rgb(var(--c-success) / <alpha-value>)',
      },
      fontFamily: {
        sans: ['-apple-system', 'BlinkMacSystemFont', 'Segoe UI', 'Inter', 'Helvetica', 'Arial', 'sans-serif'],
        mono: ['ui-monospace', 'SFMono-Regular', 'Menlo', 'Monaco', 'Consolas', 'monospace'],
      },
      borderRadius: {
        DEFAULT: '14px',
      },
    },
  },
} satisfies Config
