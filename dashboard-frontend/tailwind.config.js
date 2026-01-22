import {
  brandColors,
  lightModeColors,
  darkModeColors,
  LIGHT_BORDER,
  DARK_BORDER,
} from '@metalbear/ui';

/** @type {import('tailwindcss').Config} */
export default {
  content: [
    "./index.html",
    "./src/**/*.{js,ts,jsx,tsx}",
    "./node_modules/@metalbear/ui/**/*.{js,ts,jsx,tsx}",
  ],
  darkMode: 'class',
  theme: {
    extend: {
      fontFamily: {
        sans: ['Poppins', 'system-ui', 'sans-serif'],
        heading: ['Poppins', 'system-ui', 'sans-serif'],
        body: ['Poppins', 'system-ui', 'sans-serif'],
        code: ['Monaco', 'Consolas', 'monospace'],
      },
      colors: {
        // Brand colors from @metalbear/ui
        brand: brandColors,
        // Semantic colors
        primary: {
          DEFAULT: brandColors.purple,
          light: brandColors.purpleMedium,
          dark: brandColors.purpleDark,
        },
        secondary: {
          DEFAULT: brandColors.yellow,
        },
        destructive: {
          DEFAULT: brandColors.redBlush,
        },
        // Light mode
        light: {
          background: lightModeColors.background,
          foreground: lightModeColors.foreground,
          primary: lightModeColors.primary,
          secondary: lightModeColors.secondary,
          muted: lightModeColors.muted,
          accent: lightModeColors.accent,
          border: LIGHT_BORDER,
        },
        // Dark mode
        dark: {
          background: darkModeColors.background,
          foreground: darkModeColors.foreground,
          card: darkModeColors.card,
          muted: darkModeColors.muted,
          border: DARK_BORDER,
        },
      },
      boxShadow: {
        'brand': `0 4px 14px 0 ${brandColors.purple}59`,
        'brand-hover': `0 6px 20px 0 ${brandColors.purple}80`,
      },
    },
  },
  plugins: [],
}
