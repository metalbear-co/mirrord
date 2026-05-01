import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  Label,
  Switch,
} from '@metalbear/ui'
import { Monitor, Moon, Sun } from 'lucide-react'
import type { ThemePref } from '../theme'

interface Props {
  open: boolean
  onOpenChange: (open: boolean) => void
  theme: ThemePref
  onThemeChange: (t: ThemePref) => void
  telemetryEnabled: boolean
  onTelemetryChange: (enabled: boolean) => void
}

const THEME_OPTIONS: { value: ThemePref; label: string; icon: typeof Sun }[] = [
  { value: 'system', label: 'System', icon: Monitor },
  { value: 'light', label: 'Light', icon: Sun },
  { value: 'dark', label: 'Dark', icon: Moon },
]

export default function SettingsDialog({
  open,
  onOpenChange,
  theme,
  onThemeChange,
  telemetryEnabled,
  onTelemetryChange,
}: Props) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Settings</DialogTitle>
          <DialogDescription>
            Preferences for this browser. Stored locally and never sent
            anywhere.
          </DialogDescription>
        </DialogHeader>

        <div className="mt-2 space-y-6">
          <div className="space-y-2">
            <Label className="text-section">Appearance</Label>
            <p className="text-meta text-muted-foreground">
              Match your operating system, or pick a fixed theme.
            </p>
            <div className="inline-flex items-center rounded-md border border-border p-0.5 self-start">
              {THEME_OPTIONS.map((opt) => {
                const Icon = opt.icon
                const active = theme === opt.value
                return (
                  <Button
                    key={opt.value}
                    type="button"
                    variant="ghost"
                    size="sm"
                    onClick={() => onThemeChange(opt.value)}
                    className={
                      active
                        ? 'h-7 px-3 gap-1.5 text-foreground bg-muted'
                        : 'h-7 px-3 gap-1.5 text-muted-foreground hover:text-foreground'
                    }
                  >
                    <Icon className="h-3.5 w-3.5" />
                    {opt.label}
                  </Button>
                )
              })}
            </div>
          </div>

          <div className="flex items-start justify-between gap-6">
            <div className="space-y-1.5">
              <Label
                htmlFor="telemetry-toggle"
                className="text-section font-medium"
              >
                Share anonymous usage analytics
              </Label>
              <p className="text-meta text-muted-foreground leading-relaxed">
                Helps us see which parts of the session monitor are used. File
                paths, pod names, URLs, and request bodies are never captured —
                session replays mask all text and input values, so only click
                and scroll behavior is recorded.
              </p>
            </div>
            <Switch
              id="telemetry-toggle"
              checked={telemetryEnabled}
              onCheckedChange={onTelemetryChange}
              className="mt-1 shrink-0"
            />
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}
