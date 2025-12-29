import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/// Values that we pass to the `className`, e.g. `cn("text-sm text-muted-foreground", className)`
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
