import {
  brandColors,
  darkModeColors,
  DARK_BORDER,
  FONT_FAMILY_SANS,
  FONT_FAMILY_CODE,
} from '@metalbear/ui';

/** @type {import('tailwindcss').Config} */
export default {
  content: [
    './index.html',
    './src/**/*.{js,ts,jsx,tsx}',
    './node_modules/@metalbear/ui/**/*.{js,ts,jsx,tsx}',
  ],
  darkMode: 'class',
  theme: {
    extend: {
      fontFamily: {
        sans: FONT_FAMILY_SANS.split(', '),
        code: FONT_FAMILY_CODE.split(', '),
      },
      colors: {
        brand: brandColors,
        primary: {
          DEFAULT: brandColors.purple,
          light: brandColors.purpleMedium,
          dark: brandColors.purpleDark,
        },
        dark: {
          background: darkModeColors.background,
          foreground: darkModeColors.foreground,
          card: darkModeColors.card,
          muted: darkModeColors.muted,
          border: DARK_BORDER,
        },
      },
    },
  },
  plugins: [],
}
