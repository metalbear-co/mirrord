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
            Preferences for this browser. Stored locally and never sent anywhere.
          </DialogDescription>
        </DialogHeader>

        <div className="divide-border border-border mt-2 flex flex-col divide-y overflow-hidden rounded-lg border">
          <div className="bg-card flex items-center justify-between gap-4 px-4 py-3">
            <div className="min-w-0">
              <Label className="text-section font-medium">Appearance</Label>
              <p className="text-meta text-muted-foreground mt-0.5">
                Match your OS, or pick a fixed theme.
              </p>
            </div>
            <div className="border-border inline-flex shrink-0 items-center rounded-md border p-0.5">
              {THEME_OPTIONS.map((opt) => {
                const Icon = opt.icon
                const active = theme === opt.value
                return (
                  <Button
                    key={opt.value}
                    type="button"
                    variant="ghost"
                    size="sm"
                    aria-label={opt.label}
                    onClick={() => onThemeChange(opt.value)}
                    className={
                      active
                        ? 'text-foreground bg-muted h-7 gap-1.5 px-2.5'
                        : 'text-muted-foreground hover:text-foreground h-7 gap-1.5 px-2.5'
                    }
                  >
                    <Icon className="h-3.5 w-3.5" />
                    {opt.label}
                  </Button>
                )
              })}
            </div>
          </div>

          <div className="bg-card flex items-center justify-between gap-4 px-4 py-3">
            <div className="min-w-0">
              <Label htmlFor="telemetry-toggle" className="text-section font-medium">
                Anonymous usage analytics
              </Label>
              <p className="text-meta text-muted-foreground mt-0.5">
                Click + scroll behavior only. No paths, hosts, headers, or bodies.
              </p>
            </div>
            <Switch
              id="telemetry-toggle"
              checked={telemetryEnabled}
              onCheckedChange={onTelemetryChange}
              className="shrink-0"
            />
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}
