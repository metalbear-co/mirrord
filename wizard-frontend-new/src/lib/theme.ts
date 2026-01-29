import {
  brandColors,
  lightModeColors,
  darkModeColors,
  LIGHT_BORDER,
  DARK_BORDER,
  DARK_CARD,
} from "@metalbear/ui";

export { brandColors, lightModeColors, darkModeColors };

export const themeColors = {
  light: {
    background: lightModeColors.background,
    foreground: lightModeColors.foreground,
    card: "#FFFFFF",
    muted: lightModeColors.muted,
    "muted-foreground": "#6b6b80",
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
    "muted-foreground": "#a8a8c0",
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
    root.classList.add("dark");
  } else {
    root.classList.remove("dark");
  }
}
