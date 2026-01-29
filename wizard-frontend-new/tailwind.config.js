import {
  brandColors,
  lightModeColors,
  darkModeColors,
  LIGHT_BORDER,
  DARK_BORDER,
  FONT_FAMILY_SANS,
  FONT_FAMILY_HEADING,
  FONT_FAMILY_BODY,
  FONT_FAMILY_CODE,
  FONT_SIZE_HEADING_1,
  FONT_SIZE_HEADING_2,
  FONT_SIZE_HEADING_3,
  FONT_SIZE_HEADING_4,
  FONT_SIZE_BODY_LG,
  FONT_SIZE_BODY_MD,
  FONT_SIZE_BODY_SM,
  FONT_WEIGHT_REGULAR,
  FONT_WEIGHT_MEDIUM,
  FONT_WEIGHT_SEMIBOLD,
  FONT_WEIGHT_BOLD,
  LINE_HEIGHT_TIGHT,
  LINE_HEIGHT_SNUG,
  LINE_HEIGHT_NORMAL,
  LINE_HEIGHT_RELAXED,
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
        sans: FONT_FAMILY_SANS.split(', '),
        heading: FONT_FAMILY_HEADING.split(', '),
        body: FONT_FAMILY_BODY.split(', '),
        code: FONT_FAMILY_CODE.split(', '),
      },
      fontSize: {
        'h1': [FONT_SIZE_HEADING_1, { lineHeight: LINE_HEIGHT_TIGHT }],
        'h2': [FONT_SIZE_HEADING_2, { lineHeight: LINE_HEIGHT_TIGHT }],
        'h3': [FONT_SIZE_HEADING_3, { lineHeight: LINE_HEIGHT_SNUG }],
        'h4': [FONT_SIZE_HEADING_4, { lineHeight: LINE_HEIGHT_SNUG }],
        'body-lg': [FONT_SIZE_BODY_LG, { lineHeight: LINE_HEIGHT_NORMAL }],
        'body-md': [FONT_SIZE_BODY_MD, { lineHeight: LINE_HEIGHT_NORMAL }],
        'body-sm': [FONT_SIZE_BODY_SM, { lineHeight: LINE_HEIGHT_NORMAL }],
      },
      fontWeight: {
        regular: FONT_WEIGHT_REGULAR,
        medium: FONT_WEIGHT_MEDIUM,
        semibold: FONT_WEIGHT_SEMIBOLD,
        bold: FONT_WEIGHT_BOLD,
      },
      lineHeight: {
        tight: LINE_HEIGHT_TIGHT,
        snug: LINE_HEIGHT_SNUG,
        normal: LINE_HEIGHT_NORMAL,
        relaxed: LINE_HEIGHT_RELAXED,
      },
      colors: {
        brand: brandColors,
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
        light: {
          background: lightModeColors.background,
          foreground: lightModeColors.foreground,
          primary: lightModeColors.primary,
          secondary: lightModeColors.secondary,
          muted: lightModeColors.muted,
          accent: lightModeColors.accent,
          border: LIGHT_BORDER,
        },
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
        'brutal': '-7px 6.5px 0px rgba(0,0,0,1)',
      },
      borderRadius: {
        lg: '0.75rem',
        md: '0.5rem',
        sm: '0.25rem',
      },
    },
  },
  plugins: [],
}
