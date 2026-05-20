import type { Config } from 'tailwindcss'

// Brand tokens mirror the marketing site (stardelt-www/assets/css/style.css)
// so Nova feels like the same product.
export default {
  content: ['./index.html', './src/**/*.{ts,tsx}'],
  theme: {
    extend: {
      colors: {
        bg:        '#0b0e1f',
        'bg-elev': '#11152b',
        'bg-soft': '#161b35',
        border:    '#232a4d',
        text:      '#e7ebff',
        'text-dim':'#a4abd0',
        'text-mute':'#7079a3',
        accent:       '#5b6cff',
        'accent-deep':'#1e2a78',
        success: '#5ee6a8',
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
