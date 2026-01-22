import {
  brandColors,
  lightModeColors,
  darkModeColors,
  LIGHT_BORDER,
  DARK_BORDER,
  DARK_CARD,
} from '@metalbear/ui';

export { brandColors, lightModeColors, darkModeColors };

export const themeColors = {
  light: {
    background: lightModeColors.background,
    foreground: lightModeColors.foreground,
    card: '#FFFFFF',
    muted: lightModeColors.muted,
    'muted-foreground': '#6b6b80',
    border: LIGHT_BORDER,
    primary: lightModeColors.primary,
    secondary: lightModeColors.secondary,
    destructive: lightModeColors.destructive,
  },
  dark: {
    background: darkModeColors.background,
    foreground: darkModeColors.foreground,
    card: DARK_CARD,
    muted: darkModeColors.muted,
    'muted-foreground': '#a8a8c0',
    border: DARK_BORDER,
    primary: brandColors.purple,
    secondary: brandColors.yellow,
    destructive: brandColors.redBlush,
  },
} as const;

export function applyTheme(isDark: boolean): void {
  const colors = isDark ? themeColors.dark : themeColors.light;
  const root = document.documentElement;

  Object.entries(colors).forEach(([key, value]) => {
    root.style.setProperty(`--${key}`, value);
  });

  if (isDark) {
    root.classList.add('dark');
  } else {
    root.classList.remove('dark');
  }
}

export function getChartColors(isDark: boolean) {
  return {
    primary: brandColors.purple,           // #756DF3
    secondary: isDark ? '#a8a0f7' : '#ACACAC',  // lighter purple in dark mode, grey in light mode
    grid: isDark ? DARK_BORDER : LIGHT_BORDER,
    axis: isDark ? '#a8a8c0' : '#6b6b80',
    tooltipBg: isDark ? DARK_CARD : '#FFFFFF',
    tooltipBorder: isDark ? DARK_BORDER : LIGHT_BORDER,
    text: isDark ? darkModeColors.foreground : lightModeColors.foreground,
  };
}
